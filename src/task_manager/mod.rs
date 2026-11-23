//! Qt-independent render queue. The scheduler owns processes; consumers only read snapshots.
mod worker;

use crate::RenderOptions;
use anyhow::{Context, Result, ensure};
use std::collections::HashSet;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use worker::{Terminal, Worker};

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TaskId(u64);
impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for TaskId {
    type Err = std::num::ParseIntError;
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
    Cancelled,
    Failed,
}
impl TaskState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
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
    pub options: Arc<RenderOptions>,
    pub stage: String,
    pub detail: String,
    pub done: u64,
    pub total: u64,
    pub elapsed: f64,
    pub log: String,
    pub preview_frame: u64,
    pub preview_note: String,
}

struct Reservation {
    path: PathBuf,
    _file: File,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
struct Task {
    info: TaskInfo,
    interval: u32,
    worker: Option<Worker>,
    reservation: Option<Reservation>,
    preview: Option<(u64, RgbaFrame)>,
}
struct State {
    tasks: Vec<Task>,
    outputs: HashSet<PathBuf>,
    max_concurrent: usize,
    executable: Arc<PathBuf>,
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
    pub fn with_worker_executable(executable: PathBuf) -> std::io::Result<Self> {
        let state = Arc::new(Mutex::new(State {
            tasks: Vec::new(),
            outputs: HashSet::new(),
            max_concurrent: 1,
            executable: Arc::new(executable),
        }));
        let (wake, receive) = mpsc::sync_channel(1);
        let shared = Arc::clone(&state);
        let scheduler = thread::Builder::new()
            .name("goosy-task-manager".into())
            .spawn(move || {
                loop {
                    match receive.recv_timeout(Duration::from_millis(25)) {
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        _ => advance(&mut shared.lock().unwrap()),
                    }
                }
                // Take ownership before joining: never hold the snapshot lock during shutdown.
                let tasks = std::mem::take(&mut shared.lock().unwrap().tasks);
                drop(tasks);
            })?;
        Ok(Self {
            state,
            wake: Some(wake),
            scheduler: Some(scheduler),
        })
    }
    /// Preflight only; enqueue repeats this check while atomically reserving the destination.
    pub fn check_output(&self, output: &Path) -> Result<()> {
        let (output, key) = output_identity(output)?;
        let state = self.state.lock().unwrap();
        check_output(&state, &output, &key)
    }

    pub fn enqueue(&self, mut options: RenderOptions, preview_interval: u32) -> Result<TaskId> {
        ensure!(preview_interval > 0, "预览间隔必须大于零");
        let (output, key) = output_identity(&options.output)?;
        let mut state = self.state.lock().unwrap();
        check_output(&state, &output, &key)?;
        let reservation_path = reservation_path(&output, &key);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&reservation_path)
            .with_context(|| {
                format!(
                    "无法预留输出（可能被另一 Goosy 占用）：{}",
                    output.display()
                )
            })?;
        let reservation = Reservation {
            path: reservation_path,
            _file: file,
        };
        options.output = output;
        // Resolve relative inputs now, not when a queued task eventually starts.
        let cwd = std::env::current_dir()?;
        let absolute = |path: &mut PathBuf| {
            if path.is_relative() {
                *path = cwd.join(&*path);
            }
        };
        absolute(&mut options.song);
        for path in [
            &mut options.lyrics,
            &mut options.cover,
            &mut options.background,
        ]
        .into_iter()
        .flatten()
        {
            absolute(path);
        }
        let id = TaskId(
            NEXT_TASK_ID
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .map_err(|_| anyhow::anyhow!("任务 ID 耗尽"))?,
        );
        let preview_note = if options.encoder.is_zero_copy() {
            "零拷贝模式不提供 CPU 帧预览".into()
        } else {
            String::new()
        };
        state.outputs.insert(key);
        state.tasks.push(Task {
            info: TaskInfo {
                id,
                state: TaskState::Queued,
                pending: None,
                options: Arc::new(options),
                stage: "等待并发槽位".into(),
                detail: String::new(),
                done: 0,
                total: 0,
                elapsed: 0.0,
                log: String::new(),
                preview_frame: 0,
                preview_note,
            },
            interval: preview_interval,
            worker: None,
            reservation: Some(reservation),
            preview: None,
        });
        drop(state);
        self.wake();
        Ok(id)
    }

