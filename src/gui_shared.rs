use libgoosy::image_asset::SceneImageAssets;
use libgoosy::{
    LyricsStyle, background, cover_renderer, geometry::FrameGeometry, image_asset, layout::Layout,
    lrc, lyrics_renderer::LyricsRenderer, video,
};
use std::path::{Path, PathBuf};

pub(crate) const STYLE_PREVIEW_MAX_WIDTH: u32 = 640;
pub(crate) const STYLE_PREVIEW_MAX_HEIGHT: u32 = 360;
const STYLE_PREVIEW_TIME_MS: u64 = 2_500;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StylePreviewScene<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) title: String,
    pub(crate) cover_path: Option<PathBuf>,
    pub(crate) background_path: Option<PathBuf>,
    pub(crate) embedded_cover_audio: Option<PathBuf>,
    pub(crate) render_translation: bool,
    pub(crate) render_background_vocal: bool,
    pub(crate) sample_text: Option<&'a [String; 5]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreviewFrame {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<u8>,
}

pub(crate) fn style_preview_dimensions(width: u32, height: u32) -> (u32, u32) {
    let scale = (STYLE_PREVIEW_MAX_WIDTH as f64 / width.max(1) as f64)
        .min(STYLE_PREVIEW_MAX_HEIGHT as f64 / height.max(1) as f64)
        .min(1.0);
    (
        (width as f64 * scale).round().max(1.0) as u32,
        (height as f64 * scale).round().max(1.0) as u32,
    )
}

fn style_preview_lines(sample_text: Option<&[String; 5]>) -> Vec<lrc::LyricLine> {
    let [previous, current, translation, backing, next] = sample_text
        .map(|text| text.each_ref().map(String::as_str))
        .unwrap_or([
            "上一行歌词 · Previous line",
            "正在演唱的歌词，用于预览换行与行高",
            "Live style preview for spacing and line height",
            "伴唱歌词 · Background vocal",
            "下一行歌词 · Next line",
        ]);
    vec![
        lrc::LyricLine {
            start_ms: 0,
            end_ms: 1_500,
            text: previous.to_owned(),
            translation: None,
            agent_id: None,
            is_duet: false,
            is_background: false,
            background_vocal: None,
            words: Vec::new(),
        },
        lrc::LyricLine {
            start_ms: 1_500,
            end_ms: 4_500,
            text: current.to_owned(),
            translation: Some(translation.to_owned()),
            agent_id: None,
            is_duet: false,
            is_background: false,
            background_vocal: Some(lrc::BackgroundVocal {
                start_ms: 2_000,
                end_ms: 3_500,
                text: backing.to_owned(),
                translation: None,
                words: vec![lrc::LyricWord {
                    start_ms: 2_000,
                    end_ms: 3_500,
                    text: backing.to_owned(),
                }],
            }),
            words: Vec::new(),
        },
        lrc::LyricLine {
            start_ms: 4_500,
            end_ms: 6_500,
            text: next.to_owned(),
            translation: None,
            agent_id: None,
            is_duet: false,
            is_background: false,
            background_vocal: None,
            words: Vec::new(),
        },
    ]
}

pub(crate) fn load_style_preview_assets(
    scene: &StylePreviewScene<'_>,
) -> Result<SceneImageAssets, String> {
    let cover_path = scene.cover_path.as_deref();
    let background_path = scene.background_path.as_deref();
    let embedded_cover = if cover_path.is_none() {
        match scene.embedded_cover_audio.as_deref() {
            Some(audio) => video::embedded_cover_image(audio)
                .map_err(|error| format!("读取音频内嵌封面失败：{error:#}"))?,
            None => None,
        }
    } else {
        None
    };
    image_asset::load_scene_image_assets(background_path, cover_path, embedded_cover.as_deref())
        .map_err(|error| format!("读取预览图片失败：{error:#}"))
}

