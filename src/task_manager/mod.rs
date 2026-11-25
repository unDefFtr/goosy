//! Qt-independent render queue. The scheduler owns processes; consumers only read snapshots.
mod redo;
pub use redo::{AssetIssue, AssetRole, RedoDraft};
mod worker;

use crate::{
    RenderOptions,
    history::{History, Record},
    workspace::{RecoveredWorkspace, VerifiedPublication, Workspace},
};
use anyhow::{Context, Result, bail, ensure};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use worker::{Terminal, Worker};

static NEXT_ACTIVITY_SEQ: AtomicU64 = AtomicU64::new(1);

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn next_activity_seq() -> u64 {
    NEXT_ACTIVITY_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn touch_activity(info: &mut TaskInfo) {
    info.last_activity_seq = next_activity_seq();
    info.updated_at_ms = now_epoch_ms();
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TaskId(Uuid);
impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for TaskId {
    type Err = uuid::Error;
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    Pause,
    Resume,
    Cancel,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskState {
    Queued,
    Starting,
    Running,
    Paused,
    Completed,
    Interrupted,
    Cancelled,
    Failed,
    Deleting,
}
impl TaskState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Deleting => "deleting",
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed | Self::Deleting)
    }
    fn from_name(value: &str) -> Result<Self> {
        [
            Self::Queued,
            Self::Starting,
            Self::Running,
            Self::Paused,
            Self::Interrupted,
            Self::Completed,
            Self::Cancelled,
            Self::Failed,
            Self::Deleting,
        ]
        .into_iter()
        .find(|state| state.name() == value)
        .with_context(|| format!("未知历史任务状态：{value}"))
    }
}

pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}
#[derive(Clone)]
pub struct TaskInfo {
    pub id: TaskId,
    pub state: TaskState,
    pub pending: Option<Control>,
    /// Recovery, verification or archival still owns this task's lifecycle.
    pub settling: bool,
    pub options: Arc<RenderOptions>,
    pub stage: String,
    pub detail: String,
    pub done: u64,
    pub total: u64,
    pub elapsed: f64,
    pub submitted: u64,
    pub committed: u64,
    pub output_size: u64,
    pub created_at_ms: u64,
    pub thumbnail: Option<PathBuf>,
    pub fps: f64,
    pub log: String,
    pub preview_frame: u64,
    pub preview_note: String,
    pub last_activity_seq: u64,
    pub updated_at_ms: u64,
}

impl TaskInfo {
    /// Commands exposed to clients, in menu order. A stop can supersede a pending pause/resume.
    pub fn actions(&self) -> &'static [&'static str] {
        use TaskState::*;
        if self.settling { return &["redo", "openOutput"]; }
        match (self.state, self.pending) {
            (Deleting, _) => &["openOutput", "delete"],
            (Running, None) => &["pause", "cancel", "redo", "openOutput", "delete"],
            (Paused, None) => &["resume", "cancel", "redo", "openOutput", "delete"],
            (Interrupted, None) => &["resume", "redo", "openOutput", "delete"],
            (Queued | Starting | Running | Paused, None | Some(Control::Pause | Control::Resume)) => {
                &["cancel", "redo", "openOutput", "delete"]
            }
            _ => &["redo", "openOutput", "delete"],
        }
    }
}

struct Task {
    info: TaskInfo,
    record: Record,
    workspace: PathBuf,
    marker: PathBuf,
    worker: Option<Worker>,
    maintenance: Option<Maintenance>,
    elapsed_base: f64,
    retry_recovery: Option<Instant>,
    thumbnail_attempted: bool,
    preview: Option<(u64, RgbaFrame)>,
}

impl Task {
    fn from_record(record: Record, history: &History) -> Result<Self> {
        let workspace = history.root().join("tasks").join(record.id.to_string());
        let marker = reservation_path(&record.options.output)?;
        let thumbnail = crate::history::thumbnail_path(history.root(), record.id);
        let thumbnail = thumbnail.is_file().then_some(thumbnail);
        let thumbnail_attempted = thumbnail.is_some();
        let info = TaskInfo {
            settling: false,
            thumbnail,
            id: TaskId(record.id),
            state: match record.state.as_str() {
                "cancelling" => TaskState::Interrupted,
                crate::history::DELETING_RESERVED => TaskState::Deleting,
                state => TaskState::from_name(state)?,
            },
            pending: (record.state == "cancelling").then_some(Control::Cancel),
            options: Arc::new(record.options.clone()),
            stage: String::new(),
            detail: record.error.clone(),
            done: record.committed,
            submitted: record.committed,
            committed: record.committed,
            total: record.total,
            elapsed: record.elapsed,
            output_size: record.output_size,
            fps: 0.0,
            log: String::new(),
            preview_frame: 0,
            preview_note: if record.options.encoder.is_zero_copy() {
                "零拷贝模式不提供 CPU 帧预览".into()
            } else {
                String::new()
            },
            created_at_ms: record.created_ms,
            updated_at_ms: record.updated_ms,
            last_activity_seq: 0,
        };
        Ok(Self {
            elapsed_base: info.elapsed,
            info,
            record,
            workspace,
            marker,
            thumbnail_attempted,
            worker: None,
            maintenance: None,
            retry_recovery: None,
            preview: None,
        })
    }

