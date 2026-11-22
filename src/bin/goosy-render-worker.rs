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
        song: PathBuf,
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
        println!("GOOSY_STAGE {key} {message}");
        std::io::stdout().flush()?;
    }
    Ok(())
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
            ensure_renderer_backend(width, height, fps, progress_events)?;
            emit_stage(
                progress_events,
                "initializing-renderer",
                "正在初始化图形后端",
            )?;
            let options = RenderOptions {
                song,
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
            let mut preview_dir = preview_dir;
            let mut preview_pixels = Vec::new();
            let (control_sender, control_receiver) = mpsc::channel();
            thread::spawn(move || {
                let stdin = std::io::stdin();
                for command in stdin.lock().lines().map_while(Result::ok) {
                    let _ = control_sender.send(command.trim().to_owned());
                }
            });
            let mut paused = false;
            #[cfg(target_os = "linux")]
            eprintln!(
                "video encoder: {}",
                libgoosy::video::preferred_video_encoder_name()
            );
            let has_preview = preview_dir.is_some();
            let preview_enabled = has_preview && !options.encoder.is_zero_copy();
            let _stats = libgoosy::render_with_frame_progress_control_stats_lazy(
                &options,
                move |done, total| {
                    preview_enabled && (done == 1 || done == total || done % preview_interval == 0)
                },
                |done, total, elapsed, pixels| {
                    if progress_events {
                        println!("GOOSY_PROGRESS {done} {total} {elapsed:.3}");
                    }
                    let preview_due = preview_enabled
                        && (done == 1 || done == total || done % preview_interval == 0);
                    if preview_due {
                        let result = preview_dir.as_deref().map(|directory| {
                            write_preview_frame(
                                directory,
                                done,
                                pixels,
                                width,
                                height,
                                &mut preview_pixels,
                            )
                        });
                        match result {
                            Some(Ok((preview_width, preview_height))) => {
                                println!("GOOSY_PREVIEW {done} {preview_width} {preview_height}");
                            }
                            Some(Err(error)) => {
                                eprintln!(
                                    "goosy: disable live preview after write failure: {error}"
                                );
                                preview_dir = None;
                            }
                            None => {}
                        }
                    }
                    if progress_events || preview_due {
                        let _ = std::io::stdout().flush();
                    }
                },
                || loop {
                    if paused {
                        match control_receiver.recv() {
                            Ok(command) if command == "resume" => paused = false,
                            Ok(command) if command == "stop" => return RenderControl::Stop,
                            Ok(_) => {}
                            Err(_) => return RenderControl::Stop,
                        }
                    } else {
                        match control_receiver.try_recv() {
                            Ok(command) if command == "pause" => paused = true,
                            Ok(command) if command == "stop" => return RenderControl::Stop,
                            Ok(_) | Err(mpsc::TryRecvError::Empty) => {
                                return RenderControl::Continue;
                            }
                            Err(mpsc::TryRecvError::Disconnected) => {
                                return RenderControl::Continue;
                            }
                        }
                    }
                },
            )?;
            #[cfg(target_os = "linux")]
            {
                println!(
                    "GOOSY_COMPLETE {} {:.3}",
                    _stats.frames, _stats.elapsed_seconds
                );
                std::io::stdout().flush()?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, WorkerCodec, WorkerEncoder, downscale_rgba_nearest, preview_dimensions};
    use clap::Parser;

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
