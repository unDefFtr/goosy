use crate::gui_shared::{
    StylePreviewScene, load_style_preview_assets, random_sample_range, render_style_preview,
    sample_output_path, scan_sibling_assets,
};
use goosy_rinui::{FrameUpdate, RgbaFrame, Snapshot, ViewModel};
use libgoosy::task_manager::{Control, TaskId, TaskInfo, TaskManager};
use libgoosy::{EncoderBackend, RenderOptions, VideoCodec, video};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

const DEFAULT_PREVIEW_INTERVAL: u32 = 15;
const DEFAULT_SAMPLE_DURATION: u32 = 15_000;
const MAX_RECENT_TASKS: usize = 20;

fn path_to_file_url(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::with_capacity(raw.len() + 8);
    if raw.starts_with('/') {
        encoded.push_str("file://");
    } else {
        encoded.push_str("file:///");
    }
    for byte in raw.bytes() {
        match byte {
            b'%' | b'#' | b'?' | b' ' => {
                use std::fmt::Write as _;
                let _ = write!(&mut encoded, "%{byte:02X}");
            }
            _ => encoded.push(byte as char),
        }
    }
    encoded
}

#[derive(Default)]
struct Assets {
    audio: Option<PathBuf>,
    lyrics: Vec<PathBuf>,
    cover: Vec<PathBuf>,
    lyrics_index: usize,
    cover_index: usize,
    lyrics_text: String,
    notice: String,
}

impl Assets {
    fn selected(candidates: &[PathBuf], index: usize) -> Option<&Path> {
        index
            .checked_sub(1)
            .and_then(|i| candidates.get(i))
            .map(PathBuf::as_path)
    }

    fn pick(candidates: &mut Vec<PathBuf>, path: Option<PathBuf>) -> usize {
        let Some(path) = path else { return 0 };
        if let Some(index) = candidates.iter().position(|candidate| *candidate == path) {
            index + 1
        } else {
            candidates.push(path);
            candidates.len()
        }
    }

    fn labels<'a>(fallback: &'a str, paths: &'a [PathBuf]) -> impl Iterator<Item = &'a str> {
        std::iter::once(fallback).chain(paths.iter().map(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("未知文件名")
        }))
    }
}

// Only the latest scheduled work and its latest result are retained. The wake channel has no
// payload, so replacing a pending request does not accumulate frames or asset probes.
struct LatestJob<I, O> {
    slots: Arc<Mutex<JobSlots<I, O>>>,
    wake: mpsc::SyncSender<()>,
}

struct JobSlots<I, O> {
    request: Option<I>,
    result: Option<O>,
}

impl<I: Send + 'static, O: Send + 'static> LatestJob<I, O> {
    fn new(name: &str, mut perform: impl FnMut(I) -> O + Send + 'static) -> std::io::Result<Self> {
        let slots = Arc::new(Mutex::new(JobSlots::<I, O> {
            request: None,
            result: None,
        }));
        let thread_slots = Arc::clone(&slots);
        let (wake, recv) = mpsc::sync_channel(1);
        thread::Builder::new().name(name.into()).spawn(move || {
            while recv.recv().is_ok() {
                let request = thread_slots.lock().unwrap().request.take();
                if let Some(request) = request {
                    let result = perform(request);
                    thread_slots.lock().unwrap().result = Some(result);
                }
            }
        })?;
        Ok(Self { slots, wake })
    }

    fn request(&self, request: I) -> bool {
        self.slots.lock().unwrap().request = Some(request);
        !matches!(
            self.wake.try_send(()),
            Err(mpsc::TrySendError::Disconnected(()))
        )
    }

    fn poll(&self) -> Option<O> {
        self.slots.lock().unwrap().result.take()
    }
}

#[derive(Clone, Eq, PartialEq)]
struct PreviewAssets {
    background: Option<PathBuf>,
    cover: Option<PathBuf>,
    embedded: Option<PathBuf>,
}

struct AudioSelection {
    serial: u64,
    lyrics: Vec<PathBuf>,
    cover: Vec<PathBuf>,
    title: Option<String>,
    notice: String,
}

type StyleRequest = (u64, StylePreviewScene, libgoosy::LyricsStyle);
type StyleResult = (u64, Result<RgbaFrame, String>);

#[derive(Default)]
struct Wizard {
    active: bool,
    step: u32,
    checked: bool,
    issues: Vec<Value>,
    mode: u32,
    created_id: Option<TaskId>,
    sample_range: Option<(u64, u64)>,
    completed: u32,
}

pub(crate) struct Model {
    options: RenderOptions,
    assets: Assets,
    audio_job: LatestJob<(u64, PathBuf), AudioSelection>,
    audio_serial: u64,
    matching_assets: bool,
    title_edited: bool,
    lyrics_edited: bool,
    cover_edited: bool,
    preview_interval: u32,
    sample_start: u32,
    sample_duration: u32,
    style_job: LatestJob<StyleRequest, StyleResult>,
    style_serial: u64,
    style_dirty: bool,
    style_error: String,
    style: FrameUpdate,
    render: FrameUpdate,
    manager: TaskManager,
    selected_task: Option<TaskId>,
    preview_cache: VecDeque<(TaskId, libgoosy::task_manager::RgbaFrame)>,
    pending_task_thumbnails: Vec<(String, FrameUpdate)>,
    task_error: String,
    last_task_json: String,
    project_dirty: bool,
    wizard: Wizard,
}

