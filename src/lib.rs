pub mod background;
pub mod cover_renderer;
pub mod easing;
pub mod encoder;
pub mod ffi;
pub mod frame;
pub mod geometry;
#[cfg(target_os = "linux")]
pub mod gpu_interop;
#[cfg(target_os = "windows")]
pub mod win_qsv;
mod history;
pub mod image_asset;
pub mod layout;
pub mod lrc;
pub mod lyrics_renderer;
mod media;
pub mod pdf_renderer;
pub mod renderer;
pub mod spring;
pub mod surface;
pub mod task_manager;
pub mod ttml;
pub mod video;
pub mod workspace;
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
pub use lrc::{BackgroundVocal, LyricLine, LyricWord};
pub use lyrics_renderer::LyricsStyle;
pub use pdf_renderer::{PdfOptions, render_lyrics_pdf};
pub use renderer::Renderer;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LyricFormat {
    #[default]
    Auto,
    Lrc,
    Ttml,
    Yrc,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    let need_cpu =
        !(cfg!(target_os = "macos") || cfg!(target_os = "windows"))
            || options.encoder == EncoderBackend::Pipe;
    render_with_frame_progress_control_stats_lazy(options, move |_, _| need_cpu, on_frame, control)
}

pub fn render_with_frame_progress_control_stats_lazy<P, F, C>(
    options: &RenderOptions,
    need_pixels: P,
    on_frame: F,
    control: C,
) -> anyhow::Result<RenderStats>
where
    P: FnMut(u64, u64) -> bool,
    F: FnMut(u64, u64, f64, &[u8]),
    C: FnMut() -> RenderControl,
{
    let mut scene = prepare_scene(options)?;
    let mut events = FrameEvents::new(need_pixels, on_frame, |_, _, _, _| {});
    let total = scene.total_frames;
    let frame = new_frame(options);
    events.encode(
        &mut scene,
        options,
        0..total,
        create_sink(
            options,
            &options.output,
            (!options.no_audio).then_some(&options.song),
        )?,
        frame,
        control,
    )?;
    Ok(events.stats(total))
}

/// Encoder-side progress. The existing frame callback still runs before submission,
/// while its pixel slice belongs to the renderer. Only `Committed` is durable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncoderProgress {
    Submitted,
    Committed,
}

/// Drive a managed render from its verified workspace. `at_boundary` may block for
/// pause/resume, or return false to leave with all previous commits intact. `None`
/// denotes such a controlled exit; a completed render always has a publication receipt.
pub fn render_workspace(
    work: &mut workspace::Workspace,
    need_pixels: impl FnMut(u64, u64) -> bool,
    on_frame: impl FnMut(u64, u64, f64, &[u8]),
    encoder_progress: impl FnMut(EncoderProgress, u64, u64, f64),
    mut at_boundary: impl FnMut() -> bool,
) -> Result<Option<RenderStats>> {
    let mut events = FrameEvents::new(need_pixels, on_frame, encoder_progress);
    if work.published_size()?.is_some() {
        return Ok(Some(events.stats(work.total())));
    }
    if !events.boundary(&mut at_boundary) {
        return Ok(None);
    }
    // Finalization can be retried without opening any renderer or GPU device.
    if let Some((start_ms, duration_ms, total)) = work.range() {
        if work.committed() == total {
            media::finalize(work, start_ms, duration_ms, total)?;
            return Ok(Some(events.stats(total)));
        }
    }
    let mut scene = prepare_scene(work.options())?;
    work.set_render_backend(&scene.renderer.resume_environment()?)?;
    let Scene {
        start_ms,
        duration_ms,
        total_frames,
        ..
    } = scene;
    work.set_range(start_ms, duration_ms, total_frames)?;
    if let Some(checkpoint) = work.checkpoint()? {
        scene.renderer.restore_checkpoint(&checkpoint)?;
    }
    let mut frame = new_frame(work.options());
    while work.committed() < total_frames {
        if !events.boundary(&mut at_boundary) {
            return Ok(None);
        }
        let start = work.committed();
        let end = start
            .saturating_add(work.segment_frames())
            .min(total_frames);
        anyhow::ensure!(end > start, "empty render segment at frame {start}");
        let path = work.segment_path(start);
        let options = work.options();
        let sink = create_sink(options, &path, None)?;
        frame = events.encode(&mut scene, options, start..end, sink, frame, || {
            RenderControl::Continue
        })?;
        let signature = media::validate_segment(&path, options, end - start)?;
        work.commit(start, end, &scene.renderer.save_checkpoint(), &signature)?;
        (events.encoder_progress)(
            EncoderProgress::Committed,
            end,
            total_frames,
            events.started.elapsed().as_secs_f64(),
        );
    }
    if !events.boundary(&mut at_boundary) {
        return Ok(None);
    }
    drop((frame, scene));
    media::finalize(work, start_ms, duration_ms, total_frames)?;
    Ok(Some(events.stats(total_frames)))
}