    fn save(&mut self, history: &History) -> Result<()> {
        self.record.state.clear();
        self.record.state.push_str(if self.info.pending == Some(Control::Cancel) {
            "cancelling"
        } else {
            self.info.state.name()
        });
        self.record.updated_ms = self.info.updated_at_ms;
        self.record.elapsed = self.info.elapsed;
        self.record.committed = self.info.committed;
        self.record.total = self.info.total;
        self.record.output_size = self.info.output_size;
        self.record.error.clone_from(&self.info.detail);
        history.save(&self.record).context("保存任务历史")
    }

    fn committed_progress(&mut self, committed: u64, total: u64) {
        self.info.done = committed;
        self.info.submitted = committed;
        self.info.committed = committed;
        self.info.total = total;
    }

    fn snapshot(&self) -> TaskInfo {
        let mut info = self.info.clone();
        info.settling = self.maintenance.is_some() || self.retry_recovery.is_some();
        // SQLite completion precedes archive. Publish the terminal snapshot only once
        // archive and reservation release have both returned.
        if info.settling && info.state == TaskState::Completed {
            info.state = TaskState::Running;
        }
        info
    }
}

struct State {
    tasks: Vec<Task>,
    // Only queued, live and still-locking orphan workers need scheduler ticks.
    scheduled: Vec<usize>,
    history: History,
    executable: Arc<PathBuf>,
    storage_error: Option<String>,
}

impl State {
    fn writable(&self) -> Result<()> {
        if let Some(message) = &self.storage_error {
            bail!("{message}");
        }
        Ok(())
    }
}

