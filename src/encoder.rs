use crate::frame::{FrameSink, RenderedFrame};
use anyhow::{Context, Result, bail};
use ffmpeg_next as ffmpeg;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};

struct AvBufferRef(*mut ffmpeg::ffi::AVBufferRef);

impl AvBufferRef {
    fn new(pointer: *mut ffmpeg::ffi::AVBufferRef) -> Result<Self> {
        if pointer.is_null() {
            bail!("FFmpeg returned a null AVBufferRef");
        }
        Ok(Self(pointer))
    }

    fn as_ptr(&self) -> *mut ffmpeg::ffi::AVBufferRef {
        self.0
    }

    fn clone_ref(&self) -> Result<*mut ffmpeg::ffi::AVBufferRef> {
        let clone = unsafe { ffmpeg::ffi::av_buffer_ref(self.0) };
        if clone.is_null() {
            bail!("FFmpeg failed to clone an AVBufferRef");
        }
        Ok(clone)
    }
}

impl Drop for AvBufferRef {
    fn drop(&mut self) {
        unsafe { ffmpeg::ffi::av_buffer_unref(&mut self.0) };
    }
}

struct CudaFrames {
    frames: AvBufferRef,
}

impl CudaFrames {
    fn new(width: u32, height: u32) -> Result<Self> {
        let mut device = null_mut();
        let result = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device,
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                null(),
                null_mut(),
                0,
            )
        };
        if result < 0 {
            if !device.is_null() {
                let _ = AvBufferRef::new(device);
            }
            return Err(ffmpeg::Error::from(result))
                .context("create CUDA FFmpeg hardware device context");
        }
        let device = AvBufferRef::new(device).context("create CUDA FFmpeg hardware device context")?;

        let frames = unsafe { ffmpeg::ffi::av_hwframe_ctx_alloc(device.as_ptr()) };
        let frames = AvBufferRef::new(frames).context("allocate CUDA FFmpeg hardware frames context")?;
        let frames_context = unsafe { (*frames.as_ptr()).data as *mut ffmpeg::ffi::AVHWFramesContext };
        if frames_context.is_null() {
            bail!("allocate CUDA FFmpeg hardware frames context returned null data");
        }
        unsafe {
            // Fixed dimensions/format and 20 preallocated surfaces; FFmpeg's standard pool may grow.
            (*frames_context).initial_pool_size = 20;
            (*frames_context).format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA;
            (*frames_context).sw_format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P;
            (*frames_context).width = width as std::ffi::c_int;
            (*frames_context).height = height as std::ffi::c_int;
        }
        let result = unsafe { ffmpeg::ffi::av_hwframe_ctx_init(frames.as_ptr()) };
        if result < 0 {
            return Err(ffmpeg::Error::from(result))
                .context("initialize CUDA FFmpeg hardware frames context");
        }
        // The initialized frames context retains the device reference.
        drop(device);
        Ok(Self { frames })
    }

    fn as_ptr(&self) -> *mut ffmpeg::ffi::AVBufferRef {
        self.frames.as_ptr()
    }

    fn clone_ref(&self) -> Result<*mut ffmpeg::ffi::AVBufferRef> {
        self.frames.clone_ref()
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EncoderBackend {
    #[default]
    Pipe,
    FfmpegNext,
}

impl EncoderBackend {
    pub const ALL: [Self; 2] = [Self::Pipe, Self::FfmpegNext];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pipe => "PipeEncoder（外部 FFmpeg）",
            Self::FfmpegNext => "ffmpeg-next Encoder（自动选择）",
        }
    }

    pub fn cli_value(self) -> &'static str {
        match self {
            Self::Pipe => "pipe",
            Self::FfmpegNext => "ffmpeg-next",
        }
    }
}

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn staging_path(output: &Path) -> Result<PathBuf> {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let stem = output
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("output");
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    for _ in 0..1_000 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{stem}.goosy-{}-{sequence}{extension}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => {
                std::fs::remove_file(&candidate).with_context(|| {
                    format!("remove reserved staging file {}", candidate.display())
                })?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reserve staging file {}", candidate.display()));
            }
        }
    }
    bail!(
        "unable to choose a unique staging file beside {}",
        output.display()
    )
}

