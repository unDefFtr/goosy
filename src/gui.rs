use crate::gui_shared::{
    LYRIC_EXTENSIONS, PreviewFrame, StylePreviewScene, load_style_preview_assets,
    random_sample_range as random_sample_window,
    render_style_preview as render_style_preview_frame, sample_output_path, scan_sibling_assets,
};
use eframe::egui;
use libgoosy::{
    EncoderBackend, LyricFormat, LyricsStyle, RenderOptions, VideoCodec,
    image_asset::SceneImageAssets,
    lrc,
    task_manager::{Control, TaskId, TaskManager, TaskState},
    video,
};
use rfd::FileDialog;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

const DEFAULT_PREVIEW_INTERVAL: u32 = 15;
const RENDER_TWO_COLUMN_MIN_WIDTH: f32 = 760.0;

fn render_page_uses_columns(available_width: f32) -> bool {
    available_width >= RENDER_TWO_COLUMN_MIN_WIDTH
}

fn style_percent_slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::Slider::new(value, range).suffix("%").show_value(true));
    });
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StylePreviewKey {
    style: [u32; 10],
    scene: StylePreviewScene,
}

fn preview_color_image(frame: PreviewFrame) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied(
        [frame.width as usize, frame.height as usize],
        &frame.pixels,
    )
}

#[cfg(test)]
fn render_style_preview(
    style: LyricsStyle,
    scene: &StylePreviewScene,
    cover_asset: Option<&libgoosy::image_asset::ImageAsset>,
) -> Result<egui::ColorImage, String> {
    let assets = SceneImageAssets {
        background: cover_asset.cloned(),
        cover: cover_asset.cloned(),
    };
    render_style_preview_frame(style, scene, &assets).map(preview_color_image)
}

struct StylePreviewRequest {
    key: StylePreviewKey,
    style: LyricsStyle,
    context: egui::Context,
}

struct StylePreviewOutput {
    key: StylePreviewKey,
    image: Result<egui::ColorImage, String>,
}

fn spawn_style_preview_worker() -> (
    mpsc::Sender<StylePreviewRequest>,
    mpsc::Receiver<StylePreviewOutput>,
) {
    let (request_sender, request_receiver) = mpsc::channel::<StylePreviewRequest>();
    let (output_sender, output_receiver) = mpsc::channel::<StylePreviewOutput>();
    thread::spawn(move || {
        let mut cached_assets: Option<(
            (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>),
            Result<SceneImageAssets, String>,
        )> = None;
        while let Ok(mut request) = request_receiver.recv() {
            while let Ok(newer) = request_receiver.try_recv() {
                request = newer;
            }
            let source = (
                request.key.scene.background_path.clone(),
                request.key.scene.cover_path.clone(),
                request.key.scene.embedded_cover_audio.clone(),
            );
            if cached_assets.as_ref().map(|cached| &cached.0) != Some(&source) {
                cached_assets = Some((source, load_style_preview_assets(&request.key.scene)));
            }
            let image = match cached_assets
                .as_ref()
                .expect("preview asset cache")
                .1
                .as_ref()
            {
                Ok(assets) => render_style_preview_frame(
                    request.style,
                    &request.key.scene,
                    assets,
                )
                .map(preview_color_image),
                Err(error) => Err(error.clone()),
            };
            let key = request.key;
            if output_sender
                .send(StylePreviewOutput { key, image })
                .is_err()
            {
                return;
            }
            request.context.request_repaint();
        }
    });
    (request_sender, output_receiver)
}

const BUNDLED_CJK_FONT: &[u8] = include_bytes!("../assets/fonts/NotoSansCJKsc-Regular.otf");

fn configure_chinese_font(context: &egui::Context) {
    let mut definitions = egui::FontDefinitions::default();
    definitions.font_data.insert(
        "goosy_cjk".to_owned(),
        Arc::new(egui::FontData::from_owned(BUNDLED_CJK_FONT.to_vec())),
    );
    definitions
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .expect("egui proportional family")
        .insert(0, "goosy_cjk".to_owned());
    definitions
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .expect("egui monospace family")
        .insert(0, "goosy_cjk".to_owned());
    context.set_fonts(definitions);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppPage {
    Render,
    Lyrics,
}


pub fn run() -> eframe::Result<()> {
    #[allow(unused_mut)] // Only Windows overrides the default setup.
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    #[cfg(target_os = "windows")]
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        // Limit instance creation itself: enumerating Vulkan adapters can crash
        // inside Intel's igvk64.dll before an adapter is selected. A high-power
        // preference alone cannot prevent that. Keep WGPU_BACKEND as an opt-in
        // override for diagnostics.
        setup.instance_descriptor.backends =
            eframe::wgpu::Backends::from_env().unwrap_or(eframe::wgpu::Backends::DX12);
    }
    let options = eframe::NativeOptions {
        wgpu_options,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 620.0])
            .with_min_inner_size([700.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "GoosyRenderer",
        options,
        Box::new(|creation_context| {
            configure_chinese_font(&creation_context.egui_ctx);
            let manager = TaskManager::new()?;
            Ok(Box::new(GoosyApp {
                manager: Some(manager),
                ..GoosyApp::default()
            }))
        }),
    )
}

struct GoosyApp {
    audio: Option<PathBuf>,
    lyrics_candidates: Vec<PathBuf>,
    cover_candidates: Vec<PathBuf>,
    selected_lyrics: Option<usize>,
    selected_cover: Option<usize>,
    title: String,
    output: Option<PathBuf>,
    width: u32,
    height: u32,
    fps: u32,
    font_scale_percent: u32,
    encoder_backend: EncoderBackend,
    video_codec: VideoCodec,
    nvenc_preset: String,
    translation_font_scale_percent: u32,
    background_font_scale_percent: u32,
    line_height_percent: u32,
    line_spacing_percent: u32,
    translation_gap_percent: u32,
    background_gap_percent: u32,
    horizontal_padding_percent: u32,
    lyric_blur_sigma_step: u32,
    debug_overlays: bool,
    use_embedded_cover: bool,
    no_audio: bool,
    manager: Option<TaskManager>,
    selected_task: Option<TaskId>,
    state: Option<TaskState>,
    pending: Option<Control>,
    settling: bool,
    preview_interval: u32,
    preview_texture: Option<egui::TextureHandle>,
    preview_frame: u64,
    progress: f32,
    speed_history: VecDeque<(f32, f32)>,
    current_speed: f32,
    style_preview_key: Option<StylePreviewKey>,
    style_preview_texture: Option<egui::TextureHandle>,
    style_preview_error: Option<String>,
    style_preview_sender: mpsc::Sender<StylePreviewRequest>,
    style_preview_receiver: mpsc::Receiver<StylePreviewOutput>,
    status: String,
    page: AppPage,
    render_translation: bool,
    render_background_vocal: bool,
    speed_printer: bool,
    auto_exclude_credits: bool,
    lyric_lines: Vec<lrc::LyricLine>,
    manual_excluded_lines: Vec<bool>,
    auto_excluded_lines: Vec<bool>,
}