pub(crate) fn render_style_preview(
    style: LyricsStyle,
    scene: &StylePreviewScene<'_>,
    assets: &SceneImageAssets,
) -> Result<PreviewFrame, String> {
    use skia_safe::{AlphaType, ColorType, IPoint, ImageInfo, surfaces};

    let output_width = scene.width.max(1);
    let output_height = scene.height.max(1);
    let preview_dimensions = style_preview_dimensions(output_width, output_height);
    let preview_size = (preview_dimensions.0 as i32, preview_dimensions.1 as i32);
    let geometry = FrameGeometry::for_frame(output_width, output_height);
    let lines = style_preview_lines(scene.sample_text);
    let renderer = LyricsRenderer::new_with_options(
        &lines,
        geometry.lyrics,
        scene.render_translation,
        scene.render_background_vocal,
        style,
    )
    .map_err(|error| error.to_string())?;
    let mut layout = Layout::new(
        &lines,
        geometry.lyrics.height,
        renderer.group_heights(),
        renderer.group_gap(),
        renderer.interlude_slot_height(),
    );
    layout.update(&lines, STYLE_PREVIEW_TIME_MS, 30);
    let mut background = if let Some(asset) = assets.background.as_ref() {
        background::BackgroundRenderer::from_image_asset(asset)
            .map_err(|error| error.to_string())?
    } else {
        background::BackgroundRenderer::dynamic()
    };
    let mut cover = assets
        .cover
        .as_ref()
        .map(|asset| {
            cover_renderer::CoverRenderer::from_image_asset(asset, Some(scene.title.clone()))
        })
        .transpose()
        .map_err(|error| error.to_string())?;

    let mut surface = surfaces::raster_n32_premul(preview_size)
        .ok_or_else(|| "无法创建样式预览画布".to_owned())?;
    let canvas = surface.canvas();
    canvas.clear(skia_safe::Color::BLACK);
    let saved = canvas.save();
    canvas.scale((
        preview_dimensions.0 as f32 / output_width as f32,
        preview_dimensions.1 as f32 / output_height as f32,
    ));
    background
        .draw(canvas, &geometry, STYLE_PREVIEW_TIME_MS)
        .map_err(|error| error.to_string())?;
    if let Some(cover) = &mut cover {
        cover
            .draw(canvas, &geometry)
            .map_err(|error| error.to_string())?;
    }
    renderer
        .draw(
            canvas,
            &lines,
            &layout,
            STYLE_PREVIEW_TIME_MS,
            output_height,
        )
        .map_err(|error| error.to_string())?;
    canvas.restore_to_count(saved);

    let info = ImageInfo::new(preview_size, ColorType::RGBA8888, AlphaType::Premul, None);
    let row_bytes = preview_dimensions.0 as usize * 4;
    let mut pixels = vec![0; info.compute_byte_size(row_bytes)];
    if !surface.read_pixels(&info, &mut pixels, row_bytes, IPoint::new(0, 0)) {
        return Err("读取样式预览像素失败".to_owned());
    }
    Ok(PreviewFrame {
        width: preview_dimensions.0,
        height: preview_dimensions.1,
        pixels,
    })
}

pub(crate) const LYRIC_EXTENSIONS: &[&str] = &["lrc", "ttml", "xml", "yrc"];
pub(crate) const COVER_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff", "avif", "heic", "heif",
];

pub(crate) fn has_extension(extension: &str, accepted: &[&str]) -> bool {
    accepted
        .iter()
        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
}

pub(crate) fn scan_sibling_assets(audio: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let Some(parent) = audio.parent() else {
        return (Vec::new(), Vec::new());
    };
    let Some(stem) = audio.file_stem().and_then(|stem| stem.to_str()) else {
        return (Vec::new(), Vec::new());
    };
    let target_stem = stem.to_ascii_lowercase();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return (Vec::new(), Vec::new());
    };
    let mut lyrics = Vec::new();
    let mut covers = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path == audio {
            continue;
        }
        let Some(candidate_stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if candidate_stem.to_ascii_lowercase() != target_stem {
            continue;
        }
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            continue;
        };
        if has_extension(extension, LYRIC_EXTENSIONS) {
            lyrics.push(path);
        } else if has_extension(extension, COVER_EXTENSIONS) {
            covers.push(path);
        }
    }
    lyrics.sort();
    covers.sort();
    (lyrics, covers)
}

pub(crate) fn random_sample_range(
    lines: &[lrc::LyricLine],
    excluded: &[usize],
) -> Option<(u64, u64)> {
    let end_ms = lines
        .iter()
        .map(|line| line.end_ms)
        .max()?
        .saturating_add(1_000);
    let duration_ms = 15_000.min(end_ms);
    if duration_ms == 0 {
        return None;
    }
    let max_start = end_ms.saturating_sub(duration_ms);
    let eligible = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| !line.text.trim().is_empty() && !excluded.contains(index))
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return None;
    }
    let (_, line) = eligible.get(
        (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos() as usize)
            % eligible.len(),
    )?;
    let start = line.start_ms.saturating_sub(3_000).min(max_start);
    Some((start, start.saturating_add(duration_ms)))
}

pub(crate) fn sample_output_path(output: &Path, start_ms: u64) -> Option<PathBuf> {
    let stem = output.file_stem()?.to_str()?;
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("mp4");
    Some(output.with_file_name(format!("{stem}.sample-{start_ms}ms.{extension}")))
}
