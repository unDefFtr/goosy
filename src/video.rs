use crate::frame::{FrameSink, RenderedFrame};
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::LazyLock;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct AudioMetadata {
    pub duration_seconds: f64,
    pub title: Option<String>,
    pub lyrics: Option<String>,
}

fn probe_audio(song: &Path) -> Result<AudioMetadata> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:format_tags",
            "-of",
            "json",
        ])
        .arg(song)
        .output()
        .with_context(|| format!("run ffprobe for {}", song.display()))?;
    if !output.status.success() {
        bail!(
            "ffprobe failed for {}: {}",
            song.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse ffprobe JSON")?;
    let format = document
        .get("format")
        .context("ffprobe JSON has no format")?;
    let duration_seconds = format
        .get("duration")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .parse::<f64>()
        .with_context(|| format!("parse audio duration for {}", song.display()))?;
    let tags = format.get("tags").and_then(serde_json::Value::as_object);
    let find_tag = |names: &[&str]| {
        tags.and_then(|tags| {
            tags.iter()
                .find(|(key, _)| names.iter().any(|name| key.eq_ignore_ascii_case(name)))
                .and_then(|(_, value)| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        })
    };
    Ok(AudioMetadata {
        duration_seconds,
        title: find_tag(&["title", "sort_name", "©nam"]),
        lyrics: find_tag(&["lyrics", "lyric", "©lyr"]),
    })
}

pub fn audio_metadata(song: &Path) -> Result<AudioMetadata> {
    probe_audio(song)
}

pub fn embedded_cover_image(song: &Path) -> Result<Option<Vec<u8>>> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(song)
        .args([
            "-map",
            "0:v:0?",
            "-an",
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-c:v",
            "png",
            "pipe:1",
        ])
        .output()
        .with_context(|| format!("run ffmpeg to extract cover from {}", song.display()))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(output.stdout))
}

static FFMPEG_ENCODERS: LazyLock<Option<String>> = LazyLock::new(|| {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
});

#[derive(Clone, Debug, Eq, PartialEq)]
enum VideoEncoder {
    Software,
    #[cfg(target_os = "macos")]
    VideoToolbox,
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Nvenc,
    #[cfg(target_os = "windows")]
    Amf,
    #[cfg(target_os = "windows")]
    Qsv,
    #[cfg(target_os = "linux")]
    Vaapi {
        device: PathBuf,
    },
}

impl VideoEncoder {
    fn name(&self) -> &'static str {
        match self {
            Self::Software => "libx264",
            #[cfg(target_os = "macos")]
            Self::VideoToolbox => "h264_videotoolbox",
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            Self::Nvenc => "h264_nvenc",
            #[cfg(target_os = "windows")]
            Self::Amf => "h264_amf",
            #[cfg(target_os = "windows")]
            Self::Qsv => "h264_qsv",
            #[cfg(target_os = "linux")]
            Self::Vaapi { .. } => "h264_vaapi",
        }
    }
}

fn encoder_available_in(encoders: &str, name: &str) -> bool {
    encoders.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next().is_some() && fields.next() == Some(name)
    })
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn encoder_available(name: &str) -> bool {
    FFMPEG_ENCODERS
        .as_deref()
        .is_some_and(|encoders| encoder_available_in(encoders, name))
}

#[cfg(target_os = "linux")]
fn select_linux_video_encoder(
    encoders: &str,
    render_nodes: &[PathBuf],
    mut probe: impl FnMut(&VideoEncoder) -> bool,
) -> VideoEncoder {
    if encoder_available_in(encoders, "h264_nvenc") {
        let encoder = VideoEncoder::Nvenc;
        if probe(&encoder) {
            return encoder;
        }
    }
    if encoder_available_in(encoders, "h264_vaapi") {
        let mut render_nodes = render_nodes.to_vec();
        render_nodes.sort();
        for device in render_nodes {
            let encoder = VideoEncoder::Vaapi { device };
            if probe(&encoder) {
                return encoder;
            }
        }
    }
    VideoEncoder::Software
}

#[cfg(target_os = "linux")]
fn linux_render_nodes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        return Vec::new();
    };
    let mut nodes = entries
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("renderD"))
                .then(|| entry.path())
        })
        .collect::<Vec<_>>();
    nodes.sort();
    nodes
}