impl Model {
    pub fn new() -> std::io::Result<Self> {
        let mut options = RenderOptions::new(PathBuf::new(), PathBuf::new());
        options.encoder = EncoderBackend::Pipe; // Match the legacy GUI default.
        let audio_job = LatestJob::new("goosy-asset-probe", |(serial, audio): (u64, PathBuf)| {
            let (lyrics, cover) = scan_sibling_assets(&audio);
            let (title, notice) = match video::audio_metadata(&audio) {
                Ok(metadata) => {
                    let notice = if !lyrics.is_empty() {
                        "已匹配同名素材".to_owned()
                    } else if metadata.lyrics.is_some() {
                        "将使用音频内嵌歌词".to_owned()
                    } else {
                        "未找到同名或内嵌歌词，请手动选择".to_owned()
                    };
                    (metadata.title, notice)
                }
                Err(error) => (None, format!("读取音频元数据失败：{error:#}")),
            };
            AudioSelection {
                serial,
                lyrics,
                cover,
                title,
                notice,
            }
        })?;
        let mut cached = None;
        let style_job = LatestJob::new(
            "goosy-style-preview",
            move |(serial, scene, style): StyleRequest| {
                let sources = PreviewAssets {
                    background: scene.background_path.clone(),
                    cover: scene.cover_path.clone(),
                    embedded: scene.embedded_cover_audio.clone(),
                };
                if !cached.as_ref().is_some_and(|(key, _)| *key == sources) {
                    cached = Some((sources, load_style_preview_assets(&scene)));
                }
                let result = match &cached.as_ref().expect("preview asset cache").1 {
                    Ok(assets) => {
                        render_style_preview(style, &scene, assets).map(|frame| RgbaFrame {
                            width: frame.width,
                            height: frame.height,
                            pixels: frame.pixels,
                        })
                    }
                    Err(error) => Err(error.clone()),
                };
                (serial, result)
            },
        )?;
        Ok(Self {
            options,
            assets: Assets::default(),
            audio_job,
            audio_serial: 0,
            matching_assets: false,
            title_edited: false,
            lyrics_edited: false,
            cover_edited: false,
            preview_interval: DEFAULT_PREVIEW_INTERVAL,
            sample_start: 0,
            sample_duration: DEFAULT_SAMPLE_DURATION,
            style_job,
            style_serial: 0,
            style_dirty: true,
            style_error: String::new(),
            style: FrameUpdate::Keep,
            render: FrameUpdate::Keep,
            manager: TaskManager::new()?,
            selected_task: None,
            preview_cache: VecDeque::new(),
            pending_task_thumbnails: Vec::new(),
            task_error: String::new(),
            last_task_json: String::new(),
            project_dirty: true,
            wizard: Wizard::default(),
        })
    }

    fn project_json(&self) -> Value {
        let label = |path: &Path| path.to_string_lossy().into_owned();
        json!({
            "audio": self.assets.audio.as_deref().map(label).unwrap_or_default(),
            "background": self.options.background.as_deref().map(label).unwrap_or_default(),
            "output": label(&self.options.output), "title": self.options.title.as_deref().unwrap_or_default(),
            "notice": self.assets.notice, "canStart": !self.matching_assets && self.assets.audio.is_some() && !self.options.output.as_os_str().is_empty(),
            "lyricsCandidates": Assets::labels("内嵌歌词", &self.assets.lyrics).collect::<Vec<_>>(),
            "coverCandidates": Assets::labels("无外部封面", &self.assets.cover).collect::<Vec<_>>(),
            "lyricsIndex": self.assets.lyrics_index, "coverIndex": self.assets.cover_index,
            "settings": serde_json::from_str::<Value>(&self.defaults_json()).expect("serialized settings"),
            "wizard": {
                "active": self.wizard.active, "step": self.wizard.step,
                "checked": self.wizard.checked, "issues": self.wizard.issues,
                "mode": self.wizard.mode,
                "completed": self.wizard.completed,
                "createdId": self.wizard.created_id.map(|id| id.to_string()),
                "effectiveOutput": if self.wizard.mode == 1 {
                    sample_output_path(&self.options.output, u64::from(self.sample_start))
                } else if let Some((start, _)) = self.wizard.sample_range {
                    sample_output_path(&self.options.output, start)
                } else { Some(self.options.output.clone()) }.map(|path| label(&path)),
            },
            "lyricsPath": Assets::selected(&self.assets.lyrics, self.assets.lyrics_index).map(label),
            "lyricsContent": self.assets.lyrics_text,
            "coverPath": Assets::selected(&self.assets.cover, self.assets.cover_index).map(label),
        })
    }

    fn task_thumbnail_url(&self, task: &TaskInfo) -> String {
        if self.preview_cache.iter().any(|(id, _)| *id == task.id) {
            return format!("image://goosy/task/{}/{}", task.id, task.preview_frame);
        }
        for candidate in [
            task.options.background.as_deref(),
            task.options.cover.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if candidate.is_file() {
                return path_to_file_url(candidate);
            }
        }
        String::new()
    }