pub struct FfmpegNextEncoder {
    output: Option<ffmpeg::format::context::Output>,
    output_path: PathBuf,
    staging_path: Option<PathBuf>,
    audio_path: Option<PathBuf>,
    hw_frame: Option<ffmpeg::util::frame::video::Video>,
    encoder: ffmpeg::codec::encoder::video::Encoder,
    cuda_frames: Option<CudaFrames>,
    scaler: ffmpeg::software::scaling::Context,
    rgba: ffmpeg::util::frame::video::Video,
    yuv420p: ffmpeg::util::frame::video::Video,
    stream_index: usize,
    stream_time_base: ffmpeg::Rational,
    width: u32,
    height: u32,
    row_bytes: usize,
    pts: i64,
    finished: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EncoderProfile {
    name: &'static str,
    options: &'static [(&'static str, &'static str)],
}

const LIBX264_OPTIONS: &[(&str, &str)] = &[("preset", "veryfast"), ("crf", "20")];
#[cfg(any(target_os = "linux", target_os = "windows"))]
const NVENC_OPTIONS: &[(&str, &str)] = &[("preset", "p5"), ("b", "8M")];
#[cfg(target_os = "windows")]
const AMF_OPTIONS: &[(&str, &str)] = &[("quality", "quality"), ("b", "8M")];
#[cfg(target_os = "windows")]
const QSV_OPTIONS: &[(&str, &str)] = &[("b", "8M")];
#[cfg(target_os = "macos")]
const VIDEOTOOLBOX_OPTIONS: &[(&str, &str)] = &[("b", "8M")];

#[cfg(target_os = "linux")]
const ENCODER_PROFILES: &[EncoderProfile] = &[
    EncoderProfile {
        name: "h264_nvenc",
        options: NVENC_OPTIONS,
    },
    EncoderProfile {
        name: "libx264",
        options: LIBX264_OPTIONS,
    },
];

#[cfg(target_os = "macos")]
const ENCODER_PROFILES: &[EncoderProfile] = &[
    EncoderProfile {
        name: "h264_videotoolbox",
        options: VIDEOTOOLBOX_OPTIONS,
    },
    EncoderProfile {
        name: "libx264",
        options: LIBX264_OPTIONS,
    },
];

#[cfg(target_os = "windows")]
const ENCODER_PROFILES: &[EncoderProfile] = &[
    EncoderProfile {
        name: "h264_nvenc",
        options: NVENC_OPTIONS,
    },
    EncoderProfile {
        name: "h264_amf",
        options: AMF_OPTIONS,
    },
    EncoderProfile {
        name: "h264_qsv",
        options: QSV_OPTIONS,
    },
    EncoderProfile {
        name: "libx264",
        options: LIBX264_OPTIONS,
    },
];

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const ENCODER_PROFILES: &[EncoderProfile] = &[EncoderProfile {
    name: "libx264",
    options: LIBX264_OPTIONS,
}];

fn encoder_profiles() -> &'static [EncoderProfile] {
    ENCODER_PROFILES
}

fn configure_encoder_options(profile: &EncoderProfile) -> ffmpeg::Dictionary<'_> {
    let mut options = ffmpeg::Dictionary::new();
    for &(key, value) in profile.options {
        options.set(key, value);
    }
    options
}

