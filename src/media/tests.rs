use super::*;
use std::fs;

fn ffmpeg() -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-nostdin", "-hide_banner", "-v", "error", "-y"]);
    command
}

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn video(path: &Path, fps: u32, frames: u64, color: &str, profile: &str) {
    run(ffmpeg()
        .args(["-f", "lavfi", "-i"])
        .arg(format!("color=c={color}:s=32x24:r={fps}"))
        .arg("-frames:v")
        .arg(frames.to_string())
        .args([
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-profile:v",
            profile,
        ])
        .arg(path));
}

fn options(dir: &Path, fps: u32) -> RenderOptions {
    let mut options = RenderOptions::new(dir.join("song.wav"), dir.join("out.mp4"));
    options.width = 32;
    options.height = 24;
    options.fps = fps;
    options
}

#[test]
fn deletion_only_unlinks_verified_unprotected_stage_files() {
    use crate::history::{History, Record};

    let dir = tempfile::tempdir().unwrap();
    let history = History::open(&dir.path().join("data")).unwrap();
    let id = Uuid::new_v4();
    let work_dir = history.root().join("tasks").join(id.to_string());
    let opts = options(dir.path(), 8);
    fs::write(&opts.song, b"song").unwrap();
    let work = Workspace::create(&work_dir, &opts, 1).unwrap();
    let owned = MuxStage::allocate(&work).unwrap();
    let input = MuxStage::allocate(&work).unwrap();
    let unverified = MuxStage::allocate(&work).unwrap();
    let extra = owned.dir.join("notes");
    let decoy = dir.path().join(format!(".goosy-render-{id}-aaaaaaaaaaaaaaaa.part.mp4"));
    fs::write(&opts.output, b"finished movie").unwrap();
    fs::write(&input.movie, b"user input").unwrap();
    fs::write(&unverified.movie, b"foreign stage").unwrap();
    fs::write(unverified.dir.join("owner"), [0; 32]).unwrap();
    fs::write(&extra, b"unknown file").unwrap();
    fs::write(&decoy, b"other task's movie").unwrap();
    #[cfg(unix)]
    let link = {
        let stage = MuxStage::allocate(&work).unwrap();
        fs::remove_file(&stage.movie).unwrap();
        std::os::unix::fs::symlink(&decoy, &stage.movie).unwrap();
        stage.movie
    };
    drop(work);

    let record = Record {
        id, state: "deleting".into(), options: opts.clone(), created_ms: 0, updated_ms: 0,
        elapsed: 0.0, committed: 0, total: 0, output_size: 0, error: String::new(), interval: 1,
    };
    history.insert(&record).unwrap();
    let other_id = Uuid::new_v4();
    history.insert(&Record {
        id: other_id,
        options: RenderOptions { song: input.movie.clone(), output: decoy.clone(), ..opts.clone() },
        state: "deleting".into(), created_ms: 0, updated_ms: 0, elapsed: 0.0, committed: 0,
        total: 0, output_size: 0, error: String::new(), interval: 1,
    }).unwrap();
    // History remembers the input under its stage's owner even after its own row is gone.
    history.protected_files(other_id).unwrap();
    history.finish_deletion(other_id).unwrap();
    let protected = history.protected_files(id).unwrap();
    let lock = history.deletion_lock(&work_dir).unwrap();
    let parent = crate::history::DeletionParent::open(opts.output.parent().unwrap()).unwrap();
    history.clean_task_files(&record, &work_dir, lock, &protected, &parent).unwrap();
    assert!(!owned.movie.exists() && !owned.dir.join("owner").exists());
    assert!(!input.dir.join("owner").exists());
    assert!(!work_dir.exists());
    assert_eq!(fs::read(&opts.output).unwrap(), b"finished movie");
    assert_eq!(fs::read(&input.movie).unwrap(), b"user input");
    assert_eq!(fs::read(&unverified.movie).unwrap(), b"foreign stage");
    assert_eq!(fs::read(unverified.dir.join("owner")).unwrap(), [0; 32]);
    assert_eq!(fs::read(extra).unwrap(), b"unknown file");
    assert_eq!(fs::read(&decoy).unwrap(), b"other task's movie");
    #[cfg(unix)]
    assert_eq!(link.read_link().unwrap(), decoy);
}

