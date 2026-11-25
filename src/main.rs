mod gui;
mod gui_shared;

#[cfg(not(target_os = "windows"))]
use libgoosy::{
    LyricsStyle, lrc, pdf_renderer,
    task_manager::{Control, TaskId, TaskManager, TaskState},
    ttml, video, yrc,
};

use anyhow::Result;
#[cfg(not(target_os = "windows"))]
use anyhow::{Context, bail};
#[cfg(not(target_os = "windows"))]
use clap::ValueEnum;
use clap::{Parser, Subcommand};
#[cfg(not(target_os = "windows"))]
use indicatif::{ProgressBar, ProgressStyle};
#[cfg(not(target_os = "windows"))]
use std::fs;
#[cfg(not(target_os = "windows"))]
use std::io::Write;
#[cfg(not(target_os = "windows"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_os = "windows"))]
use std::time::Duration;

#[cfg(not(target_os = "windows"))]
#[derive(Clone, Copy, Debug, ValueEnum)]
enum LyricFormat {
    Auto,
    Lrc,
    Ttml,
    Yrc,
}

#[cfg(not(target_os = "windows"))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum CliVideoCodec {
    #[default]
    H264,
    #[value(alias = "h265")]
    Hevc,
}

#[cfg(not(target_os = "windows"))]
impl From<CliVideoCodec> for libgoosy::VideoCodec {
    fn from(value: CliVideoCodec) -> Self {
        match value {
            CliVideoCodec::H264 => Self::H264,
            CliVideoCodec::Hevc => Self::Hevc,
        }
    }
}

#[cfg(not(target_os = "windows"))]
#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliEncoder {
    #[value(name = "pipe")]
    Pipe,
    #[value(name = "ffmpeg-next")]
    FfmpegNext,
    #[cfg(target_os = "linux")]
    #[value(name = "ffmpeg-next-nvenc-zero-copy")]
    FfmpegNextNvencZeroCopy,
}

#[cfg(not(target_os = "windows"))]
impl Default for CliEncoder {
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            Self::FfmpegNext
        } else {
            Self::Pipe
        }
    }
}

#[cfg(not(target_os = "windows"))]
impl From<CliEncoder> for libgoosy::EncoderBackend {
    fn from(value: CliEncoder) -> Self {
        match value {
            CliEncoder::Pipe => Self::Pipe,
            CliEncoder::FfmpegNext => Self::FfmpegNext,
            #[cfg(target_os = "linux")]
            CliEncoder::FfmpegNextNvencZeroCopy => Self::FfmpegNextNvencZeroCopy,
        }
    }
}
#[cfg(target_os = "windows")]
#[derive(Parser)]
#[command(name = "goosy", about = "GoosyRenderer GUI")]
struct WindowsCli {
    #[command(subcommand)]
    command: Option<WindowsCommand>,
}

#[cfg(target_os = "windows")]
#[derive(Subcommand)]
enum WindowsCommand {
    Gui,
}

#[cfg(target_os = "windows")]
fn main() -> Result<()> {
    match WindowsCli::parse().command.unwrap_or(WindowsCommand::Gui) {
        WindowsCommand::Gui => gui::run().map_err(|error| anyhow::anyhow!(error.to_string())),
    }
}

#[cfg(not(target_os = "windows"))]
#[derive(Parser, Debug)]
#[command(name = "goosy", about = "Render scrolling lyric videos")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
#[cfg(not(target_os = "windows"))]
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
        #[arg(long, value_enum, default_value_t = CliEncoder::default())]
        encoder: CliEncoder,
        #[arg(long, default_value = "p5")]
        nvenc_preset: String,
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
        #[arg(long, value_enum, default_value_t = LyricFormat::Auto)]
        format: LyricFormat,
        #[arg(long, value_enum, default_value_t = CliVideoCodec::H264)]
        codec: CliVideoCodec,
    },
    /// Continue a previously interrupted render from its last verified segment.
    Resume {
        task: TaskId,
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
        #[arg(long, value_enum, default_value_t = LyricFormat::Auto)]
        format: LyricFormat,
    },
    Gui,
}

