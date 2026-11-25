use anyhow::{Context, Result, ensure};
use libgoosy::{EncoderBackend, RenderOptions, workspace::Workspace};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use uuid::Uuid;

fn worker(dir: &Path) -> Result<Child> {
    Ok(Command::new(env!("CARGO_BIN_EXE_goosy-render-worker"))
        .args(["render", "--progress-events", "--workspace"])
        .arg(dir)
        .env("GOOSY_RENDER_BACKEND", "raster")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?)
}

#[test]
fn killed_worker_resumes_after_verified_segment() -> Result<()> {
    let root = tempfile::tempdir()?;
    let song = root.path().join("fixture.wav");
    let lyrics = root.path().join("fixture.lrc");
    let output = root.path().join("final.mp4");
    ensure!(
        Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "sine=duration=2"])
            .arg(&song)
            .status()?
            .success(),
        "could not create audio fixture"
    );
    fs::write(&lyrics, "[00:00.00]First\n[00:01.00]Second\n")?;
    let mut options = RenderOptions::new(song, &output);
    options.lyrics = Some(lyrics);
    options.width = 192;
    options.height = 108;
    options.fps = 12;
    options.encoder = EncoderBackend::Pipe;
    let dir = root.path().join(Uuid::new_v4().to_string());
    drop(Workspace::create(&dir, &options, 1)?);

    // Kill once the encoder has accepted a frame in segment two, before finish/commit.
    let mut first = worker(&dir)?;
    let mut committed_first = false;
    let mut killed = false;
    for line in BufReader::new(first.stdout.take().unwrap()).lines() {
        let line = line?;
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next()) {
            (Some("GOOSY_COMMITTED"), Some("12")) => committed_first = true,
            (Some("GOOSY_SUBMITTED"), Some("13")) if committed_first => {
                first.kill()?;
                killed = true;
                break;
            }
            _ => {}
        }
    }
    let status = first.wait()?;
    ensure!(
        killed && !status.success(),
        "worker never reached the interruption point: {status}"
    );
    assert!(!output.exists());
    let work = Workspace::open(&dir)?;
    assert_eq!(work.committed(), 12);
    let segment = work
        .segments()
        .into_iter()
        .next()
        .context("first segment missing")?;
    let original = fs::read(&segment)?;
    drop(work);

    let mut resumed = worker(&dir)?;
    let mut first_rendered = None;
    for line in BufReader::new(resumed.stdout.take().unwrap()).lines() {
        let line = line?;
        if let Some(progress) = line.strip_prefix("GOOSY_PROGRESS ") {
            first_rendered
                .get_or_insert(progress.split_whitespace().next().unwrap().parse::<u64>()?);
        }
    }
    ensure!(resumed.wait()?.success(), "worker could not resume");
    assert_eq!(first_rendered, Some(13));
    assert_eq!(fs::read(segment)?, original);
    assert!(output.is_file());
    let decoded = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(output)
        .output()?;
    assert!(decoded.status.success());
    assert_eq!(std::str::from_utf8(&decoded.stdout)?.trim(), "24");
    Ok(())
}
