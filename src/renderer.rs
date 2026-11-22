use crate::frame::RenderedFrame;
use anyhow::{Result, bail};
use skia_safe::{AlphaType, Color, ColorSpace, ColorType, ISize, ImageInfo};

use crate::background::BackgroundRenderer;
use crate::cover_renderer::CoverRenderer;
use crate::geometry::FrameGeometry;
use crate::layout::Layout;
use crate::lrc::LyricLine;
use crate::lyrics_renderer::{LyricsRenderer, LyricsStyle};

pub struct Renderer {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub lines: Vec<LyricLine>,
    geometry: FrameGeometry,
    layout: Layout,
    background: BackgroundRenderer,
    cover: Option<CoverRenderer>,
    lyrics: LyricsRenderer,
    surface: crate::surface::SurfaceRenderer,
    info: ImageInfo,
    row_bytes: usize,
}

impl Renderer {
    pub fn new(width: u32, height: u32, fps: u32, lines: Vec<LyricLine>) -> Result<Self> {
        Self::with_background(width, height, fps, lines, BackgroundRenderer::dynamic())
    }

    pub fn with_background(
        width: u32,
        height: u32,
        fps: u32,
        lines: Vec<LyricLine>,
        background: BackgroundRenderer,
    ) -> Result<Self> {
        Self::with_scene(width, height, fps, lines, background, None)
    }
    pub fn with_scene_options(
        width: u32,
        height: u32,
        fps: u32,
        lines: Vec<LyricLine>,
        background: BackgroundRenderer,
        cover: Option<CoverRenderer>,
        render_translation: bool,
        render_background_vocal: bool,
        lyrics_style: LyricsStyle,
        excluded_lines: &[usize],
    ) -> Result<Self> {
        Self::with_scene_options_for_encoder(
            width,
            height,
            fps,
            lines,
            background,
            cover,
            render_translation,
            render_background_vocal,
            lyrics_style,
            excluded_lines,
            crate::EncoderBackend::Pipe,
        )
    }

    pub fn with_scene_options_for_encoder(
        width: u32,
        height: u32,
        fps: u32,
        mut lines: Vec<LyricLine>,
        background: BackgroundRenderer,
        cover: Option<CoverRenderer>,
        render_translation: bool,
        render_background_vocal: bool,
        lyrics_style: LyricsStyle,
        excluded_lines: &[usize],
        encoder: crate::EncoderBackend,
    ) -> Result<Self> {
        if width == 0 || height == 0 || fps == 0 {
            bail!("width, height, and fps must be positive");
        }
        if !excluded_lines.is_empty() {
            let excluded = excluded_lines
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>();
            lines = lines
                .into_iter()
                .enumerate()
                .filter_map(|(index, line)| (!excluded.contains(&index)).then_some(line))
                .collect();
        }
        let geometry = FrameGeometry::for_frame(width, height);
        let lyrics = LyricsRenderer::new_with_options(
            &lines,
            geometry.lyrics,
            render_translation,
            render_background_vocal,
            lyrics_style,
        )?;
        let layout = Layout::new(
            &lines,
            geometry.lyrics.height,
            lyrics.group_heights(),
            lyrics.group_gap(),
            lyrics.interlude_slot_height(),
        );
        let surface = crate::surface::SurfaceRenderer::try_new_for_encoder(encoder, width, height)?;
        Ok(Self {
            width,
            height,
            fps,
            lines,
            geometry,
            layout,
            background,
            cover,
            lyrics,
            surface,
            info: ImageInfo::new(
                ISize::new(width as i32, height as i32),
                ColorType::RGBA8888,
                AlphaType::Premul,
                ColorSpace::new_srgb(),
            ),
            row_bytes: width as usize * 4,
        })
    }

    pub fn with_scene(
        width: u32,
        height: u32,
        fps: u32,
        lines: Vec<LyricLine>,
        background: BackgroundRenderer,
        cover: Option<CoverRenderer>,
    ) -> Result<Self> {
        Self::with_scene_options(
            width,
            height,
            fps,
            lines,
            background,
            cover,
            true,
            true,
            LyricsStyle::default(),
            &[],
        )
    }