/// Dropping the manager stops and reaps all owned workers, including paused ones.
pub struct TaskManager {
    state: Arc<Mutex<State>>,
    wake: Option<mpsc::SyncSender<()>>,
    scheduler: Option<JoinHandle<()>>,
}
impl TaskManager {
    pub fn new() -> std::io::Result<Self> {
        let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "goosy-render-worker.exe"
        } else {
            "goosy-render-worker"
        });
        Self::with_worker_executable(executable)
    }

    /// Explicit executable path for packaged hosts and headless consumers.
    pub fn with_worker_executable(executable: PathBuf) -> io::Result<Self> {
        let root = History::default_root().map_err(io::Error::other)?;
        Self::with_data_dir(executable, &root)
    }

    /// Open one isolated durable queue. A data root has exactly one scheduler owner.
    pub fn with_data_dir(executable: PathBuf, root: &Path) -> io::Result<Self> {
        let root = std::path::absolute(root)?;
        let history = History::open(&root).map_err(io::Error::other)?;
        let records = history.load().map_err(io::Error::other)?;
        let mut tasks = Vec::with_capacity(records.len());
        let mut scheduled = Vec::new();
        for record in records {
            let mut task = Task::from_record(record, &history).map_err(io::Error::other)?;
            if task.info.state == TaskState::Deleting {
                match delete_task(&mut task, &history) {
                    Ok(()) => continue,
                    Err(error) => task.info.detail = format!("删除未完成，可重试：{error:#}"),
                }
            } else if task.info.state.is_terminal() {
                // A surviving ownership token also records that post-commit cleanup is pending.
                after_terminal(&mut task, &history).map_err(io::Error::other)?;
            } else {
                // Initialization owns this queue exclusively; finish recovery before exposing it.
                let inspection = inspect_task(&task.workspace, task.info.state == TaskState::Queued);
                apply_recovery(&mut task, &history, inspection, None, None).map_err(io::Error::other)?;
                while let Some(job) = &mut task.maintenance {
                    job.join();
                    poll_maintenance(&mut task, &history).map_err(io::Error::other)?;
                }
                if task.retry_recovery.is_some() {
                    scheduled.push(tasks.len());
                }
            }
            tasks.push(task);
        }
        let state = Arc::new(Mutex::new(State {
            tasks,
            scheduled,
            history,
            executable: Arc::new(executable),
            storage_error: None,
        }));
        let (wake, receive) = mpsc::sync_channel(1);
        let shared = Arc::clone(&state);
        let scheduler = thread::Builder::new()
            .name("goosy-task-manager".into())
            .spawn(move || {
                loop {
                    let shutdown = matches!(
                        receive.recv_timeout(Duration::from_millis(25)),
                        Err(mpsc::RecvTimeoutError::Disconnected)
                    );
                    let mut state = shared.lock().unwrap();
                    if state.storage_error.is_none() {
                        if let Err(error) = advance(&mut state, shutdown) {
                            storage_failure(&mut state, &error);
                        }
                    }
                    if shutdown {
                        if !state.tasks.iter().any(|task| task.maintenance.is_some()) { break; }
                        let handles: Vec<_> = state.tasks.iter_mut().filter_map(|task| {
                            task.maintenance.as_mut()?.thread.take()
                        }).collect();
                        drop(state);
                        for handle in handles { let _ = handle.join(); }
                    }
                }
            })?;
        Ok(Self {
            state,
            wake: Some(wake),
            scheduler: Some(scheduler),
        })
    }
    /// Preflight only; enqueue repeats this check while atomically reserving the destination.
    pub fn check_output(&self, output: &Path) -> Result<()> {
        let output = output_identity(output)?;
        check_output(&output)
    }

    pub fn enqueue(&self, options: RenderOptions, preview_interval: u32) -> Result<TaskId> {
        self.enqueue_with_inputs(options, preview_interval, None)
    }

    fn enqueue_with_inputs(
        &self,
        mut options: RenderOptions,
        preview_interval: u32,
        expected: Option<&crate::workspace::InputFingerprints>,
    ) -> Result<TaskId> {
        ensure!(preview_interval > 0, "预览间隔必须大于零");
        options.output = output_identity(&options.output)?;
        check_output(&options.output)?;
        let cwd = std::env::current_dir()?;
        for path in std::iter::once(&mut options.song).chain(
            [
                &mut options.lyrics,
                &mut options.cover,
                &mut options.background,
            ]
            .into_iter()
            .flatten(),
        ) {
            if path.is_relative() {
                *path = cwd.join(&*path);
            }
        }
        let id = Uuid::new_v4();
        let now = now_epoch_ms();
        let record = Record {
            id,
            options,
            state: "queued".into(),
            created_ms: now,
            updated_ms: now,
            elapsed: 0.0,
            committed: 0,
            total: 0,
            output_size: 0,
            error: String::new(),
            interval: preview_interval,
        };
        let (mut task, seconds, reserved) = {
            let state = self.state.lock().unwrap();
            state.writable()?;
            let mut task = Task::from_record(record, &state.history)?;
            task.info.stage = "等待并发槽位".into();
            touch_activity(&mut task.info);
            // The history row identifies the owner of every subsequently created file.
            task.save(&state.history)?;
            let reserved = check_output(&task.info.options.output).and_then(|()| reserve(&task));
            (task, state.history.settings.segment_seconds, reserved)
        };
        // Hash source assets outside the snapshot lock, with the destination already reserved.
        let created = if reserved.is_ok() {
            Workspace::create_checked(&task.workspace, &task.info.options, seconds, expected).map(drop)
        } else {
            Ok(())
        };
        let mut state = self.state.lock().unwrap();
        state.writable()?;
        let index = state.tasks.len();
        let result = match (reserved, created) {
            (Err(error), _) => {
                finish(
                    &mut task,
                    &state.history,
                    Terminal::Failed(format!("{error:#}")),
                )?;
                Err(error)
            }
            (Ok(()), Err(error)) => {
                finish(
                    &mut task,
                    &state.history,
                    Terminal::Failed(format!("创建工作目录失败：{error:#}")),
                )?;
                if expected.is_some() { Err(error) } else { Ok(TaskId(id)) }
            }
            (Ok(()), Ok(())) => {
                state.scheduled.push(index);
                Ok(TaskId(id))
            }
        };
        state.tasks.push(task);
        next_activity_seq();
        drop(state);
        self.wake();
        result
    }

    /// Control live work or explicitly schedule a validated interrupted task.
    pub fn command(&self, id: TaskId, control: Control) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.writable()?;
        let State {
            tasks,
            history,
            scheduled,
            ..
        } = &mut *state;
        let index = tasks
            .iter()
            .position(|task| task.info.id == id)
            .context("任务不存在")?;
        let task = &mut tasks[index];
        ensure!(task.maintenance.is_none(), "正在校验或清理任务");
        ensure!(!task.info.state.is_terminal(), "任务已结束");
        ensure!(
            task.info.pending.is_none()
                || (control == Control::Cancel && task.info.pending != Some(Control::Cancel)),
            "正在等待 Worker 确认"
        );
        match (task.info.state, control) {
            (TaskState::Interrupted, Control::Resume) => {
                let (reply, receive) = mpsc::channel();
                start_recovery(task, None, Some(reply));
                if !scheduled.contains(&index) {
                    scheduled.push(index);
                }
                drop(state);
                self.wake();
                return receive.recv().context("任务恢复线程已停止")?;
            }
            (TaskState::Queued | TaskState::Interrupted, Control::Cancel) => {
                ensure!(
                    task.retry_recovery.is_none(),
                    "正在等待前一 Worker 释放工作目录"
                );
                finish(task, history, Terminal::Cancelled)?;
            }
            (TaskState::Running, Control::Pause)
            | (TaskState::Paused, Control::Resume)
            | (TaskState::Starting | TaskState::Running | TaskState::Paused, Control::Cancel) => {
                ensure!(
                    task.worker
                        .as_ref()
                        .is_some_and(|worker| worker.command(control)),
                    "Worker 控制通道不可用"
                );
                task.info.pending = Some(control);
                touch_activity(&mut task.info);
                if control == Control::Cancel {
                    // A manager crash closes stdin. Recovery must remember an accepted stop even
                    // when the child reports an interruption instead of acknowledging the command.
                    task.save(history)?;
                }
            }
            _ => bail!("当前任务状态不支持此操作"),
        }
        drop(state);
        self.wake();
        Ok(())
    }

    pub fn output_directory(&self, id: TaskId) -> Result<PathBuf> {
        let parent = {
            let state = self.state.lock().unwrap();
            let task = state.tasks.iter().find(|task| task.info.id == id).context("任务不存在")?;
            task.info.options.output.parent().context("输出路径缺少目录")?.to_owned()
        };
        let metadata = fs::metadata(&parent)
            .with_context(|| format!("输出目录不存在或无法访问：{}", parent.display()))?;
        ensure!(metadata.is_dir(), "输出路径的父级不是目录：{}", parent.display());
        Ok(parent)
    }

    /// Drop the history row only after the owned data has been durably cleaned. A failed or
    /// interrupted deletion remains visible, unschedulable, and retryable across restarts.
    pub fn delete(&self, id: TaskId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.writable()?;
        let State { tasks, history, scheduled, .. } = &mut *state;
        let index = tasks.iter().position(|task| task.info.id == id).context("任务不存在")?;
        let task = &mut tasks[index];
        ensure!(task.maintenance.is_none(), "正在校验或清理任务");
        if let Err(error) = delete_task(task, history) {
            if task.info.state == TaskState::Deleting {
                task.info.detail = format!("删除未完成，可重试：{error:#}");
                touch_activity(&mut task.info);
            }
            return Err(error);
        }
        tasks.remove(index);
        scheduled.retain_mut(|slot| {
            if *slot == index {
                return false;
            }
            if *slot > index {
                *slot -= 1;
            }
            true
        });
        next_activity_seq();
        self.wake();
        Ok(())
    }

    pub fn set_max_concurrent(&self, value: usize) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.writable()?;
        state.history.set_max_concurrent(value)?;
        next_activity_seq();
        self.wake();
        Ok(())
    }
    pub fn max_concurrent(&self) -> usize {
        self.state.lock().unwrap().history.settings.max_concurrent
    }
    pub fn revision(&self) -> u64 {
        NEXT_ACTIVITY_SEQ.load(Ordering::Relaxed)
    }
    pub fn task(&self, id: TaskId) -> Option<TaskInfo> {
        self.state
            .lock()
            .unwrap()
            .tasks
            .iter()
            .find(|task| task.info.id == id)
            .map(Task::snapshot)
    }
    pub fn tasks(&self) -> Vec<TaskInfo> {
        self.state
            .lock()
            .unwrap()
            .tasks
            .iter()
            .map(Task::snapshot)
            .collect()
    }
    pub fn take_preview(&self, id: TaskId) -> Option<(u64, RgbaFrame)> {
        self.state
            .lock()
            .unwrap()
            .tasks
            .iter_mut()
            .find(|task| task.info.id == id)?
            .preview
            .take()
    }
    fn wake(&self) {
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }
}
impl Drop for TaskManager {
    fn drop(&mut self) {
        self.wake.take();
        if let Some(scheduler) = self.scheduler.take() {
            let _ = scheduler.join();
        }
    }
}


