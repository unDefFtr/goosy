use super::{Control, RgbaFrame};
use crate::RenderOptions;
use anyhow::{Context, Result};
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
    Failed(String),
}

#[derive(Default)]
pub(super) struct Update {
    pub stage: Option<String>,
    pub progress: Option<(u64, u64, f64)>,
    pub preview: Option<(u64, RgbaFrame)>,
    pub paused: Option<bool>,
    pub terminal: Option<Terminal>,
    pub log: Option<String>,
    pub warning: Option<String>,
}

#[derive(Default)]
struct Feed {
    pending: Update,
    cancelled: bool,
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
    ) -> Result<Self> {
        let (send, recv) = mpsc::sync_channel(2);
        let feed = Arc::new(Mutex::new(Feed::default()));
        let updates = Arc::clone(&feed);
        let thread = thread::Builder::new()
            .name("goosy-worker-client".into())
            .spawn(move || {
                let terminal = supervise(&options, preview_interval, recv, &updates, &executable)
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
        self.commands.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn worker_command(
    executable: &Path,
    options: &RenderOptions,
    preview_dir: &Path,
    interval: u32,
) -> Command {
    let mut cmd = Command::new(executable);
    cmd.arg("render");
    let style = options.lyrics_style;
    for (flag, value) in [
        ("--font-scale", style.font_scale),
        ("--translation-font-scale", style.translation_font_scale),
        ("--background-font-scale", style.background_font_scale),
        ("--line-height-scale", style.line_height_scale),
        ("--line-spacing-scale", style.group_gap_scale),
        ("--translation-gap-scale", style.translation_gap_scale),
        ("--background-gap-scale", style.background_gap_scale),
        ("--horizontal-padding-scale", style.horizontal_padding_scale),
    ] {
        cmd.arg(flag).arg(value.to_string());
    }
    for (flag, value) in [
        ("--width", options.width),
        ("--height", options.height),
        ("--fps", options.fps),
        ("--lyric-blur-sigma-step", style.lyric_blur_sigma_step),
        ("--preview-interval", interval),
    ] {
        cmd.arg(flag).arg(value.to_string());
    }
    for (flag, enabled) in [
        ("--debug-overlays", style.debug_overlays),
        ("--no-audio", options.no_audio),
        ("--no-embedded-cover", options.no_embedded_cover),
        ("--no-translation", !options.render_translation),
        ("--no-background-vocal", !options.render_background_vocal),
    ] {
        if enabled {
            cmd.arg(flag);
        }
    }
    for (flag, path) in [
        ("--cover", &options.cover),
        ("--background", &options.background),
    ] {
        if let Some(path) = path {
            cmd.arg(flag).arg(path);
        }
    }
    if let Some(title) = &options.title {
        cmd.arg(format!("--title={title}"));
    }
    if let Some(value) = options.sample_start_ms {
        cmd.arg("--sample-start-ms").arg(value.to_string());
    }
    if let Some(value) = options.sample_duration_ms {
        cmd.arg("--sample-duration-ms").arg(value.to_string());
    }
    for index in &options.excluded_lines {
        cmd.arg("--exclude-line").arg(index.to_string());
    }
    let format = match options.format {
        crate::LyricFormat::Auto => "auto",
        crate::LyricFormat::Lrc => "lrc",
        crate::LyricFormat::Ttml => "ttml",
        crate::LyricFormat::Yrc => "yrc",
    };
    cmd.arg("--output").arg(&options.output).args([
        "--progress-events",
        "--encoder",
        options.encoder.cli_value(),
        "--codec",
        options.codec.cli_value(),
        "--nvenc-preset",
        &options.nvenc_preset,
        "--format",
        format,
    ]);
    if !options.encoder.is_zero_copy() {
        cmd.arg("--preview-dir").arg(preview_dir);
    }
    cmd.arg("--").arg(&options.song);
    if let Some(lyrics) = &options.lyrics {
        cmd.arg(lyrics);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

fn supervise(
    options: &RenderOptions,
    interval: u32,
    commands: mpsc::Receiver<Control>,
    feed: &Mutex<Feed>,
    executable: &Path,
) -> Result<Terminal> {
    let directory = tempfile::Builder::new()
        .prefix("goosy-preview-")
        .tempdir()
        .context("创建实时预览目录")?;
    let mut child = worker_command(executable, options, directory.path(), interval)
        .spawn()
        .with_context(|| format!("启动渲染 Worker {}", executable.display()))?;
    let mut stdin = child.stdin.take().expect("piped worker stdin");
    let stdout = child.stdout.take().expect("piped worker stdout");
    let stderr = child.stderr.take().expect("piped worker stderr");
    let status = thread::scope(|scope| {
        let output = scope.spawn(|| read_stdout(stdout, directory.path(), feed));
        let diagnostics = scope.spawn(|| read_stderr(stderr, feed));
        let status = (|| {
            let mut shutdown = None;
            let mut stop_sent = false;
            let interval = Duration::from_millis(25);
            loop {
                if let Some(status) = child.try_wait()? {
                    break Ok(status);
                }
                if let Some(deadline) = shutdown {
                    if Instant::now() >= deadline {
                        child.kill()?;
                        break child.wait();
                    }
                    thread::sleep(interval);
                    continue;
                }
                let control = match commands.recv_timeout(interval) {
                    Ok(control) => control,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        shutdown = Some(Instant::now() + Duration::from_secs(2));
                        if stop_sent {
                            continue;
                        }
                        Control::Cancel
                    }
                };
                if matches!(control, Control::Cancel) {
                    shutdown.get_or_insert_with(|| Instant::now() + Duration::from_secs(10));
                    stop_sent = true;
                }
                if let Err(error) = stdin.write_all(control.wire()) {
                    feed.lock().unwrap().pending.warning =
                        Some(format!("渲染控制通道已断开：{error}"));
                }
            }
        })();
        if status.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        drop(stdin);
        // Drain both pipes after exit so the final frame, acknowledgement and errors are retained.
        let output = output
            .join()
            .map_err(|_| anyhow::anyhow!("Worker 输出读取线程异常退出"))?;
        let diagnostics = diagnostics
            .join()
            .map_err(|_| anyhow::anyhow!("Worker 日志读取线程异常退出"))?;
        output.context("读取 Worker 标准输出")?;
        diagnostics.context("读取 Worker 日志")?;
        status.context("等待渲染 Worker 退出")
    })?;
    Ok(if status.success() {
        Terminal::Completed
    } else if status.code() == Some(1) && feed.lock().unwrap().cancelled {
        Terminal::Cancelled
    } else {
        Terminal::Failed(format!("渲染 Worker 异常退出（{status}）"))
    })
}

fn read_stdout(stdout: impl Read, directory: &Path, feed: &Mutex<Feed>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stdout);
    let mut bytes = Vec::new();
    while reader.read_until(b'\n', &mut bytes)? != 0 {
        let line = String::from_utf8_lossy(&bytes);
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("GOOSY_STAGE") => {
                if let Some(message) = line.trim().splitn(3, ' ').nth(2) {
                    feed.lock().unwrap().pending.stage = Some(message.to_owned());
                }
            }
            Some("GOOSY_PROGRESS") => {
                let progress = (|| {
                    Some((
                        fields.next()?.parse().ok()?,
                        fields.next()?.parse().ok()?,
                        fields.next()?.parse().ok()?,
                    ))
                })();
                if let Some(progress) = progress {
                    feed.lock().unwrap().pending.progress = Some(progress);
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
                    Some("paused") => feed.pending.paused = Some(true),
                    Some("resumed") => feed.pending.paused = Some(false),
                    Some("cancelled") => feed.cancelled = true,
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
