use libgoosy::task_manager::{Control, OutputError, TaskId, TaskManager, TaskState};
use libgoosy::{EncoderBackend, RenderOptions};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn wait(manager: &TaskManager, id: TaskId, state: TaskState) {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let task = manager.task(id).unwrap();
        if task.state == state {
            return;
        }
        assert!(
            !task.state.is_terminal() && task.state != TaskState::Interrupted,
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
fn manager(root: &Path) -> TaskManager {
    TaskManager::with_data_dir(
        PathBuf::from(env!("CARGO_BIN_EXE_goosy-render-worker")),
        root,
    )
    .unwrap()
}

#[cfg(unix)]
fn worker_process(id: TaskId) -> (String, PathBuf) {
    let processes = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .unwrap();
    let text = String::from_utf8(processes.stdout).unwrap();
    let identity = id.to_string();
    let line = text
        .lines()
        .find(|line| {
            line.contains("goosy-render-worker") && line.contains(&identity)
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
                "sine=frequency=440:duration=10",
                "-y"
            ])
            .arg(dir.path().join("song.wav"))
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(
        dir.path().join("song.lrc"),
        "[00:00.00]Queue lifecycle\n[00:09.00]End\n",
    )
    .unwrap();
    let root = dir.path().join("data");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("settings.toml"), "segment_seconds = 1\n").unwrap();
    let manager = manager(&root);
    assert!(TaskManager::with_data_dir(PathBuf::new(), &root).is_err());
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
    let creation_order = [first, second].map(|id| manager.task(id).unwrap().list_order_seq);
    assert!(creation_order[0] < creation_order[1]);
    assert_eq!(manager.tasks()[1].state, TaskState::Queued);
    assert!(
        manager
            .enqueue(options(dir.path(), "./first.mp4", 500), 1)
            .is_err()
    );
    let other = self::manager(&dir.path().join("other-data"));
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
    let completed = manager.task(first).unwrap();
    assert_eq!([first, second].map(|id| manager.task(id).unwrap().list_order_seq), creation_order);
    assert_eq!(completed.done, completed.total);
    assert_eq!(completed.committed, completed.total);
    assert_eq!(
        completed.output_size,
        std::fs::metadata(&completed.options.output).unwrap().len()
    );
    assert_eq!(first.to_string().parse::<TaskId>().unwrap(), first);
    let first_workspace = root.join("tasks").join(first.to_string());
    assert!(first_workspace.join("config.json").is_file());
    assert_eq!(
        std::fs::read_dir(first_workspace.join("segments"))
            .unwrap()
            .count(),
        0
    );
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
        .enqueue(options(dir.path(), "target.mp4", 9000), 1)
        .unwrap();
    let queued = manager
        .enqueue(options(dir.path(), "queued.mp4", 500), 1)
        .unwrap();
    wait(&manager, target, TaskState::Running);
    assert!(manager.delete(target).unwrap_err().to_string().contains("先取消"));
    manager.command(target, Control::Pause).unwrap();
    wait(&manager, target, TaskState::Paused);
    assert!(manager.delete(target).is_err());
    let disposable = manager.enqueue(options(dir.path(), "unused.mp4", 500), 1).unwrap();
    assert_eq!(manager.task(disposable).unwrap().state, TaskState::Queued);
    manager.delete(disposable).unwrap();
    assert!(!root.join("tasks").join(disposable.to_string()).exists());
    manager.check_output(&dir.path().join("unused.mp4")).unwrap();
    assert_eq!(
        manager.tasks()[3].state,
        TaskState::Queued,
        "paused worker retains slot"
    );
    let done = manager.tasks()[2].done;
    assert!(done > 0);
    assert_eq!(done, manager.task(target).unwrap().committed);
    assert_eq!(done, manager.task(target).unwrap().submitted);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(manager.tasks()[2].done, done);
    manager.command(target, Control::Resume).unwrap();
    wait(&manager, target, TaskState::Running);
    manager.command(target, Control::Cancel).unwrap();
    wait(&manager, target, TaskState::Cancelled);
    wait(&manager, queued, TaskState::Completed);
    assert!(!manager.task(target).unwrap().detail.is_empty());
    manager.delete(target).unwrap();
    assert!(manager.task(target).is_none());
    assert!(!root.join("tasks").join(target.to_string()).exists());

    let mut broken = options(dir.path(), "broken.mp4", 500);
    broken.song = dir.path().join("missing.wav");
    let failed = manager.enqueue(broken, 1).unwrap();
    let recovery = manager
        .enqueue(options(dir.path(), "recovery.mp4", 500), 1)
        .unwrap();
    wait(&manager, failed, TaskState::Failed);
    wait(&manager, recovery, TaskState::Completed);
    assert!(manager.task(failed).unwrap().list_order_seq < manager.task(recovery).unwrap().list_order_seq);
    assert!(
        manager
            .enqueue(options(dir.path(), "second.mp4", 500), 1)
            .is_err()
    );

    #[cfg(unix)]
    {
        manager.set_max_concurrent(2).unwrap();
        let a = manager
            .enqueue(options(dir.path(), "parallel-a.mp4", 9000), 1)
            .unwrap();
        let b = manager
            .enqueue(options(dir.path(), "parallel-b.mp4", 9000), 1)
            .unwrap();
        wait(&manager, a, TaskState::Running);
        wait(&manager, b, TaskState::Running);
        manager.command(a, Control::Pause).unwrap();
        manager.command(b, Control::Pause).unwrap();
        wait(&manager, a, TaskState::Paused);
        wait(&manager, b, TaskState::Paused);
        let (pid_a, preview_a) = worker_process(a);
        let (pid_b, preview_b) = worker_process(b);
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
        wait(&manager, a, TaskState::Interrupted);
        assert!(manager.task(a).unwrap().committed > 0);
        manager.command(a, Control::Cancel).unwrap();
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
        .enqueue(options(dir.path(), "paused.mp4", 3500), 1)
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
    let boundary = manager.task(paused).unwrap().committed;
    assert!(boundary > 0);
    manager.set_max_concurrent(2).unwrap();
    let completed_second_timestamp = manager.task(second).unwrap().updated_at_ms;
    drop(manager);

    // The output survived publication but the process died before writing its final SQLite row.
    // Another completed task's output was explicitly removed by the user.
    let db = rusqlite::Connection::open(root.join("history.sqlite3")).unwrap();
    db.execute(
        "UPDATE tasks SET state='running', committed_frames=0, output_size=0 WHERE uuid=?1",
        [first.to_string()],
    )
    .unwrap();
    let stale_marker = dir.path().join(".second.mp4.goosy-lock");
    std::fs::write(
        &stale_marker,
        uuid::Uuid::parse_str(&second.to_string())
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    std::fs::remove_file(dir.path().join("second.mp4")).unwrap();
    db.execute(
        "UPDATE tasks SET state='running', committed_frames=9999 WHERE uuid=?1",
        [paused.to_string()],
    )
    .unwrap();
    drop(db);

    // The dead owner's worker still has its exclusive workspace lock while draining a segment.
    let worker_lock = std::fs::File::options()
        .read(true)
        .write(true)
        .open(
            root.join("tasks")
                .join(paused.to_string())
                .join("worker.lock"),
        )
        .unwrap();
    worker_lock.try_lock().unwrap();
    let reopened = self::manager(&root);
    assert_eq!([first, second].map(|id| reopened.task(id).unwrap().list_order_seq), creation_order);
    assert!(reopened.delete(paused).is_err());
    assert!(reopened.command(paused, Control::Resume).is_err());
    assert!(reopened.command(paused, Control::Cancel).is_err());
    drop(worker_lock);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !reopened.task(paused).unwrap().detail.is_empty() {
        assert!(
            Instant::now() < deadline,
            "orphan worker lock was never released"
        );
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!stale_marker.exists());
    assert_eq!(reopened.max_concurrent(), 2);
    let restored = reopened.task(paused).unwrap();
    assert_eq!(restored.state, TaskState::Interrupted);
    assert_eq!(restored.done, boundary);
    assert_eq!(restored.committed, boundary);
    assert_eq!(reopened.task(first).unwrap().state, TaskState::Completed);
    assert_eq!(
        reopened.task(first).unwrap().output_size,
        completed.output_size
    );
    assert_eq!(reopened.task(failed).unwrap().state, TaskState::Failed);
    assert_eq!(
        reopened.task(cancelled).unwrap().state,
        TaskState::Cancelled
    );
    assert!(reopened.command(second, Control::Resume).is_err());
    assert_eq!(
        reopened.task(second).unwrap().updated_at_ms,
        completed_second_timestamp
    );
    assert!(!dir.path().join("second.mp4").exists());
    let lyrics = dir.path().join("song.lrc");
    let original = std::fs::read(&lyrics).unwrap();
    std::fs::write(&lyrics, "[00:00.00]Different song").unwrap();
    let rejected = reopened.command(paused, Control::Resume).unwrap_err();
    assert!(format!("{rejected:#}").contains("song.lrc"));
    assert_eq!(reopened.task(paused).unwrap().state, TaskState::Interrupted);
    std::fs::write(&lyrics, original).unwrap();
    reopened.command(paused, Control::Resume).unwrap();
    wait(&reopened, paused, TaskState::Completed);
    let resumed = reopened.task(paused).unwrap();
    assert_eq!(resumed.committed, resumed.total);
    assert_eq!(resumed.committed, 105);
    assert!(resumed.output_size > 0);
    assert!(resumed.elapsed >= restored.elapsed);
    let movie = fs::read(&completed.options.output).unwrap();
    reopened.delete(first).unwrap();
    assert!(!first_workspace.exists());
    assert_eq!(fs::read(&completed.options.output).unwrap(), movie);
    drop(reopened);
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with("goosy-lock")
    }));
}

