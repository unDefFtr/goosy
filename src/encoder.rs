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

#[cfg(target_os = "linux")]
unsafe extern "C" fn free_gpu_payload(opaque: *mut std::ffi::c_void, _data: *mut u8) {
    if !opaque.is_null() {
        drop(unsafe {
            Box::from_raw(opaque as *mut std::sync::Arc<crate::gpu_interop::GpuFramePayload>)
        });
    }
}

struct CudaFrames {
    frames: AvBufferRef,
}

impl CudaFrames {
    fn new(width: u32, height: u32) -> Result<Self> {
        Self::new_with_format(
            width,
            height,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P,
        )
    }

    fn new_with_format(
        width: u32,
        height: u32,
        sw_format: ffmpeg::ffi::AVPixelFormat,
    ) -> Result<Self> {
        Self::new_with_format_and_context(width, height, sw_format, false)
    }

    fn new_with_format_and_context(
        width: u32,
        height: u32,
        sw_format: ffmpeg::ffi::AVPixelFormat,
        primary_context: bool,
    ) -> Result<Self> {
        let mut options = null_mut();
        if primary_context {
            let key = std::ffi::CString::new("current_ctx").unwrap();
            let value = std::ffi::CString::new("1").unwrap();
            unsafe {
                ffmpeg::ffi::av_dict_set(&mut options, key.as_ptr(), value.as_ptr(), 0);
            }
        }
        let mut device = null_mut();
        let result = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device,
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                null(),
                options,
                0,
            )
        };
        unsafe { ffmpeg::ffi::av_dict_free(&mut options) };
        if result < 0 {
            if !device.is_null() {
                let _ = AvBufferRef::new(device);
            }
            return Err(ffmpeg::Error::from(result))
                .context("create CUDA FFmpeg hardware device context");
        }
        let device =
            AvBufferRef::new(device).context("create CUDA FFmpeg hardware device context")?;

        let frames = unsafe { ffmpeg::ffi::av_hwframe_ctx_alloc(device.as_ptr()) };
        let frames =
            AvBufferRef::new(frames).context("allocate CUDA FFmpeg hardware frames context")?;
        let frames_context =
            unsafe { (*frames.as_ptr()).data as *mut ffmpeg::ffi::AVHWFramesContext };
        if frames_context.is_null() {
            bail!("allocate CUDA FFmpeg hardware frames context returned null data");
        }
        unsafe {
            (*frames_context).initial_pool_size = 20;
            (*frames_context).format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA;
            (*frames_context).sw_format = sw_format;
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
    #[cfg(target_os = "linux")]
    FfmpegNextNvencZeroCopy,
}

