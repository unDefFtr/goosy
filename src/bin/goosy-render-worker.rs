use anyhow::Result;
#[cfg(target_os = "windows")]
use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};
use libgoosy::{
    EncoderBackend, LyricFormat, LyricsStyle, RenderControl, RenderOptions, VideoCodec,
};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
#[derive(Parser)]
#[command(
    name = "goosy-render-worker",
    about = "Isolated Goosy rendering worker"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Render {
        #[arg(required_unless_present = "workspace")]
        song: Option<PathBuf>,
        /// Resume a managed task using the options saved in its workspace.
        #[arg(long, value_name = "DIRECTORY")]
        workspace: Option<PathBuf>,
        #[arg(value_name = "LYRICS")]
        lyrics: Option<PathBuf>,
        #[arg(short, long, default_value = "out.mp4")]
        output: PathBuf,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        #[arg(long, default_value_t = 1.0)]
        font_scale: f32,
        #[arg(long, default_value_t = 0.65)]
        translation_font_scale: f32,
        #[arg(long, default_value_t = 0.7)]
        background_font_scale: f32,
        #[arg(long, default_value_t = 0.65)]
        line_height_scale: f32,
        #[arg(long, default_value_t = 0.8)]
        line_spacing_scale: f32,
        #[arg(long, default_value_t = 1.0)]
        translation_gap_scale: f32,
        #[arg(long, default_value_t = 1.0)]
        background_gap_scale: f32,
        #[arg(long, default_value_t = 1.0)]
        horizontal_padding_scale: f32,
        #[arg(long, default_value_t = 6)]
        lyric_blur_sigma_step: u32,
        #[arg(long, help = "draw lyric containers, glyph boxes, and gaps")]
        debug_overlays: bool,
        #[arg(long, hide = true)]
        sample_start_ms: Option<u64>,
        #[arg(long, hide = true)]
        sample_duration_ms: Option<u64>,
        #[arg(long)]
        background: Option<PathBuf>,
        #[arg(long)]
        cover: Option<PathBuf>,
        #[arg(long, visible_alias = "song-name")]
        title: Option<String>,
        #[arg(long)]
        no_embedded_cover: bool,
        #[arg(long)]
        no_audio: bool,
        #[arg(long)]
        no_translation: bool,
        #[arg(long)]
        no_background_vocal: bool,
        #[arg(long = "exclude-line", value_delimiter = ',')]
        exclude_lines: Vec<usize>,
        #[arg(long, hide = true)]
        progress_events: bool,
        #[arg(long, hide = true)]
        preview_dir: Option<PathBuf>,
        #[arg(
            long,
            hide = true,
            default_value_t = 15,
            value_parser = clap::value_parser!(u64).range(1..)
        )]
        preview_interval: u64,
        #[arg(long, hide = true, value_enum, default_value_t = WorkerEncoder::Pipe)]
        encoder: WorkerEncoder,
        #[arg(long, hide = true, value_enum, default_value_t = WorkerCodec::H264)]
        codec: WorkerCodec,
        #[arg(long, hide = true, default_value = "p5")]
        nvenc_preset: String,
        #[arg(long, value_enum, default_value_t = WorkerLyricFormat::Auto)]
        format: WorkerLyricFormat,
    },
    Pdf {
        song: PathBuf,
        #[arg(value_name = "LYRICS")]
        lyrics: Option<PathBuf>,
        #[arg(short, long, default_value = "lyrics.pdf")]
        output: PathBuf,
        #[arg(long, visible_alias = "song-name")]
        title: Option<String>,
        #[arg(long)]
        no_translation: bool,
        #[arg(long)]
        no_background_vocal: bool,
        #[arg(long)]
        speed_printer: bool,
        #[arg(long = "exclude-line", value_delimiter = ',')]
        exclude_lines: Vec<usize>,
        #[arg(long, value_enum, default_value_t = WorkerLyricFormat::Auto)]
        format: WorkerLyricFormat,
    },
    #[cfg(target_os = "windows")]
    #[command(hide = true)]
    BackendProbe {
        #[arg(long)]
        width: u32,
        #[arg(long)]
        height: u32,
    },
    #[cfg(target_os = "windows")]
    #[command(hide = true)]
    RendererProbe {
        #[arg(long)]
        width: u32,
        #[arg(long)]
        height: u32,
        #[arg(long, default_value_t = 30)]
        fps: u32,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum WorkerLyricFormat {
    Auto,
    Lrc,
    Ttml,
    Yrc,
}

impl From<WorkerLyricFormat> for LyricFormat {
    fn from(value: WorkerLyricFormat) -> Self {
        match value {
            WorkerLyricFormat::Auto => Self::Auto,
            WorkerLyricFormat::Lrc => Self::Lrc,
            WorkerLyricFormat::Ttml => Self::Ttml,
            WorkerLyricFormat::Yrc => Self::Yrc,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum, Eq, PartialEq)]
enum WorkerEncoder {
    #[default]
    Pipe,
    #[value(name = "ffmpeg-next")]
    FfmpegNext,
    #[cfg(target_os = "linux")]
    #[value(name = "ffmpeg-next-nvenc-zero-copy")]
    FfmpegNextNvencZeroCopy,
}

impl From<WorkerEncoder> for EncoderBackend {
    fn from(value: WorkerEncoder) -> Self {
        match value {
            WorkerEncoder::Pipe => Self::Pipe,
            WorkerEncoder::FfmpegNext => Self::FfmpegNext,
            #[cfg(target_os = "linux")]
            WorkerEncoder::FfmpegNextNvencZeroCopy => Self::FfmpegNextNvencZeroCopy,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum, Eq, PartialEq)]
enum WorkerCodec {
    #[default]
    H264,
    #[value(alias = "h265")]
    Hevc,
}

impl From<WorkerCodec> for VideoCodec {
    fn from(value: WorkerCodec) -> Self {
        match value {
            WorkerCodec::H264 => Self::H264,
            WorkerCodec::Hevc => Self::Hevc,
        }
    }
}

fn emit_stage(enabled: bool, key: &str, message: &str) -> Result<()> {
    if enabled {
        writeln!(std::io::stdout(), "GOOSY_STAGE {key} {message}")?;
        std::io::stdout().flush()?;
    }
    Ok(())
}

// Optional protocol events. Older clients ignore these lines and retain the same exit status.
fn emit_control(enabled: bool, state: &str) {
    if enabled {
        let _ = writeln!(std::io::stdout(), "GOOSY_CONTROL {state}");
        let _ = std::io::stdout().flush();
    }
}

#[cfg(target_os = "windows")]
fn ensure_renderer_backend(width: u32, height: u32, fps: u32, progress_events: bool) -> Result<()> {
    let requested = std::env::var("GOOSY_RENDER_BACKEND")
        .unwrap_or_else(|_| "auto".to_owned())
        .trim()
        .to_ascii_lowercase();
    emit_stage(
        progress_events,
        "probing-render-backend",
        "正在隔离检测图形后端",
    )?;
    let executable = std::env::current_exe().context("locate rendering worker")?;
    let backend_probe = |backend: &str| -> Result<std::process::ExitStatus> {
        std::process::Command::new(&executable)
            .arg("backend-probe")
            .arg("--width")
            .arg(width.to_string())
            .arg("--height")
            .arg(height.to_string())
            .env("GOOSY_RENDER_BACKEND", backend)
            .status()
            .with_context(|| format!("run {backend} backend probe"))
    };
    let renderer_probe = |backend: &str| -> Result<std::process::ExitStatus> {
        emit_stage(
            progress_events,
            "probing-renderer-initialization",
            "正在隔离检测文本与首帧初始化",
        )?;
        std::process::Command::new(&executable)
            .arg("renderer-probe")
            .arg("--width")
            .arg(width.to_string())
            .arg("--height")
            .arg(height.to_string())
            .arg("--fps")
            .arg(fps.to_string())
            .env("GOOSY_RENDER_BACKEND", backend)
            .status()
            .with_context(|| format!("run {backend} renderer initialization probe"))
    };
    let automatic = requested == "auto";
    let requested_backend = requested.as_str();
    let candidates: &[&str] = if automatic {
        &["d3d12", "angle", "raster"]
    } else {
        std::slice::from_ref(&requested_backend)
    };
    for backend in candidates {
        let backend_status = backend_probe(backend)?;
        if !backend_status.success() {
            if !automatic {
                bail!("requested {backend} backend probe failed: {backend_status}");
            }
            eprintln!("goosy: {backend} backend probe failed ({backend_status}); trying fallback");
            continue;
        }
        let renderer_status = renderer_probe(backend)?;
        if !renderer_status.success() {
            if !automatic {
                bail!(
                    "requested {backend} renderer initialization probe failed: {renderer_status}"
                );
            }
            eprintln!(
                "goosy: {backend} renderer initialization probe failed ({renderer_status}); trying fallback"
            );
            continue;
        }
        // SAFETY: the worker is single-threaded before renderer construction.
        unsafe { std::env::set_var("GOOSY_RENDER_BACKEND", backend) };
        return Ok(());
    }
    bail!("all renderer backend probes failed")
}

#[cfg(not(target_os = "windows"))]
fn ensure_renderer_backend(
    _width: u32,
    _height: u32,
    _fps: u32,
    _progress_events: bool,
) -> Result<()> {
    Ok(())
}

const PREVIEW_MAX_WIDTH: u32 = 640;
const PREVIEW_MAX_HEIGHT: u32 = 360;

fn preview_dimensions(width: u32, height: u32) -> (u32, u32) {
    let scale = (PREVIEW_MAX_WIDTH as f64 / width.max(1) as f64)
        .min(PREVIEW_MAX_HEIGHT as f64 / height.max(1) as f64)
        .min(1.0);
    (
        (width as f64 * scale).round().max(1.0) as u32,
        (height as f64 * scale).round().max(1.0) as u32,
    )
}

fn downscale_rgba_nearest(
    source: &[u8],
    width: u32,
    height: u32,
    preview: &mut Vec<u8>,
) -> std::io::Result<(u32, u32)> {
    let source_size = width as usize * height as usize * 4;
    if source.len() != source_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("expected {source_size} RGBA bytes, got {}", source.len()),
        ));
    }
    let (preview_width, preview_height) = preview_dimensions(width, height);
    preview.resize(preview_width as usize * preview_height as usize * 4, 0);
    for preview_y in 0..preview_height as usize {
        let source_y = preview_y * height as usize / preview_height as usize;
        for preview_x in 0..preview_width as usize {
            let source_x = preview_x * width as usize / preview_width as usize;
            let source_offset = (source_y * width as usize + source_x) * 4;
            let preview_offset = (preview_y * preview_width as usize + preview_x) * 4;
            preview[preview_offset..preview_offset + 4]
                .copy_from_slice(&source[source_offset..source_offset + 4]);
        }
    }
    Ok((preview_width, preview_height))
}

