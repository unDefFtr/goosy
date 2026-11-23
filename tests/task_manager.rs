use libgoosy::task_manager::{Control, TaskId, TaskManager, TaskState};
use libgoosy::{EncoderBackend, RenderOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn wait(manager: &TaskManager, id: TaskId, state: TaskState) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let tasks = manager.tasks();
        let task = tasks.iter().find(|task| task.id == id).unwrap();
        if task.state == state {
            return;
        }
        assert!(
            !task.state.is_terminal(),
            "expected {state:?}, got {:?}: {}\n{}",
            task.state,
            task.detail,
            task.log
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {state:?}: {:?}",
            task.state
        );
        thread::sleep(Duration::from_millis(10));
    }
}
fn options(dir: &Path, name: &str, duration: u64) -> RenderOptions {
    let mut options = RenderOptions::new(dir.join("song.wav"), dir.join(name));
    options.lyrics = Some(dir.join("song.lrc"));
    options.width = 640;
    options.height = 360;
    options.fps = 30;
    options.sample_start_ms = Some(0);
    options.sample_duration_ms = Some(duration);
    options.no_audio = true;
    options.encoder = EncoderBackend::Pipe;
    options
}
fn manager() -> TaskManager {
    TaskManager::with_worker_executable(PathBuf::from(env!("CARGO_BIN_EXE_goosy-render-worker")))
        .unwrap()
}

#[cfg(unix)]
fn worker_process(output: &Path) -> (String, PathBuf) {
    let processes = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .unwrap();
    let text = String::from_utf8(processes.stdout).unwrap();
    let line = text
        .lines()
        .find(|line| {
            line.contains("goosy-render-worker") && line.contains(output.to_str().unwrap())
        })
        .expect("worker process must exist");
    let fields: Vec<_> = line.split_whitespace().collect();
    let preview = fields
        .iter()
        .position(|field| *field == "--preview-dir")
        .unwrap();
    (fields[0].to_owned(), PathBuf::from(fields[preview + 1]))
}

