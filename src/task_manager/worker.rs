mod process_tree;

use super::{Control, RgbaFrame};
use crate::RenderOptions;
use anyhow::{Context, Result};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
impl Control {
    fn wire(self) -> &'static [u8] {
        match self {
            Self::Pause => b"pause\n",
            Self::Resume => b"resume\n",
            Self::Cancel => b"stop\n",
        }
    }
}

pub(super) enum Terminal {
    Completed,
    Cancelled,
    Interrupted(String),
    Failed(String),
}

const FPS_WINDOW_SECS: f64 = 2.0;

struct FpsWindow {
    samples: VecDeque<(u64, f64)>,
}

impl FpsWindow {
    fn new() -> Self {
        let mut samples = VecDeque::new();
        samples.push_back((0, 0.0));
        Self { samples }
    }

    fn reset(&mut self) {
        self.samples.clear();
    }

    fn record(&mut self, done: u64, elapsed: f64) -> f64 {
        if !elapsed.is_finite() || elapsed < 0.0 {
            return 0.0;
        }
        if let Some(&(prev_done, prev_elapsed)) = self.samples.back() {
            if done < prev_done || elapsed < prev_elapsed {
                self.samples.clear();
                self.samples.push_back((done, elapsed));
            } else if prev_elapsed == elapsed && prev_done > 0 {
                if let Some(back) = self.samples.back_mut() {
                    back.0 = done;
                }
            } else {
                self.samples.push_back((done, elapsed));
            }
        } else {
            self.samples.push_back((done, elapsed));
        }

        let cutoff = elapsed - FPS_WINDOW_SECS + 1e-9;
        while self.samples.len() > 2 && self.samples[1].1 <= cutoff {
            self.samples.pop_front();
        }

        let (Some(&(old_done, old_elapsed)), Some(&(new_done, new_elapsed))) =
            (self.samples.front(), self.samples.back())
        else {
            return 0.0;
        };
        let dt = new_elapsed - old_elapsed;
        if dt <= 0.0 || new_done <= old_done {
            return 0.0;
        }
        let fps = (new_done - old_done) as f64 / dt;
        if fps.is_finite() { fps } else { 0.0 }
    }
}

#[derive(Default)]
pub(super) struct Update {
    pub stage: Option<String>,
    pub progress: Option<(u64, u64, f64)>,
    pub submitted: Option<(u64, u64, f64)>,
    pub committed: Option<(u64, u64, f64)>,
    pub complete_elapsed: Option<f64>,
    pub fps: Option<f64>,
    pub preview: Option<(u64, RgbaFrame)>,
    pub paused: Option<bool>,
    pub terminal: Option<Terminal>,
    pub log: Option<String>,
    pub warning: Option<String>,
}

#[derive(Default)]
struct Feed {
    pending: Update,
    highest_rendered: u64,
    highest_submitted: u64,
    highest_committed: u64,
    last_progress: Option<Instant>,
    cancelled: bool,
    interrupted: bool,
    failed: bool,
}

pub(super) struct Worker {
    commands: Option<mpsc::SyncSender<Control>>,
    thread: Option<JoinHandle<()>>,
    feed: Arc<Mutex<Feed>>,
}

impl Worker {
    pub fn start(
        options: Arc<RenderOptions>,
        preview_interval: u32,
        executable: Arc<PathBuf>,
        workspace: PathBuf,
    ) -> Result<Self> {
        let (send, recv) = mpsc::sync_channel(2);
        let feed = Arc::new(Mutex::new(Feed::default()));
        let updates = Arc::clone(&feed);
        let thread = thread::Builder::new()
            .name("goosy-worker-client".into())
            .spawn(move || {
                let terminal = supervise(
                    &options,
                    preview_interval,
                    recv,
                    &updates,
                    &executable,
                    &workspace,
                )
                .unwrap_or_else(|error| Terminal::Failed(format!("{error:#}")));
                updates.lock().unwrap().pending.terminal = Some(terminal);
            })?;
        Ok(Self {
            commands: Some(send),
            thread: Some(thread),
            feed,
        })
    }

    pub fn command(&self, control: Control) -> bool {
        self.commands
            .as_ref()
            .is_some_and(|send| send.try_send(control).is_ok())
    }

    pub fn disconnect(&mut self) {
        self.commands.take();
    }