    fn task_json(&self) -> Value {
        let tasks = self.manager.tasks();
        let task_value = |task: &TaskInfo| {
            json!({
                "id": task.id.to_string(), "state": task.state.name(),
                "pending": task.pending.map(|control| match control {
                    Control::Pause => "pause", Control::Resume => "resume", Control::Cancel => "cancel",
                }).unwrap_or(""),
                "stage": task.stage, "detail": task.detail,
                "done": task.done, "total": task.total, "elapsed": task.elapsed, "fps": task.fps, "log": task.log,
                "previewFrame": task.preview_frame, "previewNote": task.preview_note,
                "updatedAt": task.updated_at_ms, "lastActivitySeq": task.last_activity_seq,
                "thumbnail": self.task_thumbnail_url(task),
                "output": task.options.output.to_string_lossy(),
                "title": task.options.title.as_deref().unwrap_or("未命名任务"),
                "config": {
                    "audio": task.options.song.to_string_lossy(),
                    "lyrics": task.options.lyrics.as_ref().map(|path| path.to_string_lossy()),
                    "cover": task.options.cover.as_ref().map(|path| path.to_string_lossy()),
                    "background": task.options.background.as_ref().map(|path| path.to_string_lossy()),
                    "width": task.options.width, "height": task.options.height, "fps": task.options.fps,
                    "encoder": task.options.encoder.label(), "codec": task.options.codec.label(),
                    "sampleStart": task.options.sample_start_ms,
                    "sampleDuration": task.options.sample_duration_ms,
                },
            })
        };
        let mut recent: Vec<&TaskInfo> = tasks.iter().collect();
        recent.sort_by(|left, right| {
            (right.last_activity_seq, right.updated_at_ms, right.id)
                .cmp(&(left.last_activity_seq, left.updated_at_ms, left.id))
        });
        recent.truncate(MAX_RECENT_TASKS);
        let mut value = tasks
            .iter()
            .find(|task| Some(task.id) == self.selected_task)
            .map(task_value)
            .unwrap_or_else(|| json!({"state": "idle", "pending": ""}));
        value["tasks"] = tasks.iter().map(task_value).collect::<Vec<_>>().into();
        value["recentTasks"] = recent.into_iter().map(task_value).collect::<Vec<_>>().into();
        value["selectedId"] = self
            .selected_task
            .map(|id| id.to_string())
            .unwrap_or_default()
            .into();
        value["maxConcurrent"] = self.manager.max_concurrent().into();
        value["error"] = self.task_error.clone().into();
        value["styleError"] = self.style_error.clone().into();
        value
    }

    fn selected_audio(&mut self, path: PathBuf) {
        let stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("lyrics");
        self.options.output = path.with_file_name(format!("{stem}.mp4"));
        self.options.title = Some(stem.to_owned());
        self.assets = Assets {
            audio: Some(path.clone()),
            notice: "正在匹配同名素材…".into(),
            ..Assets::default()
        };
        self.title_edited = false;
        self.lyrics_edited = false;
        self.cover_edited = false;
        self.audio_serial += 1;
        self.matching_assets = self.audio_job.request((self.audio_serial, path));
        self.style_dirty = true;
        if !self.matching_assets {
            self.assets.notice = "素材读取线程已退出".into();
        }
    }

    fn poll_assets(&mut self) {
        let Some(selection) = self
            .audio_job
            .poll()
            .filter(|result| result.serial == self.audio_serial)
        else {
            return;
        };
        self.matching_assets = false;
        self.assets.notice = selection.notice;
        if !self.lyrics_edited {
            self.assets.lyrics = selection.lyrics;
            self.assets.lyrics_index = usize::from(!self.assets.lyrics.is_empty());
        }
        if !self.cover_edited {
            self.assets.cover = selection.cover;
            self.assets.cover_index = usize::from(!self.assets.cover.is_empty());
        }
        if !self.title_edited && selection.title.is_some() {
            self.options.title = selection.title;
        }
        self.draft_changed();
        self.project_dirty = true;
        self.refresh_lyrics();
        self.style_dirty = true;
    }

    fn poll_style(&mut self) {
        if std::mem::take(&mut self.style_dirty) {
            let cover =
                Assets::selected(&self.assets.cover, self.assets.cover_index).map(Path::to_owned);
            let embedded = (cover.is_none() && !self.options.no_embedded_cover)
                .then(|| self.assets.audio.clone())
                .flatten();
            let scene = StylePreviewScene {
                width: self.options.width,
                height: self.options.height,
                title: self.options.title.clone().unwrap_or_default(),
                background_path: self.options.background.clone(),
                cover_path: cover,
                embedded_cover_audio: embedded,
                render_translation: self.options.render_translation,
                render_background_vocal: self.options.render_background_vocal,
            };
            self.style_serial += 1;
            if !self
                .style_job
                .request((self.style_serial, scene, self.options.lyrics_style))
            {
                self.style_error = "样式预览线程已退出".into();
            }
        }
        if let Some((_, result)) = self
            .style_job
            .poll()
            .filter(|(serial, _)| *serial == self.style_serial)
        {
            match result {
                Ok(image) => {
                    self.style = FrameUpdate::Replace(image);
                    if !self.style_error.is_empty() {
                        self.style_error.clear();
                    }
                }
                Err(error) => {
                    self.style = FrameUpdate::Clear;
                    self.style_error = error;
                }
            }
        }
    }
    fn draft_changed(&mut self) {
        self.wizard.checked = false;
        self.wizard.issues.clear();
        self.wizard.sample_range = None;
        self.wizard.completed = 0;
        self.project_dirty = true;
    }
    fn refresh_lyrics(&mut self) {
        self.assets.lyrics_text =
            if let Some(path) = Assets::selected(&self.assets.lyrics, self.assets.lyrics_index) {
                std::fs::read_to_string(path).unwrap_or_default()
            } else {
                self.assets
                    .audio
                    .as_ref()
                    .and_then(|audio| video::audio_metadata(audio).ok())
                    .and_then(|metadata| metadata.lyrics)
                    .unwrap_or_default()
            };
    }

    fn navigate_wizard(&mut self, target: u32) {
        let target = target.min(3);
        if !self.wizard.active {
            return;
        }
        if target == self.wizard.step {
            return;
        }
        if target > self.wizard.step {
            self.validate_draft(target);
            if let Some(first) = self
                .wizard
                .issues
                .iter()
                .filter_map(|issue| issue["step"].as_u64())
                .min()
            {
                self.wizard.step = first as u32;
                self.wizard.completed &= (1 << first) - 1;
                self.project_dirty = true;
                return;
            }
            self.wizard.completed |= (1 << target) - 1;
            self.wizard.checked = false;
        }
        self.wizard.step = target;
        self.project_dirty = true;
    }