struct Scene {
    renderer: Renderer,
    start_ms: u64,
    duration_ms: u64,
    total_frames: u64,
}

/// Full render timeline before applying a sample range, in milliseconds.
pub fn render_duration_ms(no_audio: bool, audio_duration_seconds: f64, last_lyric_end_ms: u64) -> u64 {
    if no_audio {
        last_lyric_end_ms.saturating_add(1_000)
    } else {
        (audio_duration_seconds * 1_000.0).ceil() as u64
    }
}

fn prepare_scene(options: &RenderOptions) -> Result<Scene> {
    if options.width == 0 || options.height == 0 || options.fps == 0 {
        anyhow::bail!("width, height, and fps must be positive");
    }
    options.encoder.validate_codec(options.codec)?;
    let metadata = video::audio_metadata(&options.song)?;
    let title = options.title.clone().or(metadata.title);
    let text = if let Some(lyrics) = options.lyrics.as_deref() {
        fs::read_to_string(lyrics)
            .map_err(|error| anyhow::anyhow!("read lyrics {}: {error}", lyrics.display()))?
    } else {
        metadata.lyrics.ok_or_else(|| {
            anyhow::anyhow!("no external lyrics path and no embedded lyrics metadata")
        })?
    };
    let lines = parse_lyrics(&text, options.format)?;
    let last_line_end_ms = lines.iter().map(|line| line.end_ms).max().unwrap_or(0);
    let full_duration_ms = render_duration_ms(options.no_audio, metadata.duration_seconds, last_line_end_ms);
    let render_start_ms = options.sample_start_ms.unwrap_or(0).min(full_duration_ms);
    let render_duration_ms = options
        .sample_duration_ms
        .unwrap_or(full_duration_ms.saturating_sub(render_start_ms))
        .min(full_duration_ms.saturating_sub(render_start_ms));
    if render_duration_ms == 0 {
        anyhow::bail!("render duration must be positive");
    }
    let total_frames =
        u64::try_from((u128::from(render_duration_ms) * u128::from(options.fps)).div_ceil(1_000))?;
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
    let renderer = Renderer::with_scene_options_for_encoder(
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
    Ok(Scene {
        renderer,
        start_ms: render_start_ms,
        duration_ms: render_duration_ms,
        total_frames,
    })
}

fn create_sink(
    options: &RenderOptions,
    path: &Path,
    audio: Option<&Path>,
) -> Result<Box<dyn FrameSink>> {
    video::create_encoder_with_codec_and_preset(
        options.encoder,
        options.codec,
        path,
        options.width,
        options.height,
        options.fps,
        audio,
        NvencPreset::from_value(&options.nvenc_preset),
    )
}

fn new_frame(options: &RenderOptions) -> RenderedFrame {
    #[cfg(target_os = "linux")]
    if options.encoder.is_zero_copy() {
        return RenderedFrame::new_gpu_only(options.width, options.height);
    }
    RenderedFrame::new(options.width, options.height)
}

struct FrameEvents<P, F, S> {
    need_pixels: P,
    on_frame: F,
    encoder_progress: S,
    started: Instant,
    profile: bool,
}

impl<P, F, S> FrameEvents<P, F, S>
where
    P: FnMut(u64, u64) -> bool,
    F: FnMut(u64, u64, f64, &[u8]),
    S: FnMut(EncoderProgress, u64, u64, f64),
{
    fn new(need_pixels: P, on_frame: F, encoder_progress: S) -> Self {
        Self {
            need_pixels,
            on_frame,
            encoder_progress,
            started: Instant::now(),
            profile: std::env::var_os("GOOSY_PROFILE").is_some(),
        }
    }

    fn stats(&self, frames: u64) -> RenderStats {
        RenderStats {
            frames,
            elapsed_seconds: self.started.elapsed().as_secs_f64(),
        }
    }

    fn boundary(&mut self, control: &mut impl FnMut() -> bool) -> bool {
        let before = Instant::now();
        let keep_running = control();
        self.started += before.elapsed(); // Exclude time spent paused from active render time.
        keep_running
    }

    /// Own the sink until its encoder has drained and the muxer has closed. Return
    /// the recycled buffer so segment changes need no fresh full-resolution allocation.
    fn encode(
        &mut self,
        scene: &mut Scene,
        options: &RenderOptions,
        frames: std::ops::Range<u64>,
        mut sink: Box<dyn FrameSink>,
        mut frame: RenderedFrame,
        mut control: impl FnMut() -> RenderControl,
    ) -> Result<RenderedFrame> {
        let require_cpu_pixels = sink.requires_cpu_pixels();
        let total = scene.total_frames;
        let count = frames.end - frames.start;
        let mut render_seconds = 0.0;
        let mut submit_seconds = 0.0;
        for index in frames {
            loop {
                match control() {
                    RenderControl::Continue => break,
                    RenderControl::Pause => {
                        std::thread::sleep(std::time::Duration::from_millis(40))
                    }
                    RenderControl::Stop => anyhow::bail!("render stopped by user"),
                }
            }
            let tick = self.profile.then(Instant::now);
            let done = index + 1;
            let t_ms =
                scene.start_ms + (u128::from(index) * 1_000 / u128::from(options.fps)) as u64;
            let read_pixels = require_cpu_pixels
                || (!options.encoder.is_zero_copy() && (self.need_pixels)(done, total));
            if require_cpu_pixels {
                scene.renderer.render_frame_into(t_ms, &mut frame)?;
            } else {
                scene
                    .renderer
                    .render_frame_into_opts(t_ms, &mut frame, read_pixels)?;
            }
            if let Some(tick) = tick {
                render_seconds += tick.elapsed().as_secs_f64();
            }
            (self.on_frame)(
                done,
                total,
                self.started.elapsed().as_secs_f64(),
                if read_pixels { frame.pixels() } else { &[] },
            );
            let tick = self.profile.then(Instant::now);
            frame = sink.submit(frame)?;
            if let Some(tick) = tick {
                submit_seconds += tick.elapsed().as_secs_f64();
            }
            (self.encoder_progress)(
                EncoderProgress::Submitted,
                done,
                total,
                self.started.elapsed().as_secs_f64(),
            );
        }
        let tick = self.profile.then(Instant::now);
        sink.finish()?;
        if let Some(tick) = tick {
            eprintln!(
                "render profile: frames={count} total={:.6}s render={render_seconds:.6}s submit={submit_seconds:.6}s finish={:.6}s",
                self.started.elapsed().as_secs_f64(),
                tick.elapsed().as_secs_f64()
            );
        }
        Ok(frame)
    }
}

pub fn read_lyrics_file(
    path: impl AsRef<Path>,
    format: LyricFormat,
) -> anyhow::Result<Vec<LyricLine>> {
    let text = fs::read_to_string(path)?;
    parse_lyrics(&text, format)
}
