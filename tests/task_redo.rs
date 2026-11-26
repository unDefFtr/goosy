use libgoosy::task_manager::{AssetIssueReason, AssetRole, Control, OutputError, TaskId, TaskManager, TaskState};
use libgoosy::workspace::Workspace;
use libgoosy::{EncoderBackend, LyricFormat, RenderOptions};
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn manager(root: &Path, executable: &Path) -> TaskManager {
    TaskManager::with_data_dir(executable.to_owned(), root).unwrap()
}

fn wait(manager: &TaskManager, id: TaskId, state: TaskState) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let task = manager.task(id).unwrap();
        if task.state == state {
            return;
        }
        assert!(!task.state.is_terminal() && Instant::now() < deadline, "{:?}: {}", task.state, task.detail);
        thread::sleep(Duration::from_millis(20));
    }
}

fn json(path: PathBuf) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn history_row(root: &Path, id: TaskId) -> Vec<rusqlite::types::Value> {
    let db = Connection::open(root.join("history.sqlite3")).unwrap();
    let mut select = db.prepare("SELECT * FROM tasks WHERE uuid=?1").unwrap();
    let columns = select.column_count();
    select.query_row([id.to_string()], |row| (0..columns).map(|i| row.get(i)).collect()).unwrap()
}

fn files(dir: PathBuf) -> Vec<(PathBuf, Vec<u8>)> {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|entry| {
        let path = entry.unwrap().path();
        let content = fs::read(&path).unwrap();
        (path, content)
    }).collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[test]
fn completed_sample_creates_independent_history_and_empty_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let root = dir.join("data");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("settings.toml"), "keep_segments = true\n").unwrap();
    let render = manager(&root, Path::new(env!("CARGO_BIN_EXE_goosy-render-worker")));
    let mut options = RenderOptions::new(dir.join("song.wav"), dir.join("video.mp4"));
    assert!(Command::new("ffmpeg").args(["-v", "error", "-f", "lavfi", "-i", "sine=duration=0.5"])
        .arg(&options.song).status().unwrap().success());
    let lyrics = dir.join("song.lrc");
    fs::write(&lyrics, "[00:00.00]First line\n[00:00.20]Second line\n").unwrap();
    options.lyrics = Some(lyrics);
    let image = dir.join("art.png");
    let info = skia_safe::ImageInfo::new_n32_premul((2, 2), None);
    let mut pixels = [255; 16];
    let png = skia_safe::Pixmap::new(&info, &mut pixels, 8).unwrap()
        .encode(skia_safe::EncodedImageFormat::PNG, 100).unwrap();
    fs::write(&image, png).unwrap();
    options.cover = Some(image.clone());
    options.background = Some(image);
    options.title = Some("Historical title".into());
    options.width = 320;
    options.height = 180;
    options.fps = 10;
    options.format = LyricFormat::Lrc;
    options.encoder = EncoderBackend::Pipe;
    options.nvenc_preset = "p3".into();
    options.no_embedded_cover = true;
    options.render_translation = false;
    options.render_background_vocal = false;
    options.excluded_lines = vec![1];
    options.lyrics_style.font_scale = 1.2;
    options.lyrics_style.lyric_blur_sigma_step = 2;
    options.sample_start_ms = Some(100);
    options.sample_duration_ms = Some(2000); // Clipped by the actual audio duration.
    let source = render.enqueue(options, 7).unwrap();
    wait(&render, source, TaskState::Completed);
    let original = render.task(source).unwrap().options;
    drop(render);

    let work = root.join("tasks").join(source.to_string());
    let manifest = fs::read(work.join("manifest.json")).unwrap();
    let config = fs::read(work.join("config.json")).unwrap();
    let segments = files(work.join("segments"));
    let checkpoints = files(work.join("checkpoints"));
    assert!(!segments.is_empty() && !checkpoints.is_empty());
    let movie = fs::read(&original.output).unwrap();
    let row = history_row(&root, source);
    let range = json(work.join("manifest.json"))["range"].clone();

    // Leave the renderer unavailable for the second enqueue, so the first manifest can be
    // inspected deterministically without the new worker racing to commit its first frame.
    let reopened = manager(&root, &dir.join("unavailable-worker"));
    let lock = fs::File::options().read(true).write(true).open(work.join("worker.lock")).unwrap();
    lock.try_lock().unwrap();
    let mut draft = reopened.redo_draft(source).unwrap();
    drop(lock);
    assert_eq!(draft.preview_interval, 7);
    assert_ne!(draft.options.output, original.output);
    let mut expected = (*original).clone();
    expected.output.clone_from(&draft.options.output);
    expected.sample_start_ms = range["start_ms"].as_u64();
    expected.sample_duration_ms = range["duration_ms"].as_u64();
    assert_ne!(expected.sample_duration_ms, original.sample_duration_ms);
    assert_eq!(serde_json::to_value(&draft.options).unwrap(), serde_json::to_value(expected).unwrap());
    for role in AssetRole::ALL {
        assert!(draft.inspect(role).is_none());
    }
    draft.options.title = Some("Edited title".into());
    let new = draft.submit(&reopened).unwrap();
    assert_ne!(source, new);
    let fresh = root.join("tasks").join(new.to_string());
    let first = json(fresh.join("manifest.json"));
    assert_eq!(first["task"], new.to_string());
    assert!(first["segments"].as_array().unwrap().is_empty());
    assert!(first["range"].is_null() && first["publication"].is_null());
    assert!(files(fresh.join("segments")).is_empty());
    assert!(files(fresh.join("checkpoints")).is_empty());
    let saved: RenderOptions = serde_json::from_value(json(fresh.join("config.json"))["options"].take()).unwrap();
    assert_eq!(serde_json::to_value(saved).unwrap(), serde_json::to_value(&draft.options).unwrap());
    assert_eq!(reopened.tasks().len(), 2);
    assert_ne!(history_row(&root, new), row);
    assert_eq!(history_row(&root, source), row);
    assert_eq!(fs::read(&original.output).unwrap(), movie);
    assert_eq!(fs::read(work.join("manifest.json")).unwrap(), manifest);
    assert_eq!(fs::read(work.join("config.json")).unwrap(), config);
    assert_eq!(files(work.join("segments")), segments);
    assert_eq!(files(work.join("checkpoints")), checkpoints);

    Workspace::cleanup_published(&work).unwrap();
    assert!(work.join("manifest.completed.json").is_file());
    let mut archived = reopened.redo_draft(source).unwrap();
    assert_eq!(archived.options.sample_duration_ms, draft.options.sample_duration_ms);
    assert!(archived.inspect(AssetRole::Audio).is_none());
}

