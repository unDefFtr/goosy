pub mod background;
pub mod cover_renderer;
pub mod easing;
pub mod encoder;
pub mod ffi;
pub mod frame;
pub mod geometry;
#[cfg(target_os = "linux")]
pub mod gpu_interop;
pub mod image_asset;
pub mod layout;
pub mod lrc;
pub mod lyrics_renderer;
pub mod pdf_renderer;
pub mod renderer;
pub mod spring;
pub mod surface;
pub mod ttml;
pub mod video;
pub mod yrc;

// Prefer the discrete adapter on Windows hybrid-GPU systems. NVIDIA Optimus and
// AMD PowerXpress inspect these exports before the GPU backend creates its device.
#[cfg(target_os = "windows")]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static NvOptimusEnablement: u32 = 1;

#[cfg(target_os = "windows")]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static AmdPowerXpressRequestHighPerformance: u32 = 1;
pub use anyhow::Result;
pub use encoder::{EncoderBackend, NvencPreset, VideoCodec};
pub use frame::{FrameSink, RenderedFrame};
use indicatif::{ProgressBar, ProgressStyle};
pub use lrc::{BackgroundVocal, LyricLine, LyricWord};
pub use lyrics_renderer::LyricsStyle;
pub use pdf_renderer::{PdfOptions, render_lyrics_pdf};
pub use renderer::Renderer;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LyricFormat {
    #[default]
    Auto,
    Lrc,
    Ttml,
    Yrc,
}
#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub song: PathBuf,
    pub lyrics: Option<PathBuf>,
    pub output: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub background: Option<PathBuf>,
    pub cover: Option<PathBuf>,
    pub title: Option<String>,
    pub no_embedded_cover: bool,
    pub no_audio: bool,
    pub render_translation: bool,
    pub lyrics_style: LyricsStyle,
    pub render_background_vocal: bool,
    pub excluded_lines: Vec<usize>,
    pub format: LyricFormat,
    pub sample_start_ms: Option<u64>,
    pub sample_duration_ms: Option<u64>,
    pub encoder: encoder::EncoderBackend,
    pub codec: encoder::VideoCodec,
    pub nvenc_preset: String,
}
impl RenderOptions {
    pub fn new(song: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Self {
        Self {
            song: song.into(),
            lyrics: None,
            output: output.into(),
            width: 1_920,
            height: 1_080,
            fps: 30,
            background: None,
            cover: None,
            title: None,
            no_embedded_cover: false,
            no_audio: false,
            render_translation: true,
            lyrics_style: LyricsStyle::default(),
            render_background_vocal: true,
            excluded_lines: Vec::new(),
            format: LyricFormat::Auto,
            sample_start_ms: None,
            sample_duration_ms: None,
            encoder: if cfg!(target_os = "macos") {
                encoder::EncoderBackend::FfmpegNext
            } else {
                encoder::EncoderBackend::Pipe
            },
            codec: encoder::VideoCodec::H264,
            nvenc_preset: "p5".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RenderControl {
    Continue,
    Pause,
    Stop,
}

pub fn parse_lyrics(input: &str, format: LyricFormat) -> anyhow::Result<Vec<LyricLine>> {
    match format {
        LyricFormat::Auto if ttml::looks_like_ttml(input) => ttml::parse_ttml(input),
        LyricFormat::Auto if yrc::looks_like_yrc(input) => yrc::parse_yrc(input),
        LyricFormat::Auto | LyricFormat::Lrc => lrc::parse_lrc(input),
        LyricFormat::Ttml => ttml::parse_ttml(input),
        LyricFormat::Yrc => yrc::parse_yrc(input),
    }
}

pub fn render(options: &RenderOptions) -> anyhow::Result<()> {
    render_with_progress(options, |_, _, _| {})
}

pub fn render_with_progress<F>(options: &RenderOptions, mut on_progress: F) -> anyhow::Result<()>
where
    F: FnMut(u64, u64, f64),
{
    render_with_frame_progress_control_stats_lazy(
        options,
        |_, _| false,
        |done, total, elapsed, _pixels| {
            on_progress(done, total, elapsed);
        },
        || RenderControl::Continue,
    )
    .map(|_| ())
}

pub fn render_with_frame_progress<F>(options: &RenderOptions, on_frame: F) -> anyhow::Result<()>
where
    F: FnMut(u64, u64, f64, &[u8]),
{
    render_with_frame_progress_control(options, on_frame, || RenderControl::Continue)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderStats {
    pub frames: u64,
    pub elapsed_seconds: f64,
}

pub fn render_with_frame_progress_control<F, C>(
    options: &RenderOptions,
    on_frame: F,
    control: C,
) -> anyhow::Result<()>
where
    F: FnMut(u64, u64, f64, &[u8]),
    C: FnMut() -> RenderControl,
{
    render_with_frame_progress_control_stats(options, on_frame, control).map(|_| ())
}

pub fn render_with_frame_progress_control_stats<F, C>(
    options: &RenderOptions,
    on_frame: F,
    control: C,
) -> anyhow::Result<RenderStats>
where
    F: FnMut(u64, u64, f64, &[u8]),
    C: FnMut() -> RenderControl,
{
    let need_cpu = !cfg!(target_os = "macos") || options.encoder == EncoderBackend::Pipe;
    render_with_frame_progress_control_stats_lazy(options, move |_, _| need_cpu, on_frame, control)
}

pub fn render_with_frame_progress_control_stats_lazy<P, F, C>(
    options: &RenderOptions,
    mut need_pixels: P,
    mut on_frame: F,
    mut control: C,
) -> anyhow::Result<RenderStats>
where
    P: FnMut(u64, u64) -> bool,
    F: FnMut(u64, u64, f64, &[u8]),
    C: FnMut() -> RenderControl,
{
    if options.width == 0 || options.height == 0 || options.fps == 0 {
        anyhow::bail!("width, height, and fps must be positive");
    }
    options.encoder.validate_codec(options.codec)?;
    let metadata = video::audio_metadata(&options.song)?;
    let title = options.title.clone().or(metadata.title.clone());
    let text = if let Some(lyrics) = options.lyrics.as_deref() {
        fs::read_to_string(lyrics)
            .map_err(|error| anyhow::anyhow!("read lyrics {}: {error}", lyrics.display()))?
    } else {
        metadata.lyrics.clone().ok_or_else(|| {
            anyhow::anyhow!("no external lyrics path and no embedded lyrics metadata")
        })?
    };
    let lines = parse_lyrics(&text, options.format)?;
    let last_line_end_ms = lines.iter().map(|line| line.end_ms).max().unwrap_or(0);
    let full_duration_ms = if options.no_audio {
        last_line_end_ms.saturating_add(1_000)
    } else {
        (metadata.duration_seconds * 1_000.0).ceil() as u64
    };
    let render_start_ms = options.sample_start_ms.unwrap_or(0).min(full_duration_ms);
    let render_duration_ms = options
        .sample_duration_ms
        .unwrap_or(full_duration_ms.saturating_sub(render_start_ms))
        .min(full_duration_ms.saturating_sub(render_start_ms));
    if render_duration_ms == 0 {
        anyhow::bail!("render duration must be positive");
    }
    let total_frames = (render_duration_ms as f64 / 1_000.0 * options.fps as f64).ceil() as u64;
    let embedded_cover = if options.cover.is_none() && !options.no_embedded_cover {
        video::embedded_cover_image(&options.song)?
    } else {
        None
    };
    let image_assets = image_asset::load_scene_image_assets(
        options.background.as_deref(),
        options.cover.as_deref(),
        embedded_cover.as_deref(),
    )?;
    let background_layer = match image_assets.background.as_ref() {
        Some(asset) => background::BackgroundRenderer::from_image_asset(asset)?,
        None => background::BackgroundRenderer::dynamic(),
    };
    let cover_layer = image_assets
        .cover
        .as_ref()
        .map(|asset| cover_renderer::CoverRenderer::from_image_asset(asset, title))
        .transpose()?;
    let mut renderer = Renderer::with_scene_options_for_encoder(
        options.width,
        options.height,
        options.fps,
        lines,
        background_layer,
        cover_layer,
        options.render_translation,
        options.render_background_vocal,
        options.lyrics_style,
        &options.excluded_lines,
        options.encoder,
    )?;
    let mut sink = video::create_encoder_with_codec_and_preset(
        options.encoder,
        options.codec,
        &options.output,
        options.width,
        options.height,
        options.fps,
        (!options.no_audio).then_some(options.song.as_path()),
        NvencPreset::from_value(&options.nvenc_preset),
    )?;
    let require_cpu_pixels = sink.requires_cpu_pixels();
    let profile = std::env::var_os("GOOSY_PROFILE").is_some();
    let mut render_seconds = 0.0;
    let mut submit_seconds = 0.0;
    let finish_seconds;
    let progress = ProgressBar::hidden();
    progress.set_style(ProgressStyle::default_bar());
    #[cfg(target_os = "linux")]
    let mut frame = if options.encoder.is_zero_copy() {
        RenderedFrame::new_gpu_only(options.width, options.height)
    } else {
        RenderedFrame::new(options.width, options.height)
    };
    #[cfg(not(target_os = "linux"))]
    let mut frame = RenderedFrame::new(options.width, options.height);
    let render_started = Instant::now();
    for frame_index in 0..total_frames {
        loop {
            match control() {
                RenderControl::Continue => break,
                RenderControl::Pause => std::thread::sleep(std::time::Duration::from_millis(40)),
                RenderControl::Stop => anyhow::bail!("render stopped by user"),
            }
        }
        let phase_started = Instant::now();
        let t_ms = render_start_ms + frame_index * 1_000 / options.fps as u64;
        let read_cpu_pixels = require_cpu_pixels
            || (!options.encoder.is_zero_copy() && need_pixels(frame_index + 1, total_frames));
        if require_cpu_pixels {
            renderer.render_frame_into(t_ms, &mut frame)?;
        } else {
            renderer.render_frame_into_opts(t_ms, &mut frame, read_cpu_pixels)?;
        }
        render_seconds += phase_started.elapsed().as_secs_f64();
        progress.inc(1);
        let callback_pixels: &[u8] = if read_cpu_pixels {
            frame.pixels()
        } else {
            &[]
        };
        on_frame(
            frame_index + 1,
            total_frames,
            render_started.elapsed().as_secs_f64(),
            callback_pixels,
        );
        let phase_started = Instant::now();
        frame = sink.submit(frame)?;
        submit_seconds += phase_started.elapsed().as_secs_f64();
    }
    let phase_started = Instant::now();
    sink.finish()?;
    finish_seconds = phase_started.elapsed().as_secs_f64();
    let elapsed_seconds = render_started.elapsed().as_secs_f64();
    if profile {
        eprintln!("render profile: frames={} total={:.6}s render={:.6}s submit={:.6}s finish={:.6}s", total_frames, elapsed_seconds, render_seconds, submit_seconds, finish_seconds);
    }
    progress.finish_and_clear();
    Ok(RenderStats { frames: total_frames, elapsed_seconds })
}

pub fn read_lyrics_file(
    path: impl AsRef<Path>,
    format: LyricFormat,
) -> anyhow::Result<Vec<LyricLine>> {
    let text = fs::read_to_string(path)?;
    parse_lyrics(&text, format)
}