#[test]
fn invalid_container_or_private_stage_is_rejected_before_history() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let manager = manager(&root);
    let missing_name = manager.check_output(Path::new("")).unwrap_err();
    assert_eq!(missing_name.downcast_ref::<OutputError>(), Some(&OutputError::MissingFileName));
    let missing_dir = manager.check_output(&dir.path().join("missing").join("out.mp4")).unwrap_err();
    assert_eq!(missing_dir.downcast_ref::<OutputError>(), Some(&OutputError::ParentNotFound));
    assert_eq!(missing_dir.downcast_ref::<std::io::Error>().unwrap().kind(), std::io::ErrorKind::NotFound);
    for name in ["video", "video."] {
        let options = options(dir.path(), name, 500);
        let error = manager.check_output(&options.output).unwrap_err();
        assert_eq!(error.downcast_ref::<OutputError>(), Some(&OutputError::MissingExtension));
        assert_eq!(error.to_string(), "输出路径缺少容器扩展名");
        assert_eq!(manager.enqueue(options, 1).unwrap_err().to_string(), "输出路径缺少容器扩展名");
    }
    let private = dir.path().join(format!(".goosy-stage-{}-{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()));
    let nested = private.join("child");
    fs::create_dir_all(&nested).unwrap();
    let draft = options(&nested, "out.mp4", 500);
    let error = manager.check_output(&draft.output).unwrap_err();
    assert_eq!(error.downcast_ref::<OutputError>(), Some(&OutputError::PrivateDirectory(nested.canonicalize().unwrap())));
    assert!(manager.enqueue(draft, 1).is_err());
    let reserved = dir.path().canonicalize().unwrap().join("reserved.mp4");
    fs::write(dir.path().join(".reserved.mp4.goosy-lock"), b"reservation").unwrap();
    let error = manager.check_output(&reserved).unwrap_err();
    assert_eq!(error.downcast_ref::<OutputError>(), Some(&OutputError::Reserved(reserved)));
    let db = rusqlite::Connection::open(root.join("history.sqlite3")).unwrap();
    assert_eq!(db.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(fs::read_dir(root.join("tasks")).unwrap().count(), 0);
}

#[test]
fn workspace_creation_failure_is_visible_and_releases_the_output() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let manager = manager(&root);
    let tasks = root.join("tasks");
    std::fs::remove_dir(&tasks).unwrap();
    std::fs::write(&tasks, b"obstruct workspace creation").unwrap();
    let options = options(dir.path(), "out.mp4", 500);
    let id = manager.enqueue(options.clone(), 1).unwrap();
    let task = manager.task(id).unwrap();
    assert_eq!(task.state, TaskState::Failed);
    assert!(!task.detail.is_empty());
    manager.check_output(&options.output).unwrap();
    drop(manager);
    std::fs::remove_file(tasks).unwrap();
    let reopened = self::manager(&root);
    assert_eq!(reopened.task(id).unwrap().detail, task.detail);
}

// Seed durable state at precise crash boundaries without starting a render process.
fn insert_history(root: &Path, id: uuid::Uuid, state: &str, options: &RenderOptions) -> TaskId {
    let db = rusqlite::Connection::open(root.join("history.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO tasks (uuid,created_ms,updated_ms,state,options_json,manifest_version,
         preview_interval,committed_frames,total_frames,elapsed_seconds,output_size,error)
         VALUES (?1,1,1,?2,?3,?4,10,0,0,0,0,'')",
        rusqlite::params![id.to_string(), state, serde_json::to_string(options).unwrap(),
            libgoosy::workspace::MANIFEST_VERSION],
    ).unwrap();
    id.to_string().parse().unwrap()
}

#[test]
fn menu_actions_and_output_directory_use_the_task_snapshot() {
    use Control::{Cancel, Pause, Resume};
    use TaskState::*;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let output = dir.path().join("exports");
    fs::create_dir(&output).unwrap();
    drop(manager(&root));
    let id = insert_history(&root, uuid::Uuid::new_v4(), "failed", &options(&output, "out.mp4", 500));
    let manager = manager(&root);
    let mut task = manager.task(id).unwrap();
    let base = &["redo", "openOutput", "delete"][..];
    let stop = &["cancel", "redo", "openOutput", "delete"][..];
    for (state, pending, expected) in [
        (Queued, None, stop), (Starting, None, stop),
        (Running, None, &["pause", "cancel", "redo", "openOutput", "delete"]),
        (Paused, None, &["resume", "cancel", "redo", "openOutput", "delete"]),
        (Interrupted, None, &["resume", "redo", "openOutput", "delete"]),
        (Completed, None, base), (Failed, None, base), (Cancelled, None, base),
        (Running, Some(Pause), stop), (Paused, Some(Resume), stop),
        (Running, Some(Cancel), base), (Interrupted, Some(Cancel), base),
        (Deleting, None, &["openOutput", "delete"]),
    ] {
        task.state = state;
        task.pending = pending;
        assert_eq!(task.actions(), expected, "{state:?} {pending:?}");
    }
    assert_eq!(manager.output_directory(id).unwrap(), output);
    fs::remove_dir(&output).unwrap();
    assert!(manager.output_directory(id).is_err());
    assert!(!output.exists());
    fs::write(&output, b"not a directory").unwrap();
    assert!(manager.output_directory(id).is_err());
}

#[test]
fn deleting_completed_history_preserves_outputs_inputs_and_other_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    drop(manager(&root));
    let uuid = uuid::Uuid::new_v4();
    let work = root.join("tasks").join(uuid.to_string());
    fs::create_dir_all(work.join("segments")).unwrap();
    fs::create_dir_all(work.join("checkpoints")).unwrap();
    let mut source = options(dir.path(), "ignored.mp4", 500);
    source.output = work.join("segments/00000000000000000000.mp4");
    let asset = work.join("checkpoints/00000000000000000001.json");
    source.song = asset.clone();
    #[cfg(unix)]
    {
        source.song = dir.path().join("asset-link");
        std::os::unix::fs::symlink(&asset, &source.song).unwrap();
    }
    fs::write(&asset, b"user song").unwrap();
    fs::write(&source.output, b"finished video").unwrap();
    let other = options(dir.path(), "other.mp4", 500);
    let other = RenderOptions { output: work.join("config.json"), ..other };
    fs::write(&other.output, b"another task's output").unwrap();
    let id = insert_history(&root, uuid, "completed", &source);
    let other_id = insert_history(&root, uuid::Uuid::new_v4(), "completed", &other);
    let forgotten = RenderOptions { output: work.join("manifest.json"), ..other.clone() };
    fs::write(&forgotten.output, b"an export with deleted history").unwrap();
    let forgotten_id = insert_history(&root, uuid::Uuid::new_v4(), "completed", &forgotten);
    let segment = work.join("segments/00000000000000000030.mp4");
    let checkpoint = work.join("checkpoints/00000000000000000030.json");
    let manifest = work.join("manifest.completed.json");
    let thumbnail = root.join("thumbnails").join(format!("{uuid}.png"));
    let stage = source.output.with_file_name(format!(".goosy-render-{uuid}-1234567890ABCDEF.part.mp4"));
    for artifact in [&segment, &checkpoint, &manifest, &thumbnail, &stage] {
        fs::write(artifact, b"owned").unwrap();
    }
    let manager = manager(&root);
    manager.delete(forgotten_id).unwrap();
    drop(manager);
    let manager = self::manager(&root);
    manager.delete(id).unwrap();
    assert!(manager.task(id).is_none());
    assert_eq!(manager.task(other_id).unwrap().state, TaskState::Completed);
    assert_eq!(fs::read(&stage).unwrap(), b"owned", "a legacy stage name proves no ownership");
    for artifact in [&segment, &checkpoint, &manifest, &thumbnail] {
        assert!(!artifact.exists(), "{}", artifact.display());
    }
    assert_eq!(fs::read(&source.output).unwrap(), b"finished video");
    assert_eq!(fs::read(&source.song).unwrap(), b"user song");
    assert_eq!(fs::read(&other.output).unwrap(), b"another task's output");
    assert_eq!(fs::read(&forgotten.output).unwrap(), b"an export with deleted history");
    drop(manager);
    assert!(self::manager(&root).task(id).is_none());
}