fn select_encoder<'a, T, F>(
    profiles: &'a [EncoderProfile],
    mut try_open: F,
) -> Result<(&'a EncoderProfile, T)>
where
    F: FnMut(&EncoderProfile) -> Result<Option<T>>,
{
    let mut attempts = Vec::with_capacity(profiles.len());
    let mut available = false;
    for profile in profiles {
        match try_open(profile) {
            Ok(Some(value)) => return Ok((profile, value)),
            Ok(None) => attempts.push(format!("{}: unavailable", profile.name)),
            Err(error) => {
                available = true;
                attempts.push(format!("{}: {error:#}", profile.name));
            }
        }
    }
    if !available {
        bail!(
            "ffmpeg-next no candidate encoder is available (candidates: {})",
            attempts
                .iter()
                .map(|attempt| attempt.strip_suffix(": unavailable").unwrap_or(attempt))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    bail!(
        "ffmpeg-next could not open any candidate encoder: {}",
        attempts.join("; ")
    )
}

fn open_video_encoder(
    profile: &EncoderProfile,
    codec: ffmpeg::Codec,
    width: u32,
    height: u32,
    fps: u32,
    global_header: bool,
    cuda_frames: Option<&CudaFrames>,
) -> Result<ffmpeg::codec::encoder::video::Encoder> {
    let context = ffmpeg::codec::context::Context::new_with_codec(codec);
    let mut video = context
        .encoder()
        .video()
        .with_context(|| format!("create ffmpeg-next {} video encoder", profile.name))?;
    video.set_width(width);
    video.set_height(height);
    if profile.name == "h264_nvenc" {
        video.set_format(ffmpeg::format::Pixel::CUDA);
        let frames_ref = cuda_frames
            .context("CUDA frames context missing for h264_nvenc")?
            .clone_ref()
            .context("clone CUDA frames context for h264_nvenc")?;
        unsafe {
            (*video.as_mut_ptr()).hw_frames_ctx = frames_ref;
        }
    } else {
        video.set_format(ffmpeg::format::Pixel::YUV420P);
    }
    video.set_time_base((1, fps as i32));
    video.set_frame_rate(Some((fps as i32, 1)));
    if global_header {
        video.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
    }
    video
        .open_as_with(codec, configure_encoder_options(profile))
        .with_context(|| format!("open ffmpeg-next {} encoder", profile.name))
}

impl FfmpegNextEncoder {
    pub fn new(
        output: &Path,
        width: u32,
        height: u32,
        fps: u32,
        audio: Option<&Path>,
    ) -> Result<Self> {
        ffmpeg::init().context("initialize ffmpeg-next")?;
        let staging = audio.map(|_| staging_path(output)).transpose()?;
        let video_path = staging.as_deref().unwrap_or(output);
        let mut format = ffmpeg::format::output(video_path)
            .with_context(|| format!("open ffmpeg-next output {}", video_path.display()))?;
        let global_header = format
            .format()
            .flags()
            .contains(ffmpeg::format::Flags::GLOBAL_HEADER);
        let (profile, (codec, encoder, cuda_frames)) = select_encoder(encoder_profiles(), |profile| {
            let Some(codec) = ffmpeg::encoder::find_by_name(profile.name) else {
                return Ok(None);
            };
            let cuda_frames = (profile.name == "h264_nvenc")
                .then(|| CudaFrames::new(width, height))
                .transpose()
                .with_context(|| format!("initialize CUDA frames for {}", profile.name))?;
            let encoder = open_video_encoder(
                profile,
                codec,
                width,
                height,
                fps,
                global_header,
                cuda_frames.as_ref(),
            )?;
            Ok(Some((codec, encoder, cuda_frames)))
        })
        .with_context(|| {
            format!(
                "select ffmpeg-next video encoder for {}",
                video_path.display()
            )
        })?;
        eprintln!("ffmpeg-next selected video encoder: {}", profile.name);
        let mut stream = format
            .add_stream(codec)
            .context("add ffmpeg-next video stream")?;
        stream.set_parameters(&encoder);
        stream.set_time_base((1, fps as i32));
        let stream_index = stream.index();
        format
            .write_header()
            .context("write ffmpeg-next output header")?;
        let stream_time_base = format
            .stream(stream_index)
            .context("ffmpeg-next output stream disappeared")?
            .time_base();
        let scaler = ffmpeg::software::scaling::Context::get(
            ffmpeg::format::Pixel::RGBA,
            width,
            height,
            ffmpeg::format::Pixel::YUV420P,
            width,
            height,
            ffmpeg::software::scaling::flag::Flags::BILINEAR,
        )
        .context("create RGBA to YUV420P scaler")?;
        Ok(Self {
            output: Some(format),
            output_path: output.to_owned(),
            staging_path: staging,
            audio_path: audio.map(Path::to_owned),
            hw_frame: cuda_frames
                .as_ref()
                .map(|_| ffmpeg::util::frame::video::Video::empty()),
            encoder,
            cuda_frames,
            scaler,
            rgba: ffmpeg::util::frame::video::Video::new(
                ffmpeg::format::Pixel::RGBA,
                width,
                height,
            ),
            yuv420p: ffmpeg::util::frame::video::Video::new(
                ffmpeg::format::Pixel::YUV420P,
                width,
                height,
            ),
            stream_index,
            stream_time_base,
            width,
            height,
            row_bytes: width as usize * 4,
            pts: 0,
            finished: false,
        })
    }

    fn validate(&self, frame: &RenderedFrame) -> Result<()> {
        if frame.width() != self.width
            || frame.height() != self.height
            || frame.row_bytes() != self.row_bytes
            || frame.pixels().len() != self.height as usize * self.row_bytes
        {
            bail!(
                "invalid frame dimensions or pixel buffer: expected {}x{} with {} bytes per row",
                self.width,
                self.height,
                self.row_bytes
            );
        }
        Ok(())
    }

    fn write_packets(&mut self) -> Result<()> {
        let output = self
            .output
            .as_mut()
            .context("ffmpeg-next output is closed")?;
        loop {
            let mut packet = ffmpeg::Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    packet.set_stream(self.stream_index);
                    packet.rescale_ts(self.encoder.time_base(), self.stream_time_base);
                    packet
                        .write_interleaved(output)
                        .context("write ffmpeg-next video packet")?;
                }
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => {
                    break;
                }
                Err(ffmpeg::Error::Eof) => break,
                Err(error) => return Err(error).context("receive ffmpeg-next video packet"),
            }
        }
        Ok(())
    }

    fn mux_audio(&self, staging: &Path, audio: &Path) -> Result<()> {
        let mut command = Command::new("ffmpeg");
        command
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(staging)
            .args(["-i"])
            .arg(audio)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:v",
                "copy",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-movflags",
                "+faststart",
                "-shortest",
            ])
            .arg(&self.output_path);
        let display = format!("{command:?}");
        let result = command
            .output()
            .with_context(|| format!("run audio mux command: {display}"))?;
        if !result.status.success() {
            bail!(
                "audio mux failed ({})\ncommand: {display}\nstderr: {}",
                result.status,
                String::from_utf8_lossy(&result.stderr).trim()
            );
        }
        Ok(())
    }
}

