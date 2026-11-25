use crate::{RenderOptions, workspace::{self, Workspace}};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tempfile::{Builder, NamedTempFile};
use uuid::Uuid;

#[derive(Clone, Copy)]
struct TimeBase {
    num: u32,
    den: u32,
}

impl TimeBase {
    fn from_stream(stream: &Value) -> Result<Self> {
        let (num, den) = stream["time_base"]
            .as_str()
            .and_then(|s| s.split_once('/'))
            .context("missing stream time base")?;
        let tb = Self {
            num: num.parse()?,
            den: den.parse()?,
        };
        ensure!(tb.num > 0 && tb.den > 0, "invalid stream time base");
        Ok(tb)
    }

    // Convert units / rate seconds to the nearest representable stream timestamp.
    fn ticks(self, units: u64, rate: u32) -> Result<i64> {
        let divisor = u128::from(rate) * u128::from(self.num);
        Ok(i64::try_from(
            (u128::from(units) * u128::from(self.den) + divisor / 2) / divisor,
        )?)
    }
}

struct Audio {
    clock: TimeBase,
    sample_rate: u32,
    duration_ms: u64,
}

struct Streams {
    video_clock: TimeBase,
    pixel_format: String,
    signature: String,
    audio: Option<Audio>,
}

fn streams(
    path: &Path,
    options: &RenderOptions,
    frames: u64,
    audio_ms: Option<u64>,
) -> Result<Streams> {
    let result = Command::new("ffprobe")
        .args(["-v", "error", "-show_data_hash", "sha256", "-show_entries",
            "stream=codec_type,codec_name,codec_tag_string,profile,level,width,height,pix_fmt,sample_aspect_ratio,field_order,color_range,color_space,color_transfer,color_primaries,chroma_location,time_base,duration_ts,sample_rate,extradata_size,extradata_hash",
            "-of", "json", "-i"])
        .arg(path).output().context("start ffprobe stream inspection")?;
    ensure!(
        result.status.success() && result.stderr.is_empty(),
        "ffprobe stream inspection failed ({}): {}",
        result.status,
        String::from_utf8_lossy(&result.stderr).trim()
    );
    let document: Value = serde_json::from_slice(&result.stdout)?;
    let streams = document["streams"]
        .as_array()
        .context("no streams in ffprobe result")?;
    ensure!(
        streams.len() == 1 + usize::from(audio_ms.is_some()),
        "unexpected number of streams"
    );
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .context("missing video stream")?;
    ensure!(
        video["codec_name"] == options.codec.cli_value(),
        "unexpected video codec"
    );
    ensure!(
        video["width"].as_u64() == Some(options.width.into())
            && video["height"].as_u64() == Some(options.height.into()),
        "unexpected video dimensions"
    );
    let pixel_format = video["pix_fmt"]
        .as_str()
        .context("missing pixel format")?
        .to_owned();
    ensure!(
        video["extradata_size"]
            .as_u64()
            .is_some_and(|size| size > 0)
            && video["extradata_hash"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
        "missing decoder configuration"
    );
    let video_clock = TimeBase::from_stream(video)?;
    ensure!(
        video_clock.ticks(1, options.fps)? > 0,
        "time base cannot represent requested frame rate"
    );
    if let Some(duration) = video["duration_ts"].as_i64() {
        ensure!(
            duration == video_clock.ticks(frames, options.fps)?,
            "incorrect video duration ({duration} ticks)"
        );
    }
    // Only length-independent properties go into the segment compatibility identifier.
    // Extradata includes AVC SPS/PPS or HEVC VPS/SPS/PPS. Concat also requires the same clock.
    let mut signature = video.as_object().context("invalid video stream")?.clone();
    signature.remove("duration_ts");
    let audio = if let Some(duration_ms) = audio_ms {
        let stream = streams
            .iter()
            .find(|s| s["codec_type"] == "audio")
            .context("missing audio stream")?;
        ensure!(stream["codec_name"] == "aac", "unexpected audio codec");
        let clock = TimeBase::from_stream(stream)?;
        let sample_rate: u32 = stream["sample_rate"]
            .as_str()
            .context("missing sample rate")?
            .parse()?;
        ensure!(sample_rate > 0, "invalid sample rate");
        if let Some(duration) = stream["duration_ts"].as_i64() {
            // The source duration and the MP4 edit list may be rounded to milliseconds.
            ensure!(
                duration.abs_diff(clock.ticks(duration_ms, 1_000)?)
                    <= clock.ticks(1, 1_000)? as u64 + 1,
                "audio duration differs from requested range"
            );
        }
        Some(Audio {
            clock,
            sample_rate,
            duration_ms,
        })
    } else {
        None
    };
    Ok(Streams {
        video_clock,
        pixel_format,
        signature: serde_json::to_string(&signature)?,
        audio,
    })
}

fn field<'a>(line: &'a str, name: &str) -> Result<&'a str> {
    line.split('|')
        .filter_map(|s| s.split_once('='))
        .find_map(|(k, v)| (k == name).then_some(v))
        .with_context(|| format!("missing ffprobe field {name}"))
}