#[test]
fn failed_task_checks_each_relinked_or_accepted_input_and_reserves_a_new_output() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let root = dir.join("data");
    let manager = manager(&root, &dir.join("unavailable-worker"));
    let mut options = RenderOptions::new(dir.join("song"), dir.join("export.mp4"));
    let lyrics = dir.join("lyrics");
    let cover = dir.join("cover");
    let background = dir.join("background");
    for (path, data) in [(&options.song, "audio"), (&lyrics, "lyric"), (&cover, "cover"), (&background, "scene")] {
        fs::write(path, data).unwrap();
    }
    options.lyrics = Some(lyrics.clone());
    options.cover = Some(cover.clone());
    options.background = Some(background.clone());
    let source = manager.enqueue(options.clone(), 3).unwrap();
    wait(&manager, source, TaskState::Failed);
    fs::write(dir.join("export-redo.mp4"), "user's export").unwrap();
    fs::write(dir.join(".export-redo-2.mp4.goosy-lock"), "another reservation").unwrap();
    let mut redo = manager.redo_draft(source).unwrap();
    assert_eq!(redo.options.output.file_name().unwrap(), "export-redo-3.mp4");
    let available = redo.options.output.clone();
    redo.options.output = options.output.clone();
    assert!(redo.submit(&manager).is_err()); // Even if the failed task never produced its output.
    redo.options.output = available;
    fs::write(&redo.options.output, "arrived after the wizard opened").unwrap();
    let error = redo.submit(&manager).unwrap_err();
    assert_eq!(error.downcast_ref::<OutputError>(), Some(&OutputError::AlreadyExists(redo.options.output.clone())));
    assert_eq!(fs::read(&redo.options.output).unwrap(), b"arrived after the wizard opened");
    redo.options.output = dir.canonicalize().unwrap().join("alternate.mp4");

    let moved = dir.join("moved-audio");
    fs::rename(&options.song, &moved).unwrap();
    let issue = redo.inspect(AssetRole::Audio).unwrap();
    assert_eq!(issue.path, options.song);
    assert!(!issue.can_accept());
    assert!(matches!(issue.reason, AssetIssueReason::Unreadable(_)));
    assert!(redo.accept(AssetRole::Audio).is_err());
    assert!(redo.submit(&manager).is_err());
    redo.options.song = moved;
    assert!(redo.inspect(AssetRole::Audio).is_none());
    // Each role checks its own fingerprint, even if an alternative is another original input.
    redo.options.cover = Some(background.clone());
    assert!(redo.inspect(AssetRole::Cover).unwrap().can_accept());
    assert!(redo.inspect(AssetRole::Background).is_none());
    redo.options.cover = Some(cover);
    assert!(redo.accept(AssetRole::Cover).is_err());
    assert!(redo.inspect(AssetRole::Cover).is_none());
    fs::write(&lyrics, "other").unwrap(); // Same byte count, different hash.
    let issue = redo.inspect(AssetRole::Lyrics).unwrap();
    assert_eq!(issue.path, lyrics);
    assert!(issue.can_accept());
    assert!(matches!(issue.reason, AssetIssueReason::ContentChanged));
    assert!(redo.submit(&manager).is_err());
    fs::write(&lyrics, "third").unwrap();
    assert!(redo.accept(AssetRole::Lyrics).is_err());
    assert!(redo.inspect(AssetRole::Lyrics).unwrap().can_accept());
    redo.accept(AssetRole::Lyrics).unwrap();
    assert!(redo.inspect(AssetRole::Lyrics).is_none());
    fs::write(&lyrics, "changed again").unwrap();
    assert!(redo.submit(&manager).is_err());
    assert_eq!(manager.tasks().len(), 1);
    assert!(redo.inspect(AssetRole::Lyrics).unwrap().can_accept());
    redo.accept(AssetRole::Lyrics).unwrap();
    let id = redo.submit(&manager).unwrap();
    assert_ne!(source, id);
    let config = json(root.join("tasks").join(id.to_string()).join("config.json"));
    let path = serde_json::to_value(&lyrics).unwrap();
    let asset = config["inputs"].as_array().unwrap().iter().find(|a| a["path"] == path).unwrap();
    assert_eq!(asset["content"]["blake3"], blake3::hash(b"changed again").to_hex().as_str());
}

