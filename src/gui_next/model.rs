use crate::gui_shared::{
    StylePreviewScene, load_style_preview_assets, random_sample_range, render_style_preview,
    sample_output_path, scan_sibling_assets,
};
use goosy_rinui::{FrameUpdate, RgbaFrame, Snapshot, ViewModel};
use libgoosy::task_manager::{AssetRole, Control, RedoDraft, TaskId, TaskInfo, TaskManager};
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
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    let raw = raw.replace('\\', "/");
    let (prefix, path) = if let Some(server_path) = raw.strip_prefix("//?/UNC/") {
        ("file://", server_path)
    } else {
        let path = raw.strip_prefix("//?/").unwrap_or(&raw);
        let prefix = if path.starts_with("//") {
            "file:"
        } else if path.starts_with('/') {
            "file://"
        } else {
            "file:///"
        };
        (prefix, path)
    };
    let mut encoded = String::with_capacity(prefix.len() + path.len());
    encoded.push_str(prefix);
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(&mut encoded, "%{byte:02X}");
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
    generation: u64,
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

type StyleRequest = (u64, u64, StylePreviewScene, libgoosy::LyricsStyle);
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

// Keep the session's ordinary draft while the wizard edits a history snapshot.
struct RedoSession {
    draft: RedoDraft,
    saved_options: RenderOptions,
    saved_assets: Assets,
    saved_wizard: Wizard,
    saved_preview_interval: u32,
    saved_matching: bool,
    saved_edited: (bool, bool, bool),
}

fn asset_field(role: AssetRole) -> &'static str {
    match role {
        AssetRole::Audio => "audio",
        AssetRole::Lyrics => "lyrics",
        AssetRole::Cover => "cover",
        AssetRole::Background => "background",
    }
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
    preview_asset_generation: u64,
    style_dirty: bool,
    style_error: String,
    style: FrameUpdate,
    render: FrameUpdate,
    manager: TaskManager,
    selected_task: Option<TaskId>,
    preview_cache: VecDeque<(TaskId, libgoosy::task_manager::RgbaFrame)>,
    pending_task_thumbnails: Vec<(String, FrameUpdate)>,
    task_error: String,
    last_task_revision: Option<u64>,
    project_dirty: bool,
    wizard: Wizard,
    redo: Option<RedoSession>,
    redo_revision: u64,
}

impl Model {
    pub fn new() -> std::io::Result<Self> {
        Self::with_manager(TaskManager::new()?)
    }