fn decoded_frames(
    reader: impl BufRead,
    streams: &Streams,
    options: &RenderOptions,
    frames: u64,
) -> Result<()> {
    let mut reader = reader;
    let mut line = String::new();
    let mut video_frames = 0u64;
    let mut last_dts: Option<i64> = None;
    let mut audio_samples = 0u64;
    let mut last_audio_frame = 0u64;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let record = line.trim_end();
        if record.starts_with("packet|") && field(record, "codec_type")? == "video" {
            let dts = field(record, "dts")?
                .parse::<i64>()
                .context("missing video decode timestamp")?;
            if let Some(prev) = last_dts {
                ensure!(prev < dts, "non-increasing video decode timestamp");
            } else {
                ensure!(
                    field(record, "flags")?.contains('K'),
                    "first video packet is not a keyframe"
                );
            }
            last_dts = Some(dts);
        } else if record.starts_with("frame|") {
            let pts: i64 = field(record, "pts")?
                .parse()
                .context("missing presentation timestamp")?;
            match field(record, "media_type")? {
                "video" => {
                    ensure!(video_frames < frames, "too many video frames");
                    ensure!(
                        video_frames != 0 || field(record, "key_frame")? == "1",
                        "first decoded frame is not a keyframe"
                    );
                    ensure!(
                        pts == streams.video_clock.ticks(video_frames, options.fps)?,
                        "wrong presentation timestamp {pts} at frame {video_frames}"
                    );
                    ensure!(
                        field(record, "width")?.parse::<u32>()? == options.width
                            && field(record, "height")?.parse::<u32>()? == options.height
                            && field(record, "pix_fmt")? == streams.pixel_format,
                        "decoded video format changes at frame {video_frames}"
                    );
                    video_frames += 1;
                }
                "audio" => {
                    let a = streams.audio.as_ref().context("unexpected audio frame")?;
                    ensure!(
                        pts.abs_diff(a.clock.ticks(audio_samples, a.sample_rate)?) <= 1,
                        "gap or offset at decoded audio sample {audio_samples}"
                    );
                    last_audio_frame = field(record, "nb_samples")?.parse()?;
                    ensure!(last_audio_frame > 0, "empty audio frame");
                    audio_samples = audio_samples
                        .checked_add(last_audio_frame)
                        .context("audio sample count overflow")?;
                }
                other => bail!("unexpected decoded media type {other}"),
            }
        }
    }
    ensure!(
        last_dts.is_some() && video_frames == frames,
        "decoded {video_frames} frames, expected {frames}"
    );
    if let Some(a) = &streams.audio {
        // Decoders can emit a full final AAC block, including padding after the edit list.
        let expected = u128::from(a.duration_ms) * u128::from(a.sample_rate) / 1_000;
        let ms = u128::from(a.sample_rate).div_ceil(1_000);
        let decoded = u128::from(audio_samples);
        ensure!(
            decoded > 0
                && decoded + ms >= expected
                && decoded <= expected + ms + u128::from(last_audio_frame),
            "decoded audio does not cover the requested range"
        );
    }
    Ok(())
}