#[test]
fn corrupt_staging_record_keeps_its_directory_and_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = options(tmp.path(), 8);
    fs::write(&opts.song, b"song").unwrap();
    let work = Workspace::create(&tmp.path().join(Uuid::new_v4().to_string()), &opts, 1).unwrap();
    let stage = MuxStage::allocate(&work).unwrap();
    fs::write(&stage.movie, b"unfinished").unwrap();
    fs::write(&stage.record_path, b"{broken").unwrap();
    assert!(clean_staging(&work).is_err());
    assert_eq!(fs::read(stage.movie).unwrap(), b"unfinished");
    assert_eq!(fs::read(stage.record_path).unwrap(), b"{broken");
}

#[test]
fn offline_parent_keeps_record_until_its_owner_can_be_checked() {
    let tmp = tempfile::tempdir().unwrap();
    let exports = tmp.path().join("exports");
    let offline = tmp.path().join("offline");
    fs::create_dir(&exports).unwrap();
    let mut opts = options(tmp.path(), 8);
    opts.output = exports.join("out.mp4");
    fs::write(&opts.song, b"song").unwrap();
    let work = Workspace::create(&tmp.path().join(Uuid::new_v4().to_string()), &opts, 1).unwrap();
    let stage = MuxStage::allocate(&work).unwrap();
    fs::remove_file(stage.dir.join("owner")).unwrap();
    fs::write(&stage.movie, b"unclaimed").unwrap();
    fs::rename(&exports, &offline).unwrap();
    assert!(clean_staging(&work).is_err());
    assert!(stage.record_path.is_file());
    fs::rename(&offline, &exports).unwrap();
    clean_staging(&work).unwrap();
    assert!(!stage.record_path.exists());
    assert_eq!(fs::read(stage.movie).unwrap(), b"unclaimed");
}

#[cfg(unix)]
#[test]
fn staging_directory_link_is_never_followed_even_with_a_matching_token() {
    let tmp = tempfile::tempdir().unwrap();
    let opts = options(tmp.path(), 8);
    fs::write(&opts.song, b"song").unwrap();
    let work = Workspace::create(&tmp.path().join(Uuid::new_v4().to_string()), &opts, 1).unwrap();
    let stage = MuxStage::allocate(&work).unwrap();
    let elsewhere = tmp.path().join("elsewhere");
    fs::write(&stage.movie, b"user file").unwrap();
    fs::rename(&stage.dir, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &stage.dir).unwrap();
    clean_staging(&work).unwrap();
    assert!(stage.dir.is_symlink());
    assert_eq!(fs::read(&stage.movie).unwrap(), b"user file");
    assert_eq!(fs::read(elsewhere.join("owner")).unwrap(), stage.record.token());
}