    pub fn backend_details(&self) -> String {
        self.surface.backend_details()
    }
    pub fn backend_name(&self) -> &'static str {
        self.surface.backend_name()
    }

    pub fn render_frame(&mut self, t_ms: u64) -> Result<RenderedFrame> {
        #[cfg(target_os = "linux")]
        let mut frame = if self.surface.is_external_vulkan() {
            RenderedFrame::new_gpu_only(self.width, self.height)
        } else {
            RenderedFrame::new(self.width, self.height)
        };
        #[cfg(not(target_os = "linux"))]
        let mut frame = RenderedFrame::new(self.width, self.height);
        self.render_frame_into(t_ms, &mut frame)?;
        Ok(frame)
    }

    pub fn render_frame_into(&mut self, t_ms: u64, frame: &mut RenderedFrame) -> Result<()> {
        self.render_frame_impl(t_ms, frame, true, false)
    }

    pub fn render_frame_into_opts(
        &mut self,
        t_ms: u64,
        frame: &mut RenderedFrame,
        read_cpu_pixels: bool,
    ) -> Result<()> {
        self.render_frame_impl(t_ms, frame, read_cpu_pixels, true)
    }

    fn render_frame_impl(
        &mut self,
        t_ms: u64,
        frame: &mut RenderedFrame,
        read_cpu_pixels: bool,
        gpu_frame: bool,
    ) -> Result<()> {
        if frame.width() != self.width || frame.height() != self.height {
            bail!(
                "invalid frame dimensions: expected {}x{}, got {}x{}",
                self.width,
                self.height,
                frame.width(),
                frame.height()
            );
        }
        if frame.row_bytes() != self.row_bytes
            || (!self.surface.is_external_vulkan()
                && frame.pixels().len() != self.height as usize * self.row_bytes)
        {
            bail!("invalid frame pixel buffer");
        }
        self.layout.update(&self.lines, t_ms, self.fps);
        let geometry = self.geometry;
        let lines = &self.lines;
        let layout = &self.layout;
        let background = &mut self.background;
        let cover = &mut self.cover;
        let lyrics = &self.lyrics;
        let draw = |canvas: &skia_safe::Canvas| {
            canvas.clear(Color::BLACK);
            background.draw(canvas, &geometry, t_ms)?;
            if let Some(cover) = cover {
                cover.draw(canvas, &geometry)?;
            }
            lyrics.draw(canvas, lines, layout, t_ms, geometry.lyrics.height as u32)
        };
        if gpu_frame || self.surface.is_external_vulkan() {
            self.surface
                .render_into_frame(&self.info, self.row_bytes, frame, read_cpu_pixels, draw)
        } else {
            #[cfg(target_os = "macos")]
            frame.set_gpu_frame(None);
            #[cfg(target_os = "linux")]
            frame.clear_gpu_payload();
            self.surface
                .render_into(&self.info, self.row_bytes, frame.pixels_mut(), draw)
        }
    }
}