#[cfg(target_os = "linux")]
fn probe_linux_video_encoder(encoder: &VideoEncoder) -> bool {
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error"]);
    if let VideoEncoder::Vaapi { device } = encoder {
        command.arg("-vaapi_device").arg(device);
    }
    command.args([
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=256x256:d=0.04",
        "-frames:v",
        "1",
        "-an",
    ]);
    match encoder {
        VideoEncoder::Nvenc => {
            command.args(["-c:v", "h264_nvenc", "-preset", "p5", "-b:v", "8M"]);
        }
        VideoEncoder::Vaapi { .. } => {
            command.args([
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-b:v",
                "8M",
            ]);
        }
        VideoEncoder::Software => return false,
    }
    command
        .args(["-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "windows")]
fn windows_encoder_usable(encoder: &VideoEncoder) -> bool {
    if !encoder_available(encoder.name()) {
        return false;
    }
    Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:d=0.04",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            encoder.name(),
            "-f",
            "null",
            "-",
        ])
        .status()
        .is_ok_and(|status| status.success())
}

fn detect_preferred_video_encoder() -> VideoEncoder {
    #[cfg(target_os = "linux")]
    {
        return select_linux_video_encoder(
            FFMPEG_ENCODERS.as_deref().unwrap_or_default(),
            &linux_render_nodes(),
            probe_linux_video_encoder,
        );
    }
    #[cfg(target_os = "macos")]
    {
        return if encoder_available("h264_videotoolbox") {
            VideoEncoder::VideoToolbox
        } else {
            VideoEncoder::Software
        };
    }
    #[cfg(target_os = "windows")]
    {
        return [VideoEncoder::Nvenc, VideoEncoder::Amf, VideoEncoder::Qsv]
            .into_iter()
            .find(windows_encoder_usable)
            .unwrap_or(VideoEncoder::Software);
    }
    #[allow(unreachable_code)]
    VideoEncoder::Software
}

static PREFERRED_VIDEO_ENCODER: LazyLock<VideoEncoder> =
    LazyLock::new(detect_preferred_video_encoder);

pub fn preferred_video_encoder_name() -> &'static str {
    PREFERRED_VIDEO_ENCODER.name()
}

fn add_video_encoder(command: &mut Command, encoder: &VideoEncoder) {
    match encoder {
        #[cfg(target_os = "macos")]
        VideoEncoder::VideoToolbox => {
            command.args([
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "h264_videotoolbox",
                "-b:v",
                "8M",
            ]);
        }
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        VideoEncoder::Nvenc => {
            command.args([
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "h264_nvenc",
                "-preset",
                "p5",
                "-b:v",
                "8M",
            ]);
        }
        #[cfg(target_os = "windows")]
        VideoEncoder::Amf => {
            command.args([
                "-pix_fmt", "yuv420p", "-c:v", "h264_amf", "-quality", "quality", "-b:v", "8M",
            ]);
        }
        #[cfg(target_os = "windows")]
        VideoEncoder::Qsv => {
            command.args(["-pix_fmt", "yuv420p", "-c:v", "h264_qsv", "-b:v", "8M"]);
        }
        #[cfg(target_os = "linux")]
        VideoEncoder::Vaapi { .. } => {
            command.args([
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-b:v",
                "8M",
            ]);
        }
        VideoEncoder::Software => {
            command.args([
                "-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "veryfast", "-crf", "20",
            ]);
        }
    };
}

struct FfmpegPipe {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    command: String,
}

fn video_writer_command(
    output: &Path,
    width: u32,
    height: u32,
    fps: u32,
    audio: Option<&Path>,
    encoder: &VideoEncoder,
) -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-y", "-hide_banner", "-loglevel", "error"]);
    #[cfg(target_os = "linux")]
    if let VideoEncoder::Vaapi { device } = encoder {
        command.arg("-vaapi_device").arg(device);
    }
    command.args([
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-s",
        &format!("{width}x{height}"),
        "-r",
        &fps.to_string(),
        "-i",
        "pipe:0",
    ]);
    if let Some(audio) = audio {
        command
            .arg("-i")
            .arg(audio)
            .args(["-map", "0:v:0", "-map", "1:a:0"]);
    }
    add_video_encoder(&mut command, encoder);
    if audio.is_some() {
        command.args(["-c:a", "aac", "-b:a", "192k"]);
    }
    command.args(["-movflags", "+faststart"]);
    if audio.is_some() {
        command.arg("-shortest");
    }
    command.arg(output);
    command
}