#[test]
fn cancelled_and_missing_provenance_require_explicit_approval_for_unknown_content() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let root = dir.join("data");
    let executable = dir.join("unavailable-worker");
    let first = manager(&root, &executable);
    let options = RenderOptions::new(dir.join("input"), dir.join("result.mp4"));
    // Creation failed before config.json existed. SQLite still holds the complete options.
    let missing = first.enqueue(options.clone(), 13).unwrap();
    wait(&first, missing, TaskState::Failed);
    fs::write(&options.song, "recovered source").unwrap();
    let mut unknown = first.redo_draft(missing).unwrap();
    assert_eq!(unknown.preview_interval, 13);
    let issue = unknown.inspect(AssetRole::Audio).unwrap();
    assert_eq!(issue.path, options.song);
    assert!(issue.can_accept());
    assert!(matches!(issue.reason, AssetIssueReason::MissingFingerprint));
    assert!(unknown.submit(&first).is_err());
    unknown.accept(AssetRole::Audio).unwrap();
    let id = unknown.submit(&first).unwrap();
    wait(&first, id, TaskState::Failed);
    drop(first);

    // Simulate a persisted in-flight row after loss of the original manager. Cancel it through
    // the normal interrupted-task control path, then reopen with an old per-asset fingerprint absent.
    let db = Connection::open(root.join("history.sqlite3")).unwrap();
    db.execute("UPDATE tasks SET state='running' WHERE uuid=?1", [id.to_string()]).unwrap();
    drop(db);
    let recovered = manager(&root, &executable);
    assert_eq!(recovered.task(id).unwrap().state, TaskState::Interrupted);
    recovered.command(id, Control::Cancel).unwrap();
    assert_eq!(recovered.task(id).unwrap().state, TaskState::Cancelled);
    drop(recovered);
    let work = root.join("tasks").join(id.to_string());
    let mut config = json(work.join("config.json"));
    config["inputs"][0].as_object_mut().unwrap().remove("content");
    let bytes = serde_json::to_vec(&config).unwrap();
    fs::write(work.join("config.json"), &bytes).unwrap();
    let mut manifest = json(work.join("manifest.json"));
    manifest["config_blake3"] = blake3::hash(&bytes).to_hex().to_string().into();
    fs::write(work.join("manifest.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
    let reopened = manager(&root, &executable);
    let mut redo = reopened.redo_draft(id).unwrap();
    assert!(redo.inspect(AssetRole::Audio).unwrap().can_accept());
    assert!(redo.submit(&reopened).is_err());
    redo.accept(AssetRole::Audio).unwrap();
    assert_ne!(redo.submit(&reopened).unwrap(), id);
    assert_eq!(reopened.task(id).unwrap().state, TaskState::Cancelled);
}