fn delete_task(task: &mut Task, history: &History) -> Result<()> {
    const LIVE: &str = "Worker 尚未停止，请先取消任务并等待 Worker 安全退出后再删除";
    ensure!(task.worker.is_none() && task.retry_recovery.is_none(), "{LIVE}");
    let guard = history.deletion_lock(&task.workspace).map_err(|error| {
        if matches!(error.downcast_ref::<fs::TryLockError>(), Some(fs::TryLockError::WouldBlock)) {
            error.context(LIVE)
        } else {
            error
        }
    })?;
    // Persist intent before the first unlink. In particular a queued task can never start
    // against a partially removed workspace, even if the process dies during cleanup.
    if task.info.state != TaskState::Deleting {
        let reserved = matches!(task.info.state, TaskState::Queued | TaskState::Interrupted)
            && (guard.is_some() || exists(&task.workspace)?);
        history.begin_deletion(&mut task.record, reserved)?;
        task.info.state = TaskState::Deleting;
        task.info.pending = None;
    }
    let parent = task.marker.parent().context("缺少输出目录")?;
    let directory = crate::history::DeletionParent::open(parent)?;
    let owned = owns_marker(task).with_context(|| format!("检查输出目录的预留标记：{}", parent.display()))?;
    ensure!(task.record.state != crate::history::DELETING_RESERVED || owned,
        "等待原输出目录及其预留标记：{}", parent.display());
    directory.recheck()?;
    history.confirm_deletion_parent(&mut task.record, &directory, owned)
        .with_context(|| format!("确认原输出目录：{}", parent.display()))?;
    let protected = history.protected_files(task.record.id)?;
    history.clean_task_files(&task.record, &task.workspace, guard, &protected, &directory)?;
    directory.recheck()?;
    if !protected.contains(&task.marker)? {
        release_owned(task)?;
    }
    directory.recheck()?;
    history.finish_deletion(task.record.id)
}

fn output_identity(path: &Path) -> Result<PathBuf> {
    ensure!(path.extension().is_some_and(|ext| !ext.is_empty()), "输出路径缺少容器扩展名");
    let name = path.file_name().context("输出路径缺少文件名")?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let output = parent.canonicalize().context("输出目录不存在")?.join(name);
    Ok(output)
}

fn reservation_path(output: &Path) -> Result<PathBuf> {
    let name = output
        .file_name()
        .context("输出路径缺少文件名")?
        .to_string_lossy();
    let name = if cfg!(any(target_os = "macos", windows)) {
        name.to_lowercase()
    } else {
        name.into_owned()
    };
    Ok(output.with_file_name(format!(".{name}.goosy-lock")))
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("检查 {}", path.display())),
    }
}