    pub fn join(&mut self) {
        self.disconnect();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                self.feed.lock().unwrap().pending.terminal =
                    Some(Terminal::Interrupted("渲染监控线程意外退出".into()));
            }
        }
    }

    pub fn poll(&self) -> Update {
        let finished = self.thread.as_ref().is_some_and(JoinHandle::is_finished);
        let mut update = std::mem::take(&mut self.feed.lock().unwrap().pending);
        if finished {
            update
                .terminal
                .get_or_insert_with(|| Terminal::Failed("渲染监控线程意外退出".into()));
        }
        update
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.join();
    }
}

fn worker_command(
    executable: &Path,
    options: &RenderOptions,
    preview_dir: &Path,
    interval: u32,
    workspace: &Path,
) -> Command {
    let mut cmd = Command::new(executable);
    cmd.arg("render").arg("--workspace").arg(workspace);
    cmd.arg("--progress-events")
        .arg("--preview-interval")
        .arg(interval.to_string());
    if !options.encoder.is_zero_copy() {
        cmd.arg("--preview-dir").arg(preview_dir);
    }
    cmd
}

// A dead supervisor can leave preview handoffs behind. Reclaim recognized cache files
// after reaping the worker or while holding its workspace lock during recovery.
pub(super) fn clean_preview(workspace: &Path) -> Result<()> {
    let directory = workspace.join("preview-cache");
    match std::fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(meta) => anyhow::ensure!(
            meta.is_dir(),
            "invalid preview cache: {}",
            directory.display()
        ),
    }
    for entry in std::fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name
            .to_str()
            .and_then(|s| s.strip_suffix(".rgba"))
            .is_some_and(|s| s.parse::<u64>().is_ok())
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    std::fs::remove_dir(directory)?;
    Ok(())
}

fn supervise(
    options: &RenderOptions,
    interval: u32,
    commands: mpsc::Receiver<Control>,
    feed: &Arc<Mutex<Feed>>,
    executable: &Path,
    workspace: &Path,
) -> Result<Terminal> {
    let directory = workspace.join("preview-cache");
    std::fs::create_dir(&directory).context("创建实时预览目录")?;
    let result = supervise_process(
        worker_command(executable, options, &directory, interval, workspace),
        commands,
        feed,
        &directory,
        Duration::from_secs(30),
        Duration::from_secs(120),
    );
    // A detached reaper may still own a live Worker. Recovery will retry
    // under this same lock once the Worker releases its workspace.
    if let Err(error) = crate::workspace::lock(workspace, false).and_then(|_lock| clean_preview(workspace)) {
        feed.lock().unwrap().pending.warning = Some(format!("清理预览缓存失败：{error:#}"));
    }
    result
}