    fn validate_draft(&mut self, before_step: u32) {
        let mut issues = Vec::new();
        let mut sample_range = None;
        let mut issue = |step, field: &str, message: String| {
            if step >= before_step {
                return;
            }
            let brief = match field {
                "audio" if message.starts_with("无法") => "无法读取音频，请重新选择。",
                "lyrics" if message.starts_with("无法") => "无法读取歌词，请选择有效的歌词文件。",
                "cover" | "background" if message.starts_with("无法") => {
                    "无法读取图片，请重新选择。"
                }
                "encoder" => "编码器不支持此格式，请更换格式。",
                "output" if message.contains("已存在") || message.contains("已被") => {
                    "输出路径已占用，请另选文件名。"
                }
                "output" if message.contains("目录不存在") => "输出目录不存在，请重新选择。",
                "output" if message.contains("缺少文件名") => "请选择输出文件。",
                _ => message.as_str(),
            };
            issues.push(json!({"step": step, "field": field, "message": brief,
                "detail": if brief == message { "" } else { message.as_str() }}));
        };
        if self.matching_assets {
            issue(0, "audio", "素材读取中，请稍后检查。".into());
        }
        match self.assets.audio.as_ref() {
            None => issue(0, "audio", "请选择音频文件".into()),
            Some(audio) => match video::audio_metadata(audio) {
                Err(error) => issue(0, "audio", format!("无法读取音频：{error:#}")),
                Ok(metadata) => {
                    let lyrics = if let Some(path) =
                        Assets::selected(&self.assets.lyrics, self.assets.lyrics_index)
                    {
                        libgoosy::read_lyrics_file(path, self.options.format)
                    } else {
                        metadata
                            .lyrics
                            .as_deref()
                            .ok_or_else(|| anyhow::anyhow!("音频没有内嵌歌词，请选择歌词文件"))
                            .and_then(|text| libgoosy::parse_lyrics(text, self.options.format))
                    };
                    match lyrics {
                        Err(error) => issue(0, "lyrics", format!("无法读取歌词：{error:#}")),
                        Ok(lines) => {
                            if lines.is_empty() {
                                issue(0, "lyrics", "歌词为空，请选择其他文件。".into());
                            }
                            if self.wizard.mode == 2 {
                                sample_range = self.wizard.sample_range.or_else(|| {
                                    random_sample_range(&lines, &self.options.excluded_lines)
                                });
                                if sample_range.is_none() {
                                    issue(0, "lyrics", "无可采样歌词，请更换歌词文件。".into());
                                }
                            }
                            let duration = lines
                                .iter()
                                .map(|line| line.end_ms)
                                .max()
                                .unwrap_or(0)
                                .saturating_add(1_000);
                            if self.wizard.mode == 1 && u64::from(self.sample_start) >= duration {
                                issue(2, "sample", "起点超出歌词时长，请减小采样起点。".into());
                            }
                        }
                    }
                }
            },
        }
        for (field, path) in [
            (
                "cover",
                Assets::selected(&self.assets.cover, self.assets.cover_index),
            ),
            ("background", self.options.background.as_deref()),
        ] {
            if let Some(path) = path {
                if let Err(error) = libgoosy::image_asset::ImageAsset::from_path(path) {
                    issue(0, field, format!("无法读取图片：{error:#}"));
                }
            }
        }
        let codec = if self.options.encoder.is_zero_copy() {
            VideoCodec::H264
        } else {
            self.options.codec
        };
        if let Err(error) = self.options.encoder.validate_codec(codec) {
            issue(2, "encoder", format!("{error:#}"));
        }
        if self.options.width % 2 != 0 || self.options.height % 2 != 0 {
            issue(2, "dimensions", "宽度和高度须为偶数，请调整尺寸。".into());
        }
        let output = if self.wizard.mode == 1 {
            sample_output_path(&self.options.output, u64::from(self.sample_start))
        } else if let Some((start, _)) = sample_range {
            sample_output_path(&self.options.output, start)
        } else {
            Some(self.options.output.clone())
        };
        match output {
            Some(output) => {
                if let Err(error) = self.manager.check_output(&output) {
                    issue(2, "output", format!("{error:#}"));
                }
            }
            None => issue(2, "output", "请选择有效的输出路径".into()),
        }
        self.wizard.issues = issues;
        self.wizard.checked = true;
        self.wizard.sample_range = sample_range;
        self.project_dirty = true;
    }