impl Default for GoosyApp {
    fn default() -> Self {
        let (style_preview_sender, style_preview_receiver) = spawn_style_preview_worker();
        Self {
            audio: None,
            lyrics_candidates: Vec::new(),
            cover_candidates: Vec::new(),
            selected_lyrics: None,
            selected_cover: None,
            title: String::new(),
            output: None,
            width: 1920,
            height: 1080,
            fps: 30,
            font_scale_percent: 100,
            translation_font_scale_percent: 65,
            background_font_scale_percent: 70,
            line_height_percent: 65,
            line_spacing_percent: 80,
            translation_gap_percent: 100,
            background_gap_percent: 100,
            horizontal_padding_percent: 100,
            lyric_blur_sigma_step: 6,
            debug_overlays: false,
            encoder_backend: EncoderBackend::Pipe,
            video_codec: VideoCodec::H264,
            nvenc_preset: "p5".to_owned(),
            use_embedded_cover: true,
            no_audio: false,
            manager: None,
            selected_task: None,
            state: None,
            pending: None,
            settling: false,
            preview_interval: DEFAULT_PREVIEW_INTERVAL,
            preview_texture: None,
            preview_frame: 0,
            progress: 0.0,
            speed_history: VecDeque::new(),
            current_speed: 0.0,
            style_preview_key: None,
            style_preview_texture: None,
            style_preview_error: None,
            style_preview_sender,
            style_preview_receiver,
            status: String::new(),
            page: AppPage::Render,
            render_translation: true,
            render_background_vocal: true,
            speed_printer: false,
            auto_exclude_credits: false,
            lyric_lines: Vec::new(),
            manual_excluded_lines: Vec::new(),
            auto_excluded_lines: Vec::new(),
        }
    }
}

impl GoosyApp {
    fn select_audio(&mut self, path: PathBuf) {
        let (lyrics, covers) = scan_sibling_assets(&path);
        let stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("lyrics")
            .to_owned();
        let output = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{stem}.mp4"));
        let metadata_title = video::audio_metadata(&path)
            .ok()
            .and_then(|metadata| metadata.title);
        self.audio = Some(path);
        self.lyrics_candidates = lyrics;
        self.cover_candidates = covers;
        self.selected_lyrics = (!self.lyrics_candidates.is_empty()).then_some(0);
        self.selected_cover = (!self.cover_candidates.is_empty()).then_some(0);
        self.title = metadata_title.unwrap_or(stem);
        self.output = Some(output);
        self.status = if self.selected_lyrics.is_some() {
            "已根据音频名称找到歌词和封面候选".to_owned()
        } else {
            "未找到同名歌词，将尝试读取音频内嵌歌词".to_owned()
        };
        self.reload_lyrics_preview();
    }