#[test]
fn interrupted_deletion_waits_for_original_output_disk_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let exports = dir.path().join("exports");
    let offline = dir.path().join("disconnected");
    fs::create_dir(&exports).unwrap();
    drop(manager(&root));
    let uuid = uuid::Uuid::new_v4();
    let mut source = options(dir.path(), "out.mp4", 500);
    source.output = exports.join("out.mp4");
    fs::write(&source.song, b"audio").unwrap();
    fs::write(source.lyrics.as_ref().unwrap(), b"lyrics").unwrap();
    let work = root.join("tasks").join(uuid.to_string());
    drop(libgoosy::workspace::Workspace::create(&work, &source, 1).unwrap());
    let id = insert_history(&root, uuid, "interrupted", &source);
    let marker = exports.join(".out.mp4.goosy-lock");
    fs::write(&marker, uuid.as_bytes()).unwrap();
    let queue = manager(&root);
    assert_eq!(queue.task(id).unwrap().state, TaskState::Interrupted);
    let attempt = uuid::Uuid::new_v4();
    let record = work.join("output").join(format!("stage-{attempt}.json"));
    let bytes = serde_json::to_vec(&serde_json::json!({"task": uuid, "attempt": attempt})).unwrap();
    fs::write(&record, bytes).unwrap();

    fs::rename(&exports, &offline).unwrap();
    let error = queue.delete(id).unwrap_err();
    assert!(format!("{error:#}").contains(exports.to_str().unwrap()));
    assert_eq!(queue.task(id).unwrap().state, TaskState::Deleting);
    assert!(record.is_file());
    drop(queue);
    let reopened = manager(&root);
    assert_eq!(reopened.task(id).unwrap().state, TaskState::Deleting);
    drop(reopened);

    fs::write(&exports, b"not a directory").unwrap();
    let reopened = manager(&root);
    let pending = reopened.task(id).unwrap();
    assert_eq!(pending.state, TaskState::Deleting);
    assert!(pending.detail.contains(exports.to_str().unwrap()));
    assert!(record.is_file());
    drop(reopened);
    fs::remove_file(&exports).unwrap();

    // An empty mount point or another volume can occupy the original path.
    fs::create_dir(&exports).unwrap();
    let reopened = manager(&root);
    assert_eq!(reopened.task(id).unwrap().state, TaskState::Deleting);
    let foreign = uuid::Uuid::new_v4();
    fs::write(&marker, foreign.as_bytes()).unwrap();
    assert!(reopened.delete(id).is_err());
    assert_eq!(fs::read(&marker).unwrap().as_slice(), foreign.as_bytes());
    assert!(record.is_file());
    drop(reopened);
    fs::rename(&exports, dir.path().join("replacement")).unwrap();
    fs::rename(&offline, &exports).unwrap();
    let reopened = manager(&root);
    assert!(reopened.task(id).is_none());
    assert!(!work.exists());
    assert!(!marker.exists());
    reopened.check_output(&source.output).unwrap();
    reopened.enqueue(source, 1).unwrap();
}