    fn start(&mut self, action: &str) {
        if self.matching_assets || self.options.output.as_os_str().is_empty() {
            return;
        }
        let Some(song) = self.assets.audio.clone() else {
            return;
        };
        let mut options = self.options.clone();
        options.song = song;
        options.lyrics =
            Assets::selected(&self.assets.lyrics, self.assets.lyrics_index).map(Path::to_owned);
        options.cover =
            Assets::selected(&self.assets.cover, self.assets.cover_index).map(Path::to_owned);
        options.title = options
            .title
            .take()
            .filter(|title| !title.trim().is_empty());
        if options.encoder.is_zero_copy() {
            options.codec = VideoCodec::H264;
        }
        self.render = FrameUpdate::Clear;
        if action == "sample" {
            let start = u64::from(self.sample_start);
            let Some(output) = sample_output_path(&options.output, start) else {
                self.fail("无法生成采样输出文件名".into());
                return;
            };
            options.output = output;
            options.sample_start_ms = Some(start);
            options.sample_duration_ms = Some(u64::from(self.sample_duration));
            options.no_audio = true;
        }
        if action == "randomSample" {
            let result = (|| -> anyhow::Result<()> {
                let (start, end) = if let Some(range) = self.wizard.sample_range {
                    range
                } else {
                    let lines = if let Some(lyrics) = &options.lyrics {
                        libgoosy::read_lyrics_file(lyrics, options.format)?
                    } else {
                        let metadata = video::audio_metadata(&options.song)?;
                        let text = metadata
                            .lyrics
                            .ok_or_else(|| anyhow::anyhow!("没有可用于采样的歌词"))?;
                        libgoosy::parse_lyrics(&text, options.format)?
                    };
                    random_sample_range(&lines, &options.excluded_lines)
                        .ok_or_else(|| anyhow::anyhow!("没有可用于采样的歌词"))?
                };
                options.output = sample_output_path(&options.output, start)
                    .ok_or_else(|| anyhow::anyhow!("无法生成采样输出文件名"))?;
                options.sample_start_ms = Some(start);
                options.sample_duration_ms = Some(end - start);
                options.no_audio = true;
                Ok(())
            })();
            if let Err(error) = result {
                self.fail(format!("准备随机采样失败：{error:#}"));
                return;
            }
        }
        match self.manager.enqueue(options, self.preview_interval) {
            Ok(id) => {
                self.selected_task = Some(id);
                self.task_error.clear();
                self.wizard.created_id = Some(id);
                self.wizard.active = false;
                self.project_dirty = true;
            }
            Err(error) => self.fail(format!("创建任务失败：{error:#}")),
        }
    }

    fn fail(&mut self, message: String) {
        self.task_error = message;
        self.wizard.issues = vec![json!({"step": 2, "field": "output",
            "message": "创建失败，请检查输出路径后重试。", "detail": self.task_error})];
        self.project_dirty = true;
    }

    fn restore_task_preview(&mut self, id: TaskId) {
        self.render = self
            .preview_cache
            .iter()
            .find(|(task_id, _)| *task_id == id)
            .map(|(_, frame)| {
                FrameUpdate::Replace(RgbaFrame {
                    width: frame.width,
                    height: frame.height,
                    pixels: frame.pixels.clone(),
                })
            })
            .unwrap_or(FrameUpdate::Clear);
    }

    fn poll_worker(&mut self) {
        // One latest frame per task, capped at 16 previews (at most ~14 MiB at 640 × 360).
        // Qt owns the published pixels; retain a separate copy only for reselecting a task.
        for task in self.manager.tasks() {
            let Some((_, frame)) = self.manager.take_preview(task.id) else {
                continue;
            };
            if let Some(index) = self.preview_cache.iter().position(|(id, _)| *id == task.id) {
                self.preview_cache.remove(index);
            }
            self.pending_task_thumbnails.push((
                task.id.to_string(),
                FrameUpdate::Replace(RgbaFrame {
                    width: frame.width,
                    height: frame.height,
                    pixels: frame.pixels.clone(),
                }),
            ));
            self.preview_cache.push_back((task.id, frame));
            if self.preview_cache.len() > 16 {
                if let Some((evicted_id, _)) = self.preview_cache.pop_front() {
                    self.pending_task_thumbnails
                        .push((evicted_id.to_string(), FrameUpdate::Clear));
                }
            }
            if self.selected_task == Some(task.id) {
                self.restore_task_preview(task.id);
            }
        }
    }
}

impl ViewModel for Model {
    fn defaults_json(&self) -> String {
        let style = self.options.lyrics_style;
        let percent = |value: f32| (value * 100.0).round() as u32;
        json!({
            "width": self.options.width, "height": self.options.height, "fps": self.options.fps,
            "previewInterval": self.preview_interval,
            "fontScale": percent(style.font_scale), "translationFontScale": percent(style.translation_font_scale),
            "backgroundFontScale": percent(style.background_font_scale), "lineHeight": percent(style.line_height_scale),
            "lineSpacing": percent(style.group_gap_scale), "translationGap": percent(style.translation_gap_scale),
            "backgroundGap": percent(style.background_gap_scale), "horizontalPadding": percent(style.horizontal_padding_scale),
            "blurStep": style.lyric_blur_sigma_step, "debugOverlays": style.debug_overlays,
            "embeddedCover": !self.options.no_embedded_cover, "includeAudio": !self.options.no_audio,
            "translation": self.options.render_translation, "backgroundVocal": self.options.render_background_vocal,
            "encoder": EncoderBackend::ALL.iter().position(|encoder| *encoder == self.options.encoder).unwrap_or(0),
            "codec": VideoCodec::ALL.iter().position(|codec| *codec == self.options.codec).unwrap_or(0),
            "nvencPreset": self.options.nvenc_preset.strip_prefix('p').and_then(|value| value.parse::<u32>().ok()).unwrap_or(5).saturating_sub(1),
            "encoders": EncoderBackend::ALL.iter().map(|encoder| encoder.label()).collect::<Vec<_>>(),
            "codecs": VideoCodec::ALL.iter().map(|codec| codec.label()).collect::<Vec<_>>(),
            "zeroCopyEncoder": EncoderBackend::ALL.iter().position(|encoder| encoder.is_zero_copy()).map_or(-1, |index| index as i32),
            "sampleStart": self.sample_start, "sampleDuration": self.sample_duration,
        }).to_string()
    }

    fn select_asset(&mut self, kind: &str, path: Option<PathBuf>) {
        match (kind, path) {
            ("audio", Some(path)) => self.selected_audio(path),
            ("lyrics", path) => {
                self.lyrics_edited = true;
                self.assets.lyrics_index = Assets::pick(&mut self.assets.lyrics, path);
                self.refresh_lyrics();
            }
            ("cover", path) => {
                self.cover_edited = true;
                self.assets.cover_index = Assets::pick(&mut self.assets.cover, path);
                self.style_dirty = true;
            }
            ("background", path) => {
                self.options.background = path;
                self.style_dirty = true;
            }
            ("output", Some(path)) => self.options.output = path,
            _ => return,
        }
        self.project_dirty = true;
        self.draft_changed();
    }