#[test]
fn fractional_boundaries_preserve_every_frame_and_trim_the_song() {
    let dir = tempfile::tempdir().unwrap();
    let options = options(dir.path(), 60);
    // Millisecond-rounded MP4 durations would drift over half a frame by the end.
    let red = dir.path().join("red.mp4");
    let blue = dir.path().join("blue.mp4");
    video(&red, 60, 1, "red", "high");
    video(&blue, 60, 1, "blue", "high");
    assert_eq!(
        validate_segment(&red, &options, 1).unwrap(),
        validate_segment(&blue, &options, 1).unwrap()
    );
    run(ffmpeg()
        .args([
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:duration=1",
            "-af",
            "adelay=1000:all=1",
        ])
        .arg(&options.song));
    let mut segments = vec![(red, 1); 35];
    segments.push((blue, 1));
    let work_dir = dir.path().join(Uuid::new_v4().to_string());
    let work = Workspace::create(&work_dir, &options, 1).unwrap();
    let stage = MuxStage::allocate(&work).unwrap();
    mux(&stage.movie, &segments, &options, 1_000, 600).unwrap();
    validate(&stage.movie, &options, 36, Some(600)).unwrap();

    let timings = run(Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=pts_time",
            "-of",
            "json",
            "-i",
        ])
        .arg(&stage.movie));
    let timings: Value = serde_json::from_slice(&timings).unwrap();
    let timings = timings["frames"].as_array().unwrap();
    assert_eq!(timings.len(), 36);
    for (i, frame) in timings.iter().enumerate() {
        let time = frame["pts_time"].as_str().unwrap().parse::<f64>().unwrap();
        assert!(
            (time - i as f64 / 60.0).abs() < 1e-6,
            "timestamp at {i}: {time}"
        );
    }
    let rgb = run(ffmpeg().arg("-i").arg(&stage.movie).args([
        "-map",
        "0:v:0",
        "-vf",
        "scale=1:1",
        "-pix_fmt",
        "rgb24",
        "-f",
        "rawvideo",
        "pipe:1",
    ]));
    assert_eq!(rgb.len(), 36 * 3);
    for pixel in rgb[..35 * 3].chunks_exact(3) {
        assert!(pixel[0] > 200 && pixel[2] < 30);
    }
    assert!(rgb[35 * 3 + 2] > 200 && rgb[35 * 3] < 30);
    // The source's first second is silent. Check samples from within the requested range.
    let pcm = run(ffmpeg()
        .arg("-i")
        .arg(&stage.movie)
        .args(["-map", "0:a:0", "-ac", "1", "-f", "s16le", "pipe:1"]));
    let middle = pcm.chunks_exact(2).skip(4_800).take(16_000);
    let peak = middle
        .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
        .max()
        .unwrap();
    assert!(peak > 2_000, "expected the trimmed tone, peak was {peak}");
}

#[test]
fn independently_decodes_b_frames_and_identifies_decoder_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let options = options(dir.path(), 30);
    let high = dir.path().join("high.mp4");
    let baseline = dir.path().join("baseline.mp4");
    video(&high, 30, 8, "blue", "high");
    video(&baseline, 30, 8, "blue", "baseline");
    let high_signature = validate_segment(&high, &options, 8).unwrap();
    let baseline_signature = validate_segment(&baseline, &options, 8).unwrap();
    let high_config: Value = serde_json::from_str(&high_signature).unwrap();
    let baseline_config: Value = serde_json::from_str(&baseline_signature).unwrap();
    assert_ne!(
        high_config["extradata_hash"],
        baseline_config["extradata_hash"]
    );
    assert!(validate_segment(&high, &options, 7).is_err());

    // Keep the index, sample count and codec header intact but break the first packet payload.
    let packet = run(Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_packets",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pos,size",
            "-of",
            "json",
            "-i",
        ])
        .arg(&high));
    let packet: Value = serde_json::from_slice(&packet).unwrap();
    let packet = &packet["packets"][0];
    let pos = packet["pos"].as_str().unwrap().parse::<usize>().unwrap();
    let size = packet["size"].as_str().unwrap().parse::<usize>().unwrap();
    let mut bytes = fs::read(&high).unwrap();
    bytes[pos..pos + size].fill(0xff);
    fs::write(&high, bytes).unwrap();
    assert!(validate_segment(&high, &options, 8).is_err());
}