impl EncoderBackend {
    pub const ALL: &'static [Self] = &[
        Self::Pipe,
        Self::FfmpegNext,
        #[cfg(target_os = "linux")]
        Self::FfmpegNextNvencZeroCopy,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Pipe => "PipeEncoder（外部 FFmpeg）",
            Self::FfmpegNext => "ffmpeg-next Encoder（自动选择）",
            #[cfg(target_os = "linux")]
            Self::FfmpegNextNvencZeroCopy => "Vulkan/CUDA/NVENC zero-copy",
        }
    }

    pub fn cli_value(self) -> &'static str {
        match self {
            Self::Pipe => "pipe",
            Self::FfmpegNext => "ffmpeg-next",
            #[cfg(target_os = "linux")]
            Self::FfmpegNextNvencZeroCopy => "ffmpeg-next-nvenc-zero-copy",
        }
    }

    pub fn is_zero_copy(self) -> bool {
        #[cfg(target_os = "linux")]
        return matches!(self, Self::FfmpegNextNvencZeroCopy);
        #[cfg(not(target_os = "linux"))]
        false
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NvencPreset {
    P1,
    P2,
    P3,
    P4,
    #[default]
    P5,
    P6,
    P7,
}
impl NvencPreset {
    pub fn from_value(value: &str) -> Self {
        match value { "p1" => Self::P1, "p2" => Self::P2, "p3" => Self::P3, "p4" => Self::P4, "p6" => Self::P6, "p7" => Self::P7, _ => Self::P5 }
    }
}

impl NvencPreset {
    pub const ALL: &'static [Self] = &[Self::P1, Self::P2, Self::P3, Self::P4, Self::P5, Self::P6, Self::P7];
    pub const fn value(self) -> &'static str {
        match self { Self::P1 => "p1", Self::P2 => "p2", Self::P3 => "p3", Self::P4 => "p4", Self::P5 => "p5", Self::P6 => "p6", Self::P7 => "p7" }
    }
}
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod backend_tests {
    use super::EncoderBackend;

    #[test]
    fn backend_cli_literals_are_stable() {
        assert_eq!(EncoderBackend::Pipe.cli_value(), "pipe");
        assert_eq!(EncoderBackend::FfmpegNext.cli_value(), "ffmpeg-next");
        #[cfg(target_os = "linux")]
        assert_eq!(
            EncoderBackend::FfmpegNextNvencZeroCopy.cli_value(),
            "ffmpeg-next-nvenc-zero-copy"
        );
    }

    #[test]
    fn zero_copy_profile_is_explicit_only() {
        assert!(!EncoderBackend::Pipe.is_zero_copy());
        assert!(!EncoderBackend::FfmpegNext.is_zero_copy());
        #[cfg(target_os = "linux")]
        assert!(EncoderBackend::FfmpegNextNvencZeroCopy.is_zero_copy());
    }
}

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
    scaler: Option<ffmpeg::software::scaling::Context>,
    rgba: ffmpeg::util::frame::video::Video,
    yuv420p: ffmpeg::util::frame::video::Video,
    stream_index: usize,
    stream_time_base: ffmpeg::Rational,
    width: u32,
    height: u32,
    row_bytes: usize,
    pts: i64,
    finished: bool,
    zero_copy: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EncoderProfile {
    name: &'static str,
    options: &'static [(&'static str, &'static str)],
}

const LIBX264_OPTIONS: &[(&str, &str)] = &[("preset", "veryfast"), ("crf", "20")];
const NVENC_OPTIONS: &[(&str, &str)] = &[
    ("preset", "p5"),
    ("b", "8M"),
    ("bf", "0"),
    ("rc-lookahead", "0"),
    ("zerolatency", "1"),
];
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
    nvenc_preset: NvencPreset,
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
    let mut options = configure_encoder_options(profile);
    if profile.name == "h264_nvenc" {
        options.set("preset", nvenc_preset.value());
    }
    video
        .open_as_with(codec, options)
        .with_context(|| format!("open ffmpeg-next {} encoder", profile.name))
}
impl FfmpegNextEncoder {
    pub fn new(output: &Path, width: u32, height: u32, fps: u32, audio: Option<&Path>, nvenc_preset: NvencPreset) -> Result<Self> {
        Self::new_internal(output, width, height, fps, audio, false, nvenc_preset)
    }

    #[cfg(target_os = "linux")]
    pub fn new_zero_copy(output: &Path, width: u32, height: u32, fps: u32, audio: Option<&Path>, nvenc_preset: NvencPreset) -> Result<Self> {
        Self::new_internal(output, width, height, fps, audio, true, nvenc_preset)
    }