fn validate(
    path: &Path,
    options: &RenderOptions,
    frames: u64,
    audio_ms: Option<u64>,
) -> Result<String> {
    ensure!(
        frames > 0 && options.fps > 0,
        "invalid video frame count or rate"
    );
    let streams = streams(path, options, frames, audio_ms)?;
    // Stream packet and decoded frame records so validation memory is independent of movie length.
    // ffprobe can exit successfully after a decoder error, so also check its error log.
    let mut errors = tempfile::tempfile().context("create decoder error log")?;
    let mut child = Command::new("ffprobe").args(["-v", "error", "-err_detect", "explode", "-show_frames", "-show_packets",
        "-show_entries", "frame=media_type,key_frame,pts,width,height,pix_fmt,nb_samples:packet=codec_type,dts,flags:frame_side_data=:packet_side_data=",
        "-of", "compact=p=1:nk=0", "-i"])
        .arg(path).stdin(Stdio::null()).stderr(errors.try_clone()?).stdout(Stdio::piped()).spawn().context("start independent decoder")?;
    let output = child.stdout.take().context("open decoder stdout")?;
    let parsed = decoded_frames(BufReader::new(output), &streams, options, frames);
    if parsed.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().context("wait for decoder")?;
    errors.rewind()?;
    let mut diagnostic = Vec::new();
    errors.take(4096).read_to_end(&mut diagnostic)?;
    ensure!(
        diagnostic.is_empty(),
        "decoder error: {}",
        String::from_utf8_lossy(&diagnostic).trim()
    );
    parsed?;
    ensure!(status.success(), "decoder exited with {status}");
    Ok(streams.signature)
}

pub(crate) fn validate_segment(
    path: &Path,
    options: &RenderOptions,
    expected_frames: u64,
) -> Result<String> {
    validate(path, options, expected_frames, None)
        .with_context(|| format!("validate segment {}", path.display()))
}

fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1_000, ms % 1_000)
}

fn concat_list(segments: &[(PathBuf, u64)], fps: u32) -> Result<NamedTempFile> {
    let dir = segments
        .first()
        .and_then(|s| s.0.parent())
        .context("no video segments")?;
    let mut list = Builder::new()
        .prefix(".goosy-concat-")
        .suffix(".ffconcat")
        .tempfile_in(dir)?;
    let mut writer = std::io::BufWriter::new(list.as_file_mut());
    writeln!(writer, "ffconcat version 1.0")?;
    let mut frames = 0u64;
    let mut start_us = 0u128;
    for (path, length) in segments {
        ensure!(
            path.parent() == Some(dir),
            "segments must share a directory"
        );
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("invalid segment filename")?;
        // Workspace segment names are generated locally. No user path is interpolated into ffconcat.
        ensure!(
            name.bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')),
            "unsafe segment filename"
        );
        frames = frames
            .checked_add(*length)
            .context("frame count overflow")?;
        // The concat demuxer uses AV_TIME_BASE (microseconds). Round each absolute boundary,
        // then subtract: neither container millisecond rounding nor per-segment error accumulates.
        let end_us = (u128::from(frames) * 1_000_000 + u128::from(fps / 2)) / u128::from(fps);
        let duration = end_us - start_us;
        writeln!(
            writer,
            "file {name}\nduration {}.{:06}",
            duration / 1_000_000,
            duration % 1_000_000
        )?;
        start_us = end_us;
    }
    writer.flush()?;
    drop(writer);
    Ok(list)
}

const STAGE_PREFIX: &str = ".goosy-stage-";

fn hyphenated_uuid(text: &str) -> Option<Uuid> {
    if text.len() != 36 { return None; }
    Uuid::parse_str(text).ok()
}

pub(crate) fn private_stage_owner(name: &OsStr) -> Option<Uuid> {
    let ids = name.to_str()?.strip_prefix(STAGE_PREFIX)?;
    let (task, attempt) = ids.split_at_checked(36)?;
    hyphenated_uuid(attempt.strip_prefix('-')?)?;
    hyphenated_uuid(task)
}