#[test]
fn failed_publication_preserves_existing_file_and_committed_segments_for_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options(dir.path(), 8);
    opts.no_audio = true;
    run(ffmpeg()
        .args(["-f", "lavfi", "-i", "sine=duration=1"])
        .arg(&opts.song));
    let task_dir = dir.path().join(uuid::Uuid::new_v4().to_string());
    let mut workspace = Workspace::create(&task_dir, &opts, 1).unwrap();
    workspace.set_range(0, 1_000, 8).unwrap();
    workspace.set_render_backend("ffmpeg/libx264").unwrap();
    let segment = workspace.segment_path(0);
    video(&segment, 8, 8, "red", "high");
    let signature = validate_segment(&segment, &opts, 8).unwrap();
    let lines = crate::parse_lyrics("[00:00.00]fixture", crate::LyricFormat::Lrc).unwrap();
    let mut layout = crate::layout::Layout::new(&lines, 24.0, &[10.0], 3.0, 6.0);
    layout.update(&lines, 875, 8);
    let checkpoint = layout.save_checkpoint();
    workspace.commit(0, 8, &checkpoint, &signature).unwrap();
    fs::write(&opts.output, b"existing user file").unwrap();
    let own_orphan = MuxStage::allocate(&workspace).unwrap();
    let wrong_stream = vec![(opts.song.clone(), 8)];
    assert!(mux(&own_orphan.movie, &wrong_stream, &opts, 0, 1_000).is_err());
    let decoy = dir.path().join(format!(".goosy-render-{}-0123456789ABCDEF.part.mp4", workspace.id()));
    fs::write(&decoy, b"another task's finished export").unwrap();
    assert!(finalize(&mut workspace, 0, 1_000, 8).is_err());
    assert!(!own_orphan.dir.exists() && !own_orphan.record_path.exists());
    assert_eq!(fs::read(&decoy).unwrap(), b"another task's finished export");
    assert_eq!(fs::read(&opts.output).unwrap(), b"existing user file");
    assert_eq!(workspace.verified_segments().unwrap().len(), 1);
    fs::remove_file(&opts.output).unwrap();
    // A competing publisher can win after validation, including after the receipt is saved.
    let before_move = MuxStage::allocate(&workspace).unwrap();
    fs::write(&before_move.movie, b"verified bytes").unwrap();
    workspace.prepare_publication(&before_move.movie).unwrap();
    drop(workspace);
    assert_eq!(Workspace::recover_published(&task_dir).unwrap(), None);
    assert_eq!(fs::read(&before_move.movie).unwrap(), b"verified bytes");
    #[cfg(unix)]
    {
        let absent = dir.path().join("absent");
        std::os::unix::fs::symlink(&absent, &opts.output).unwrap();
        assert!(workspace::publish_noclobber(&before_move.movie, &opts.output).is_err());
        assert_eq!(opts.output.read_link().unwrap(), absent);
        assert_eq!(fs::read(&before_move.movie).unwrap(), b"verified bytes");
        fs::remove_file(&opts.output).unwrap();
    }
    fs::write(&opts.output, b"winning publication").unwrap();
    assert!(workspace::publish_noclobber(&before_move.movie, &opts.output).is_err());
    assert_eq!(fs::read(&before_move.movie).unwrap(), b"verified bytes");
    assert_eq!(fs::read(&opts.output).unwrap(), b"winning publication");
    fs::remove_file(&opts.output).unwrap();
    let mut workspace = Workspace::open(&task_dir).unwrap();
    finalize(&mut workspace, 0, 1_000, 8).unwrap();
    assert!(!before_move.dir.exists() && !before_move.record_path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let control = fs::File::create(dir.path().join("control")).unwrap();
        assert_eq!(fs::metadata(&opts.output).unwrap().permissions().mode() & 0o777,
            control.metadata().unwrap().permissions().mode() & 0o777);
    }
    let expected_size = fs::metadata(&opts.output).unwrap().len();
    assert_eq!(workspace.published_size().unwrap(), Some(expected_size));
    // Simulate a crash immediately after the atomic move and before stage cleanup.
    let after_move = MuxStage::allocate(&workspace).unwrap();
    fs::rename(&opts.output, &after_move.movie).unwrap();
    workspace::publish_noclobber(&after_move.movie, &opts.output).unwrap();
    drop(workspace);
    assert_eq!(Workspace::recover_published(&task_dir).unwrap(), Some((8, expected_size)));
    assert!(!after_move.dir.exists() && !after_move.record_path.exists());
    let work = Workspace::open(&task_dir).unwrap();
    let leftover = MuxStage::allocate(&work).unwrap();
    drop(work);
    let mut reopened = Workspace::open(&task_dir).unwrap();
    assert!(!leftover.dir.exists() && !leftover.record_path.exists());
    assert_eq!(fs::read(&decoy).unwrap(), b"another task's finished export");
    assert_eq!(reopened.published_size().unwrap(), Some(expected_size));
    finalize(&mut reopened, 0, 1_000, 8).unwrap();
    validate(&opts.output, &opts, 8, None).unwrap();
}