#[cfg(not(target_os = "windows"))]
fn main() -> Result<()> {
    let command = if std::env::args_os().nth(1).is_none() {
        Command::Gui
    } else {
        Cli::parse().command
    };
    match command {
        Command::Gui => gui::run().map_err(|error| anyhow::anyhow!(error.to_string())),
        Command::Resume { task } => {
            let manager = TaskManager::new()?;
            manager.command(task, Control::Resume)?;
            wait_for_render(&manager, task, false)
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
        } => export_pdf(
            song,
            lyrics,
            output,
            title,
            no_translation,
            no_background_vocal,
            speed_printer,
            exclude_lines,
            format,
        ),
        Command::Render {
            song,
            lyrics,
            output,
            width,
            height,
            fps,
            encoder,
            nvenc_preset,
            font_scale,
            translation_font_scale,
            background_font_scale,
            line_height_scale,
            translation_gap_scale,
            line_spacing_scale,
            background_gap_scale,
            horizontal_padding_scale,
            lyric_blur_sigma_step,
            debug_overlays,
            background,
            cover,
            title,
            no_embedded_cover,
            no_audio,
            progress_events,
            no_translation,
            no_background_vocal,
            exclude_lines,
            format,
            codec,
        } => render(
            song,
            lyrics,
            output,
            width,
            height,
            fps,
            encoder.into(),
            nvenc_preset,
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
            background,
            cover,
            title,
            no_embedded_cover,
            no_audio,
            progress_events,
            no_translation,
            no_background_vocal,
            exclude_lines,
            format,
            codec.into(),
        ),
    }
}

#[cfg(not(target_os = "windows"))]
fn parse_lyrics_text(
    text: &str,
    lyrics: Option<&Path>,
    format: LyricFormat,
) -> Result<Vec<lrc::LyricLine>> {
    let extension = lyrics
        .and_then(Path::extension)
        .and_then(|extension| extension.to_str());
    match format {
        LyricFormat::Auto
            if ttml::looks_like_ttml(text)
                || extension.is_some_and(|value| value.eq_ignore_ascii_case("ttml")) =>
        {
            ttml::parse_ttml(text)
        }
        LyricFormat::Auto
            if yrc::looks_like_yrc(text)
                || extension.is_some_and(|value| value.eq_ignore_ascii_case("yrc")) =>
        {
            yrc::parse_yrc(text)
        }
        LyricFormat::Auto | LyricFormat::Lrc => lrc::parse_lrc(text),
        LyricFormat::Ttml => ttml::parse_ttml(text),

        LyricFormat::Yrc => yrc::parse_yrc(text),
    }
}

#[cfg(not(target_os = "windows"))]
fn emit_render_stage(enabled: bool, key: &str, message: &str) -> Result<()> {
    if enabled {
        println!("GOOSY_STAGE {key} {message}");
        std::io::stdout().flush()?;
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn render(
    song: PathBuf,
    lyrics: Option<PathBuf>,

    output: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    encoder: libgoosy::EncoderBackend,
    nvenc_preset: String,
    font_scale: f32,
    translation_font_scale: f32,
    background_font_scale: f32,
    line_height_scale: f32,
    line_spacing_scale: f32,
    translation_gap_scale: f32,
    background_gap_scale: f32,
    horizontal_padding_scale: f32,
    lyric_blur_sigma_step: u32,
    debug_overlays: bool,
    background: Option<PathBuf>,
    cover: Option<PathBuf>,
    title: Option<String>,
    no_embedded_cover: bool,
    no_audio: bool,
    progress_events: bool,
    no_translation: bool,
    no_background_vocal: bool,
    exclude_lines: Vec<usize>,
    format: LyricFormat,
    codec: libgoosy::VideoCodec,
) -> Result<()> {
    let extension = lyrics
        .as_deref()
        .and_then(Path::extension)
        .and_then(|ext| ext.to_str());
    let format = match format {
        LyricFormat::Auto if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("ttml")) => {
            libgoosy::LyricFormat::Ttml
        }
        LyricFormat::Auto if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("yrc")) => {
            libgoosy::LyricFormat::Yrc
        }
        LyricFormat::Auto => libgoosy::LyricFormat::Auto,
        LyricFormat::Ttml => libgoosy::LyricFormat::Ttml,
        LyricFormat::Yrc => libgoosy::LyricFormat::Yrc,
        LyricFormat::Lrc => libgoosy::LyricFormat::Lrc,
    };
    let manager = TaskManager::new()?;
    let task = manager.enqueue(
        libgoosy::RenderOptions {
            song,
            lyrics,
            output,
            width,
            height,
            fps,
            background,
            cover,
            title,
            no_embedded_cover,
            no_audio,
            render_translation: !no_translation,
            render_background_vocal: !no_background_vocal,
            excluded_lines: exclude_lines,
            format,
            sample_start_ms: None,
            sample_duration_ms: None,
            encoder,
            codec,
            nvenc_preset,
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
        },
        u32::MAX,
    )?;
    eprintln!("Task {task}");
    wait_for_render(&manager, task, progress_events)
}