pub(crate) fn private_stage_dir_name(name: &OsStr) -> bool {
    private_stage_owner(name).is_some()
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StageRecord {
    task: Uuid,
    attempt: Uuid,
}

impl StageRecord {
    fn token(&self) -> [u8; 32] {
        let mut token = [0; 32];
        token[..16].copy_from_slice(self.task.as_bytes());
        token[16..].copy_from_slice(self.attempt.as_bytes());
        token
    }
}

fn metadata_if_present(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.is_symlink()
}

/// Only the workspace record and its matching external owner token authorize cleanup.
/// A crash before the token is durable can leave an empty, unclaimed directory.
struct MuxStage {
    record: StageRecord,
    record_path: PathBuf,
    dir: PathBuf,
    movie: PathBuf,
}

impl MuxStage {
    fn paths(records: &Path, output: &Path, record: StageRecord) -> Result<Self> {
        let extension = output.extension().filter(|ext| !ext.is_empty())
            .context("output has no container extension")?;
        let dir = output_parent(output).join(format!("{STAGE_PREFIX}{}-{}", record.task, record.attempt));
        let movie = dir.join("movie").with_extension(extension);
        let record_path = records.join(format!("stage-{}.json", record.attempt));
        Ok(Self { record, record_path, dir, movie })
    }

    fn allocate(work: &Workspace) -> Result<Self> {
        let records = work.stage_records();
        workspace::directory(&records)?;
        loop {
            let record = StageRecord { task: work.id(), attempt: Uuid::new_v4() };
            let stage = Self::paths(&records, &work.options().output, record)?;
            if metadata_if_present(&stage.record_path)?.is_some() {
                continue;
            }
            workspace::atomic_write(&stage.record_path, &serde_json::to_vec(&stage.record)?)?;
            #[cfg(unix)]
            let created = {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o700).create(&stage.dir)
            };
            #[cfg(not(unix))]
            let created = fs::create_dir(&stage.dir);
            match created {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    fs::remove_file(&stage.record_path)?;
                    workspace::sync_dir(&records)?;
                    continue;
                }
                result => result.with_context(|| format!("create private mux directory {}", stage.dir.display()))?,
            }
            let mut new_file = fs::OpenOptions::new();
            new_file.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                new_file.mode(0o600);
            }
            let mut owner = new_file.open(stage.dir.join("owner"))?;
            owner.write_all(&stage.record.token())?;
            owner.sync_all()?;
            workspace::sync_dir(&stage.dir)?;
            workspace::sync_dir(output_parent(&stage.dir))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                new_file.mode(0o666);
            }
            new_file.open(&stage.movie)?;
            return Ok(stage);
        }
    }

    fn owns_directory(&self, owner: &Path) -> Result<bool> {
        let Some(meta) = metadata_if_present(&self.dir)? else { return Ok(false) };
        if !meta.is_dir() || is_link(&meta) {
            return Ok(false);
        }
        workspace::directory(&self.dir)?;
        let Some(meta) = metadata_if_present(owner)? else { return Ok(false) };
        if !meta.is_file() || is_link(&meta) || meta.len() != 32 {
            return Ok(false);
        }
        Ok(fs::read(owner)? == self.record.token())
    }

    fn cleanup(&self, movie_moved: bool, protected: &impl Fn(&Path) -> Result<bool>) -> Result<()> {
        let parent = output_parent(&self.dir);
        let owner = self.dir.join("owner");
        if self.owns_directory(&owner)? {
            if !movie_moved && let Some(meta) = metadata_if_present(&self.movie)?
                && meta.is_file() && !is_link(&meta) && !protected(&self.movie)?
            {
                fs::remove_file(&self.movie)?;
            }
            // Persist the unlink before releasing the proof of ownership.
            workspace::sync_dir(&self.dir)?;
            if !protected(&owner)? {
                fs::remove_file(owner)?;
                workspace::sync_dir(&self.dir)?;
            }
            if !protected(&self.dir)? {
                match fs::remove_dir(&self.dir) {
                    Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => (),
                    result => result?,
                }
            }
        }
        // NotFound above could also mean a disconnected volume. In that case the
        // record must survive so the original directory can be inspected on retry.
        fs::read_dir(parent).with_context(|| format!("inspect output directory {}", parent.display()))?;
        workspace::sync_dir(parent)?;
        if !protected(&self.record_path)? {
            fs::remove_file(&self.record_path)?;
            workspace::sync_dir(output_parent(&self.record_path))?;
        }
        Ok(())
    }
}

pub(crate) fn clean_staging(work: &Workspace) -> Result<()> {
    clean_stage_records(&work.stage_records(), work.id(), &work.options().output, |_| Ok(false))
}

/// The caller holds this task's workspace lock during record enumeration and cleanup.
pub(crate) fn clean_stage_records(
    records: &Path,
    task: Uuid,
    output: &Path,
    protected: impl Fn(&Path) -> Result<bool>,
) -> Result<()> {
    workspace::directory(records)?;
    for entry in fs::read_dir(records)? {
        let entry = entry?;
        let name = entry.file_name();
        let attempt = name.to_str().and_then(|s| s.strip_prefix("stage-"))
            .and_then(|s| s.strip_suffix(".json")).and_then(hyphenated_uuid);
        let Some(attempt) = attempt else { continue };
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        ensure!(meta.is_file() && !is_link(&meta), "invalid staging record {}", path.display());
        let record: StageRecord = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("read staging record {}", path.display()))?;
        ensure!(record.task == task && record.attempt == attempt, "staging record identity mismatch: {}", path.display());
        let stage = MuxStage::paths(records, output, record)?;
        ensure!(stage.record_path == path, "noncanonical staging record {}", path.display());
        stage.cleanup(false, &protected).with_context(|| format!("clean mux stage {}", stage.dir.display()))?;
    }
    Ok(())
}