    fn new_internal(output: &Path, width: u32, height: u32, fps: u32, audio: Option<&Path>, zero_copy: bool, nvenc_preset: NvencPreset) -> Result<Self> {
        ffmpeg::init().context("initialize ffmpeg-next")?;
        let staging = audio.map(|_| staging_path(output)).transpose()?;
        let video_path = staging.as_deref().unwrap_or(output);
        let mut format = ffmpeg::format::output(video_path)
            .with_context(|| format!("open ffmpeg-next output {}", video_path.display()))?;
        let global_header = format
            .format()
            .flags()
            .contains(ffmpeg::format::Flags::GLOBAL_HEADER);
        let (profile, (codec, encoder, cuda_frames)) =
            select_encoder(encoder_profiles(), |profile| {
                let Some(codec) = ffmpeg::encoder::find_by_name(profile.name) else {
                    return Ok(None);
                };
                if zero_copy && profile.name != "h264_nvenc" {
                    return Ok(None);
                }
                let cuda_frames = (profile.name == "h264_nvenc")
                    .then(|| {
                        CudaFrames::new_with_format_and_context(
                            width,
                            height,
                            if zero_copy {
                                ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12
                            } else {
                                ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_YUV420P
                            },
                            zero_copy,
                        )
                    })
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
                    nvenc_preset,
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
        let scaler = (!zero_copy)
            .then(|| {
                ffmpeg::software::scaling::Context::get(
                    ffmpeg::format::Pixel::RGBA,
                    width,
                    height,
                    ffmpeg::format::Pixel::YUV420P,
                    width,
                    height,
                    ffmpeg::software::scaling::flag::Flags::BILINEAR,
                )
            })
            .transpose()
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
            zero_copy,
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

    fn submit_zero_copy(&mut self, mut frame: RenderedFrame) -> Result<RenderedFrame> {
        let payload = frame
            .gpu_payload()
            .context("zero-copy encoder requires a GPU frame payload")?
            .clone()
            .downcast::<crate::gpu_interop::GpuFramePayload>()
            .map_err(|_| anyhow::anyhow!("zero-copy frame payload has an incompatible owner"))?;
        if frame.width() != self.width || frame.height() != self.height {
            bail!("zero-copy frame dimensions do not match encoder");
        }
        let layout = payload.layout();
        if layout.width != self.width
            || layout.height != self.height
            || payload.pointer() == 0
            || payload.mapping_size() < layout.size as u64
            || layout.y_pitch > i32::MAX as usize
            || layout.uv_pitch > i32::MAX as usize
            || layout
                .uv_offset
                .checked_add(layout.uv_pitch * (layout.height / 2) as usize)
                .is_none_or(|end| end > payload.mapping_size() as usize)
        {
            bail!("zero-copy frame has invalid CUDA NV12 allocation bounds or pitch");
        }
        payload.wait_for_cuda(self.pts.max(0) as u64)?;
        payload.claim_encoder()?;
        let pointer = payload.pointer();
        frame.clear_gpu_payload();
        let frames = self
            .cuda_frames
            .as_ref()
            .context("zero-copy encoder has no CUDA frames context")?;
        let mut hw_frame = ffmpeg::util::frame::video::Video::empty();
        let frames_ref = frames
            .clone_ref()
            .context("clone CUDA frames context for zero-copy frame")?;
        let opaque = Box::into_raw(Box::new(payload)) as *mut std::ffi::c_void;
        let buffer = unsafe {
            ffmpeg::ffi::av_buffer_create(
                pointer as *mut u8,
                layout.size,
                Some(free_gpu_payload),
                opaque,
                0,
            )
        };
        if buffer.is_null() {
            unsafe {
                drop(Box::from_raw(
                    opaque as *mut std::sync::Arc<crate::gpu_interop::GpuFramePayload>,
                ));
                ffmpeg::ffi::av_buffer_unref(&mut (frames_ref as *mut _));
            }
            bail!("FFmpeg failed to create CUDA frame owner buffer");
        }
        unsafe {
            let raw = hw_frame.as_mut_ptr();
            (*raw).format = ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA as i32;
            (*raw).width = self.width as i32;
            (*raw).height = self.height as i32;
            (*raw).pts = self.pts;
            (*raw).data[0] = pointer as *mut u8;
            (*raw).data[1] = pointer
                .checked_add(layout.uv_offset as u64)
                .context("NV12 UV pointer overflow")? as *mut u8;
            (*raw).extended_data = (*raw).data.as_mut_ptr();
            (*raw).linesize[0] = layout.y_pitch as i32;
            (*raw).linesize[1] = layout.uv_pitch as i32;
            (*raw).buf[0] = buffer;
            (*raw).hw_frames_ctx = frames_ref;
        }
        let send_result = self.encoder.send_frame(&hw_frame);
        unsafe { ffmpeg::ffi::av_frame_unref(hw_frame.as_mut_ptr()) };
        if let Err(error) = send_result {
            return Err(error).context("send zero-copy CUDA frame to NVENC");
        }
        self.pts += 1;
        self.write_packets()?;
        Ok(frame)
    }
}

#[cfg(target_os = "linux")]
const ZERO_COPY_QUEUE_CAPACITY: usize = 8;

// Keep rendering ahead of NVENC so the render thread does not wait on each
// CUDA semaphore before submitting the next Vulkan frame.

// FFmpeg owns the CUDA and scaler contexts exclusively after construction;
// the zero-copy worker is their sole user, so ownership can move to it.
#[cfg(target_os = "linux")]
unsafe impl Send for FfmpegNextEncoder {}

#[cfg(target_os = "linux")]
pub struct AsyncZeroCopyEncoder {
    sender: Option<std::sync::mpsc::SyncSender<RenderedFrame>>,
    worker: Option<std::thread::JoinHandle<Result<()>>>,
    spare: RenderedFrame,
    width: u32,
    height: u32,
}

#[cfg(target_os = "linux")]
impl AsyncZeroCopyEncoder {
    pub fn new(
        output: &Path,
        width: u32,
        height: u32,
        fps: u32,
        audio: Option<&Path>,
        nvenc_preset: NvencPreset,
    ) -> Result<Self> {
        let encoder = FfmpegNextEncoder::new_zero_copy(
            output,
            width,
            height,
            fps,
            audio,
            nvenc_preset,
        )?;
        let (sender, frames) =
            std::sync::mpsc::sync_channel::<RenderedFrame>(ZERO_COPY_QUEUE_CAPACITY);
        let worker = std::thread::Builder::new()
            .name("nvenc-zero-copy".to_owned())
            .spawn(move || {
                let mut encoder = encoder;
                for mut frame in frames {
                    frame.drop_cpu_pixels();
                    encoder.submit_zero_copy(frame)?;
                }
                encoder.finish()
            })
            .context("spawn zero-copy encoder thread")?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
            spare: RenderedFrame::new(width, height),
            width,
            height,
        })
    }
}

#[cfg(target_os = "linux")]
impl FrameSink for AsyncZeroCopyEncoder {
    fn submit(&mut self, frame: RenderedFrame) -> Result<RenderedFrame> {
        if frame.width() != self.width || frame.height() != self.height {
            bail!(
                "invalid frame dimensions: expected {}x{}",
                self.width,
                self.height
            );
        }
        let result = self
            .sender
            .as_ref()
            .context("zero-copy encoder is already finished")?
            .send(frame);
        let spare = std::mem::replace(
            &mut self.spare,
            RenderedFrame::new(self.width, self.height),
        );
        if let Err(error) = result {
            self.sender.take();
            self.spare = spare;
            return Err(error).context("send frame to zero-copy encoder");
        }
        Ok(spare)
    }

    fn finish(&mut self) -> Result<()> {
        self.sender.take();
        let worker = self
            .worker
            .take()
            .context("zero-copy encoder already finished")?;
        match worker.join() {
            Ok(result) => result,
            Err(_) => bail!("zero-copy encoder thread panicked"),
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for AsyncZeroCopyEncoder {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl FrameSink for FfmpegNextEncoder {
    fn submit(&mut self, frame: RenderedFrame) -> Result<RenderedFrame> {
        if self.finished {
            bail!("ffmpeg-next encoder is already finished");
        }
        if self.zero_copy {
            #[cfg(target_os = "linux")]
            return self.submit_zero_copy(frame);
            #[cfg(not(target_os = "linux"))]
            unreachable!("zero-copy encoder is Linux-only");
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
            .as_mut()
            .context("ordinary encoder has no RGBA scaler")?
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
                "h264_nvenc" => [
                    ("preset", "p5"),
                    ("b", "8M"),
                    ("bf", "0"),
                    ("rc-lookahead", "0"),
                    ("zerolatency", "1"),
                ]
                .as_slice(),
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