#[cfg(not(target_os = "windows"))]
fn wait_for_render(manager: &TaskManager, id: TaskId, progress_events: bool) -> Result<()> {
    let progress = ProgressBar::new(0);
    progress.set_style(ProgressStyle::with_template(
        "{msg} [{wide_bar}] {pos}/{len}",
    )?);
    let mut previous = None;
    loop {
        let task = manager.task(id).context("task missing from queue")?;
        if previous != Some((task.done, task.state)) {
            progress.set_length(task.total);
            progress.set_position(task.done);
            progress.set_message(task.stage.clone());
            emit_render_stage(progress_events, task.state.name(), &task.stage)?;
            if progress_events && task.total != 0 {
                println!(
                    "GOOSY_PROGRESS {} {} {:.3}",
                    task.done, task.total, task.elapsed
                );
                std::io::stdout().flush()?;
            }
            previous = Some((task.done, task.state));
        }
        match task.state {
            TaskState::Completed => {
                progress.finish();
                return Ok(());
            }
            TaskState::Failed
            | TaskState::Cancelled
            | TaskState::Deleting
            | TaskState::Interrupted
            | TaskState::Paused => {
                bail!("task {id} {}: {}", task.state.name(), task.detail)
            }
            _ => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}
#[cfg(not(target_os = "windows"))]
fn export_pdf(
    song: PathBuf,
    lyrics: Option<PathBuf>,
    output: PathBuf,
    title: Option<String>,
    no_translation: bool,
    no_background_vocal: bool,
    speed_printer: bool,
    exclude_lines: Vec<usize>,
    format: LyricFormat,
) -> Result<()> {
    let metadata = video::audio_metadata(&song)?;
    let title = title.or(metadata.title.clone());
    let text = if let Some(lyrics) = lyrics.as_deref() {
        fs::read_to_string(lyrics).with_context(|| format!("read lyrics {}", lyrics.display()))?
    } else {
        metadata
            .lyrics
            .clone()
            .context("no external lyrics path and no embedded lyrics metadata")?
    };
    let lines = parse_lyrics_text(&text, lyrics.as_deref(), format)?;
    let pages = pdf_renderer::render_lyrics_pdf(
        &output,
        &lines,
        &pdf_renderer::PdfOptions {
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

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;

    #[test]
    fn auto_uses_ttml_extension_fallback() {
        let input = r#"<body><p begin="1s" end="2s">hello</p></body>"#;
        let lines =
            parse_lyrics_text(input, Some(Path::new("lyrics.ttml")), LyricFormat::Auto).unwrap();
        assert_eq!(lines[0].text, "hello");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn render_accepts_explicit_zero_copy_encoder() {
        let cli = Cli::try_parse_from([
            "goosy",
            "render",
            "song.wav",
            "--encoder",
            "ffmpeg-next-nvenc-zero-copy",
        ])
        .unwrap();
        let Command::Render { encoder, .. } = cli.command else {
            panic!("expected render command");
        };
        assert_eq!(
            libgoosy::EncoderBackend::from(encoder).cli_value(),
            "ffmpeg-next-nvenc-zero-copy"
        );
    }
}