    fn reload_lyrics_preview(&mut self) {
        self.lyric_lines.clear();
        self.manual_excluded_lines.clear();
        self.auto_excluded_lines.clear();
        let Some(audio) = self.audio.as_deref() else {
            return;
        };
        let lyrics_path = self
            .selected_lyrics
            .and_then(|index| self.lyrics_candidates.get(index))
            .cloned();
        let text = match lyrics_path.as_deref() {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    self.status = format!("读取歌词失败：{error}");
                    return;
                }
            },
            None => match video::audio_metadata(audio)
                .ok()
                .and_then(|metadata| metadata.lyrics)
            {
                Some(text) => text,
                None => {
                    self.status = "当前音频没有可预览的内嵌歌词".to_owned();
                    return;
                }
            },
        };
        match libgoosy::parse_lyrics(&text, LyricFormat::Auto) {
            Ok(lines) => {
                self.lyric_lines = lines;
                self.manual_excluded_lines = vec![false; self.lyric_lines.len()];
                self.recompute_auto_exclusions();
            }
            Err(error) => {
                self.status = format!("解析歌词失败：{error}");
            }
        }
    }

    fn recompute_auto_exclusions(&mut self) {
        self.auto_excluded_lines = if self.auto_exclude_credits {
            detect_credit_lines(&self.lyric_lines)
        } else {
            vec![false; self.lyric_lines.len()]
        };
    }

    fn excluded_line_indices(&self) -> Vec<usize> {
        self.lyric_lines
            .iter()
            .enumerate()
            .filter_map(|(index, _)| {
                (self
                    .manual_excluded_lines
                    .get(index)
                    .copied()
                    .unwrap_or(false)
                    || self
                        .auto_excluded_lines
                        .get(index)
                        .copied()
                        .unwrap_or(false))
                .then_some(index)
            })
            .collect()
    }

    fn format_line_time(time_ms: u64) -> String {
        format!(
            "{:02}:{:02}.{:03}",
            time_ms / 60_000,
            (time_ms / 1_000) % 60,
            time_ms % 1_000
        )
    }

    fn draw_lyrics_page(&mut self, ui: &mut egui::Ui) {
        ui.heading("歌词行控制");
        ui.label("取消勾选的行不会进入渲染布局，后续歌词的时间轴保持不变。");
        ui.add_space(8.0);
        let auto_changed = ui
            .checkbox(
                &mut self.auto_exclude_credits,
                "自动排除头部/尾部的歌手、作曲、作词等署名信息",
            )
            .changed();
        if auto_changed {
            self.recompute_auto_exclusions();
        }
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.render_translation, "渲染翻译");
            ui.checkbox(&mut self.render_background_vocal, "渲染伴唱");
        });
        ui.checkbox(&mut self.speed_printer, "速印机优化（黑白双色）");
        ui.add_space(6.0);
        if ui
            .add_enabled(
                !self.lyric_lines.is_empty() && !self.blocks_new_work(),
                egui::Button::new("导出可打印 PDF…"),
            )
            .clicked()
        {
            self.export_pdf();
        }
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        ui.separator();
        if self.lyric_lines.is_empty() {
            ui.label("选择音频或歌词文件后，这里会显示可单独排除的歌词行。");
            return;
        }
        let excluded_count = self.excluded_line_indices().len();
        ui.label(format!(
            "共 {} 行，当前排除 {} 行",
            self.lyric_lines.len(),
            excluded_count
        ));
        egui::ScrollArea::vertical()
            .id_salt("lyric_line_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for index in 0..self.lyric_lines.len() {
                    let line = &self.lyric_lines[index];
                    let auto_excluded = self.auto_excluded_lines[index];
                    let mut excluded = self.manual_excluded_lines[index] || auto_excluded;
                    let label = format!(
                        "{}  {}",
                        Self::format_line_time(line.start_ms),
                        if line.text.trim().is_empty() {
                            "（空行）"
                        } else {
                            line.text.trim()
                        }
                    );
                    let changed = ui
                        .add_enabled(!auto_excluded, egui::Checkbox::new(&mut excluded, label))
                        .changed();
                    if changed {
                        self.manual_excluded_lines[index] = excluded;
                    }
                    if auto_excluded {
                        ui.label("自动识别的署名行");
                    }
                }
            });
    }

    fn export_pdf(&mut self) {
        if self.lyric_lines.is_empty() {
            self.status = "请先选择有效的歌词文件".to_owned();
            return;
        }
        let default_name = self
            .audio
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .unwrap_or("lyrics");
        let Some(mut output) = FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(format!("{default_name}.pdf"))
            .save_file()
        else {
            return;
        };
        if output.extension().is_none() {
            output.set_extension("pdf");
        }
        let Some(audio) = self.audio.as_ref() else {
            self.status = "请先选择音频".to_owned();
            return;
        };
        let Ok(mut executable) = std::env::current_exe() else {
            self.status = "无法定位 goosy 可执行文件".to_owned();
            return;
        };
        executable.set_file_name(if cfg!(windows) {
            "goosy-render-worker.exe"
        } else {
            "goosy-render-worker"
        });
        if !executable.is_file() {
            self.status = format!("找不到独立渲染进程：{}", executable.display());
            return;
        }
        let lyrics = self
            .selected_lyrics
            .and_then(|index| self.lyrics_candidates.get(index));
        let mut command = Command::new(executable);
        command.arg("pdf").arg(audio);
        if let Some(lyrics) = lyrics {
            command.arg(lyrics);
        }
        command.arg("--output").arg(&output);
        if !self.title.trim().is_empty() {
            command.arg("--title").arg(self.title.trim());
        }
        if !self.render_translation {
            command.arg("--no-translation");
        }
        if !self.render_background_vocal {
            command.arg("--no-background-vocal");
        }
        if self.speed_printer {
            command.arg("--speed-printer");
        }
        for index in self.excluded_line_indices() {
            command.arg("--exclude-line").arg(index.to_string());
        }
        self.status = match command.output() {
            Ok(result) if result.status.success() => {
                format!("PDF 导出完成：{}", output.display())
            }
            Ok(result) => format!(
                "PDF 导出失败：{}",
                String::from_utf8_lossy(&result.stderr).trim()
            ),
            Err(error) => format!("PDF 导出失败：{error}"),
        };
    }

    fn choose_audio(&mut self) {
        if let Some(path) = FileDialog::new()
            .add_filter(
                "Audio",
                &["mp3", "flac", "m4a", "wav", "aac", "ogg", "opus"],
            )
            .pick_file()
        {
            self.select_audio(path);
        }
    }

    fn choose_lyrics(&mut self) {
        let Some(path) = FileDialog::new()
            .add_filter("Lyrics", LYRIC_EXTENSIONS)
            .pick_file()
        else {
            return;
        };
        if !self.lyrics_candidates.contains(&path) {
            self.lyrics_candidates.push(path.clone());
        }
        self.selected_lyrics = self
            .lyrics_candidates
            .iter()
            .position(|candidate| candidate == &path);
        self.reload_lyrics_preview();
    }

    fn choose_cover(&mut self) {
        let Some(path) = FileDialog::new()
            .add_filter(
                "Images",
                &[
                    "jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff", "avif", "heic",
                    "heif",
                ],
            )
            .pick_file()
        else {
            return;
        };
        if !self.cover_candidates.contains(&path) {
            self.cover_candidates.push(path.clone());
        }
        self.selected_cover = self
            .cover_candidates
            .iter()
            .position(|candidate| candidate == &path);
    }

    fn current_lyrics_style(&self) -> LyricsStyle {
        LyricsStyle {
            font_scale: self.font_scale_percent as f32 / 100.0,
            translation_font_scale: self.translation_font_scale_percent as f32 / 100.0,
            background_font_scale: self.background_font_scale_percent as f32 / 100.0,
            line_height_scale: self.line_height_percent as f32 / 100.0,
            group_gap_scale: self.line_spacing_percent as f32 / 100.0,
            translation_gap_scale: self.translation_gap_percent as f32 / 100.0,
            background_gap_scale: self.background_gap_percent as f32 / 100.0,
            horizontal_padding_scale: self.horizontal_padding_percent as f32 / 100.0,
            lyric_blur_sigma_step: self.lyric_blur_sigma_step,
            debug_overlays: self.debug_overlays,
        }
    }

    fn refresh_style_preview(&mut self, context: &egui::Context) {
        let style_key = [
            self.font_scale_percent,
            self.translation_font_scale_percent,
            self.background_font_scale_percent,
            self.line_height_percent,
            self.line_spacing_percent,
            self.translation_gap_percent,
            self.background_gap_percent,
            self.horizontal_padding_percent,
            self.lyric_blur_sigma_step,
            self.debug_overlays as u32,
        ];
        let cover_path = self
            .selected_cover
            .and_then(|index| self.cover_candidates.get(index).cloned());
        let embedded_cover_audio = if cover_path.is_none() && self.use_embedded_cover {
            self.audio.clone()
        } else {
            None
        };
        let key = StylePreviewKey {
            style: style_key,
            scene: StylePreviewScene {
                width: self.width,
                height: self.height,
                title: self.title.clone(),
                cover_path,
                background_path: None,
                embedded_cover_audio,
                render_translation: self.render_translation,
                render_background_vocal: self.render_background_vocal,
            },
        };
        if self.style_preview_key.as_ref() != Some(&key) {
            self.style_preview_key = Some(key.clone());
            self.style_preview_error = None;
            if self
                .style_preview_sender
                .send(StylePreviewRequest {
                    key,
                    style: self.current_lyrics_style(),
                    context: context.clone(),
                })
                .is_err()
            {
                self.style_preview_error = Some("样式预览工作线程已经停止".to_owned());
            }
        }

        let mut latest = None;
        for output in self.style_preview_receiver.try_iter() {
            if self.style_preview_key.as_ref() == Some(&output.key) {
                latest = Some(output.image);
            }
        }
        match latest {
            Some(Ok(image)) => {
                if let Some(texture) = &mut self.style_preview_texture {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    self.style_preview_texture = Some(context.load_texture(
                        "goosy-style-preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                self.style_preview_error = None;
            }
            Some(Err(error)) => {
                self.style_preview_error = Some(error);
            }
            None => {}
        }
    }

    fn blocks_new_work(&self) -> bool {
        matches!(
            self.state,
            Some(TaskState::Queued | TaskState::Starting | TaskState::Running | TaskState::Paused)
        )
    }

    fn send_render_command(&mut self, command: Control) {
        if let (Some(manager), Some(id)) = (&self.manager, self.selected_task) {
            if let Err(error) = manager.command(id, command) {
                self.status = format!("{error:#}");
            }
        }
    }

    fn select_task(&mut self, id: TaskId) {
        self.selected_task = Some(id);
        self.preview_texture = None;
        self.preview_frame = 0;
        self.progress = 0.0;
        self.speed_history.clear();
    }
    fn render_output_path(&self, sample: Option<(u64, u64)>) -> Option<PathBuf> {
        let output = self.output.clone()?;
        match sample {
            Some((start_ms, _)) => sample_output_path(&output, start_ms),
            None => Some(output),
        }
    }

    fn random_sample_range(&self) -> Option<(u64, u64)> {
        random_sample_window(&self.lyric_lines, &self.excluded_line_indices())
    }

    fn start_sample_render(&mut self) {
        let Some(sample) = self.random_sample_range() else {
            self.status = "没有可用于采样的歌词".to_owned();
            return;
        };
        self.start_render_with_range(Some(sample));
    }

    fn start_render(&mut self) {
        self.start_render_with_range(None);
    }

    fn start_render_with_range(&mut self, sample: Option<(u64, u64)>) {
        let (Some(audio), Some(output)) = (self.audio.clone(), self.render_output_path(sample))
        else {
            self.status = "请先选择音频和输出".to_owned();
            return;
        };
        let lyrics = self
            .selected_lyrics
            .and_then(|index| self.lyrics_candidates.get(index).cloned());
        let cover = self
            .selected_cover
            .and_then(|index| self.cover_candidates.get(index).cloned());
        let options = RenderOptions {
            song: audio,
            lyrics,
            output,
            width: self.width,
            height: self.height,
            fps: self.fps,
            background: None,
            cover,
            title: (!self.title.trim().is_empty()).then(|| self.title.trim().to_owned()),
            no_embedded_cover: !self.use_embedded_cover,
            no_audio: self.no_audio || sample.is_some(),
            render_translation: self.render_translation,
            lyrics_style: self.current_lyrics_style(),
            render_background_vocal: self.render_background_vocal,
            excluded_lines: self.excluded_line_indices(),
            format: LyricFormat::Auto,
            sample_start_ms: sample.map(|(start, _)| start),
            sample_duration_ms: sample.map(|(start, end)| end.saturating_sub(start)),
            encoder: self.encoder_backend,
            codec: self.video_codec,
            nvenc_preset: self.nvenc_preset.clone(),
        };
        if let Some(manager) = &self.manager {
            match manager.enqueue(options, self.preview_interval) {
                Ok(id) => {
                    self.select_task(id);
                    self.state = Some(TaskState::Queued);
                }
                Err(error) => self.status = format!("创建任务失败：{error:#}"),
            }
        }
    }

    fn poll_render(&mut self, context: &egui::Context) {
        let (Some(manager), Some(id)) = (&self.manager, self.selected_task) else {
            return;
        };
        let Some(task) = manager.task(id) else { return };
        self.state = Some(task.state);
        self.pending = task.pending;
        self.settling = task.settling;
        self.progress = if task.total == 0 {
            0.0
        } else {
            task.done as f32 / task.total as f32
        };
        self.current_speed = task.fps as f32;
        if task.fps > 0.0
            && self
                .speed_history
                .back()
                .is_none_or(|&(t, _)| t != task.elapsed as f32)
        {
            self.speed_history
                .push_back((task.elapsed as f32, self.current_speed));
            if self.speed_history.len() > 120 {
                self.speed_history.pop_front();
            }
        }
        self.status = format!(
            "{} · {} · {}\n已渲染 {}/{} · 已提交分段 {} 帧\n{}\n{}",
            task.options.output.display(),
            task.state.name(),
            task.stage,
            task.done,
            task.total,
            task.committed,
            task.detail,
            task.log,
        );
        if let Some((number, frame)) = manager.take_preview(id) {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.pixels,
            );
            if let Some(texture) = &mut self.preview_texture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.preview_texture = Some(context.load_texture(
                    "goosy-live-preview",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
            self.preview_frame = number;
        }
        if self.blocks_new_work() || self.settling {
            context.request_repaint_after(Duration::from_millis(100));
        }
    }
    fn draw_speed_graph(&self, ui: &mut egui::Ui) {
        if self.speed_history.is_empty() {
            return;
        }
        ui.label(format!("实时渲染速度：{:.1} 帧/秒", self.current_speed));
        let width = ui.available_width().max(240.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 132.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, egui::Color32::from_rgb(21, 27, 38));
        let max_speed = self
            .speed_history
            .iter()
            .map(|(_, speed)| *speed)
            .fold(1.0_f32, f32::max)
            * 1.15;
        for fraction in [0.25_f32, 0.5, 0.75] {
            let y = rect.bottom() - rect.height() * fraction;
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                egui::Stroke::new(1.0, egui::Color32::from_gray(48)),
            );
        }
        let count = self.speed_history.len().max(2) as f32 - 1.0;
        let points: Vec<_> = self
            .speed_history
            .iter()
            .enumerate()
            .map(|(index, (_, speed))| {
                let x = rect.left() + rect.width() * index as f32 / count;
                let y = rect.bottom() - rect.height() * (*speed / max_speed).clamp(0.0, 1.0);
                egui::pos2(x, y)
            })
            .collect();
        for pair in points.windows(2) {
            painter.line_segment(
                [pair[0], pair[1]],
                egui::Stroke::new(2.0, egui::Color32::from_rgb(73, 214, 157)),
            );
        }
        painter.text(
            rect.left_top() + egui::vec2(8.0, 7.0),
            egui::Align2::LEFT_TOP,
            format!("0–{max_speed:.0} fps"),
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(180),
        );
    }

    fn draw_render_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("素材与输出");
        ui.label("选择音频后自动匹配同名歌词与封面。");
        ui.add_space(4.0);
        egui::Grid::new("source_grid")
            .num_columns(2)
            .spacing([8.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                ui.label("音频");
                ui.horizontal(|ui| {
                    let name = self
                        .audio
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .and_then(|name| name.to_str())
                        .unwrap_or("尚未选择");
                    ui.label(name);
                    if ui.button("选择…").clicked() {
                        self.choose_audio();
                    }
                });
                ui.end_row();

                ui.label("歌词");
                let previous_lyrics = self.selected_lyrics;
                ui.horizontal(|ui| {
                    if self.lyrics_candidates.is_empty() {
                        ui.label("自动读取内嵌歌词");
                    } else {
                        let selected = Self::selected_name(
                            &self.lyrics_candidates,
                            self.selected_lyrics,
                            "选择歌词",
                        );
                        egui::ComboBox::from_id_salt("lyrics_combo")
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for (index, path) in self.lyrics_candidates.iter().enumerate() {
                                    let name = path
                                        .file_name()
                                        .and_then(|name| name.to_str())
                                        .unwrap_or("lyrics");
                                    ui.selectable_value(
                                        &mut self.selected_lyrics,
                                        Some(index),
                                        name,
                                    );
                                }
                            });
                    }
                    if ui.button("手动…").clicked() {
                        self.choose_lyrics();
                    }
                });
                ui.end_row();
                if previous_lyrics != self.selected_lyrics {
                    self.reload_lyrics_preview();
                }

                ui.label("封面");
                ui.horizontal(|ui| {
                    let selected = Self::selected_name(
                        &self.cover_candidates,
                        self.selected_cover,
                        "不显示封面",
                    );
                    if self.cover_candidates.is_empty() {
                        ui.label(if self.use_embedded_cover {
                            "使用内嵌封面"
                        } else {
                            "未找到封面"
                        });
                    } else {
                        egui::ComboBox::from_id_salt("cover_combo")
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.selected_cover, None, "不显示封面");
                                for (index, path) in self.cover_candidates.iter().enumerate() {
                                    let name = path
                                        .file_name()
                                        .and_then(|name| name.to_str())
                                        .unwrap_or("cover");
                                    ui.selectable_value(
                                        &mut self.selected_cover,
                                        Some(index),
                                        name,
                                    );
                                }
                            });
                    }
                    if ui.button("手动…").clicked() {
                        self.choose_cover();
                    }
                });
                ui.end_row();

                ui.label("歌曲名称");
                ui.text_edit_singleline(&mut self.title);
                ui.end_row();

                ui.label("输出文件");
                ui.horizontal(|ui| {
                    let display = self
                        .output
                        .as_ref()
                        .and_then(|path| path.file_name())
                        .and_then(|name| name.to_str())
                        .unwrap_or("选择音频后自动生成");
                    let response = ui.label(display);
                    if let Some(output) = &self.output {
                        response.on_hover_text(output.display().to_string());
                    }
                    if ui.button("选择…").clicked() {
                        if let Some(path) =
                            FileDialog::new().set_file_name("lyrics.mp4").save_file()
                        {
                            self.output = Some(path);
                        }
                    }
                });
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.group(|ui| {
            ui.strong("输出参数");
            ui.horizontal_wrapped(|ui| {
                ui.label("尺寸");
                ui.add(
                    egui::DragValue::new(&mut self.width)
                        .range(320..=7680)
                        .suffix(" px"),
                );
                ui.label("×");
                ui.add(
                    egui::DragValue::new(&mut self.height)
                        .range(180..=4320)
                        .suffix(" px"),
                );
                ui.label("帧率");
                ui.add(
                    egui::DragValue::new(&mut self.fps)
                        .range(1..=120)
                        .suffix(" fps"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("编码器");
                ui.add_enabled_ui(!self.blocks_new_work(), |ui| {
                    egui::ComboBox::from_id_salt("encoder_combo")
                        .selected_text(self.encoder_backend.label())
                        .show_ui(ui, |ui| {
                            for &backend in EncoderBackend::ALL {
                                ui.selectable_value(
                                    &mut self.encoder_backend,
                                    backend,
                                    backend.label(),
                                );
                            }
                        });
                    if self.encoder_backend.is_zero_copy() {
                        self.video_codec = VideoCodec::H264;
                    }
                    let codec_enabled = !self.encoder_backend.is_zero_copy();
                    ui.add_enabled_ui(codec_enabled, |ui| {
                        ui.label("格式");
                        egui::ComboBox::from_id_salt("video_codec_combo")
                            .selected_text(self.video_codec.label())
                            .show_ui(ui, |ui| {
                                for codec in VideoCodec::ALL {
                                    ui.selectable_value(&mut self.video_codec, codec, codec.label());
                                }
                            });
                    });
                });
            });
            let uses_nvenc = match self.encoder_backend {
                EncoderBackend::Pipe | EncoderBackend::FfmpegNext => {
                    video::preferred_video_encoder_name() == "h264_nvenc"
                }
                #[cfg(target_os = "linux")]
                EncoderBackend::FfmpegNextNvencZeroCopy => true,
            };
            if uses_nvenc {
                ui.add_enabled_ui(!self.blocks_new_work(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("NVENC Preset");
                        egui::ComboBox::from_id_salt("nvenc_preset_combo")
                            .selected_text(&self.nvenc_preset)
                            .show_ui(ui, |ui| {
                                for preset in ["p1", "p2", "p3", "p4", "p5", "p6", "p7"] {
                                    ui.selectable_value(
                                        &mut self.nvenc_preset,
                                        preset.to_owned(),
                                        preset,
                                    );
                                }
                            });
                    });
                });
            }
            egui::CollapsingHeader::new("歌词样式")
                .default_open(true)
                .show(ui, |ui| {
                    style_percent_slider(ui, "字号", &mut self.font_scale_percent, 50..=200);
                    style_percent_slider(
                        ui,
                        "翻译字号",
                        &mut self.translation_font_scale_percent,
                        50..=200,
                    );
                    style_percent_slider(
                        ui,
                        "伴唱字号",
                        &mut self.background_font_scale_percent,
                        50..=200,
                    );
                    style_percent_slider(ui, "段内行高", &mut self.line_height_percent, 50..=180);
                    style_percent_slider(ui, "歌词行间距", &mut self.line_spacing_percent, 0..=200);
                    style_percent_slider(
                        ui,
                        "翻译间距",
                        &mut self.translation_gap_percent,
                        0..=200,
                    );
                    style_percent_slider(ui, "伴唱间距", &mut self.background_gap_percent, 0..=200);
                    style_percent_slider(
                        ui,
                        "左右留白",
                        &mut self.horizontal_padding_percent,
                        0..=200,
                    );
                    ui.horizontal(|ui| {
                        ui.label("模糊 Sigma 步长");
                        ui.add(
                            egui::Slider::new(&mut self.lyric_blur_sigma_step, 1..=20)
                                .suffix(" / 行"),
                        );
                        ui.label(format!("最大 {}", self.lyric_blur_sigma_step * 7));
                    });
                    ui.checkbox(&mut self.debug_overlays, "调试框（容器/字形/行距）");
                });
            ui.horizontal(|ui| {
                ui.label("预览间隔");
                ui.add(
                    egui::DragValue::new(&mut self.preview_interval)
                        .range(1..=300)
                        .suffix(" 帧/次"),
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.use_embedded_cover, "使用内嵌封面");
                ui.checkbox(&mut self.no_audio, "不输出音频");
            });
        });
    }

    fn draw_render_monitor(&mut self, ui: &mut egui::Ui) {
        ui.heading("渲染状态");
        let mut selected = None;
        egui::ComboBox::from_id_salt("render-task-history")
            .selected_text("选择历史任务")
            .show_ui(ui, |ui| {
                if let Some(manager) = &self.manager {
                    for task in manager.tasks().iter().rev() {
                        if ui
                            .selectable_label(
                                self.selected_task == Some(task.id),
                                format!(
                                    "{} · {}",
                                    task.options.output.display(),
                                    task.state.name()
                                ),
                            )
                            .clicked()
                        {
                            selected = Some(task.id);
                        }
                    }
                }
            });
        if let Some(id) = selected {
            self.select_task(id);
            self.poll_render(ui.ctx());
        }
        let blocks_new_work = self.blocks_new_work();
        let can_control = self.state.is_some_and(|state| !state.is_terminal());
        let paused = matches!(self.state, Some(TaskState::Paused | TaskState::Interrupted));
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!blocks_new_work, egui::Button::new("开始渲染"))
                .clicked()
            {
                self.start_render();
                ui.ctx().request_repaint();
            }
            if ui
                .add_enabled(
                    !blocks_new_work && !self.lyric_lines.is_empty(),
                    egui::Button::new("随机采样 15 秒"),
                )
                .clicked()
            {
                self.start_sample_render();
                ui.ctx().request_repaint();
            }
            if can_control
                && ui
                    .add_enabled(
                        !self.settling
                            && self.pending.is_none()
                            && (paused || self.state == Some(TaskState::Running)),
                        egui::Button::new(if paused {
                            "继续渲染"
                        } else {
                            "暂停渲染"
                        }),
                    )
                    .clicked()
            {
                self.send_render_command(if paused { Control::Resume } else { Control::Pause });
                ui.ctx().request_repaint();
            }
            if can_control
                && ui
                    .add_enabled(
                        !self.settling && self.pending != Some(Control::Cancel),
                        egui::Button::new(
                            egui::RichText::new("取消任务")
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(176, 32, 48)),
                    )
                    .clicked()
            {
                self.send_render_command(Control::Cancel);
                ui.ctx().request_repaint();
            }
            if blocks_new_work || self.settling {
                ui.spinner();
            }
        });
        if blocks_new_work || self.progress > 0.0 {
            ui.add(egui::ProgressBar::new(self.progress).show_percentage());
        }
        if let Some(texture) = &self.preview_texture {
            ui.add_space(4.0);
            ui.label(format!("实时预览 · 第 {} 帧", self.preview_frame));
            ui.add(
                egui::Image::new(texture)
                    .max_width(ui.available_width())
                    .max_height(260.0)
                    .maintain_aspect_ratio(true),
            );
        } else if let Some(texture) = &self.style_preview_texture {
            ui.add_space(4.0);
            ui.label(format!(
                "参数即时预览 · {}×{} 等比",
                self.width, self.height
            ));
            ui.add(
                egui::Image::new(texture)
                    .max_width(ui.available_width())
                    .max_height(260.0)
                    .maintain_aspect_ratio(true),
            );
            ui.weak("拖动左侧歌词样式参数，预览会立即更新。");
        } else if let Some(error) = &self.style_preview_error {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("样式预览失败：{error}"));
        } else if !blocks_new_work {
            ui.weak("正在生成样式预览…");
        }
        if !self.speed_history.is_empty() {
            self.draw_speed_graph(ui);
        }
        if !self.status.is_empty() {
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("render_status")
                .max_height(90.0)
                .show(ui, |ui| {
                    ui.label(&self.status);
                });
        }
    }

    fn draw_render_page(&mut self, ui: &mut egui::Ui) {
        self.refresh_style_preview(ui.ctx());
        if render_page_uses_columns(ui.available_width()) {
            ui.columns(2, |columns| {
                let (left, right) = columns.split_at_mut(1);
                egui::ScrollArea::vertical()
                    .id_salt("render_settings_scroll")
                    .show(&mut left[0], |ui| self.draw_render_settings(ui));
                egui::ScrollArea::vertical()
                    .id_salt("render_monitor_scroll")
                    .show(&mut right[0], |ui| self.draw_render_monitor(ui));
            });
        } else {
            egui::ScrollArea::vertical()
                .id_salt("render_compact_scroll")
                .show(ui, |ui| {
                    self.draw_render_settings(ui);
                    ui.separator();
                    self.draw_render_monitor(ui);
                });
        }
    }

    fn selected_name(paths: &[PathBuf], index: Option<usize>, empty: &str) -> String {
        index
            .and_then(|index| paths.get(index))
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| empty.to_owned())
    }
}