impl FfmpegPipe {
    fn new(output: &Path, width: u32, height: u32, fps: u32, audio: Option<&Path>) -> Result<Self> {
        let mut command =
            video_writer_command(output, width, height, fps, audio, &PREFERRED_VIDEO_ENCODER);
        command.stdin(Stdio::piped()).stderr(Stdio::inherit());
        let command_display = format!("{command:?}");
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn ffmpeg: {command_display}"))?;
        let stdin = child.stdin.take().context("ffmpeg stdin was not piped")?;
        Ok(Self {
            child: Some(child),
            stdin: Some(stdin),
            command: command_display,
        })
    }

    fn write_frame(&mut self, pixels: &[u8]) -> Result<()> {
        if let Err(error) = self
            .stdin
            .as_mut()
            .context("video writer is already finished")?
            .write_all(pixels)
        {
            self.stdin.take();
            let output = self
                .child
                .take()
                .context("ffmpeg child already finished")?
                .wait_with_output()
                .context("wait for failed ffmpeg")?;
            bail!(
                "write RGBA frame to ffmpeg: {error}\ncommand: {}\nstderr: {}",
                self.command,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.stdin.take();
        let output = self
            .child
            .take()
            .context("ffmpeg child already finished")?
            .wait_with_output()
            .context("wait for ffmpeg")?;
        if !output.status.success() {
            bail!(
                "ffmpeg failed ({})\ncommand: {}\nstderr: {}",
                output.status,
                self.command,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

const FRAME_QUEUE_CAPACITY: usize = 1;

pub struct PipeEncoder {
    sender: Option<std::sync::mpsc::SyncSender<RenderedFrame>>,
    recycled: std::sync::mpsc::Receiver<RenderedFrame>,
    worker: Option<std::thread::JoinHandle<Result<()>>>,
    width: u32,
    height: u32,
    row_bytes: usize,
}

impl PipeEncoder {
    pub fn new(
        output: &Path,
        width: u32,
        height: u32,
        fps: u32,
        audio: Option<&Path>,
    ) -> Result<Self> {
        let writer = FfmpegPipe::new(output, width, height, fps, audio)?;
        let (sender, frames) = std::sync::mpsc::sync_channel::<RenderedFrame>(FRAME_QUEUE_CAPACITY);
        let (recycle_sender, recycled) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("ffmpeg-writer".to_owned())
            .spawn(move || {
                let mut writer = writer;
                for frame in frames {
                    let write_result = writer.write_frame(frame.pixels());
                    let _ = recycle_sender.try_send(frame);
                    write_result?;
                }
                writer.finish()
            })
            .context("spawn ffmpeg writer thread")?;
        Ok(Self {
            sender: Some(sender),
            recycled,
            worker: Some(worker),
            width,
            height,
            row_bytes: width as usize * 4,
        })
    }
}

impl FrameSink for PipeEncoder {
    fn submit(&mut self, frame: RenderedFrame) -> Result<RenderedFrame> {
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
        let sender = self
            .sender
            .as_ref()
            .context("video writer is already finished")?;
        if let Err(error) = sender.send(frame) {
            self.sender.take();
            drop(error.0);
            bail!("send frame to ffmpeg writer: worker stopped");
        }
        loop {
            match self
                .recycled
                .recv_timeout(std::time::Duration::from_millis(10))
            {
                Ok(frame) => return Ok(frame),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if self
                        .worker
                        .as_ref()
                        .is_some_and(std::thread::JoinHandle::is_finished)
                    {
                        bail!("ffmpeg writer stopped before recycling a frame");
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("ffmpeg writer stopped before recycling a frame");
                }
            }
        }
    }
    fn finish(&mut self) -> Result<()> {
        self.sender.take();
        let worker = self
            .worker
            .take()
            .context("ffmpeg writer already finished")?;
        match worker.join() {
            Ok(result) => result,
            Err(_) => bail!("ffmpeg writer thread panicked"),
        }
    }
}

pub fn create_encoder(
    backend: crate::encoder::EncoderBackend,
    output: &Path,
    width: u32,
    height: u32,
    fps: u32,
    audio: Option<&Path>,
) -> Result<Box<dyn FrameSink>> {
    match backend {
        crate::encoder::EncoderBackend::Pipe => Ok(Box::new(PipeEncoder::new(
            output, width, height, fps, audio,
        )?)),
        crate::encoder::EncoderBackend::FfmpegNext => Ok(Box::new(
            crate::encoder::FfmpegNextEncoder::new(output, width, height, fps, audio)?,
        )),
        #[cfg(target_os = "linux")]
        crate::encoder::EncoderBackend::FfmpegNextNvencZeroCopy => Ok(Box::new(
            crate::encoder::FfmpegNextEncoder::new_zero_copy(output, width, height, fps, audio)?,
        )),
    }
}

#[cfg(target_os = "linux")]
#[test]
fn zero_copy_backend_never_constructs_rawvideo_pipe() {
    let error = match create_encoder(
        crate::encoder::EncoderBackend::FfmpegNextNvencZeroCopy,
        Path::new("/tmp/zero-copy-test.mp4"),
        2,
        2,
        30,
        None,
    ) {
        Ok(_) => panic!("zero-copy requires an interop sink"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("zero-copy"));
    assert!(!error.to_string().contains("rawvideo"));
}

#[cfg(test)]
fn test_encoder(width: u32, height: u32) -> PipeEncoder {
    let (sender, _frames) = std::sync::mpsc::sync_channel::<RenderedFrame>(1);
    let (_recycle_sender, recycled) = std::sync::mpsc::sync_channel::<RenderedFrame>(1);
    PipeEncoder {
        sender: Some(sender),
        recycled,
        worker: None,
        width,
        height,
        row_bytes: width as usize * 4,
    }
}
#[test]
fn pipe_encoder_returns_the_submitted_frame_buffer() {
    let (sender, frames) = std::sync::mpsc::sync_channel::<RenderedFrame>(1);
    let (recycle_sender, recycled) = std::sync::mpsc::sync_channel::<RenderedFrame>(1);
    let worker = std::thread::spawn(move || {
        for frame in frames {
            recycle_sender.send(frame).unwrap();
        }
        Ok(())
    });
    let mut encoder = PipeEncoder {
        sender: Some(sender),
        recycled,
        worker: Some(worker),
        width: 1,
        height: 1,
        row_bytes: 4,
    };
    let frame = RenderedFrame::new(1, 1);
    let pointer = frame.pixels().as_ptr();
    let returned = encoder.submit(frame).unwrap();
    assert_eq!(returned.pixels().as_ptr(), pointer);
    encoder.finish().unwrap();
}

#[test]
fn pipe_encoder_rejects_invalid_frame_contract() {
    let mut encoder = test_encoder(2, 2);
    let error = encoder.submit(RenderedFrame::new(1, 2)).unwrap_err();
    assert!(error.to_string().contains("invalid frame dimensions"));
}

#[test]
fn pipe_encoder_reports_stopped_worker() {
    let mut encoder = test_encoder(1, 1);
    let error = encoder.submit(RenderedFrame::new(1, 1)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "send frame to ffmpeg writer: worker stopped"
    );
}

#[test]
fn pipe_encoder_reports_repeated_finish() {
    let mut encoder = test_encoder(1, 1);
    assert_eq!(
        encoder.finish().unwrap_err().to_string(),
        "ffmpeg writer already finished"
    );
    assert_eq!(
        encoder.finish().unwrap_err().to_string(),
        "ffmpeg writer already finished"
    );
}

#[test]
fn pipe_encoder_reports_worker_panic() {
    let mut encoder = test_encoder(1, 1);
    encoder.worker = Some(std::thread::spawn(|| -> Result<()> {
        panic!("test panic")
    }));
    assert_eq!(
        encoder.finish().unwrap_err().to_string(),
        "ffmpeg writer thread panicked"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn contains_sequence(args: &[String], expected: &[&str]) -> bool {
        args.windows(expected.len()).any(|window| {
            window
                .iter()
                .map(String::as_str)
                .eq(expected.iter().copied())
        })
    }

    #[test]
    fn encoder_listing_matches_the_exact_codec_token() {
        let encoders = " V....D h264_nvenc NVIDIA NVENC\n V..... h264_nvenc_extra not-real\n";
        assert!(encoder_available_in(encoders, "h264_nvenc"));
        assert!(!encoder_available_in(encoders, "nvenc"));
        assert!(!encoder_available_in(encoders, "h264_nv"));
        assert!(!encoder_available_in(encoders, "h264_nvenc_extra_suffix"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_selection_prefers_usable_nvenc() {
        let mut probes = Vec::new();
        let selected = select_linux_video_encoder(
            " V....D h264_nvenc NVENC\n V..... h264_vaapi VAAPI\n",
            &[PathBuf::from("/dev/dri/renderD128")],
            |encoder| {
                probes.push(encoder.clone());
                matches!(encoder, VideoEncoder::Nvenc)
            },
        );
        assert_eq!(selected, VideoEncoder::Nvenc);
        assert_eq!(probes, [VideoEncoder::Nvenc]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_selection_uses_first_usable_sorted_vaapi_node_after_nvenc_failure() {
        let selected = select_linux_video_encoder(
            " V....D h264_nvenc NVENC\n V..... h264_vaapi VAAPI\n",
            &[
                PathBuf::from("/dev/dri/renderD129"),
                PathBuf::from("/dev/dri/renderD128"),
            ],
            |encoder| match encoder {
                VideoEncoder::Nvenc => false,
                VideoEncoder::Vaapi { device } => device.ends_with("renderD128"),
                VideoEncoder::Software => false,
            },
        );
        assert_eq!(
            selected,
            VideoEncoder::Vaapi {
                device: PathBuf::from("/dev/dri/renderD128")
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_selection_falls_back_when_every_probe_fails() {
        let selected = select_linux_video_encoder(
            " V....D h264_nvenc NVENC\n V..... h264_vaapi VAAPI\n",
            &[PathBuf::from("/dev/dri/renderD128")],
            |_| false,
        );
        assert_eq!(selected, VideoEncoder::Software);
    }

    #[test]
    fn software_command_keeps_raw_rgba_and_libx264_settings() {
        let command = video_writer_command(
            Path::new("out.mp4"),
            1_920,
            1_080,
            30,
            None,
            &VideoEncoder::Software,
        );
        let args = command_args(&command);
        assert!(contains_sequence(
            &args,
            &["-f", "rawvideo", "-pix_fmt", "rgba"]
        ));
        assert!(contains_sequence(
            &args,
            &[
                "-pix_fmt", "yuv420p", "-c:v", "libx264", "-preset", "veryfast", "-crf", "20"
            ]
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn vaapi_command_reuses_probed_node_and_uploads_nv12() {
        let device = PathBuf::from("/dev/dri/renderD137");
        let encoder = VideoEncoder::Vaapi {
            device: device.clone(),
        };
        let command = video_writer_command(Path::new("out.mp4"), 1_920, 1_080, 30, None, &encoder);
        let args = command_args(&command);
        let device_index = args.iter().position(|arg| arg == "-vaapi_device").unwrap();
        let input_index = args.iter().position(|arg| arg == "-f").unwrap();

        assert_eq!(args[device_index + 1], device.to_string_lossy());
        assert!(device_index < input_index);
        assert!(contains_sequence(
            &args,
            &[
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "h264_vaapi",
                "-b:v",
                "8M"
            ]
        ));
        assert!(!args.iter().any(|arg| arg == "yuv420p"));
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn nvenc_command_keeps_existing_quality_settings() {
        let command = video_writer_command(
            Path::new("out.mp4"),
            1_920,
            1_080,
            30,
            None,
            &VideoEncoder::Nvenc,
        );
        let args = command_args(&command);
        assert!(contains_sequence(
            &args,
            &["-c:v", "h264_nvenc", "-preset", "p5", "-b:v", "8M"]
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn videotoolbox_command_keeps_existing_bitrate() {
        let command = video_writer_command(
            Path::new("out.mp4"),
            1_920,
            1_080,
            30,
            None,
            &VideoEncoder::VideoToolbox,
        );
        assert!(contains_sequence(
            &command_args(&command),
            &["-c:v", "h264_videotoolbox", "-b:v", "8M"]
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_hardware_commands_keep_existing_settings() {
        let amf = command_args(&video_writer_command(
            Path::new("out.mp4"),
            1_920,
            1_080,
            30,
            None,
            &VideoEncoder::Amf,
        ));
        let qsv = command_args(&video_writer_command(
            Path::new("out.mp4"),
            1_920,
            1_080,
            30,
            None,
            &VideoEncoder::Qsv,
        ));
        assert!(contains_sequence(
            &amf,
            &["-c:v", "h264_amf", "-quality", "quality", "-b:v", "8M"]
        ));
        assert!(contains_sequence(&qsv, &["-c:v", "h264_qsv", "-b:v", "8M"]));
    }
}