fn supervise_process(
    mut command: Command,
    commands: mpsc::Receiver<Control>,
    feed: &Arc<Mutex<Feed>>,
    directory: &Path,
    idle_limit: Duration,
    total_limit: Duration,
) -> Result<Terminal> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut tree = process_tree::ProcessTree::spawn(&mut command)
        .with_context(|| format!("启动渲染 Worker {:?}", command.get_program()))?;
    let mut stdin = tree.child.stdin.take().expect("piped worker stdin");
    let stdout = tree.child.stdout.take().expect("piped worker stdout");
    let stderr = tree.child.stderr.take().expect("piped worker stderr");
    let (input_tx, input_rx) = mpsc::channel::<Control>();
    let input_feed = Arc::clone(feed);
    // A Worker that stops reading stdin must never block the deadline monitor.
    let input = thread::spawn(move || {
        for control in input_rx {
            if let Err(error) = stdin.write_all(control.wire()) {
                input_feed.lock().unwrap().pending.warning = Some(format!("渲染控制通道已断开：{error}"));
                break;
            }
        }
        Ok(())
    });
    let directory = directory.to_owned();
    let output_feed = Arc::clone(feed);
    let output = thread::spawn(move || read_stdout(stdout, &directory, &output_feed));
    let diagnostic_feed = Arc::clone(feed);
    let diagnostics = thread::spawn(move || read_stderr(stderr, &diagnostic_feed));

    let mut input_tx = Some(input_tx);
    let mut waiting_since = None;
    let interrupted = loop {
        match tree.leader_exited() {
            Ok(true) => break None,
            Ok(false) => (),
            Err(error) => break Some(format!("等待渲染 Worker 退出失败：{error:#}")),
        }
        let mut interval = Duration::from_millis(25);
        if let Some(start) = waiting_since {
            let last = feed.lock().unwrap().last_progress.unwrap_or(start).max(start);
            let now = Instant::now();
            let deadline = (last + idle_limit).min(start + total_limit);
            if now >= deadline {
                break Some("渲染 Worker 未在宽限期内退出，已请求终止进程树".into());
            }
            interval = interval.min(deadline - now);
        }
        let Some(sender) = &input_tx else {
            thread::sleep(interval);
            continue;
        };
        match commands.recv_timeout(interval) {
            Ok(control) if waiting_since.is_none() => {
                if control == Control::Cancel {
                    waiting_since = Some(Instant::now());
                }
                let _ = sender.send(control);
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Closing stdin asks the Worker to commit its current segment.
                input_tx.take();
                waiting_since.get_or_insert_with(Instant::now);
            }
        }
    };
    drop(input_tx);
    // Terminate descendants even if the leader exited on its own. They may
    // have inherited our output pipes or still be writing a segment.
    let status = match tree.reap([input, output, diagnostics]) {
        Ok(status) => status,
        Err(error) => return Ok(Terminal::Interrupted(format!("回收渲染 Worker 失败：{error:#}"))),
    };
    if let Some(reason) = interrupted {
        return Ok(Terminal::Interrupted(reason));
    }
    let feed = feed.lock().unwrap();
    // An acknowledged stop is authoritative even when the child also reports a generic failure.
    Ok(if feed.cancelled {
        Terminal::Cancelled
    } else if feed.interrupted && status.success() {
        Terminal::Interrupted(String::new())
    } else if status.success() {
        Terminal::Completed
    } else if feed.failed {
        Terminal::Failed(format!("渲染 Worker 异常退出（{status}）"))
    } else {
        Terminal::Interrupted(format!("渲染 Worker 中断（{status}）"))
    })
}

fn read_stdout(stdout: impl Read, directory: &Path, feed: &Mutex<Feed>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stdout);
    let mut bytes = Vec::new();
    let mut fps_window = FpsWindow::new();
    while reader.read_until(b'\n', &mut bytes)? != 0 {
        let line = String::from_utf8_lossy(&bytes);
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("GOOSY_STAGE") => {
                if let Some(message) = line.trim().splitn(3, ' ').nth(2) {
                    feed.lock().unwrap().pending.stage = Some(message.to_owned());
                }
            }
            Some(event @ ("GOOSY_PROGRESS" | "GOOSY_SUBMITTED" | "GOOSY_COMMITTED")) => {
                let progress = (|| {
                    Some((
                        fields.next()?.parse().ok()?,
                        fields.next()?.parse().ok()?,
                        fields.next()?.parse().ok()?,
                    ))
                })();
                if let Some((done, total, elapsed)) = progress {
                    let mut feed = feed.lock().unwrap();
                    let highest = match event {
                        "GOOSY_COMMITTED" => {
                            feed.pending.committed = Some((done, total, elapsed));
                            &mut feed.highest_committed
                        }
                        "GOOSY_SUBMITTED" => {
                            feed.pending.submitted = Some((done, total, elapsed));
                            &mut feed.highest_submitted
                        }
                        _ => {
                            feed.pending.progress = Some((done, total, elapsed));
                            feed.pending.fps = Some(fps_window.record(done, elapsed));
                            &mut feed.highest_rendered
                        }
                    };
                    if done > *highest {
                        *highest = done;
                        feed.last_progress = Some(Instant::now());
                    }
                }
            }
            Some("GOOSY_COMPLETE") => {
                if let Some(elapsed) = fields.nth(1).and_then(|s| s.parse::<f64>().ok()) {
                    feed.lock().unwrap().pending.complete_elapsed = Some(elapsed);
                }
            }
            Some("GOOSY_PREVIEW") => {
                if let Some(frame) = fields.next().and_then(|field| field.parse::<u64>().ok()) {
                    let path = directory.join(format!("{frame}.rgba"));
                    let dimensions = fields
                        .next()
                        .zip(fields.next())
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                    let image = dimensions
                        .and_then(|(width, height)| read_preview(&path, width, height).ok());
                    let _ = std::fs::remove_file(path);
                    let mut feed = feed.lock().unwrap();
                    if let Some(image) = image {
                        feed.pending.preview = Some((frame, image));
                    }
                }
            }
            Some("GOOSY_CONTROL") => {
                let mut feed = feed.lock().unwrap();
                match fields.next() {
                    Some("paused") => {
                        fps_window.reset();
                        feed.pending.paused = Some(true);
                        feed.pending.fps = Some(0.0);
                    }
                    Some("resumed") => {
                        fps_window.reset();
                        feed.pending.paused = Some(false);
                    }
                    Some("failed") => feed.failed = true,
                    Some("cancelled") => feed.cancelled = true,
                    Some("interrupted") => feed.interrupted = true,
                    _ => {}
                }
            }
            _ => {}
        }
        bytes.clear();
    }
    Ok(())
}