    pub fn command(&self, id: TaskId, control: Control) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let task = state
            .tasks
            .iter_mut()
            .find(|task| task.info.id == id)
            .context("任务不存在")?;
        ensure!(!task.info.state.is_terminal(), "任务已结束");
        ensure!(
            task.info.pending.is_none() || control == Control::Cancel,
            "正在等待 Worker 确认"
        );
        if task.info.state == TaskState::Queued {
            ensure!(control == Control::Cancel, "排队任务只能取消");
            task.info.state = TaskState::Cancelled;
            task.info.stage.clear();
            task.reservation.take();
        } else {
            match control {
                Control::Pause => ensure!(
                    task.info.state == TaskState::Running,
                    "只能暂停运行中的任务"
                ),
                Control::Resume => {
                    ensure!(task.info.state == TaskState::Paused, "只能继续已暂停的任务")
                }
                Control::Cancel => {}
            }
            ensure!(
                task.worker
                    .as_ref()
                    .is_some_and(|worker| worker.command(control)),
                "Worker 控制通道不可用"
            );
            task.info.pending = Some(control);
        }
        drop(state);
        self.wake();
        Ok(())
    }

    pub fn set_max_concurrent(&self, value: usize) -> Result<()> {
        ensure!((1..=8).contains(&value), "最大并发数必须在 1–8 之间");
        self.state.lock().unwrap().max_concurrent = value;
        self.wake();
        Ok(())
    }
    pub fn max_concurrent(&self) -> usize {
        self.state.lock().unwrap().max_concurrent
    }
    pub fn tasks(&self) -> Vec<TaskInfo> {
        self.state
            .lock()
            .unwrap()
            .tasks
            .iter()
            .map(|task| task.info.clone())
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

fn output_identity(path: &Path) -> Result<(PathBuf, PathBuf)> {
    let name = path.file_name().context("输出路径缺少文件名")?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let output = parent.canonicalize().context("输出目录不存在")?.join(name);
    // Conservative on macOS (which may also have case-sensitive volumes) and Windows.
    let key = if cfg!(any(target_os = "macos", windows)) {
        PathBuf::from(output.to_string_lossy().to_lowercase())
    } else {
        output.clone()
    };
    Ok((output, key))
}

fn reservation_path(output: &Path, key: &Path) -> PathBuf {
    output.with_file_name(format!(
        ".{}.goosy-lock",
        key.file_name().unwrap().to_string_lossy()
    ))
}

fn check_output(state: &State, output: &Path, key: &Path) -> Result<()> {
    ensure!(
        std::fs::symlink_metadata(output).is_err(),
        "输出文件已存在：{}",
        output.display()
    );
    ensure!(
        !state.outputs.contains(key),
        "输出路径已被另一任务使用：{}",
        output.display()
    );
    ensure!(
        !reservation_path(output, key).exists(),
        "输出已被另一 Goosy 实例预留：{}",
        output.display()
    );
    Ok(())
}

fn finish(task: &mut Task, terminal: Terminal) {
    // Terminal is published only after wait, pipe drain and preview directory cleanup.
    task.worker.take();
    task.reservation.take();
    task.info.pending = None;
    task.info.stage.clear();
    task.info.state = match terminal {
        Terminal::Completed => TaskState::Completed,
        Terminal::Cancelled => TaskState::Cancelled,
        Terminal::Failed(error) => {
            task.info.detail = error;
            TaskState::Failed
        }
    };
}

fn advance(state: &mut State) {
    for task in &mut state.tasks {
        let Some(worker) = &task.worker else { continue };
        let update = worker.poll();
        if let Some(stage) = update.stage {
            task.info.stage = stage;
        }
        if let Some((done, total, elapsed)) = update.progress {
            task.info.done = done;
            task.info.total = total;
            task.info.elapsed = elapsed;
            if task.info.state == TaskState::Starting {
                task.info.state = TaskState::Running;
            }
            if done == total {
                task.info.stage = "正在完成编码与封装".into();
            }
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
        }
        if let Some((frame, image)) = update.preview {
            task.info.preview_frame = frame;
            task.preview = Some((frame, image));
        }
        if let Some(log) = update.log {
            task.info.log = log;
        }
        if let Some(warning) = update.warning {
            task.info.detail = warning;
        }
        if let Some(terminal) = update.terminal {
            finish(task, terminal);
        }
    }
    let mut active = state
        .tasks
        .iter()
        .filter(|task| task.worker.is_some())
        .count();
    for task in &mut state.tasks {
        if active >= state.max_concurrent {
            break;
        }
        if task.info.state != TaskState::Queued {
            continue;
        }
        if std::fs::symlink_metadata(&task.info.options.output).is_ok() {
            finish(
                task,
                Terminal::Failed("排队期间输出文件已被创建，拒绝覆盖".into()),
            );
            continue;
        }
        task.info.state = TaskState::Starting;
        task.info.stage = "启动渲染进程".into();
        match Worker::start(
            Arc::clone(&task.info.options),
            task.interval,
            Arc::clone(&state.executable),
        ) {
            Ok(worker) => {
                task.worker = Some(worker);
                active += 1;
            }
            Err(error) => finish(
                task,
                Terminal::Failed(format!("启动 Worker 失败：{error:#}")),
            ),
        }
    }
}