impl FrameSink for FfmpegNextEncoder {
    fn submit(&mut self, frame: RenderedFrame) -> Result<RenderedFrame> {
        if self.finished {
            bail!("ffmpeg-next encoder is already finished");
        }
        self.validate(&frame)?;
        let stride = self.rgba.stride(0);
        for row in 0..self.height as usize {
            let source = &frame.pixels()[row * frame.row_bytes()..(row + 1) * frame.row_bytes()];
            let destination =
                &mut self.rgba.data_mut(0)[row * stride..row * stride + self.row_bytes];
            destination.copy_from_slice(source);
        }
        let frame_pts = self.pts;
        self.rgba.set_pts(Some(frame_pts));
        self.pts += 1;
        self.scaler
            .run(&self.rgba, &mut self.yuv420p)
            .context("convert RGBA frame to YUV420P")?;
        self.yuv420p.set_pts(Some(frame_pts));
        if let (Some(cuda_frames), Some(hw_frame)) =
            (self.cuda_frames.as_ref(), self.hw_frame.as_mut())
        {
            unsafe { ffmpeg::ffi::av_frame_unref(hw_frame.as_mut_ptr()) };
            let result = unsafe {
                ffmpeg::ffi::av_hwframe_get_buffer(cuda_frames.as_ptr(), hw_frame.as_mut_ptr(), 0)
            };
            if result < 0 {
                return Err(ffmpeg::Error::from(result))
                    .context("acquire CUDA hardware frame from pool");
            }
            hw_frame.set_pts(Some(frame_pts));
            let result = unsafe {
                ffmpeg::ffi::av_hwframe_transfer_data(
                    hw_frame.as_mut_ptr(),
                    self.yuv420p.as_ptr(),
                    0,
                )
            };
            if result < 0 {
                return Err(ffmpeg::Error::from(result))
                    .context("transfer YUV420P frame to CUDA hardware frame");
            }
            self.encoder
                .send_frame(hw_frame)
                .context("send CUDA frame to ffmpeg-next encoder")?;
        } else {
            self.encoder
                .send_frame(&self.yuv420p)
                .context("send frame to ffmpeg-next encoder")?;
        }
        self.write_packets()?;
        Ok(frame)
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            bail!("ffmpeg-next encoder already finished");
        }
        self.finished = true;
        self.encoder
            .send_eof()
            .context("send EOF to ffmpeg-next encoder")?;
        self.write_packets()?;
        self.output
            .as_mut()
            .context("ffmpeg-next output is closed")?
            .write_trailer()
            .context("write ffmpeg-next output trailer")?;
        self.output.take();
        if let (Some(staging), Some(audio)) = (&self.staging_path, &self.audio_path) {
            self.mux_audio(staging, audio)
                .with_context(|| format!("mux staged video into {}", self.output_path.display()))?;
            std::fs::remove_file(staging)
                .with_context(|| format!("remove staged video {}", staging.display()))?;
        }
        Ok(())
    }
}