    fn with_manager(manager: TaskManager) -> std::io::Result<Self> {
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
            move |(serial, generation, scene, style): StyleRequest| {
                let sources = PreviewAssets {
                    generation,
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
            preview_asset_generation: 0,
            style_dirty: true,
            style_error: String::new(),
            style: FrameUpdate::Keep,
            render: FrameUpdate::Keep,
            manager,
            selected_task: None,
            preview_cache: VecDeque::new(),
            pending_task_thumbnails: Vec::new(),
            task_error: String::new(),
            last_task_revision: None,
            project_dirty: true,
            wizard: Wizard::default(),
            redo: None,
            redo_revision: 0,
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
                "redo": self.redo.is_some(), "redoRevision": self.redo_revision,
                "completed": self.wizard.completed,
                "createdId": self.wizard.created_id.map(|id| id.to_string()),
                "effectiveOutput": self.effective_output(self.wizard.sample_range).as_deref().map(label),
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
            task.thumbnail.as_deref(),
            task.options.cover.as_deref(),
            task.options.background.as_deref(),
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

    fn task_json(&self, tasks: &mut [TaskInfo]) -> Value {
        tasks.sort_unstable_by_key(|t| {
            std::cmp::Reverse((t.updated_at_ms, t.last_activity_seq, t.id))
        });
        let task_value = |task: &TaskInfo| {
            json!({
                "id": task.id.to_string(), "state": task.state.name(),
                "actions": task.actions(),
                "pending": task.pending.map(|control| match control {
                    Control::Pause => "pause", Control::Resume => "resume", Control::Cancel => "cancel",
                }).unwrap_or(""),
                "detail": task.detail,
                "done": task.done, "committed": task.committed, "submitted": task.submitted,
                "total": task.total, "elapsed": task.elapsed, "fps": task.fps,
                "createdAt": task.created_at_ms, "updatedAt": task.updated_at_ms,
                "lastActivitySeq": task.last_activity_seq, "outputSize": task.output_size,
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
        let mut value = json!({"state": "idle", "pending": ""});
        let mut rows = Vec::with_capacity(tasks.len());
        for task in tasks {
            let row = task_value(task);
            if self.selected_task == Some(task.id) {
                value = row.clone();
                value["options"] = json!(&*task.options);
                value["stage"] = json!(task.stage);
                value["previewFrame"] = task.preview_frame.into();
                value["savedPreview"] = json!(task.thumbnail.as_deref().map(path_to_file_url));
                value["previewNote"] = json!(task.preview_note);
                value["log"] = json!(task.log);
            }
            rows.push(row);
        }
        value["recentTasks"] = Value::Array(rows[..rows.len().min(MAX_RECENT_TASKS)].to_vec());
        value["tasks"] = Value::Array(rows);
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
        // Leave a pending ordinary probe in its result slot until that draft is restored.
        if self.redo.is_some() { return; }
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
                .request((self.style_serial, self.preview_asset_generation, scene, self.options.lyrics_style))
            {
                self.style_error = "样式预览线程已退出".into();
                self.last_task_revision = None;
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
                        self.last_task_revision = None;
                    }
                }
                Err(error) => {
                    self.style = FrameUpdate::Clear;
                    self.style_error = error;
                    self.last_task_revision = None;
                }
            }
        }
    }
    fn draft_changed(&mut self) {
        self.wizard.checked = false;
        self.wizard.issues.retain(|issue| issue.get("canAccept").is_some());
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

    fn begin_redo(&mut self, id: TaskId) -> anyhow::Result<()> {
        let draft = self.manager.redo_draft(id)?;
        self.restore_ordinary_draft();
        let mut assets = Assets { audio: Some(draft.options.song.clone()), ..Assets::default() };
        assets.lyrics_index = Assets::pick(&mut assets.lyrics, draft.options.lyrics.clone());
        assets.cover_index = Assets::pick(&mut assets.cover, draft.options.cover.clone());
        let range = draft.options.sample_start_ms.is_some() || draft.options.sample_duration_ms.is_some();
        let wizard = Wizard { active: true, step: 3, mode: u32::from(range), ..Wizard::default() };
        self.redo = Some(RedoSession {
            saved_options: std::mem::replace(&mut self.options, draft.options.clone()),
            saved_assets: std::mem::replace(&mut self.assets, assets),
            saved_wizard: std::mem::replace(&mut self.wizard, wizard),
            saved_preview_interval: std::mem::replace(&mut self.preview_interval, draft.preview_interval),
            saved_matching: std::mem::take(&mut self.matching_assets),
            saved_edited: (self.title_edited, self.lyrics_edited, self.cover_edited),
            draft,
        });
        self.redo_revision += 1;
        self.preview_asset_generation += 1;
        self.style = FrameUpdate::Clear;
        self.style_dirty = true;
        self.refresh_lyrics();
        self.validate_draft(3);
        Ok(())
    }

    fn restore_ordinary_draft(&mut self) {
        let Some(session) = self.redo.take() else { return };
        self.options = session.saved_options;
        self.assets = session.saved_assets;
        self.wizard = session.saved_wizard;
        self.preview_interval = session.saved_preview_interval;
        self.matching_assets = session.saved_matching;
        (self.title_edited, self.lyrics_edited, self.cover_edited) = session.saved_edited;
        self.style = FrameUpdate::Clear;
        self.preview_asset_generation += 1;
        self.style_dirty = true;
        self.project_dirty = true;
    }

    fn inspect_redo_asset(&mut self, role: AssetRole) {
        let Some(session) = self.redo.as_mut() else { return };
        let options = &mut session.draft.options;
        match role {
            AssetRole::Audio => options.song = self.assets.audio.clone().unwrap_or_default(),
            AssetRole::Lyrics => options.lyrics = Assets::selected(&self.assets.lyrics, self.assets.lyrics_index).map(Path::to_owned),
            AssetRole::Cover => options.cover = Assets::selected(&self.assets.cover, self.assets.cover_index).map(Path::to_owned),
            AssetRole::Background => options.background.clone_from(&self.options.background),
        }
        let field = asset_field(role);
        self.wizard.issues.retain(|issue| issue["field"] != field || issue.get("canAccept").is_none());
        if let Some(issue) = session.draft.inspect(role) {
            self.wizard.issues.push(json!({"step": 0, "field": field, "message": issue.message,
                "canAccept": issue.can_accept}));
        }
        self.project_dirty = true;
    }

    fn effective_output(&self, sample_range: Option<(u64, u64)>) -> Option<PathBuf> {
        if self.redo.is_none() {
            if self.wizard.mode == 1 {
                return sample_output_path(&self.options.output, u64::from(self.sample_start));
            }
            if let Some((start, _)) = sample_range {
                return sample_output_path(&self.options.output, start);
            }
        }
        Some(self.options.output.clone())
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
        self.wizard.issues.clear();
        if self.redo.is_some() {
            for role in AssetRole::ALL { self.inspect_redo_asset(role); }
        }
        let mut issues = std::mem::take(&mut self.wizard.issues);
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
                            if self.wizard.mode == 1 {
                                let (no_audio, start) = if self.redo.is_some() {
                                    (self.options.no_audio, self.options.sample_start_ms.unwrap_or(0))
                                } else {
                                    (true, u64::from(self.sample_start))
                                };
                                let last_end = lines.iter().map(|line| line.end_ms).max().unwrap_or(0);
                                let duration = libgoosy::render_duration_ms(no_audio, metadata.duration_seconds, last_end);
                                if start >= duration {
                                    let message = if no_audio {
                                        "起点超出歌词时长，请减小采样起点。"
                                    } else {
                                        "起点超出音频时长，请减小采样起点。"
                                    };
                                    issue(2, "sample", message.into());
                                }
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
        let output = self.effective_output(sample_range);
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
        if self.redo.is_none() {
            options.title = options.title.take().filter(|title| !title.trim().is_empty());
            if options.encoder.is_zero_copy() { options.codec = VideoCodec::H264; }
        }
        self.render = FrameUpdate::Clear;
        if action == "sample" && self.redo.is_none() {
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
                if self.redo.is_none() {
                    options.output = sample_output_path(&options.output, start)
                        .ok_or_else(|| anyhow::anyhow!("无法生成采样输出文件名"))?;
                }
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
        let result = if let Some(session) = &mut self.redo {
            session.draft.options = options;
            session.draft.preview_interval = self.preview_interval;
            session.draft.submit(&self.manager)
        } else {
            self.manager.enqueue(options, self.preview_interval)
        };
        match result {
            Ok(id) => {
                self.restore_ordinary_draft();
                self.selected_task = Some(id);
                self.task_error.clear();
                self.wizard.created_id = Some(id);
                self.wizard.active = false;
                self.project_dirty = true;
            }
            Err(error) => {
                self.fail(format!("创建任务失败：{error:#}"));
                if self.redo.is_some() {
                    self.validate_draft(3);
                    if self.wizard.issues.is_empty() { self.fail(self.task_error.clone()); }
                }
            }
        }
    }

    fn fail(&mut self, message: String) {
        self.task_error = message;
        self.last_task_revision = None;
        self.wizard.issues = vec![json!({"step": 2, "field": "output",
            "message": "创建任务失败", "detail": self.task_error})];
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

    fn poll_worker(&mut self, tasks: &[TaskInfo]) {
        // One latest frame per task, capped at 16 previews (at most ~14 MiB at 640 × 360).
        // Qt owns the published pixels; retain a separate copy only for reselecting a task.
        for task in tasks.iter().filter(|task| task.preview_frame > 0) {
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
            "sampleStart": if self.redo.is_some() { self.options.sample_start_ms.unwrap_or(0) } else { u64::from(self.sample_start) },
            "sampleDuration": if self.redo.is_some() { self.options.sample_duration_ms } else { Some(u64::from(self.sample_duration)) },
        }).to_string()
    }

    fn select_asset(&mut self, kind: &str, path: Option<PathBuf>) {
        match (kind, path) {
            ("audio", Some(path)) if self.redo.is_some() => {
                self.assets.audio = Some(path);
                self.refresh_lyrics();
                self.style_dirty = true;
            }
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
        if let Some(role) = AssetRole::ALL.into_iter().find(|&role| asset_field(role) == kind) {
            self.preview_asset_generation += 1;
            self.inspect_redo_asset(role);
        }
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
            let mode = value.clamp(0, 2) as u32;
            if self.wizard.mode == mode { return; }
            if self.redo.is_some() {
                self.options.sample_start_ms = (mode == 1).then_some(0);
                self.options.sample_duration_ms = (mode == 1).then_some(u64::from(DEFAULT_SAMPLE_DURATION));
                if mode != 0 { self.options.no_audio = true; }
            }
            self.wizard.mode = mode;
            self.draft_changed();
            return;
        }
        let value = value.max(0) as u32;
        if key == "maxConcurrent" {
            match self.manager.set_max_concurrent(value as usize) {
                Ok(()) => self.task_error.clear(),
                Err(error) => self.task_error = format!("并发设置失败：{error:#}"),
            }
            self.last_task_revision = None;
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
                self.inspect_redo_asset(AssetRole::Cover);
                self.project_dirty = true;
            }
            _ => {
                match key {
                    "lyricsIndex" if value as usize <= self.assets.lyrics.len() => {
                        self.lyrics_edited = true;
                        self.assets.lyrics_index = value as usize;
                        self.inspect_redo_asset(AssetRole::Lyrics);
                        self.refresh_lyrics();
                        self.project_dirty = true;
                    }
                    "fps" => self.options.fps = value.clamp(1, 120),
                    "previewInterval" => self.preview_interval = value.clamp(1, 300),
                    "sampleStart" if self.redo.is_some() => self.options.sample_start_ms = Some(u64::from(value)),
                    "sampleDuration" if self.redo.is_some() => self.options.sample_duration_ms = Some(u64::from(value.max(1))),
                    "sampleStart" => self.sample_start = value,
                    "sampleDuration" => self.sample_duration = value.max(1),
                    "encoder" => {
                        if let Some(encoder) = EncoderBackend::ALL.get(value as usize) {
                            self.options.encoder = *encoder;
                            if self.redo.is_some() && encoder.is_zero_copy() {
                                self.options.codec = VideoCodec::H264;
                            }
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
                self.restore_ordinary_draft();
                self.wizard.active = true;
                self.wizard.created_id = None;
                self.project_dirty = true;
                return;
            }
            "wizardHome" => {
                self.restore_ordinary_draft();
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
            if self.redo.is_some() { return; } // A history draft is submitted only after wizard validation.
            self.start(action);
            return;
        }
        if let Some(field) = action.strip_prefix("acceptAsset:") {
            let Some(role) = AssetRole::ALL.into_iter().find(|&role| asset_field(role) == field) else { return };
            let Some(session) = self.redo.as_mut() else { return };
            if let Err(error) = session.draft.accept(role) {
                self.task_error = format!("无法采用当前素材：{error:#}");
                self.last_task_revision = None;
            }
            self.validate_draft(3);
            self.refresh_lyrics();
            self.preview_asset_generation += 1;
            self.style_dirty = true;
            return;
        }
        let Some((action, id)) = action.split_once(':') else {
            return;
        };
        let Ok(id) = id.parse::<TaskId>() else { return };
        if action == "redo" {
            match self.begin_redo(id) {
                Ok(()) => self.task_error.clear(),
                Err(error) => self.task_error = format!("无法重做任务：{error:#}"),
            }
            self.last_task_revision = None;
            return;
        }
        if action == "select" {
            if self.selected_task != Some(id)
                && self.manager.tasks().iter().any(|task| task.id == id)
            {
                self.selected_task = Some(id);
                self.last_task_revision = None;
                self.restore_task_preview(id);
            }
        } else {
            let result = match action {
                "pause" => self.manager.command(id, Control::Pause),
                "resume" => self.manager.command(id, Control::Resume),
                "cancel" => self.manager.command(id, Control::Cancel),
                "openOutput" => self.manager.output_directory(id).and_then(|dir| {
                    anyhow::ensure!(goosy_rinui::open_directory(&path_to_file_url(&dir)),
                        "系统文件管理器无法打开目录：{}", dir.display());
                    Ok(())
                }),
                "delete" => self.manager.delete(id).map(|()| {
                    self.preview_cache.retain(|(cached, _)| *cached != id);
                    self.pending_task_thumbnails.push((id.to_string(), FrameUpdate::Clear));
                    if self.selected_task == Some(id) {
                        self.selected_task = None;
                        self.render = FrameUpdate::Clear;
                    }
                }),
                _ => return,
            };
            match result {
                Ok(()) => self.task_error.clear(),
                Err(error) => self.task_error = format!("任务操作失败：{error:#}"),
            }
            self.last_task_revision = None;
        }
    }

    fn tick(&mut self) -> Snapshot {
        self.poll_assets();
        self.poll_style();
        let revision = self.manager.revision();
        let task = (self.last_task_revision != Some(revision)).then(|| {
            let mut tasks = self.manager.tasks();
            self.poll_worker(&tasks);
            self.last_task_revision = Some(revision);
            self.task_json(&mut tasks).to_string()
        });
        Snapshot {
            project: std::mem::take(&mut self.project_dirty)
                .then(|| self.project_json().to_string()),
            task,
            style: std::mem::take(&mut self.style),
            render: std::mem::take(&mut self.render),
            task_thumbnails: std::mem::take(&mut self.pending_task_thumbnails),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated_model(dir: &Path, executable: PathBuf) -> Model {
        Model::with_manager(TaskManager::with_data_dir(executable, &dir.join("user-data")).unwrap()).unwrap()
    }

    fn snapshot(model: &Model) -> Value {
        model.task_json(&mut model.manager.tasks())
    }

    fn write_song(dir: &Path) -> PathBuf {
        write_song_with_duration(dir, 1)
    }

    fn write_song_with_duration(dir: &Path, seconds: u32) -> PathBuf {
        let path = dir.join("song.wav");
        assert!(std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={seconds}"))
            .arg("-y")
            .arg(&path).status().unwrap().success());
        path
    }

    // A controlled protocol stream lets the UI sample live progress before the process exits.
    fn protocol_worker(dir: &Path, events: &[&str]) -> PathBuf {
        let (name, header, hold) = if cfg!(windows) {
            ("worker.cmd", "@echo off\r\n", "set /p command=\r\n")
        } else {
            ("worker.sh", "#!/bin/sh\n", "read command\n")
        };
        let mut script = String::from(header);
        for event in events {
            script.push_str("echo ");
            script.push_str(event);
            script.push_str(if cfg!(windows) { "\r\n" } else { "\n" });
        }
        script.push_str(hold);
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn relinking_moved_audio_preserves_the_other_history_fields() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = isolated_model(dir.path(), dir.path().join("missing-worker"));
        let song = write_song(dir.path());
        let lyrics = dir.path().join("external.lrc");
        std::fs::write(&lyrics, "[00:00.01]Words\n").unwrap();
        let mut options = RenderOptions::new(&song, dir.path().join("video.mp4"));
        options.lyrics = Some(lyrics.clone());
        options.title = Some("Captured title".into());
        let id = model.manager.enqueue(options, 17).unwrap();
        let moved = dir.path().join("relocated.wav");
        std::fs::rename(&song, &moved).unwrap();
        model.command(&format!("redo:{id}"));
        assert!(model.wizard.issues.iter().any(|i| i["field"] == "audio" && i["canAccept"] == false));
        let output = model.options.output.clone();
        model.select_asset("audio", Some(moved.clone()));
        model.command("wizardCheck");
        assert!(model.wizard.issues.is_empty(), "{:?}", model.wizard.issues);
        assert!(!model.matching_assets);
        assert_eq!(model.assets.audio, Some(moved));
        assert_eq!(Assets::selected(&model.assets.lyrics, model.assets.lyrics_index), Some(lyrics.as_path()));
        assert_eq!(model.options.title.as_deref(), Some("Captured title"));
        assert_eq!(model.options.output, output);
    }

    #[test]
    fn redo_validates_outro_sample_against_captured_audio_setting() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = isolated_model(dir.path(), dir.path().join("missing-worker"));
        let song = write_song_with_duration(dir.path(), 8);
        let lyrics = dir.path().join("source.yrc");
        std::fs::write(&lyrics, "[0,1000](0,1000,0)Words\n").unwrap();
        let mut options = RenderOptions::new(&song, dir.path().join("outro.mp4"));
        options.format = libgoosy::LyricFormat::Yrc;
        options.lyrics = Some(lyrics);
        options.sample_start_ms = Some(5_000);
        options.sample_duration_ms = Some(500);
        options.no_audio = false;
        let original = model.manager.enqueue(options.clone(), 15).unwrap();
        model.command(&format!("redo:{original}"));
        assert!(model.wizard.issues.is_empty(), "{:?}", model.wizard.issues);
        model.command("wizardCreate");
        let created = model.wizard.created_id.unwrap();
        assert_ne!(created, original);
        let tasks = model.manager.tasks();
        let captured = &tasks.iter().find(|task| task.id == created).unwrap().options;
        assert_eq!(captured.sample_start_ms, Some(5_000));
        assert_eq!(captured.sample_duration_ms, Some(500));
        assert!(!captured.no_audio);

        for (no_audio, start, name, message) in [
            (true, 5_000, "silent.mp4", "起点超出歌词时长，请减小采样起点。"),
            (false, 8_000, "at-end.mp4", "起点超出音频时长，请减小采样起点。"),
        ] {
            options.output = dir.path().join(name);
            options.no_audio = no_audio;
            options.sample_start_ms = Some(start);
            let id = model.manager.enqueue(options.clone(), 15).unwrap();
            model.command(&format!("redo:{id}"));
            model.command("wizardCreate");
            assert!(model.wizard.created_id.is_none());
            assert!(model.wizard.issues.iter().any(|i| i["field"] == "sample" && i["message"] == message));
        }
    }

    #[test]
    fn redo_preserves_captured_range_and_rechecks_accepted_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = isolated_model(dir.path(), dir.path().join("missing-worker"));
        let song = write_song(dir.path());
        let lyrics = dir.path().join("source.lrc");
        std::fs::write(&lyrics, "[00:00.01]First\n").unwrap();
        let mut options = RenderOptions::new(&song, dir.path().join("range-200.mp4"));
        options.lyrics = Some(lyrics.clone());
        options.title = Some("History".into());
        options.sample_start_ms = Some(200);
        options.sample_duration_ms = Some(301);
        options.width = 1280;
        options.lyrics_style.font_scale = 1.23;
        options.no_audio = true;
        let id = model.manager.enqueue(options.clone(), 37).unwrap();
        let expected = json!(&*model.manager.tasks()[0].options);
        model.selected_audio(song);
        model.set_text("title", "Ordinary".into());
        model.set_number("width", 888);
        std::fs::write(&lyrics, "[00:00.01]Other\n").unwrap();
        model.command(&format!("redo:{id}"));
        let project = model.project_json();
        assert_eq!(project["wizard"]["step"], 3);
        assert_eq!(project["wizard"]["redoRevision"], 1);
        assert_eq!(project["wizard"]["effectiveOutput"], project["output"]);
        assert!(project["output"].as_str().unwrap().ends_with("range-200-redo.mp4"));
        assert_eq!(project["settings"]["previewInterval"], 37);
        assert_eq!(project["settings"]["sampleDuration"], 301);
        assert_eq!(project["lyricsIndex"], 1);
        assert!(model.wizard.issues.iter().any(|i| i["field"] == "lyrics" && i["canAccept"] == true));
        model.command("acceptAsset:lyrics");
        assert!(!model.wizard.issues.iter().any(|i| i.get("canAccept").is_some()));
        let output = dir.path().join("chosen.mp4");
        model.select_asset("output", Some(output.clone()));
        std::fs::write(&lyrics, "[00:00.01]Again\n").unwrap();
        model.command("wizardCreate");
        assert_eq!(model.manager.tasks().len(), 1);
        model.command("acceptAsset:lyrics");
        model.command("wizardCreate");
        let new_id = model.wizard.created_id.unwrap();
        assert_ne!(new_id, id);
        options.output = dir.path().canonicalize().unwrap().join(output.file_name().unwrap());
        let tasks = model.manager.tasks();
        assert_eq!(json!(&*tasks.iter().find(|t| t.id == new_id).unwrap().options), json!(options));
        assert_eq!(json!(&*tasks.iter().find(|t| t.id == id).unwrap().options), expected);
        model.poll_assets();
        assert_eq!(model.options.title.as_deref(), Some("Ordinary"));
        assert_eq!(model.options.width, 888);
        model.command(&format!("redo:{id}"));
        assert_eq!(model.project_json()["wizard"]["redoRevision"], 2);
        model.command("wizardHome");
        model.command("wizardBegin");
        assert!(model.redo.is_none());
        assert_eq!(model.options.width, 888);
    }

    #[test]
    fn deleting_from_context_menu_preserves_other_selection_and_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = isolated_model(dir.path(), dir.path().join("unused-worker"));
        // Missing input leaves a failed, deletable history entry without starting a worker.
        let failed = |name| model.manager.enqueue(
            RenderOptions::new(dir.path().join("missing.wav"), dir.path().join(name)), 1,
        ).unwrap();
        let selected = failed("selected.mp4");
        let other = failed("other.mp4");
        model.command(&format!("select:{selected}"));
        model.command(&format!("delete:{other}"));
        let view = snapshot(&model);
        assert_eq!(view["selectedId"], selected.to_string());
        assert_eq!(view["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(view["error"], "");
        model.command(&format!("openOutput:{other}"));
        assert!(!snapshot(&model)["error"].as_str().unwrap().is_empty());
        model.command(&format!("delete:{selected}"));
        let empty = snapshot(&model);
        assert_eq!(empty["selectedId"], "");
        assert!(empty["tasks"].as_array().unwrap().is_empty());
        assert!(matches!(model.render, FrameUpdate::Clear));
    }

    #[test]
    fn wizard_routes_share_prerequisites_and_keep_draft() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = isolated_model(dir.path(), dir.path().join("unused-worker"));
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

        let song = write_song(dir.path());
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
        let dir = tempfile::tempdir().unwrap();
        let mut model = isolated_model(dir.path(), dir.path().join("missing-worker"));
        let song = write_song(dir.path());
        let ids: Vec<_> = (0..3)
            .map(|index| {
                model
                    .manager
                    .enqueue(
                        RenderOptions::new(
                            &song,
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
        let cases: &[(&[&str], u64, f64)] = &[
            (&["GOOSY_PROGRESS 10 200 1.000", "GOOSY_PROGRESS 20 200 2.000",
               "GOOSY_PROGRESS 30 200 3.000", "GOOSY_PROGRESS 40 200 4.000",
               "GOOSY_PROGRESS 90 200 5.000", "GOOSY_PROGRESS 140 200 6.000"], 140, 50.0),
            (&["GOOSY_PROGRESS 20 200 2.000", "GOOSY_CONTROL paused", "GOOSY_CONTROL resumed",
               "GOOSY_PROGRESS 21 200 12.025", "GOOSY_PROGRESS 61 200 13.025"], 61, 40.0),
        ];
        for &(events, done, fps) in cases {
            let dir = tempfile::tempdir().unwrap();
            let mut model = isolated_model(dir.path(), protocol_worker(dir.path(), events));
            let options = RenderOptions::new(write_song(dir.path()), dir.path().join("out.mp4"));
            let id = model.manager.enqueue(options, 1).unwrap();
            model.command(&format!("select:{id}"));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let task = snapshot(&model);
                if task["done"] == done {
                    assert!((task["fps"].as_f64().unwrap() - fps).abs() < 1e-6);
                    assert_eq!(task["committed"], 0, "live frames must not advance the recovery point");
                    break;
                }
                assert_ne!(task["state"], "failed", "{task}");
                assert!(std::time::Instant::now() < deadline, "worker stalled: {task}");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn recent_tasks_sorted_by_activity_capped_at_twenty_with_thumbnails() {
        let dir = tempfile::tempdir().unwrap();
        let cover_path = dir.path().join("封 面#.ppm");
        std::fs::write(&cover_path, b"P6\n1 1\n255\n\xff\x80\x00").unwrap();
        let slow_worker = protocol_worker(dir.path(), &[]);
        let mut model = isolated_model(dir.path(), slow_worker);
        let song = write_song(dir.path());

        let mut ids = Vec::new();
        for index in 0..24 {
            let mut options = RenderOptions::new(
                &song,
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
        while model.manager.tasks().iter().any(|t| t.id == ids[0] && t.state == libgoosy::task_manager::TaskState::Queued) {
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

        let json = snapshot(&model);
        let recent = json["recentTasks"]
            .as_array()
            .expect("recentTasks array must be present");
        assert_eq!(recent.len(), MAX_RECENT_TASKS);
        let all = json["tasks"].as_array().unwrap();
        assert_eq!(all.len(), ids.len());
        assert_eq!(recent, &all[..MAX_RECENT_TASKS]);
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
        assert!(recent[0]["thumbnail"].as_str().unwrap().ends_with("%E5%B0%81%20%E9%9D%A2%23.ppm"));
        let keys = all.iter().map(|item| (
            item["updatedAt"].as_u64().unwrap(), item["lastActivitySeq"].as_u64().unwrap(),
            item["id"].as_str().unwrap(),
        )).collect::<Vec<_>>();
        assert!(keys.windows(2).all(|pair| pair[0] > pair[1]));

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

    #[test]
    fn failed_task_details_and_options_survive_model_restart() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("missing-worker");
        let mut model = isolated_model(dir.path(), executable.clone());
        let mut options = RenderOptions::new(write_song(dir.path()), dir.path().join("out.mp4"));
        options.title = Some("Stored title".into());
        options.lyrics_style.font_scale = 1.12;
        let expected = json!(&options);
        let id = model.manager.enqueue(options, 1).unwrap();
        model.command(&format!("select:{id}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let original = loop {
            let task = snapshot(&model);
            if task["state"] == "failed" { break task; }
            assert!(std::time::Instant::now() < deadline, "{task}");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        drop(model);
        let mut reopened = isolated_model(dir.path(), executable);
        reopened.command(&format!("select:{id}"));
        let task = snapshot(&reopened);
        assert_eq!(task["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(task["recentTasks"][0]["id"], id.to_string());
        assert_eq!(task["state"], "failed");
        assert!(!task["detail"].as_str().unwrap().is_empty());
        assert_eq!(task["detail"], original["detail"]);
        assert!(task["createdAt"].as_u64().unwrap() > 0);
        assert_eq!(task["createdAt"], original["createdAt"]);
        assert_eq!(task["outputSize"], 0);
        assert_eq!(task["options"]["title"], expected["title"]);
        assert_eq!(task["options"]["lyrics_style"], expected["lyrics_style"]);
    }
}