impl eframe::App for GoosyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_render(ui.ctx());
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("GoosyRenderer");
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.page == AppPage::Render, "渲染设置")
                    .clicked()
                {
                    self.page = AppPage::Render;
                }
                if ui
                    .selectable_label(self.page == AppPage::Lyrics, "歌词行控制")
                    .clicked()
                {
                    self.page = AppPage::Lyrics;
                }
            });
            ui.separator();
            if self.page == AppPage::Render {
                self.draw_render_page(ui);
            } else {
                self.draw_lyrics_page(ui);
            }
        });
    }
}

const CREDIT_MARKERS: &[&str] = &[
    "歌手", "作曲", "作词", "作詞", "演唱", "编曲", "制作", "原唱", "词：", "曲：", "词:", "曲:",
    "artist", "singer", "composer", "lyricist", "arranger", "producer",
];

fn detect_credit_lines(lines: &[lrc::LyricLine]) -> Vec<bool> {
    let mut excluded = vec![false; lines.len()];
    if lines.is_empty() {
        return excluded;
    }
    let edge_count = ((lines.len() + 9) / 10).clamp(2, 8).min(lines.len());
    for (index, line) in lines.iter().enumerate() {
        let in_edge = index < edge_count || index >= lines.len() - edge_count;
        let main = line.text.to_lowercase();
        let translation = line
            .translation
            .as_deref()
            .unwrap_or_default()
            .to_lowercase();
        excluded[index] = in_edge
            && CREDIT_MARKERS
                .iter()
                .any(|marker| main.contains(marker) || translation.contains(marker));
    }
    excluded
}