impl Drop for FfmpegNextEncoder {
    fn drop(&mut self) {
        self.output.take();
        if let Some(staging) = &self.staging_path {
            let _ = std::fs::remove_file(staging);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EncoderProfile, encoder_profiles, select_encoder};

    #[test]
    fn candidate_order_prefers_platform_hardware_and_ends_with_libx264() {
        let names: Vec<_> = encoder_profiles()
            .iter()
            .map(|profile| profile.name)
            .collect();
        #[cfg(target_os = "linux")]
        assert_eq!(names, ["h264_nvenc", "libx264"]);
        #[cfg(target_os = "macos")]
        assert_eq!(names, ["h264_videotoolbox", "libx264"]);
        #[cfg(target_os = "windows")]
        assert_eq!(names, ["h264_nvenc", "h264_amf", "h264_qsv", "libx264"]);
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        assert_eq!(names, ["libx264"]);
        assert_eq!(names.last(), Some(&"libx264"));
    }

    #[test]
    fn candidate_profiles_keep_exact_encoder_options() {
        for profile in encoder_profiles() {
            let expected = match profile.name {
                "libx264" => [("preset", "veryfast"), ("crf", "20")].as_slice(),
                "h264_nvenc" => [("preset", "p5"), ("b", "8M")].as_slice(),
                "h264_amf" => [("quality", "quality"), ("b", "8M")].as_slice(),
                "h264_qsv" | "h264_videotoolbox" => [("b", "8M")].as_slice(),
                name => panic!("unexpected encoder profile {name}"),
            };
            assert_eq!(profile.options, expected);
        }
    }

    #[test]
    fn hardware_open_failure_falls_back_to_next_candidate() {
        let profiles = [
            EncoderProfile {
                name: "hardware",
                options: &[],
            },
            EncoderProfile {
                name: "software",
                options: &[],
            },
        ];
        let (selected, value) = select_encoder(&profiles, |profile| {
            if profile.name == "hardware" {
                Err(anyhow::anyhow!("driver unavailable"))
            } else {
                Ok(Some(42))
            }
        })
        .unwrap();

        assert_eq!(selected.name, "software");
        assert_eq!(value, 42);
    }

    #[test]
    fn no_available_candidate_reports_candidate_names() {
        let profiles = [
            EncoderProfile {
                name: "hardware",
                options: &[],
            },
            EncoderProfile {
                name: "software",
                options: &[],
            },
        ];
        let error = select_encoder(&profiles, |_| Ok::<Option<()>, anyhow::Error>(None))
            .expect_err("selection should fail when every codec is unavailable");

        let message = error.to_string();
        assert!(message.contains("no candidate encoder is available"));
        assert!(message.contains("hardware"));
        assert!(message.contains("software"));
    }
}