fn read_preview(path: &Path, width: u32, height: u32) -> std::io::Result<RgbaFrame> {
    if !(1..=640).contains(&width) || !(1..=360).contains(&height) {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let size = u64::from(width) * u64::from(height) * 4;
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() != size {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut pixels = vec![0; size as usize];
    (&file).read_exact(&mut pixels)?;
    Ok(RgbaFrame {
        width,
        height,
        pixels,
    })
}

fn read_stderr(mut stderr: impl Read, feed: &Mutex<Feed>) -> std::io::Result<()> {
    const LIMIT: usize = 8_192;
    let mut tail = Vec::with_capacity(LIMIT);
    let mut chunk = [0; 2048];
    loop {
        let len = stderr.read(&mut chunk)?;
        if len == 0 {
            return Ok(());
        }
        let discard = (tail.len() + len).saturating_sub(LIMIT);
        tail.drain(..discard);
        tail.extend_from_slice(&chunk[..len]);
        feed.lock().unwrap().pending.log = Some(String::from_utf8_lossy(&tail).into_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const HELPER: &str = "task_manager::worker::tests::supervision_child";
    const ROLE: &str = "GOOSY_SUPERVISION_TEST_ROLE";
    const DIRECTORY: &str = "GOOSY_SUPERVISION_TEST_DIR";
    const IDLE: Duration = Duration::from_millis(350);

    fn helper(role: &str, directory: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--ignored", "--exact", HELPER, "--nocapture"]);
        command.env(ROLE, role).env(DIRECTORY, directory);
        command
    }

    #[test]
    #[ignore = "subprocess fixture for real Worker supervision"]
    fn supervision_child() {
        let Ok(role) = std::env::var(ROLE) else { return };
        let directory = PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(5));
            std::process::exit(91);
        });
        if role == "descendant" {
            let lock = std::fs::File::create(directory.join("descendant.lock")).unwrap();
            lock.try_lock().unwrap();
            std::fs::write(directory.join("descendant.ready"), b"ready").unwrap();
            // Keep both inherited output pipes and the lock until killed.
            loop {
                thread::park();
            }
        }

        println!("\nGOOSY_PROGRESS 20 10000 1\nGOOSY_SUBMITTED 10 10000 1\nGOOSY_COMMITTED 7 10000 1");
        let _descendant = helper("descendant", &directory).spawn().unwrap();
        while !directory.join("descendant.ready").exists() {
            thread::sleep(Duration::from_millis(5));
        }
        if role == "leader-exits" {
            return;
        }
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        if role == "ignore-stop" {
            assert_eq!(line, "stop\n");
        } else {
            assert!(line.is_empty(), "expected stdin EOF, got {line:?}");
        }
        std::fs::write(directory.join("requested"), line).unwrap();
        for step in 1.. {
            if role == "progress" {
                println!("GOOSY_COMMITTED {} 10000 1", 7 + step);
            } else {
                // Duplicate and regressing frames, logs and acknowledgements
                // continue arriving until the process tree is terminated.
                println!("GOOSY_COMMITTED 7 10000 2\nGOOSY_PROGRESS 4 10000 2\nGOOSY_SUBMITTED 2 10000 2\nGOOSY_CONTROL paused");
                eprintln!("still busy");
            }
            thread::sleep(Duration::from_millis(35));
        }
    }

    struct Fixture {
        directory: tempfile::TempDir,
        commands: Option<mpsc::Sender<Control>>,
        result: mpsc::Receiver<Result<Terminal>>,
        feed: Arc<Mutex<Feed>>,
    }

    impl Fixture {
        fn start(role: &str, total: Duration) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let command = helper(role, directory.path());
            let path = directory.path().to_owned();
            let (commands, incoming) = mpsc::channel();
            let (finished, result) = mpsc::channel();
            let feed = Arc::new(Mutex::new(Feed::default()));
            let updates = Arc::clone(&feed);
            thread::spawn(move || {
                let status = supervise_process(command, incoming, &updates, &path, IDLE, total);
                let _ = finished.send(status);
            });
            let start = Instant::now();
            while !directory.path().join("descendant.ready").exists() {
                assert!(start.elapsed() < Duration::from_secs(3), "descendant did not start");
                thread::sleep(Duration::from_millis(5));
            }
            Self { directory, commands: Some(commands), result, feed }
        }

        fn finish(&self, timeout: Duration) -> Terminal {
            let terminal = self.result.recv_timeout(timeout).expect("supervisor deadline").unwrap();
            let lock = std::fs::File::open(self.directory.path().join("descendant.lock")).unwrap();
            lock.try_lock().expect("descendant survived the Worker");
            assert!(self.feed.lock().unwrap().highest_committed >= 7);
            terminal
        }

        fn requested(&self) -> String {
            std::fs::read_to_string(self.directory.path().join("requested")).unwrap()
        }
    }

    #[test]
    fn disconnect_terminates_an_unresponsive_worker_tree() {
        let mut fixture = Fixture::start("ignore-eof", Duration::from_secs(3));
        fixture.commands.take();
        assert!(matches!(fixture.finish(Duration::from_secs(2)), Terminal::Interrupted(_)));
        assert_eq!(fixture.requested(), "");
    }

    #[test]
    fn repeated_controls_do_not_extend_cancel_deadline() {
        let mut fixture = Fixture::start("ignore-stop", Duration::from_secs(3));
        let commands = fixture.commands.take().unwrap();
        commands.send(Control::Cancel).unwrap();
        let repeats = thread::spawn(move || {
            for control in [Control::Pause, Control::Resume, Control::Cancel].into_iter().cycle() {
                thread::sleep(Duration::from_millis(35));
                if commands.send(control).is_err() {
                    break;
                }
            }
        });
        assert!(matches!(fixture.finish(Duration::from_secs(2)), Terminal::Interrupted(_)));
        assert_eq!(fixture.requested(), "stop\n");
        repeats.join().unwrap();
    }

    #[test]
    fn leader_exit_reclaims_descendants_before_draining_pipes() {
        let fixture = Fixture::start("leader-exits", Duration::from_secs(3));
        assert!(matches!(fixture.finish(Duration::from_secs(2)), Terminal::Completed));
    }

    #[test]
    fn increasing_frames_extend_grace_only_until_the_absolute_deadline() {
        let mut fixture = Fixture::start("progress", Duration::from_millis(1150));
        fixture.commands.take();
        assert!(matches!(fixture.result.recv_timeout(2 * IDLE), Err(mpsc::RecvTimeoutError::Timeout)));
        assert!(fixture.feed.lock().unwrap().highest_committed > 7);
        assert!(matches!(fixture.finish(Duration::from_millis(1000)), Terminal::Interrupted(_)));
        assert_eq!(fixture.requested(), "");
    }

    #[test]
    fn only_new_per_counter_highs_refresh_progress_after_polling() {
        let feed = Mutex::new(Feed::default());
        let event = |line: &str| read_stdout(Cursor::new(line), Path::new("."), &feed).unwrap();
        for line in ["GOOSY_PROGRESS 90 100 1", "GOOSY_SUBMITTED 40 100 1", "GOOSY_COMMITTED 20 100 1"] {
            let before = Instant::now() - Duration::from_secs(1);
            feed.lock().unwrap().last_progress = Some(before);
            event(line);
            assert!(feed.lock().unwrap().last_progress.unwrap() > before);
            std::mem::take(&mut feed.lock().unwrap().pending);
        }
        let last = feed.lock().unwrap().last_progress;
        event("GOOSY_COMMITTED 20 100 2\nGOOSY_SUBMITTED 3 100 3\nGOOSY_PROGRESS 89 100 4");
        assert_eq!(feed.lock().unwrap().last_progress, last);
    }

    #[test]
    fn fps_window_tracks_recent_two_seconds() {
        let mut window = FpsWindow::new();
        // First 4 seconds at 10 FPS (1 frame every 0.1s).
        for frame in 1..=40 {
            let elapsed = frame as f64 * 0.1;
            let fps = window.record(frame, elapsed);
            assert!(
                (fps - 10.0).abs() < 1e-6,
                "frame {frame} at {elapsed:.3}s: expected 10.0 FPS, got {fps}"
            );
        }
        // Next 2 seconds at 50 FPS (5 frames every 0.1s -> 1 frame every 0.02s).
        let mut last_fps = 0.0;
        for step in 1..=100 {
            let done = 40 + step;
            let elapsed = 4.0 + step as f64 * 0.02;
            last_fps = window.record(done, elapsed);
            if step == 50 {
                // At t = 5.0s, window [3.0, 5.0] has 10 frames (3..4s) + 50 frames (4..5s) = 30 FPS.
                assert!(
                    (last_fps - 30.0).abs() < 1e-6,
                    "at t=5.0s expected 30.0 FPS, got {last_fps}"
                );
            }
        }
        // At t = 6.0s, window [4.0, 6.0] is purely 50 FPS.
        assert!(
            (last_fps - 50.0).abs() < 1e-6,
            "at t=6.0s expected 50.0 FPS, got {last_fps}"
        );
    }

    #[test]
    fn fps_window_handles_slow_and_sub_millisecond_frames() {
        let mut slow = FpsWindow::new();
        assert!((slow.record(1, 3.0) - (1.0 / 3.0)).abs() < 1e-9);
        assert!((slow.record(2, 6.0) - (1.0 / 3.0)).abs() < 1e-9);

        let mut fast = FpsWindow::new();
        assert_eq!(fast.record(1, 0.0), 0.0);
        assert_eq!(fast.record(2, 0.0), 0.0);
        assert!((fast.record(3, 0.001) - 3000.0).abs() < 1e-6);
    }

    #[test]
    fn read_stdout_resets_fps_window_across_pause_and_resume() {
        let feed = Mutex::new(Feed::default());
        let dir = tempfile::tempdir().unwrap();

        let before_pause = "GOOSY_PROGRESS 10 100 1.000\nGOOSY_PROGRESS 20 100 2.000\n";
        read_stdout(Cursor::new(before_pause), dir.path(), &feed).unwrap();
        let fps = feed.lock().unwrap().pending.fps.unwrap();
        assert!((fps - 10.0).abs() < 1e-6);

        let phases = Mutex::new(Feed::default());
        let events = "GOOSY_PROGRESS 14 30 2.1\nGOOSY_SUBMITTED 13 30 2.0\nGOOSY_COMMITTED 10 30 1.5\nGOOSY_COMPLETE 30 5.0\n";
        read_stdout(Cursor::new(events), dir.path(), &phases).unwrap();
        let pending = &phases.lock().unwrap().pending;
        assert_eq!(pending.progress, Some((14, 30, 2.1)));
        assert_eq!(pending.submitted, Some((13, 30, 2.0)));
        assert_eq!(pending.committed, Some((10, 30, 1.5)));
        assert_eq!(pending.complete_elapsed, Some(5.0));

        let stream = "\
GOOSY_PROGRESS 10 100 1.000\n\
GOOSY_PROGRESS 20 100 2.000\n\
GOOSY_CONTROL paused\n";
        let paused_feed = Mutex::new(Feed::default());
        read_stdout(Cursor::new(stream), dir.path(), &paused_feed).unwrap();
        {
            let pending = &paused_feed.lock().unwrap().pending;
            assert_eq!(pending.paused, Some(true));
            assert_eq!(pending.fps, Some(0.0));
        }

        // Pause lasts 10 seconds (2.0s -> 12.0s), then resumes at 50 FPS.
        let resumed_stream = "\
GOOSY_PROGRESS 10 100 1.000\n\
GOOSY_PROGRESS 20 100 2.000\n\
GOOSY_CONTROL paused\n\
GOOSY_CONTROL resumed\n\
GOOSY_PROGRESS 21 100 12.020\n\
GOOSY_PROGRESS 71 100 13.020\n";
        let resumed_feed = Mutex::new(Feed::default());
        read_stdout(Cursor::new(resumed_stream), dir.path(), &resumed_feed).unwrap();
        let pending = &resumed_feed.lock().unwrap().pending;
        assert_eq!(pending.paused, Some(false));
        assert!(
            (pending.fps.unwrap() - 50.0).abs() < 1e-6,
            "pause duration must not drag down resumed FPS, got {:?}",
            pending.fps
        );
    }
}