    fn set_text(&mut self, key: &str, value: String) {
        if key == "title" {
            self.title_edited = true;
            self.options.title = Some(value);
            self.style_dirty = true;
            self.project_dirty = true;
            self.draft_changed();
        }
    }

    fn set_number(&mut self, key: &str, value: i32) {
        if key == "wizardStep" {
            self.navigate_wizard(value.clamp(0, 3) as u32);
            return;
        }
        if key == "renderMode" {
            self.wizard.mode = value.clamp(0, 2) as u32;
            self.draft_changed();
            return;
        }
        let value = value.max(0) as u32;
        if key == "maxConcurrent" {
            match self.manager.set_max_concurrent(value as usize) {
                Ok(()) => self.task_error.clear(),
                Err(error) => self.task_error = format!("并发设置失败：{error:#}"),
            }
            return;
        }
        self.draft_changed();
        self.project_dirty = true;
        let style = &mut self.options.lyrics_style;
        let percent = |min, max| value.clamp(min, max) as f32 / 100.0;
        match key {
            "width" => self.options.width = value.clamp(320, 7680),
            "height" => self.options.height = value.clamp(180, 4320),
            "fontScale" => style.font_scale = percent(50, 200),
            "translationFontScale" => style.translation_font_scale = percent(50, 200),
            "backgroundFontScale" => style.background_font_scale = percent(50, 200),
            "lineHeight" => style.line_height_scale = percent(50, 180),
            "lineSpacing" => style.group_gap_scale = percent(0, 200),
            "translationGap" => style.translation_gap_scale = percent(0, 200),
            "backgroundGap" => style.background_gap_scale = percent(0, 200),
            "horizontalPadding" => style.horizontal_padding_scale = percent(0, 200),
            "blurStep" => style.lyric_blur_sigma_step = value.clamp(1, 20),
            "coverIndex" => {
                if value as usize > self.assets.cover.len() {
                    return;
                }
                self.cover_edited = true;
                self.assets.cover_index = value as usize;
                self.project_dirty = true;
            }
            _ => {
                match key {
                    "lyricsIndex" if value as usize <= self.assets.lyrics.len() => {
                        self.lyrics_edited = true;
                        self.assets.lyrics_index = value as usize;
                        self.refresh_lyrics();
                        self.project_dirty = true;
                    }
                    "fps" => self.options.fps = value.clamp(1, 120),
                    "previewInterval" => self.preview_interval = value.clamp(1, 300),
                    "sampleStart" => self.sample_start = value,
                    "sampleDuration" => self.sample_duration = value.max(1),
                    "encoder" => {
                        if let Some(encoder) = EncoderBackend::ALL.get(value as usize) {
                            self.options.encoder = *encoder;
                        }
                    }
                    "codec" => {
                        if let Some(codec) = VideoCodec::ALL.get(value as usize) {
                            self.options.codec = *codec;
                        }
                    }
                    "nvencPreset" => self.options.nvenc_preset = format!("p{}", value.min(6) + 1),
                    _ => {}
                }
                return;
            }
        }
        self.style_dirty = true;
    }

    fn set_flag(&mut self, key: &str, value: bool) {
        self.project_dirty = true;
        self.draft_changed();
        match key {
            "embeddedCover" => self.options.no_embedded_cover = !value,
            "translation" => self.options.render_translation = value,
            "backgroundVocal" => self.options.render_background_vocal = value,
            "debugOverlays" => self.options.lyrics_style.debug_overlays = value,
            "includeAudio" => {
                self.options.no_audio = !value;
                return;
            }
            _ => return,
        }
        self.style_dirty = true;
    }

    fn command(&mut self, action: &str) {
        match action {
            "wizardBegin" => {
                self.wizard.active = true;
                self.wizard.created_id = None;
                self.project_dirty = true;
                return;
            }
            "wizardHome" => {
                self.wizard.active = false;
                self.project_dirty = true;
                return;
            }
            "wizardBack" => {
                self.navigate_wizard(self.wizard.step.saturating_sub(1));
                return;
            }
            "wizardNext" => {
                self.navigate_wizard((self.wizard.step + 1).min(3));
                return;
            }
            "wizardCheck" => {
                self.validate_draft(3);
                return;
            }
            "wizardCreate" => {
                if !self.wizard.active || self.wizard.step != 3 {
                    return;
                }
                self.validate_draft(3);
                if self.wizard.issues.is_empty() {
                    self.wizard.completed = 7;
                    self.start(match self.wizard.mode {
                        1 => "sample",
                        2 => "randomSample",
                        _ => "start",
                    });
                }
                return;
            }
            _ => {}
        }
        if matches!(action, "start" | "sample" | "randomSample") {
            self.start(action);
            return;
        }
        let Some((action, id)) = action.split_once(':') else {
            return;
        };
        let Ok(id) = id.parse::<TaskId>() else { return };
        if action == "select" {
            if self.selected_task != Some(id)
                && self.manager.tasks().iter().any(|task| task.id == id)
            {
                self.selected_task = Some(id);
                self.restore_task_preview(id);
            }
        } else {
            let control = match action {
                "pause" => Control::Pause,
                "resume" => Control::Resume,
                "cancel" => Control::Cancel,
                _ => return,
            };
            match self.manager.command(id, control) {
                Ok(()) => self.task_error.clear(),
                Err(error) => self.task_error = format!("任务操作失败：{error:#}"),
            }
        }
    }