#[cfg(test)]
mod tests {
    use super::{
        GoosyApp, StylePreviewKey, StylePreviewRequest, detect_credit_lines,
        render_page_uses_columns, render_style_preview, spawn_style_preview_worker,
    };
    use crate::gui_shared::{
        COVER_EXTENSIONS, LYRIC_EXTENSIONS, StylePreviewScene, has_extension,
        style_preview_dimensions,
    };
    use libgoosy::{
        EncoderBackend, LyricsStyle, VideoCodec, image_asset::ImageAsset, lrc::LyricLine,
    };

    fn line(text: &str) -> LyricLine {
        LyricLine {
            start_ms: 0,
            end_ms: 1_000,
            text: text.to_owned(),
            translation: None,
            agent_id: None,
            is_duet: false,
            is_background: false,
            background_vocal: None,
            words: Vec::new(),
        }
    }

    fn preview_scene(width: u32, height: u32) -> StylePreviewScene {
        StylePreviewScene {
            width,
            height,
            title: "Preview title".to_owned(),
            cover_path: None,
            background_path: None,
            embedded_cover_audio: None,
            render_translation: true,
            render_background_vocal: true,
        }
    }

    fn preview_key(style: [u32; 10], scene: StylePreviewScene) -> StylePreviewKey {
        StylePreviewKey { style, scene }
    }