#[test]
fn deletion_after_marker_release_requires_the_same_parent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let exports = dir.path().join("exports");
    let offline = dir.path().join("original");
    fs::create_dir(&exports).unwrap();
    drop(manager(&root));
    let uuid = uuid::Uuid::new_v4();
    let id = insert_history(&root, uuid, "deleting", &options(&exports, "out.mp4", 500));
    let marker = exports.join(".out.mp4.goosy-lock");
    fs::write(&marker, uuid.as_bytes()).unwrap();
    let db = rusqlite::Connection::open(root.join("history.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER stop_before_forget BEFORE DELETE ON tasks BEGIN
                      SELECT RAISE(ABORT, 'interrupted after marker release'); END;").unwrap();
    let queue = manager(&root);
    assert_eq!(queue.task(id).unwrap().state, TaskState::Deleting);
    assert!(!marker.exists());
    drop(queue);
    db.execute_batch("DROP TRIGGER stop_before_forget;").unwrap();
    fs::rename(&exports, &offline).unwrap();
    fs::create_dir(&exports).unwrap();
    let queue = manager(&root);
    assert_eq!(queue.task(id).unwrap().state, TaskState::Deleting);
    drop(queue);
    fs::remove_dir(&exports).unwrap();
    fs::rename(&offline, &exports).unwrap();
    let foreign = uuid::Uuid::new_v4();
    fs::write(&marker, foreign.as_bytes()).unwrap();
    assert!(manager(&root).task(id).is_none());
    assert_eq!(fs::read(marker).unwrap().as_slice(), foreign.as_bytes());
}

#[test]
fn partially_deleted_workspace_stays_tracked_and_finishes_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    drop(manager(&root));
    let uuid = uuid::Uuid::new_v4();
    let source = options(dir.path(), "out.mp4", 500);
    let id = insert_history(&root, uuid, "cancelled", &source);
    let work = root.join("tasks").join(uuid.to_string());
    fs::create_dir(&work).unwrap();
    fs::write(work.join("manifest.json"), b"manifest").unwrap();
    let blocked = root.join("thumbnails").join(format!("{uuid}.png"));
    fs::create_dir(&blocked).unwrap();
    fs::write(&source.output, b"user movie").unwrap();
    let manager = manager(&root);
    assert!(manager.delete(id).is_err());
    assert_eq!(manager.task(id).unwrap().state, TaskState::Deleting);
    assert!(manager.command(id, Control::Resume).is_err());
    assert!(!work.exists());
    drop(manager);
    let reopened = self::manager(&root);
    assert_eq!(reopened.task(id).unwrap().state, TaskState::Deleting);
    drop(reopened);
    fs::remove_dir(&blocked).unwrap();
    assert!(self::manager(&root).task(id).is_none());
    assert_eq!(fs::read(&source.output).unwrap(), b"user movie");
}