    fn tick(&mut self) -> Snapshot {
        self.poll_assets();
        self.poll_style();
        self.poll_worker();
        let task_json = self.task_json().to_string();
        let task_changed = task_json != self.last_task_json;
        self.last_task_json.clone_from(&task_json);
        Snapshot {
            project: std::mem::take(&mut self.project_dirty)
                .then(|| self.project_json().to_string()),
            task: task_changed.then_some(task_json),
            style: std::mem::take(&mut self.style),
            render: std::mem::take(&mut self.render),
            task_thumbnails: std::mem::take(&mut self.pending_task_thumbnails),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wizard_routes_share_prerequisites_and_keep_draft() {
        let mut model = Model::new().unwrap();
        model.command("wizardBegin");
        model.set_number("wizardStep", 3);
        assert_eq!(model.wizard.step, 0);
        assert!(
            model
                .wizard
                .issues
                .iter()
                .any(|issue| issue["field"] == "audio")
        );
        model.command("wizardNext");
        assert_eq!(model.wizard.step, 0);
        assert!(
            model
                .wizard
                .issues
                .iter()
                .any(|issue| issue["field"] == "audio")
        );

        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("song.wav");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=1",
                    "-y",
                ])
                .arg(&song)
                .status()
                .unwrap()
                .success()
        );
        let lyrics = dir.path().join("song.lrc");
        std::fs::write(&lyrics, "[00:00.00]Navigation\n[00:00.50]End\n").unwrap();
        model.assets.audio = Some(song);
        model.assets.lyrics_index = Assets::pick(&mut model.assets.lyrics, Some(lyrics));
        model.set_text("title", "Preserved title".into());
        model.set_number("fontScale", 112);
        model.options.output = dir.path().join("occupied.mp4");
        std::fs::write(&model.options.output, b"preserve").unwrap();