    fn test_cover_png() -> Vec<u8> {
        use skia_safe::{EncodedImageFormat, ImageInfo, Pixmap};

        let info = ImageInfo::new_n32_premul((2, 2), None);
        let mut pixels = [
            40, 80, 220, 255, 40, 80, 220, 255, 40, 80, 220, 255, 40, 80, 220, 255,
        ];
        let pixmap = Pixmap::new(&info, &mut pixels, 8).unwrap();
        pixmap.encode(EncodedImageFormat::PNG, 100).unwrap()
    }

    #[test]
    fn accepts_case_insensitive_sibling_extensions() {
        assert!(has_extension("TTML", LYRIC_EXTENSIONS));
        assert!(has_extension("JPEG", COVER_EXTENSIONS));
        assert!(has_extension("webp", COVER_EXTENSIONS));
        assert!(!has_extension("mp3", COVER_EXTENSIONS));
    }

    #[test]
    fn detects_credit_markers_only_at_edges() {
        let lines = vec![
            line("歌手：Someone"),
            line("普通歌词"),
            line("作词：Someone"),
        ];
        assert_eq!(detect_credit_lines(&lines), vec![true, false, true]);
    }

    #[test]
    fn does_not_exclude_credit_word_in_middle_of_song() {
        let mut lines = vec![line("普通歌词"); 10];
        lines[5] = line("作曲的旋律");
        assert!(!detect_credit_lines(&lines)[5]);
    }