fn mux(
    stage: &Path,
    segments: &[(PathBuf, u64)],
    options: &RenderOptions,
    start_ms: u64,
    duration_ms: u64,
) -> Result<()> {
    let list = concat_list(segments, options.fps)?;
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-nostdin",
        "-hide_banner",
        "-v",
        "error",
        "-xerror",
        "-y",
        "-f",
        "concat",
        "-safe",
        "1",
        "-auto_convert",
        "0",
        "-i",
    ])
    .arg(list.path());
    if !options.no_audio {
        cmd.arg("-ss")
            .arg(seconds(start_ms))
            .arg("-t")
            .arg(seconds(duration_ms))
            .arg("-i")
            .arg(&options.song);
    }
    cmd.args([
        "-map",
        "0:v:0",
        "-c:v",
        "copy",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
    ]);
    if !options.no_audio {
        // The task uses container duration, which can include discarded codec padding at EOF.
        // Fill the tail with silence up to the exact end of the selected range.
        cmd.args(["-map", "1:a:0", "-af"])
            .arg(format!(
                "asetpts=PTS-STARTPTS,apad,atrim=duration={}",
                seconds(duration_ms)
            ))
            .args(["-c:a", "aac", "-b:a", "192k"]);
    }
    if options
        .output
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| {
            ["mp4", "mov", "m4v"]
                .iter()
                .any(|e| s.eq_ignore_ascii_case(e))
        })
    {
        cmd.args(["-movflags", "+faststart", "-video_track_timescale"])
            .arg(options.fps.to_string());
    }
    let out = cmd
        .arg(stage)
        .output()
        .context("start final FFmpeg mux")?;
    ensure!(
        out.status.success() && out.stderr.is_empty(),
        "final FFmpeg mux failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

fn output_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) fn finalize(
    work: &mut Workspace,
    start_ms: u64,
    duration_ms: u64,
    total: u64,
) -> Result<()> {
    if work.published_size()?.is_some() {
        work.clean_published_staging();
        return Ok(());
    }
    clean_staging(work)?;
    let destination = &work.options().output;
    match std::fs::symlink_metadata(destination) {
        Ok(_) => bail!("output already exists: {}", destination.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => {
            return Err(e).with_context(|| format!("inspect output {}", destination.display()));
        }
    }
    ensure!(
        work.range() == Some((start_ms, duration_ms, total))
            && work.committed() == total
            && total > 0,
        "incomplete committed render range"
    );
    work.verify_inputs()?;
    let segments = work.verified_segments()?;
    let options = work.options();
    let encoding = work
        .encoding()
        .context("missing committed stream configuration")?;
    let mut count = 0u64;
    for (path, frames) in &segments {
        ensure!(
            validate_segment(path, options, *frames)? == encoding,
            "segment stream configuration changed: {}",
            path.display()
        );
        count = count.checked_add(*frames).context("frame count overflow")?;
    }
    ensure!(count == total, "wrong number of committed frames");
    let stage = MuxStage::allocate(work)?;
    mux(&stage.movie, &segments, options, start_ms, duration_ms)?;
    let final_signature = validate(
        &stage.movie,
        options,
        total,
        (!options.no_audio).then_some(duration_ms),
    )
    .context("validate final output before publication")?;
    let original: Value = serde_json::from_str(encoding)?;
    let remuxed: Value = serde_json::from_str(&final_signature)?;
    ensure!(
        original["extradata_hash"] == remuxed["extradata_hash"],
        "remux changed video decoder configuration"
    );
    work.verify_inputs()?;
    work.prepare_publication(&stage.movie)?;
    let output = &work.options().output;
    crate::workspace::publish_noclobber(&stage.movie, output).with_context(|| {
        format!(
            "publish final output without overwriting {}",
            output.display()
        )
    })?;
    if let Err(error) = stage.cleanup(true, &|_| Ok(false)) {
        eprintln!("clean staging after publication for {}: {error:#}", work.id());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