#[test]
fn accepted_cancel_survives_restart_and_waits_for_orphan_to_exit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    drop(manager(&root));
    let uuid = uuid::Uuid::new_v4();
    let source = options(dir.path(), "cancel.mp4", 500);
    fs::write(&source.song, b"test audio").unwrap();
    fs::write(source.lyrics.as_ref().unwrap(), b"lyrics").unwrap();
    let id = insert_history(&root, uuid, "cancelling", &source);
    let work = root.join("tasks").join(uuid.to_string());
    let worker = libgoosy::workspace::Workspace::create(&work, &source, 1).unwrap();
    let marker = dir.path().join(".cancel.mp4.goosy-lock");
    fs::write(&marker, uuid.as_bytes()).unwrap();
    let manager = manager(&root);
    assert_eq!(manager.task(id).unwrap().pending, Some(Control::Cancel));
    assert!(manager.delete(id).is_err());
    assert!(marker.is_file());
    drop(worker);
    let deadline = Instant::now() + Duration::from_secs(5);
    while manager.task(id).unwrap().state != TaskState::Cancelled {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!marker.exists());
    assert!(work.join("config.json").exists());
    assert!(!source.output.exists());
    drop(manager);
    let reopened = self::manager(&root);
    assert_eq!(reopened.task(id).unwrap().state, TaskState::Cancelled);
    reopened.delete(id).unwrap();
    assert!(!work.exists());
}

#[cfg(unix)]
#[test]
fn delete_refuses_to_follow_replaced_workspace_directories() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    drop(manager(&root));
    let uuid = uuid::Uuid::new_v4();
    let id = insert_history(&root, uuid, "failed", &options(dir.path(), "out.mp4", 500));
    let outside = dir.path().join("external");
    fs::create_dir(&outside).unwrap();
    let unrelated = outside.join("00000000000000000000.mp4");
    fs::write(&unrelated, b"user data").unwrap();
    let work = root.join("tasks").join(uuid.to_string());
    fs::create_dir(&work).unwrap();
    std::os::unix::fs::symlink(&outside, work.join("segments")).unwrap();
    let manager = manager(&root);
    assert!(manager.delete(id).is_err());
    assert_eq!(fs::read(&unrelated).unwrap(), b"user data");
    fs::remove_file(work.join("segments")).unwrap();
    manager.delete(id).unwrap();
    assert!(!work.exists());
    assert!(unrelated.exists());
}