    #[test]
    fn random_sample_selects_a_bounded_lyrics_window() {
        let mut app = GoosyApp::default();
        app.lyric_lines = vec![line("first"), line("second")];
        app.lyric_lines[0].start_ms = 20_000;
        app.lyric_lines[0].end_ms = 21_000;
        app.lyric_lines[1].start_ms = 40_000;
        app.lyric_lines[1].end_ms = 41_000;

        let (start, end) = app.random_sample_range().unwrap();
        assert_eq!(end - start, 15_000);
        assert!(start <= 27_000);
        assert!(end <= 42_000);
    }

    #[test]
    fn random_sample_returns_none_without_nonempty_lyrics() {
        let mut app = GoosyApp::default();
        app.lyric_lines = vec![line("   ")];
        assert_eq!(app.random_sample_range(), None);
    }


    #[test]
    fn render_page_switches_to_a_scrollable_single_column_on_narrow_windows() {
        assert!(!render_page_uses_columns(700.0));
        assert!(render_page_uses_columns(760.0));
        assert!(render_page_uses_columns(960.0));
    }

    #[test]
    fn render_page_stays_within_default_and_minimum_viewports() {
        for size in [
            eframe::egui::vec2(960.0, 620.0),
            eframe::egui::vec2(700.0, 480.0),
        ] {
            let context = eframe::egui::Context::default();
            let input = eframe::egui::RawInput {
                screen_rect: Some(eframe::egui::Rect::from_min_size(
                    eframe::egui::Pos2::ZERO,
                    size,
                )),
                ..Default::default()
            };
            let mut app = GoosyApp::default();

            let mut output = context.run_ui(input, |ui| app.draw_render_page(ui));
            output.textures_delta.clear();

            let used = context.globally_used_rect();
            assert!(
                used.width() <= size.x + 1.0,
                "used={used:?}, viewport={size:?}"
            );
            assert!(
                used.height() <= size.y + 1.0,
                "used={used:?}, viewport={size:?}"
            );
        }
    }