#[cfg(target_os = "windows")]
pub fn probe_renderer_initialization(width: u32, height: u32, fps: u32) -> Result<()> {
    use crate::lrc::LyricLine;

    skia_safe::icu::init();
    let _font_manager = skia_safe::FontMgr::new();
    let lines = vec![LyricLine {
        start_ms: 0,
        end_ms: 1_000,
        text: "Goosy 渲染探针".to_owned(),
        translation: Some("Renderer probe".to_owned()),
        agent_id: None,
        is_duet: false,
        is_background: false,
        background_vocal: None,
        words: Vec::new(),
    }];
    let mut renderer = Renderer::new(width, height, fps, lines)?;
    let mut frame = RenderedFrame::new(width, height);
    renderer.render_frame_into(0, &mut frame)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FrameSink;
    #[test]
    fn settled_lyrics_render_identical_frames() -> Result<()> {
        let lines = vec![
            LyricLine {
                start_ms: 0,
                end_ms: 2_000,
                text: "Stable line".to_owned(),
                translation: None,
                agent_id: None,
                is_duet: false,
                is_background: false,
                background_vocal: None,
                words: Vec::new(),
            },
            LyricLine {
                start_ms: 2_000,
                end_ms: 4_000,
                text: "Next line".to_owned(),
                translation: None,
                agent_id: None,
                is_duet: false,
                is_background: false,
                background_vocal: None,
                words: Vec::new(),
            },
        ];
        let mut renderer = Renderer::new(320, 180, 30, lines)?;
        for _ in 0..180 {
            renderer.render_frame(1_000)?;
        }
        assert_eq!(renderer.render_frame(1_000)?, renderer.render_frame(1_000)?);
        Ok(())
    }

    #[test]
    fn high_resolution_frames_are_complete_and_stable() -> Result<()> {
        let lines = vec![LyricLine {
            start_ms: 0,
            end_ms: 2_000,
            text: "High resolution stability".to_owned(),
            translation: Some("高分辨率稳定性".to_owned()),
            agent_id: None,
            is_duet: false,
            is_background: false,
            background_vocal: None,
            words: Vec::new(),
        }];
        let width = 3_840;
        let height = 2_160;
        let expected_len = width as usize * height as usize * 4;
        let mut renderer = Renderer::new(width, height, 30, lines)?;
        let mut frame = RenderedFrame::new(width, height);
        for timestamp in [0, 33, 66] {
            renderer.render_frame_into(timestamp, &mut frame)?;
            assert_eq!(frame.pixels().len(), expected_len);
        }
        renderer.render_frame_into(33, &mut frame)?;
        let repeated = frame.pixels().to_owned();
        renderer.render_frame_into(33, &mut frame)?;
        assert_eq!(frame.pixels(), repeated.as_slice());
        Ok(())
    }

    #[test]
    fn frame_sink_receives_complete_rendered_frame() -> Result<()> {
        struct Sink {
            received: Option<RenderedFrame>,
        }
        impl FrameSink for Sink {
            fn submit(&mut self, frame: RenderedFrame) -> Result<RenderedFrame> {
                self.received = Some(frame);
                Ok(RenderedFrame::new(320, 180))
            }
            fn finish(&mut self) -> Result<()> {
                Ok(())
            }
        }
        let mut renderer = Renderer::new(320, 180, 30, Vec::new())?;
        let frame = renderer.render_frame(0)?;
        let mut sink = Sink { received: None };
        let recycled = sink.submit(frame)?;
        assert_eq!(recycled.width(), 320);
        let received = sink.received.as_ref().unwrap();
        assert_eq!(
            (
                received.width(),
                received.height(),
                received.row_bytes(),
                received.pixels().len()
            ),
            (320, 180, 320 * 4, 320 * 180 * 4)
        );
        sink.finish()
    }

    #[test]
    fn excluded_lines_are_removed_without_retiming_remaining_lines() -> Result<()> {
        let lines = vec![
            LyricLine {
                start_ms: 0,
                end_ms: 1_000,
                text: "Excluded".to_owned(),
                translation: None,
                agent_id: None,
                is_duet: false,
                is_background: false,
                background_vocal: None,
                words: Vec::new(),
            },
            LyricLine {
                start_ms: 1_000,
                end_ms: 2_000,
                text: "Visible".to_owned(),
                translation: None,
                agent_id: None,
                is_duet: false,
                is_background: false,
                background_vocal: None,
                words: Vec::new(),
            },
        ];
        let renderer = Renderer::with_scene_options(
            320,
            180,
            30,
            lines,
            BackgroundRenderer::dynamic(),
            None,
            true,
            true,
            LyricsStyle::default(),
            &[0],
        )?;
        assert_eq!(renderer.lines.len(), 1);
        assert_eq!(renderer.lines[0].text, "Visible");
        assert_eq!(renderer.lines[0].start_ms, 1_000);
        Ok(())
    }
}