#[test]
fn real_workers_queue_controls_failure_and_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=120",
                "-y"
            ])
            .arg(dir.path().join("song.wav"))
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        dir.path().join("song.lrc"),
        "[00:00.00]Queue lifecycle\n[01:59.00]End\n",
    )
    .unwrap();
    let manager = manager();
    let mut draft = options(dir.path(), "first.mp4", 1000);
    manager.check_output(&draft.output).unwrap();
    manager.check_output(&draft.output).unwrap();
    let first = manager.enqueue(draft.clone(), 1).unwrap();
    assert!(manager.check_output(&draft.output).is_err());
    draft.width = 1280;
    assert_eq!(
        manager.tasks()[0].options.width,
        640,
        "draft mutations must not change snapshots"
    );
    let second = manager
        .enqueue(options(dir.path(), "second.mp4", 500), 1)
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(manager.tasks()[1].state, TaskState::Queued);
    assert!(
        manager
            .enqueue(options(dir.path(), "./first.mp4", 500), 1)
            .is_err()
    );
    let other = self::manager();
    assert!(
        other
            .enqueue(options(dir.path(), "first.mp4", 500), 1)
            .is_err()
    );
    #[cfg(unix)]
    {
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        assert!(
            manager
                .enqueue(options(&alias, "first.mp4", 500), 1)
                .is_err()
        );
        std::os::unix::fs::symlink(
            dir.path().join("not-created.mp4"),
            dir.path().join("dangling.mp4"),
        )
        .unwrap();
        assert!(
            manager
                .enqueue(options(dir.path(), "dangling.mp4", 500), 1)
                .is_err()
        );
    }
    #[cfg(any(target_os = "macos", windows))]
    assert!(
        manager
            .enqueue(options(dir.path(), "FIRST.mp4", 500), 1)
            .is_err()
    );
    wait(&manager, first, TaskState::Completed);
    wait(&manager, second, TaskState::Completed);
    assert!(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=p=0"
            ])
            .arg(dir.path().join("second.mp4"))
            .output()
            .unwrap()
            .stdout
            .starts_with(b"640,360")
    );

    let target = manager
        .enqueue(options(dir.path(), "target.mp4", 120_000), 1)
        .unwrap();
    let queued = manager
        .enqueue(options(dir.path(), "queued.mp4", 500), 1)
        .unwrap();
    wait(&manager, target, TaskState::Running);
    manager.command(target, Control::Pause).unwrap();
    wait(&manager, target, TaskState::Paused);
    assert_eq!(
        manager.tasks()[3].state,
        TaskState::Queued,
        "paused worker retains slot"
    );
    let done = manager.tasks()[2].done;
    thread::sleep(Duration::from_millis(150));
    assert_eq!(manager.tasks()[2].done, done);
    manager.command(target, Control::Resume).unwrap();
    wait(&manager, target, TaskState::Running);
    manager.command(target, Control::Cancel).unwrap();
    wait(&manager, target, TaskState::Cancelled);
    wait(&manager, queued, TaskState::Completed);

    let mut broken = options(dir.path(), "broken.mp4", 500);
    broken.song = dir.path().join("missing.wav");
    let failed = manager.enqueue(broken, 1).unwrap();
    let recovery = manager
        .enqueue(options(dir.path(), "recovery.mp4", 500), 1)
        .unwrap();
    wait(&manager, failed, TaskState::Failed);
    wait(&manager, recovery, TaskState::Completed);
    assert!(
        manager
            .enqueue(options(dir.path(), "second.mp4", 500), 1)
            .is_err()
    );

    #[cfg(unix)]
    {
        manager.set_max_concurrent(2).unwrap();
        let a = manager
            .enqueue(options(dir.path(), "parallel-a.mp4", 120_000), 1)
            .unwrap();
        let b = manager
            .enqueue(options(dir.path(), "parallel-b.mp4", 120_000), 1)
            .unwrap();
        wait(&manager, a, TaskState::Running);
        wait(&manager, b, TaskState::Running);
        manager.command(a, Control::Pause).unwrap();
        manager.command(b, Control::Pause).unwrap();
        wait(&manager, a, TaskState::Paused);
        wait(&manager, b, TaskState::Paused);
        let (pid_a, preview_a) = worker_process(&dir.path().join("parallel-a.mp4"));
        let (pid_b, preview_b) = worker_process(&dir.path().join("parallel-b.mp4"));
        assert_ne!(pid_a, pid_b);
        assert_ne!(preview_a, preview_b);
        assert!(preview_a.is_dir() && preview_b.is_dir());
        manager.set_max_concurrent(1).unwrap();
        let after_crash = manager
            .enqueue(options(dir.path(), "after-crash.mp4", 500), 1)
            .unwrap();
        assert_eq!(manager.tasks().last().unwrap().state, TaskState::Queued);
        assert!(
            Command::new("kill")
                .args(["-KILL", &pid_a])
                .status()
                .unwrap()
                .success()
        );
        wait(&manager, a, TaskState::Failed);
        assert!(
            !preview_a.exists(),
            "crashed worker preview directory must be removed"
        );
        assert_eq!(
            manager
                .tasks()
                .iter()
                .find(|task| task.id == b)
                .unwrap()
                .state,
            TaskState::Paused
        );
        assert_eq!(manager.tasks().last().unwrap().state, TaskState::Queued);
        manager.command(b, Control::Cancel).unwrap();
        wait(&manager, b, TaskState::Cancelled);
        assert!(!preview_b.exists());
        wait(&manager, after_crash, TaskState::Completed);
        let processes = Command::new("ps")
            .args(["-axo", "command="])
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&processes.stdout)
                .lines()
                .any(|line| line.contains("goosy-render-worker")
                    && line.contains(dir.path().to_str().unwrap())),
            "terminal tasks must not leave worker processes"
        );
    }

    // Retain a paused worker while cancelling an unrelated queued task.
    let paused = manager
        .enqueue(options(dir.path(), "paused.mp4", 120_000), 1)
        .unwrap();
    wait(&manager, paused, TaskState::Running);
    manager.command(paused, Control::Pause).unwrap();
    wait(&manager, paused, TaskState::Paused);
    let cancelled = manager
        .enqueue(options(dir.path(), "cancelled.mp4", 500), 1)
        .unwrap();
    manager.command(cancelled, Control::Cancel).unwrap();
    assert_eq!(manager.tasks().last().unwrap().state, TaskState::Cancelled);
    assert_eq!(
        manager
            .tasks()
            .iter()
            .find(|task| task.id == paused)
            .unwrap()
            .state,
        TaskState::Paused
    );
    assert!(!dir.path().join("cancelled.mp4").exists());
    drop(manager);
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with("goosy-lock")
    }));
}