fn write_preview_frame(
    directory: &std::path::Path,
    frame: u64,
    source: &[u8],
    width: u32,
    height: u32,
    preview: &mut Vec<u8>,
) -> std::io::Result<(u32, u32)> {
    let dimensions = downscale_rgba_nearest(source, width, height, preview)?;
    std::fs::create_dir_all(directory)?;
    std::fs::write(directory.join(format!("{frame}.rgba")), preview)?;
    Ok(dimensions)
}

enum RenderSource {
    Direct(RenderOptions),
    Managed(libgoosy::workspace::Workspace),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WorkerControl {
    Continue,
    Cancel,
    Interrupted,
}

// Managed callers poll only at committed boundaries. The same stdin protocol
// remains frame-granular for direct renders, where running without stdin is valid.
fn poll_control(
    input: &mpsc::Receiver<String>,
    managed: bool,
    mut acknowledge: impl FnMut(&str),
) -> WorkerControl {
    let mut paused = false;
    loop {
        let command = if paused {
            input.recv().ok()
        } else {
            match input.try_recv() {
                Ok(value) => Some(value),
                Err(mpsc::TryRecvError::Empty) => return WorkerControl::Continue,
                Err(mpsc::TryRecvError::Disconnected) => None,
            }
        };
        match command.as_deref() {
            Some("pause") if !paused => {
                paused = true;
                acknowledge("paused");
            }
            Some("resume") if paused => {
                paused = false;
                acknowledge("resumed");
            }
            Some("stop") => return WorkerControl::Cancel,
            None if managed || paused => return WorkerControl::Interrupted,
            None => return WorkerControl::Continue,
            Some(_) => {}
        }
    }
}

fn run_worker(
    mut source: RenderSource,
    progress_events: bool,
    mut preview_dir: Option<PathBuf>,
    preview_interval: u64,
) -> Result<()> {
    let (options, needs_renderer) = match &source {
        RenderSource::Direct(options) => (options, true),
        RenderSource::Managed(work) => (
            work.options(),
            work.range().is_none() || work.committed() < work.total(),
        ),
    };
    let (width, height) = (options.width, options.height);
    if needs_renderer {
        ensure_renderer_backend(width, height, options.fps, progress_events)?;
        emit_stage(
            progress_events,
            "initializing-renderer",
            "正在初始化图形后端",
        )?;
    } else {
        emit_stage(progress_events, "checking-publication", "正在检查最终输出")?;
    }
    let preview_enabled = preview_dir.is_some() && !options.encoder.is_zero_copy();
    let preview_due = |done, total| {
        preview_enabled && (done == 1 || done == total || done % preview_interval == 0)
    };
    let mut preview_pixels = Vec::new();
    let on_frame = |done, total, elapsed, pixels: &[u8]| {
        // The preview is best effort. A crashed supervisor may have closed both
        // pipes while a managed segment still needs to drain and commit.
        if progress_events {
            let _ = writeln!(
                std::io::stdout(),
                "GOOSY_PROGRESS {done} {total} {elapsed:.3}"
            );
        }
        if preview_due(done, total) {
            if let Some(directory) = preview_dir.as_deref() {
                match write_preview_frame(
                    directory,
                    done,
                    pixels,
                    width,
                    height,
                    &mut preview_pixels,
                ) {
                    Ok((w, h)) => {
                        let _ = writeln!(std::io::stdout(), "GOOSY_PREVIEW {done} {w} {h}");
                    }
                    Err(error) => {
                        let _ = writeln!(
                            std::io::stderr(),
                            "goosy: disable live preview after write failure: {error}"
                        );
                        preview_dir = None;
                    }
                }
            }
        }
        if progress_events || preview_due(done, total) {
            let _ = std::io::stdout().flush();
        }
    };
    let (send, input) = mpsc::channel();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if send.send(line.trim().to_owned()).is_err() {
                break;
            }
        }
    });
    let managed = matches!(source, RenderSource::Managed(_));
    let mut last_control = WorkerControl::Continue;
    let mut keep_running = || {
        last_control = poll_control(&input, managed, |state| {
            emit_control(progress_events, state)
        });
        last_control == WorkerControl::Continue
    };
    let result = match &mut source {
        RenderSource::Direct(options) => libgoosy::render_with_frame_progress_control_stats_lazy(
            options,
            preview_due,
            on_frame,
            || {
                if keep_running() {
                    RenderControl::Continue
                } else {
                    RenderControl::Stop
                }
            },
        )
        .map(Some),
        RenderSource::Managed(work) => libgoosy::render_workspace(
            work,
            preview_due,
            on_frame,
            |phase, done, total, elapsed| {
                if progress_events {
                    let event = match phase {
                        libgoosy::EncoderProgress::Submitted => "GOOSY_SUBMITTED",
                        libgoosy::EncoderProgress::Committed => "GOOSY_COMMITTED",
                    };
                    let _ = writeln!(std::io::stdout(), "{event} {done} {total} {elapsed:.3}");
                    let _ = std::io::stdout().flush();
                }
            },
            keep_running,
        ),
    };
    match last_control {
        WorkerControl::Cancel => {
            emit_control(progress_events, "cancelled");
            anyhow::bail!("render stopped by user");
        }
        WorkerControl::Interrupted if managed => {
            emit_control(progress_events, "interrupted");
            return Ok(());
        }
        _ => {}
    }
    if let Some(stats) = result? {
        if progress_events || cfg!(target_os = "linux") {
            writeln!(
                std::io::stdout(),
                "GOOSY_COMPLETE {} {:.3}",
                stats.frames,
                stats.elapsed_seconds
            )?;
            std::io::stdout().flush()?;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        #[cfg(target_os = "windows")]
        Command::BackendProbe { width, height } => {
            libgoosy::surface::probe_selected_backend(width, height)?;
            Ok(())
        }
        #[cfg(target_os = "windows")]
        Command::RendererProbe { width, height, fps } => {
            libgoosy::renderer::probe_renderer_initialization(width, height, fps)?;
            Ok(())
        }
        Command::Pdf {
            song,
            lyrics,
            output,
            title,
            no_translation,
            no_background_vocal,
            speed_printer,
            exclude_lines,
            format,
        } => {
            let metadata = libgoosy::video::audio_metadata(&song)?;
            let text = if let Some(path) = lyrics.as_deref() {
                std::fs::read_to_string(path)?
            } else {
                metadata.lyrics.ok_or_else(|| {
                    anyhow::anyhow!("no external lyrics path and no embedded lyrics metadata")
                })?
            };
            let lines = libgoosy::parse_lyrics(&text, format.into())?;
            let pages = libgoosy::render_lyrics_pdf(
                &output,
                &lines,
                &libgoosy::PdfOptions {
                    title,
                    render_translation: !no_translation,
                    render_background_vocal: !no_background_vocal,
                    speed_printer,
                    excluded_lines: exclude_lines,
                },
            )?;
            eprintln!("wrote {pages} PDF page(s): {}", output.display());
            Ok(())
        }
        Command::Render {
            workspace,
            song,
            lyrics,
            output,
            width,
            height,
            fps,
            font_scale,
            translation_font_scale,
            background_font_scale,
            line_height_scale,
            line_spacing_scale,
            translation_gap_scale,
            background_gap_scale,
            horizontal_padding_scale,
            lyric_blur_sigma_step,
            debug_overlays,
            sample_start_ms,
            sample_duration_ms,
            background,
            cover,
            title,
            no_embedded_cover,
            no_audio,
            no_translation,
            no_background_vocal,
            exclude_lines,
            progress_events,
            preview_dir,
            preview_interval,
            format,
            encoder,
            codec,
            nvenc_preset,
        } => {
            if let Some(directory) = workspace {
                let result = libgoosy::workspace::Workspace::open(&directory).and_then(|work| {
                    run_worker(RenderSource::Managed(work), progress_events, preview_dir, preview_interval)
                });
                if result.is_err() {
                    emit_control(progress_events, "failed");
                }
                return result;
            }
            let options = RenderOptions {
                song: song.ok_or_else(|| anyhow::anyhow!("render requires a song or --workspace"))?,
                lyrics,
                output,
                width,
                height,
                fps,
                lyrics_style: LyricsStyle {
                    font_scale,
                    translation_font_scale,
                    background_font_scale,
                    line_height_scale,
                    group_gap_scale: line_spacing_scale,
                    translation_gap_scale,
                    background_gap_scale,
                    horizontal_padding_scale,
                    lyric_blur_sigma_step,
                    debug_overlays,
                },
                background,
                cover,
                title,
                no_embedded_cover,
                no_audio,
                render_translation: !no_translation,
                render_background_vocal: !no_background_vocal,
                excluded_lines: exclude_lines,
                format: format.into(),
                sample_start_ms,
                sample_duration_ms,
                encoder: encoder.into(),
                codec: codec.into(),
                nvenc_preset,
            };
            run_worker(RenderSource::Direct(options), progress_events, preview_dir, preview_interval)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Cli, Command, WorkerCodec, WorkerEncoder, downscale_rgba_nearest, preview_dimensions,
    };
    use clap::Parser;

    #[test]
    fn managed_render_needs_only_a_workspace() {
        let cli = Cli::try_parse_from(["worker", "render", "--workspace", "saved"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Render {
                workspace: Some(_),
                song: None,
                ..
            }
        ));
        assert!(Cli::try_parse_from(["worker", "render"]).is_err());
    }

    #[test]
    fn boundary_pauses_then_resumes_or_exits_on_disconnect() {
        use super::{WorkerControl, poll_control};
        let (send, input) = std::sync::mpsc::channel();
        send.send("pause".into()).unwrap();
        send.send("resume".into()).unwrap();
        let mut states = Vec::new();
        assert_eq!(
            poll_control(&input, true, |s| states.push(s.to_owned())),
            WorkerControl::Continue
        );
        assert_eq!(states, ["paused", "resumed"]);
        send.send("pause".into()).unwrap();
        drop(send);
        assert_eq!(
            poll_control(&input, true, |s| assert_eq!(s, "paused")),
            WorkerControl::Interrupted
        );
        assert_eq!(poll_control(&input, false, |_| {}), WorkerControl::Continue);
        assert_eq!(
            poll_control(&input, true, |_| {}),
            WorkerControl::Interrupted
        );
    }

    #[test]
    fn resume_matches_continuous_frames_and_can_pause_before_publication() -> anyhow::Result<()> {
        use libgoosy::{
            EncoderBackend, EncoderProgress, RenderControl, RenderOptions, workspace::Workspace,
        };
        use std::cell::Cell;

        let dir = tempfile::tempdir()?;
        let song = dir.path().join("song.wav");
        let lyrics = dir.path().join("song.lrc");
        let audio = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=8000:cl=mono",
                "-t",
                "2",
            ])
            .arg(&song)
            .output()?;
        anyhow::ensure!(
            audio.status.success(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&audio.stderr)
        );
        std::fs::write(
            &lyrics,
            "[00:00.00]First\n[00:00.50]Second\n[00:01.25]Third\n[00:03.00]End\n",
        )?;
        let mut options = RenderOptions::new(song, dir.path().join("continuous.mp4"));
        options.lyrics = Some(lyrics);
        options.encoder = EncoderBackend::Pipe;
        options.no_audio = true;
        options.no_embedded_cover = true;
        options.width = 160;
        options.height = 96;
        options.fps = 4;
        options.sample_start_ms = Some(125);
        options.sample_duration_ms = Some(1_750);
        let mut reference = Vec::new();
        libgoosy::render_with_frame_progress_control_stats_lazy(
            &options,
            |_, _| true,
            |done, _, _, rgba| reference.push((done, blake3::hash(rgba))),
            || RenderControl::Continue,
        )?;

        options.output = dir.path().join("resumed.mp4");
        let workspace = dir.path().join(uuid::Uuid::new_v4().to_string());
        drop(Workspace::create(&workspace, &options, 1)?);
        let mut resumed = Vec::new();
        for (pause_on_rendered, expected_end) in [(1, 4), (7, 7)] {
            let mut work = Workspace::open(&workspace)?;
            let start = work.committed();
            let pause = Cell::new(false);
            let rendered = Cell::new(start);
            let submitted = Cell::new(start);
            let mut commits = Vec::new();
            let result = libgoosy::render_workspace(
                &mut work,
                |_, _| true,
                |done, total, _, rgba| {
                    assert_eq!(total, 7);
                    assert_eq!(done, submitted.get() + 1);
                    rendered.set(done);
                    resumed.push((done, blake3::hash(rgba)));
                    if done == pause_on_rendered {
                        pause.set(true);
                    }
                },
                |phase, done, _, _| match phase {
                    EncoderProgress::Submitted => {
                        assert_eq!(rendered.get(), done);
                        submitted.set(done);
                    }
                    EncoderProgress::Committed => {
                        assert_eq!(submitted.get(), done);
                        commits.push(done);
                    }
                },
                || !pause.get(),
            )?;
            assert!(result.is_none());
            assert_eq!(commits, [expected_end]);
            assert_eq!(work.committed(), expected_end);
            assert!(!options.output.exists());
        }
        assert_eq!(resumed, reference);
        let mut work = Workspace::open(&workspace)?;
        let published = libgoosy::render_workspace(
            &mut work,
            |_, _| false,
            |_, _, _, _| panic!("finalization rerendered a committed frame"),
            |_, _, _, _| panic!("finalization resubmitted a committed frame"),
            || true,
        )?;
        assert_eq!(published.unwrap().frames, 7);
        assert_eq!(
            work.published_size()?,
            Some(options.output.metadata()?.len())
        );
        Ok(())
    }

    #[test]
    fn preview_dimensions_fit_within_bounds_and_preserve_aspect_ratio() {
        assert_eq!(preview_dimensions(1_920, 1_080), (640, 360));
        assert_eq!(preview_dimensions(1_080, 1_920), (203, 360));
        assert_eq!(preview_dimensions(320, 180), (320, 180));
    }

    #[test]
    fn nearest_preview_uses_pixels_from_the_expected_source_cells() {
        let source = vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
        let mut preview = Vec::new();
        let dimensions = downscale_rgba_nearest(&source, 2, 2, &mut preview).unwrap();

        assert_eq!(dimensions, (2, 2));
        assert_eq!(preview, source);
    }

    #[test]
    fn encoder_argument_accepts_protocol_values() {
        for (value, expected) in [
            ("pipe", WorkerEncoder::Pipe),
            ("ffmpeg-next", WorkerEncoder::FfmpegNext),
            #[cfg(target_os = "linux")]
            (
                "ffmpeg-next-nvenc-zero-copy",
                WorkerEncoder::FfmpegNextNvencZeroCopy,
            ),
        ] {
            let cli = Cli::try_parse_from([
                "goosy-render-worker",
                "render",
                "song.wav",
                "--encoder",
                value,
            ])
            .unwrap();
            let Command::Render { encoder, .. } = cli.command else {
                panic!("expected render command");
            };
            assert_eq!(encoder, expected);
        }
    }

    #[test]
    fn render_options_parse_codec_and_nvenc_preset() {
        let cli = Cli::try_parse_from([
            "goosy-render-worker",
            "render",
            "song.wav",
            "--codec",
            "hevc",
            "--nvenc-preset",
            "p3",
        ])
        .unwrap();
        let Command::Render {
            codec,
            nvenc_preset,
            ..
        } = cli.command
        else {
            panic!("expected render command");
        };
        assert_eq!(codec, WorkerCodec::Hevc);
        assert_eq!(nvenc_preset, "p3");

        let cli = Cli::try_parse_from(["goosy-render-worker", "render", "song.wav"]).unwrap();
        let Command::Render {
            codec,
            nvenc_preset,
            ..
        } = cli.command
        else {
            panic!("expected render command");
        };
        assert_eq!(codec, WorkerCodec::H264);
        assert_eq!(nvenc_preset, "p5");
    }
}