fn check_output(output: &Path) -> Result<()> {
    ensure!(output.extension().is_some_and(|ext| !ext.is_empty()), "输出路径缺少容器扩展名");
    let parent = output.parent().context("输出路径缺少父目录")?;
    ensure!(
        !parent.components().any(|part| crate::media::private_stage_dir_name(part.as_os_str())),
        "不能导出到 Goosy 私有暂存目录：{}", parent.display()
    );
    ensure!(!exists(output)?, "输出文件已存在：{}", output.display());
    ensure!(
        !exists(&reservation_path(output)?)?,
        "输出已被另一 Goosy 任务预留：{}",
        output.display()
    );
    Ok(())
}

// A durable, no-clobber ownership token works across distinct data roots. Only its recorded
// task may delete it, under the exclusive history owner lock and after the terminal DB commit.
fn owns_marker(task: &Task) -> Result<bool> {
    match fs::symlink_metadata(&task.marker) {
        Ok(meta) if meta.file_type().is_file() && meta.len() == 16 => {
            Ok(fs::read(&task.marker)? == task.record.id.as_bytes().as_slice())
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn reserve(task: &Task) -> Result<()> {
    if owns_marker(task)? {
        return Ok(());
    }
    let parent = task.marker.parent().context("缺少输出目录")?;
    let mut marker = tempfile::NamedTempFile::new_in(parent)?;
    marker.write_all(task.record.id.as_bytes())?;
    marker.as_file().sync_all()?;
    crate::workspace::publish_noclobber(marker.path(), &task.marker)
        .with_context(|| format!("无法预留输出：{}", task.info.options.output.display()))?;
    marker.disable_cleanup(true);
    Ok(())
}

fn release_owned(task: &Task) -> Result<()> {
    if owns_marker(task)? {
        fs::remove_file(&task.marker)?;
        crate::workspace::sync_dir(task.marker.parent().context("缺少输出目录")?)?;
    }
    Ok(())
}

fn finish(task: &mut Task, history: &History, terminal: Terminal) -> Result<()> {
    task.worker.take();
    match terminal {
        Terminal::Interrupted(reason) => {
            start_recovery(task, Some(reason), None);
            return Ok(());
        }
        Terminal::Completed => {
            let dir = task.workspace.clone();
            task.info.stage = "正在校验输出".into();
            task.info.fps = 0.0;
            touch_activity(&mut task.info);
            task.maintenance = Some(Maintenance::start(Phase::Verify, None, move || {
                Ok(Outcome::Published(Workspace::verify_publication(&dir)?
                    .context("Worker 退出但最终输出未通过发布校验")?))
            }));
            return Ok(());
        }
        _ => {}
    }
    task.info.pending = None;
    task.info.stage.clear();
    task.info.fps = 0.0;
    task.info.state = match terminal {
        Terminal::Failed(mut reason) => {
            if !task.info.log.is_empty() {
                reason.push('\n');
                reason.push_str(&task.info.log);
            }
            task.info.detail = reason;
            TaskState::Failed
        }
        Terminal::Cancelled => {
            if task.info.detail.is_empty() {
                task.info.detail = "任务已由用户取消".into();
            }
            TaskState::Cancelled
        }
        Terminal::Completed | Terminal::Interrupted(_) => unreachable!(),
    };
    touch_activity(&mut task.info);
    task.save(history)?;
    after_terminal(task, history)
}

fn after_terminal(task: &mut Task, history: &History) -> Result<()> {
    let cleanup = (|| -> Result<()> {
        if !owns_marker(task)? {
            return Ok(());
        }
        if task.info.state == TaskState::Completed
            && !history.settings.keep_segments
            && exists(&task.info.options.output)?
        {
            Workspace::cleanup_published(&task.workspace)?;
        }
        release_owned(task)
    })();
    record_cleanup(task, history, cleanup)
}

fn record_cleanup(task: &mut Task, history: &History, cleanup: Result<()>) -> Result<()> {
    if let Err(error) = cleanup {
        // An offline external output drive must not hide the rest of the user's history.
        // Leave the token in place so cleanup can be retried on the next open.
        let message = format!("任务清理失败：{error:#}");
        if !task.info.detail.contains(&message) {
            if !task.info.detail.is_empty() {
                task.info.detail.push('\n');
            }
            task.info.detail.push_str(&message);
            touch_activity(&mut task.info);
            task.save(history)?;
        }
    }
    Ok(())
}

enum Outcome {
    Published(VerifiedPublication),
    Unpublished(Workspace),
    MissingManifest,
    Archived,
}

enum Phase {
    Verify,
    Recover(Option<String>),
    Archive,
}

type ResumeReply = mpsc::Sender<Result<()>>;

/// A task owns its private result channel through exactly one phase. Removing rows may
/// change scheduler indices, but cannot deliver a result to another task or phase.
struct Maintenance {
    phase: Phase,
    resume: Option<ResumeReply>,
    receive: mpsc::Receiver<Result<Outcome>>,
    thread: Option<JoinHandle<()>>,
}

impl Maintenance {
    fn start(phase: Phase, resume: Option<ResumeReply>, work: impl FnOnce() -> Result<Outcome> + Send + 'static) -> Self {
        let (send, receive) = mpsc::channel();
        let errors = send.clone();
        let thread = match thread::Builder::new().name("goosy-task-maintenance".into()).spawn(move || {
            let _ = send.send(work());
        }) {
            Ok(handle) => Some(handle),
            Err(error) => {
                let _ = errors.send(Err(error).context("无法启动任务维护线程"));
                None
            }
        };
        Self { phase, resume, receive, thread }
    }

    fn join(&mut self) {
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

fn inspect_task(dir: &Path, queued: bool) -> Result<Outcome> {
    if queued && !exists(&dir.join("manifest.json"))? {
        return Ok(Outcome::MissingManifest);
    }
    let recovered = Workspace::inspect_recovery(dir)?;
    worker::clean_preview(dir)?; // The recovered workspace still holds its lock.
    Ok(match recovered {
        RecoveredWorkspace::Published(publication) => Outcome::Published(publication),
        RecoveredWorkspace::Unpublished(work) => Outcome::Unpublished(work),
    })
}

fn start_recovery(task: &mut Task, reason: Option<String>, resume: Option<ResumeReply>) {
    let dir = task.workspace.clone();
    let queued = task.info.state == TaskState::Queued;
    task.info.stage = "正在校验恢复记录".into();
    task.info.fps = 0.0;
    touch_activity(&mut task.info);
    task.maintenance = Some(Maintenance::start(Phase::Recover(reason), resume, move || inspect_task(&dir, queued)));
}

fn complete_verified(task: &mut Task, history: &History, publication: VerifiedPublication, resume: Option<ResumeReply>) -> Result<()> {
    let total = publication.total();
    task.committed_progress(total, total);
    task.info.output_size = publication.size();
    task.info.detail.clear();
    task.info.pending = None;
    task.info.fps = 0.0;
    task.retry_recovery = None;
    task.info.state = TaskState::Completed;
    touch_activity(&mut task.info);
    task.save(history)?;
    task.info.stage = "正在清理已完成任务".into();
    let keep = history.settings.keep_segments;
    task.maintenance = Some(Maintenance::start(Phase::Archive, resume, move || {
        if !keep { publication.cleanup()?; }
        Ok(Outcome::Archived)
    }));
    Ok(())
}

fn poll_maintenance(task: &mut Task, history: &History) -> Result<()> {
    let Some(job) = &task.maintenance else { return Ok(()) };
    let result = match job.receive.try_recv() {
        Ok(result) => result,
        Err(mpsc::TryRecvError::Empty) => return Ok(()),
        Err(mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("任务维护线程意外退出")),
    };
    let job = task.maintenance.take().unwrap();
    match (job.phase, result) {
        (_, Ok(Outcome::Published(publication))) => complete_verified(task, history, publication, job.resume),
        (Phase::Recover(reason), result) => apply_recovery(task, history, result, reason, job.resume),
        (Phase::Verify, Err(error)) => finish(task, history, Terminal::Failed(format!("校验发布记录失败：{error:#}"))),
        (Phase::Archive, result) => {
            let cleanup = result.and_then(|_| release_owned(task));
            task.info.stage.clear();
            // The durable completion time is the SQLite commit, before archive.
            task.info.last_activity_seq = next_activity_seq();
            record_cleanup(task, history, cleanup)?;
            if let Some(reply) = job.resume { let _ = reply.send(Ok(())); }
            Ok(())
        }
        _ => unreachable!("unexpected verification result"),
    }
}

fn apply_recovery(task: &mut Task, history: &History, result: Result<Outcome>, reason: Option<String>, resume: Option<ResumeReply>) -> Result<()> {
    let cancelling = task.info.pending == Some(Control::Cancel);
    task.info.stage.clear();
    task.info.fps = 0.0;
    task.retry_recovery = None;
    task.committed_progress(0, 0);
    let checked = match result {
        Ok(Outcome::Published(publication)) => return complete_verified(task, history, publication, resume),
        Ok(Outcome::MissingManifest) => return finish(task, history, Terminal::Failed("工作目录创建中断，缺少初始清单".into())),
        Ok(Outcome::Unpublished(work)) => (|| -> Result<()> {
            ensure!(work.options().output == task.info.options.output, "工作目录与历史输出路径不匹配");
            task.committed_progress(work.committed(), work.total());
            if !cancelling {
                ensure!(!exists(&task.info.options.output)?, "输出文件已存在且无匹配的发布记录，拒绝覆盖");
                reserve(task)?;
            }
            Ok(())
        })(),
        Err(error) => Err(error),
        Ok(Outcome::Archived) => unreachable!("unexpected archive result"),
    };
    task.info.state = TaskState::Interrupted;
    task.info.pending = None;
    match checked {
        Ok(()) if cancelling => {
            task.info.detail = reason.filter(|s| !s.is_empty()).unwrap_or_else(|| "任务已由用户取消".into());
            task.info.state = TaskState::Cancelled;
        }
        Ok(()) if resume.is_some() => {
            task.info.detail.clear();
            task.info.state = TaskState::Queued;
            task.info.stage = "等待并发槽位".into();
        }
        Ok(()) => task.info.detail = reason.unwrap_or_default(),
        Err(error) if matches!(error.downcast_ref::<fs::TryLockError>(), Some(fs::TryLockError::WouldBlock)) => {
            if cancelling { task.info.pending = Some(Control::Cancel); }
            task.retry_recovery = Some(Instant::now() + Duration::from_millis(500));
            task.info.detail = "正在等待前一 Worker 提交分段并释放工作目录".into();
        }
        Err(error) => {
            if cancelling { task.info.state = TaskState::Cancelled; }
            task.info.detail = format!("恢复校验失败：{error:#}");
        }
    }
    touch_activity(&mut task.info);
    task.save(history)?;
    if task.info.state.is_terminal() { after_terminal(task, history)?; }
    if let Some(reply) = resume {
        let result = if task.info.state == TaskState::Queued { Ok(()) } else { Err(anyhow::anyhow!("{}", task.info.detail)) };
        let _ = reply.send(result);
    }
    Ok(())
}

fn poll(task: &mut Task, history: &History) -> Result<()> {
    let Some(worker) = &task.worker else {
        return Ok(());
    };
    let update = worker.poll();
    let previous_state = task.info.state;
    let boundary = update.committed.is_some();
    let mut touched = false;
    if let Some(stage) = update.stage {
        task.info.stage = stage;
        touched = true;
    }
    if let Some((done, total, elapsed)) = update.progress {
        task.info.done = done;
        task.info.total = total;
        task.info.elapsed = task.info.elapsed.max(task.elapsed_base + elapsed);
        if task.info.state == TaskState::Starting {
            task.info.state = TaskState::Running;
        }
        if done == total {
            task.info.stage = "正在完成编码与封装".into();
        }
        touched = true;
    }
    if let Some((done, _, _)) = update.submitted {
        task.info.submitted = done;
        touched = true;
    }
    if let Some((end, total, elapsed)) = update.committed {
        task.info.committed = end;
        task.info.total = total;
        task.info.elapsed = task.info.elapsed.max(task.elapsed_base + elapsed);
        touched = true;
    }
    if let Some(fps) = update.fps {
        task.info.fps = fps;
        touched = true;
    }
    if let Some(paused) = update.paused {
        task.info.state = if paused {
            TaskState::Paused
        } else {
            TaskState::Running
        };
        if matches!(
            (paused, task.info.pending),
            (true, Some(Control::Pause)) | (false, Some(Control::Resume))
        ) {
            task.info.pending = None;
        }
        touched = true;
    }
    if let Some((frame, mut image)) = update.preview {
        if !task.thumbnail_attempted {
            task.thumbnail_attempted = true;
            match crate::history::save_thumbnail(history.root(), task.record.id, &mut image) {
                Ok(path) => task.info.thumbnail = Some(path),
                Err(error) => task.info.preview_note = format!("保存缩略图失败：{error:#}"),
            }
        }
        task.info.preview_frame = frame;
        task.preview = Some((frame, image));
        touched = true;
    }
    if let Some(log) = update.log {
        task.info.log = log;
        touched = true;
    }
    if let Some(warning) = update.warning {
        task.info.detail = warning;
        touched = true;
    }
    if let Some(elapsed) = update.complete_elapsed {
        task.info.elapsed = task.info.elapsed.max(task.elapsed_base + elapsed);
    }
    if let Some(terminal) = update.terminal {
        finish(task, history, terminal)?;
    } else if touched {
        touch_activity(&mut task.info);
        if boundary || previous_state != task.info.state {
            task.save(history)?;
        }
    }
    Ok(())
}

fn advance(state: &mut State, shutdown: bool) -> Result<()> {
    if shutdown {
        for &index in &state.scheduled {
            if let Some(worker) = &mut state.tasks[index].worker {
                worker.disconnect();
            }
        }
    }
    let mut active = 0;
    for &index in &state.scheduled {
        let task = &mut state.tasks[index];
        if shutdown && let Some(worker) = &mut task.worker {
            worker.join();
        }
        poll(task, &state.history)?;
        poll_maintenance(task, &state.history)?;
        if !shutdown && task.maintenance.is_none() && task.retry_recovery.is_some_and(|at| at <= Instant::now()) {
            start_recovery(task, None, None);
        }
        active += usize::from(task.worker.is_some() || task.retry_recovery.is_some());
    }
    for &index in &state.scheduled {
        let task = &mut state.tasks[index];
        if task.info.state != TaskState::Queued || task.maintenance.is_some() {
            continue;
        }
        if shutdown {
            task.info.state = TaskState::Interrupted;
            task.info.stage.clear();
            touch_activity(&mut task.info);
            task.save(&state.history)?;
            continue;
        }
        if active >= state.history.settings.max_concurrent {
            break;
        }
        let ready = (|| -> Result<()> {
            ensure!(
                !exists(&task.info.options.output)?,
                "排队期间输出文件已被创建，拒绝覆盖"
            );
            ensure!(owns_marker(task)?, "任务失去输出预留");
            Ok(())
        })();
        if let Err(error) = ready {
            finish(task, &state.history, Terminal::Failed(format!("{error:#}")))?;
            continue;
        }
        task.info.state = TaskState::Starting;
        task.info.stage = "启动渲染进程".into();
        task.elapsed_base = task.info.elapsed;
        touch_activity(&mut task.info);
        task.save(&state.history)?;
        match Worker::start(
            Arc::clone(&task.info.options),
            task.record.interval,
            Arc::clone(&state.executable),
            task.workspace.clone(),
        ) {
            Ok(worker) => {
                task.worker = Some(worker);
                active += 1;
            }
            Err(error) => finish(
                task,
                &state.history,
                Terminal::Failed(format!("启动 Worker 失败：{error:#}")),
            )?,
        }
    }
    state.scheduled.retain(|&index| {
        let task = &state.tasks[index];
        task.worker.is_some()
            || task.maintenance.is_some()
            || task.info.state == TaskState::Queued
            || task.retry_recovery.is_some()
    });
    Ok(())
}

fn storage_failure(state: &mut State, error: &anyhow::Error) {
    let message = format!("任务存储不可用，队列已停止：{error:#}");
    state.storage_error = Some(message.clone());
    for &index in &state.scheduled {
        if let Some(worker) = &mut state.tasks[index].worker {
            worker.disconnect();
        }
    }
    for &index in &state.scheduled {
        let task = &mut state.tasks[index];
        task.worker.take();
        task.maintenance.take();
        task.info.state = TaskState::Interrupted;
        task.info.pending = None;
        task.info.detail.clone_from(&message);
        touch_activity(&mut task.info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::publication_probe::{Phase as ProbePhase, Probe};

    fn seed(manager: &TaskManager, song: &Path, output: PathBuf, status: TaskState) -> (TaskId, Workspace) {
        let mut options = RenderOptions::new(song.to_owned(), output);
        options.fps = 2;
        let record = Record {
            id: Uuid::new_v4(), options, state: status.name().into(), created_ms: 1, updated_ms: 1,
            elapsed: 0.0, committed: 0, total: 0, output_size: 0, error: String::new(), interval: 1,
        };
        let mut state = manager.state.lock().unwrap();
        let mut task = Task::from_record(record, &state.history).unwrap();
        task.save(&state.history).unwrap();
        reserve(&task).unwrap();
        let work = Workspace::create(&task.workspace, &task.info.options, 1).unwrap();
        let id = task.info.id;
        state.tasks.push(task);
        (id, work)
    }

    fn wait_for(manager: &TaskManager, id: TaskId, status: TaskState) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let info = manager.task(id).unwrap();
            if info.state == status { return; }
            assert!(Instant::now() < deadline, "expected {status:?}, got {:?}: {}", info.state, info.detail);
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn publication_verification_frees_queue_and_commits_before_archive() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("history");
        let manager = TaskManager::with_data_dir(temp.path().join("missing-worker"), &root).unwrap();
        let song = temp.path().join("song");
        fs::write(&song, b"input").unwrap();
        let (a, mut work) = seed(&manager, &song, temp.path().join("a.mp4"), TaskState::Running);
        let (b, other) = seed(&manager, &song, temp.path().join("b.mp4"), TaskState::Interrupted);
        drop(other);
        work.set_range(0, 1000, 2).unwrap();
        work.set_render_backend("raster").unwrap();
        let lines = crate::parse_lyrics("[00:00.00]test", crate::LyricFormat::Lrc).unwrap();
        let mut layout = crate::layout::Layout::new(&lines, 64.0, &[10.0], 3.0, 6.0);
        layout.update(&lines, 500, 2);
        fs::write(work.segment_path(0), b"segment").unwrap();
        work.commit(0, 2, &layout.save_checkpoint(), "stream").unwrap();
        let stage = temp.path().join("stage");
        fs::write(&stage, b"published bytes").unwrap();
        work.prepare_publication(&stage).unwrap();
        let output = work.options().output.clone();
        crate::workspace::publish_noclobber(&stage, &output).unwrap();
        drop(work);
        let marker = reservation_path(&output).unwrap();
        let dir = root.join("tasks").join(a.to_string());
        let (_probe, events) = Probe::install(&output);
        {
            let mut state = manager.state.lock().unwrap();
            let State { tasks, history, scheduled, .. } = &mut *state;
            let index = tasks.iter().position(|t| t.info.id == a).unwrap();
            finish(&mut tasks[index], history, Terminal::Completed).unwrap();
            scheduled.push(index);
        }
        let (phase, release_hash) = events.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(phase, ProbePhase::Hash);
        assert_eq!(manager.task(a).unwrap().stage, "正在校验输出");
        assert!(manager.delete(a).is_err());
        assert!(manager.command(a, Control::Cancel).is_err());
        assert_eq!(manager.task(b).unwrap().state, TaskState::Interrupted);
        manager.command(b, Control::Resume).unwrap();
        // The missing executable fails only once the scheduler actually runs task B.
        wait_for(&manager, b, TaskState::Failed);
        assert!(dir.join("manifest.json").is_file());
        release_hash.send(()).unwrap();

        let (phase, release_archive) = events.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(phase, ProbePhase::Archive, "unchanged output must be hashed just once");
        let db = rusqlite::Connection::open(root.join("history.sqlite3")).unwrap();
        let durable: String = db.query_row("SELECT state FROM tasks WHERE uuid=?1", [a.to_string()], |row| row.get(0)).unwrap();
        assert_eq!(durable, "completed");
        assert_ne!(manager.task(a).unwrap().state, TaskState::Completed);
        assert!(dir.join("manifest.json").is_file());
        assert!(marker.is_file());
        release_archive.send(()).unwrap();
        wait_for(&manager, a, TaskState::Completed);
        assert!(dir.join("manifest.completed.json").is_file());
        assert_eq!(fs::read(output).unwrap(), b"published bytes");
        assert!(!marker.exists());
        assert!(events.try_recv().is_err());
    }
}