    #[test]
    fn style_preview_pixels_change_with_layout_parameters() {
        let scene = preview_scene(1_920, 1_080);
        let compact = render_style_preview(
            LyricsStyle {
                line_height_scale: 0.5,
                group_gap_scale: 0.0,
                translation_gap_scale: 0.0,
                background_gap_scale: 0.0,
                horizontal_padding_scale: 0.0,
                ..LyricsStyle::default()
            },
            &scene,
            None,
        )
        .unwrap();
        let expanded = render_style_preview(
            LyricsStyle {
                line_height_scale: 1.8,
                group_gap_scale: 2.0,
                translation_gap_scale: 2.0,
                background_gap_scale: 2.0,
                horizontal_padding_scale: 2.0,
                ..LyricsStyle::default()
            },
            &scene,
            None,
        )
        .unwrap();

        assert_eq!(compact.size, expanded.size);
        assert!(
            compact
                .pixels
                .iter()
                .zip(&expanded.pixels)
                .any(|(left, right)| left != right)
        );
    }
    #[test]
    fn style_preview_worker_converges_to_the_latest_request() {
        let (sender, receiver) = spawn_style_preview_worker();
        let context = eframe::egui::Context::default();
        let first_key = preview_key(
            [100, 100, 100, 100, 100, 100, 100, 100, 6, 0],
            preview_scene(1_920, 1_080),
        );
        let final_key = preview_key(
            [120, 50, 70, 140, 180, 50, 160, 130, 6, 0],
            preview_scene(1_280, 720),
        );
        sender
            .send(StylePreviewRequest {
                key: first_key,
                style: LyricsStyle::default(),
                context: context.clone(),
            })
            .unwrap();
        sender
            .send(StylePreviewRequest {
                key: final_key.clone(),
                style: LyricsStyle {
                    font_scale: 1.2,
                    translation_font_scale: 0.5,
                    background_font_scale: 0.7,
                    line_height_scale: 1.4,
                    group_gap_scale: 1.8,
                    translation_gap_scale: 0.5,
                    background_gap_scale: 1.6,
                    horizontal_padding_scale: 1.3,
                    lyric_blur_sigma_step: 6,
                    debug_overlays: false,
                },
                context,
            })
            .unwrap();

        loop {
            let output = receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            if output.key == final_key {
                assert!(output.image.is_ok());
                break;
            }
        }
    }

    #[test]
    fn style_preview_uses_output_aspect_ratio_and_selected_cover() {
        assert_eq!(style_preview_dimensions(1_080, 1_920), (203, 360));
        let landscape = preview_scene(1_920, 1_080);
        let without_cover = render_style_preview(LyricsStyle::default(), &landscape, None).unwrap();
        let cover_asset = ImageAsset::from_bytes(&test_cover_png()).unwrap();
        let with_cover =
            render_style_preview(LyricsStyle::default(), &landscape, Some(&cover_asset)).unwrap();
        let split_x = (with_cover.size[0] as f32 * 0.381_966_011_25) as usize;
        let cover_center = split_x / 2 + with_cover.size[1] / 2 * with_cover.size[0];

        assert_eq!(with_cover.size, [640, 360]);
        assert_ne!(
            with_cover.pixels[cover_center],
            without_cover.pixels[cover_center]
        );
    }

    #[test]
    fn style_preview_font_size_is_derived_from_selected_output_resolution() {
        let low_resolution =
            render_style_preview(LyricsStyle::default(), &preview_scene(640, 360), None).unwrap();
        let full_hd =
            render_style_preview(LyricsStyle::default(), &preview_scene(1_920, 1_080), None)
                .unwrap();
        let bright_neutral_pixels = |image: &eframe::egui::ColorImage| {
            image
                .pixels
                .iter()
                .filter(|pixel| {
                    let [red, green, blue, _] = pixel.to_array();
                    red > 100
                        && red.abs_diff(green) <= 20
                        && red.abs_diff(blue) <= 20
                        && green.abs_diff(blue) <= 20
                })
                .count()
        };

        assert_eq!(low_resolution.size, full_hd.size);
        assert!(bright_neutral_pixels(&low_resolution) > bright_neutral_pixels(&full_hd));
    }
    #[test]
    fn interrupted_history_allows_draft_and_task_actions() {
        use eframe::egui;
        use super::TaskState;

        // Probe the actual widgets by hovering each painted label. This also works on
        // backends without accessibility support and never opens a native file dialog.
        fn button_enabled(mut draw: impl FnMut(&mut egui::Ui), label: &str) -> bool {
            let context = egui::Context::default();
            let mut layout = context.run_ui(Default::default(), &mut draw);
            layout.textures_delta.clear();
            let center = layout.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            }).unwrap_or_else(|| panic!("missing button: {label}"));
            context.run_ui(egui::RawInput {
                events: vec![egui::Event::PointerMoved(center)],
                ..Default::default()
            }, draw).textures_delta.clear();
            let hovered = context.interaction_snapshot(|snapshot| snapshot.hovered.clone());
            hovered.iter().any(|id| context.read_response(*id).is_some_and(|response| {
                response.enabled() && response.sense.senses_click()
            }))
        }

        let mut app = GoosyApp {
            selected_task: Some("00000000-0000-0000-0000-000000000001".parse().unwrap()),
            state: Some(TaskState::Interrupted),
            lyric_lines: vec![line("one line")],
            manual_excluded_lines: vec![false],
            auto_excluded_lines: vec![false],
            ..Default::default()
        };
        for label in ["开始渲染", "随机采样 15 秒", "继续渲染", "取消任务"] {
            assert!(button_enabled(|ui| app.draw_render_monitor(ui), label), "{label}");
        }
        assert!(button_enabled(|ui| app.draw_lyrics_page(ui), "导出可打印 PDF…"));
        app.settling = true;
        assert!(button_enabled(|ui| app.draw_render_monitor(ui), "开始渲染"));
        assert!(!button_enabled(|ui| app.draw_render_monitor(ui), "继续渲染"));
        assert!(!button_enabled(|ui| app.draw_render_monitor(ui), "取消任务"));
    }

    #[test]
    fn default_app_uses_pipe_encoder() {
        assert_eq!(GoosyApp::default().encoder_backend, EncoderBackend::Pipe);
    }

    #[test]
    fn encoder_order_labels_and_cli_values_are_stable() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            EncoderBackend::ALL,
            [
                EncoderBackend::Pipe,
                EncoderBackend::FfmpegNext,
                EncoderBackend::FfmpegNextNvencZeroCopy,
            ]
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            EncoderBackend::ALL,
            [EncoderBackend::Pipe, EncoderBackend::FfmpegNext]
        );
        assert_eq!(EncoderBackend::ALL[0].label(), "PipeEncoder（外部 FFmpeg）");
        assert_eq!(
            EncoderBackend::ALL[1].label(),
            "ffmpeg-next Encoder（自动选择）"
        );
        assert_eq!(GoosyApp::default().video_codec, VideoCodec::H264);
        assert_eq!(GoosyApp::default().nvenc_preset, "p5");
        assert_eq!(VideoCodec::ALL, [VideoCodec::H264, VideoCodec::Hevc]);
        assert_eq!(VideoCodec::H264.label(), "H.264 (AVC)");
        assert_eq!(VideoCodec::Hevc.label(), "HEVC (H.265)");
    }
}