        model.command("wizardNext");
        assert_eq!(
            model.wizard.step, 1,
            "output is not a prerequisite of style"
        );
        assert!(model.wizard.issues.is_empty());
        assert_eq!(model.wizard.completed & 1, 1);
        model.set_number("wizardStep", 3);
        assert_eq!(
            model.wizard.step, 2,
            "jump lands on earliest invalid prerequisite"
        );
        assert!(model.wizard.issues.iter().all(|issue| issue["step"] == 2));
        model.command("wizardBack");
        assert_eq!(model.wizard.step, 1);
        model.options.output = dir.path().join("available.mp4");
        model.set_number("wizardStep", 3);
        assert_eq!(model.wizard.step, 3);
        assert_eq!(model.wizard.completed, 7);
        assert_eq!(model.options.title.as_deref(), Some("Preserved title"));
        assert_eq!(model.options.lyrics_style.font_scale, 1.12);
        model.set_number("renderMode", 1);
        assert_eq!(model.wizard.completed, 0);
        model.command("wizardHome");
        model.command("wizardCreate");
        assert!(
            model.manager.tasks().is_empty(),
            "hidden wizard cannot create a duplicate"
        );
    }

    #[test]
    fn task_preview_selection_restores_only_its_own_frame() {
        let mut model = Model::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let ids: Vec<_> = (0..3)
            .map(|index| {
                model
                    .manager
                    .enqueue(
                        RenderOptions::new(
                            dir.path().join("missing.wav"),
                            dir.path().join(format!("{index}.mp4")),
                        ),
                        1,
                    )
                    .unwrap()
            })
            .collect();
        for (index, id) in ids.iter().take(2).enumerate() {
            model.preview_cache.push_back((
                *id,
                libgoosy::task_manager::RgbaFrame {
                    width: 1,
                    height: 1,
                    pixels: vec![index as u8, 0, 0, 255],
                },
            ));
        }
        for index in [0, 1, 0] {
            model.command(&format!("select:{}", ids[index]));
            let FrameUpdate::Replace(frame) = std::mem::take(&mut model.render) else {
                panic!("selected task must restore its own cached preview");
            };
            assert_eq!(frame.pixels, [index as u8, 0, 0, 255]);
        }
        model.command(&format!("select:{}", ids[2]));
        assert!(
            matches!(model.render, FrameUpdate::Clear),
            "task with no frame must not inherit another task's image"
        );
        model.render = FrameUpdate::Keep;
        model.command(&format!("select:{}", ids[2]));
        assert!(
            matches!(model.render, FrameUpdate::Keep),
            "reselecting current task must not republish or clear preview"
        );
    }

    #[test]
    fn task_json_reports_two_second_window_fps() {
        let dir = tempfile::tempdir().unwrap();
        let script = if cfg!(windows) {
            let path = dir.path().join("mock-worker.cmd");
            std::fs::write(
                &path,
                "@echo off\r\n\
echo GOOSY_PROGRESS 10 200 1.000\r\n\
echo GOOSY_PROGRESS 20 200 2.000\r\n\
echo GOOSY_PROGRESS 30 200 3.000\r\n\
echo GOOSY_PROGRESS 40 200 4.000\r\n\
echo GOOSY_PROGRESS 90 200 5.000\r\n\
echo GOOSY_PROGRESS 140 200 6.000\r\n",
            )
            .unwrap();
            path
        } else {
            let path = dir.path().join("mock-worker.sh");
            std::fs::write(
                &path,
                "#!/bin/sh\n\
printf 'GOOSY_PROGRESS 10 200 1.000\\n'\n\
printf 'GOOSY_PROGRESS 20 200 2.000\\n'\n\
printf 'GOOSY_PROGRESS 30 200 3.000\\n'\n\
printf 'GOOSY_PROGRESS 40 200 4.000\\n'\n\
printf 'GOOSY_PROGRESS 90 200 5.000\\n'\n\
printf 'GOOSY_PROGRESS 140 200 6.000\\n'\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };

        let mut model = Model::new().unwrap();
        model.manager = TaskManager::with_worker_executable(script).unwrap();
        let id = model
            .manager
            .enqueue(
                RenderOptions::new(dir.path().join("song.wav"), dir.path().join("out.mp4")),
                1,
            )
            .unwrap();
        model.selected_task = Some(id);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let task = model.task_json();
            if task["state"] == "completed" {
                let fps = task["fps"].as_f64().expect("task_json must expose fps");
                assert!(
                    (fps - 50.0).abs() < 1e-6,
                    "expected 2s windowed 50.0 FPS (not cumulative 23.3), got {fps}"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "mock worker did not finish in time: {task}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Verify pause/resume resets the 2s window and excludes paused duration.
        let pause_script = if cfg!(windows) {
            let path = dir.path().join("mock-pause-worker.cmd");
            std::fs::write(
                &path,
                "@echo off\r\n\
echo GOOSY_PROGRESS 20 200 2.000\r\n\
echo GOOSY_CONTROL paused\r\n\
echo GOOSY_CONTROL resumed\r\n\
echo GOOSY_PROGRESS 21 200 12.025\r\n\
echo GOOSY_PROGRESS 61 200 13.025\r\n",
            )
            .unwrap();
            path
        } else {
            let path = dir.path().join("mock-pause-worker.sh");
            std::fs::write(
                &path,
                "#!/bin/sh\n\
printf 'GOOSY_PROGRESS 20 200 2.000\\n'\n\
printf 'GOOSY_CONTROL paused\\n'\n\
printf 'GOOSY_CONTROL resumed\\n'\n\
printf 'GOOSY_PROGRESS 21 200 12.025\\n'\n\
printf 'GOOSY_PROGRESS 61 200 13.025\\n'\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };

        model.manager = TaskManager::with_worker_executable(pause_script).unwrap();
        let paused_id = model
            .manager
            .enqueue(
                RenderOptions::new(dir.path().join("song.wav"), dir.path().join("out2.mp4")),
                1,
            )
            .unwrap();
        model.selected_task = Some(paused_id);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let task = model.task_json();
            if task["state"] == "completed" {
                let fps = task["fps"].as_f64().expect("task_json must expose fps");
                assert!(
                    (fps - 40.0).abs() < 1e-6,
                    "pause duration must not drag down resumed 2s FPS, got {fps}"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "mock pause worker did not finish in time: {task}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn recent_tasks_sorted_by_activity_capped_at_twenty_with_thumbnails() {
        let dir = tempfile::tempdir().unwrap();
        let cover_path = dir.path().join("cover.png");
        std::fs::write(&cover_path, b"stub").unwrap();
        let mut model = Model::new().unwrap();
        // Use a non-existent worker executable that stays blocked or fails cleanly?
        // Better: use a paused/non-starting worker? Actually, if we enqueue 25 tasks with a slow mock worker
        // that stays running on task 0, tasks 1..24 stay Queued deterministically!
        let slow_worker = if cfg!(windows) {
            let path = dir.path().join("hold-worker.cmd");
            std::fs::write(&path, "@echo off\r\nset /p _hold=\r\n").unwrap();
            path
        } else {
            let path = dir.path().join("hold-worker.sh");
            std::fs::write(&path, "#!/bin/sh\nread _hold\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            path
        };
        model.manager = TaskManager::with_worker_executable(slow_worker).unwrap();

        let mut ids = Vec::new();
        for index in 0..24 {
            let mut options = RenderOptions::new(
                dir.path().join("song.wav"),
                dir.path().join(format!("out-{index}.mp4")),
            );
            options.title = Some(format!("Task {index}"));
            if index == 2 {
                options.cover = Some(cover_path.clone());
            }
            ids.push(model.manager.enqueue(options, 1).unwrap());
        }

        // Wait until task 0 starts so tasks 1..23 remain queued.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while model.manager.tasks()[0].state == libgoosy::task_manager::TaskState::Queued {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Cancel older queued task 2: its activity seq becomes the newest!
        model.command(&format!("cancel:{}", ids[2]));
        // Add cached preview for task 3 to verify image://goosy/task/<id>/... thumbnail URL.
        model.preview_cache.push_back((
            ids[3],
            libgoosy::task_manager::RgbaFrame {
                width: 1,
                height: 1,
                pixels: vec![255, 0, 0, 255],
            },
        ));

        let json = model.task_json();
        let recent = json["recentTasks"]
            .as_array()
            .expect("recentTasks array must be present");
        assert_eq!(recent.len(), MAX_RECENT_TASKS);
        assert_eq!(
            recent[0]["id"].as_str(),
            Some(ids[2].to_string().as_str()),
            "most recently updated task must appear first in recentTasks"
        );
        assert!(
            recent[0]["thumbnail"]
                .as_str()
                .unwrap_or("")
                .starts_with("file:///"),
            "task with cover image file must expose file URL thumbnail"
        );
        // Verify strictly descending lastActivitySeq across recentTasks.
        let seqs: Vec<u64> = recent
            .iter()
            .map(|item| item["lastActivitySeq"].as_u64().unwrap())
            .collect();
        assert!(seqs.windows(2).all(|pair| pair[0] > pair[1]));

        // Task 3 has a cached preview frame -> check in full tasks array.
        let task3 = json["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"].as_str() == Some(ids[3].to_string().as_str()))
            .unwrap();
        assert_eq!(
            task3["thumbnail"].as_str(),
            Some(format!("image://goosy/task/{}/0", ids[3]).as_str())
        );
    }
}
