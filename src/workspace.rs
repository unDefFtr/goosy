//! Crash-safe storage for a single render. The manifest rename commits a segment.
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::RenderOptions;
use crate::layout::LayoutCheckpoint;

pub const MANIFEST_VERSION: u32 = 1;
// Bump for changes to frame timing, rendering, checkpoint semantics or segment policy.
const RENDERER: &str = concat!("goosy-segment-v1/", env!("CARGO_PKG_VERSION"));
const OWNED_DIRS: [&str; 4] = ["segments", "checkpoints", "audio", "output"];
const ARCHIVED_MANIFEST: &str = "manifest.completed.json";

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    bytes: u64,
    blake3: String,
}

impl Fingerprint {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.len() as u64,
            blake3: blake3::hash(bytes).to_hex().to_string(),
        }
    }

    pub(crate) fn read(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        ensure!(
            file.metadata()?.is_file(),
            "expected a regular file: {}",
            path.display()
        );
        Self::hash_file(&file, path)
    }

    fn hash_file(file: &File, path: &Path) -> Result<Self> {
        let mut hasher = blake3::Hasher::new();
        hasher
            .update_reader(file)
            .with_context(|| format!("hash {}", path.display()))?;
        Ok(Self {
            bytes: hasher.count(),
            blake3: hasher.finalize().to_hex().to_string(),
        })
    }

    fn matches(&self, path: &Path) -> Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_file() || meta.len() != self.bytes => Ok(false),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err).with_context(|| format!("inspect {}", path.display())),
            Ok(_) => Ok(Self::read(path)? == *self),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Asset {
    path: PathBuf,
    // Some historical configurations have no recorded fingerprint. Resume still requires one.
    content: Option<Fingerprint>,
}

#[derive(Serialize, Deserialize)]
struct Config {
    schema_version: u32,
    task: Uuid,
    renderer: String,
    platform: String,
    working_directory: PathBuf,
    options: RenderOptions,
    segment_seconds: u32,
    #[serde(default)]
    inputs: Vec<Asset>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
struct Range {
    start_ms: u64,
    duration_ms: u64,
    total: u64,
}

#[derive(Serialize, Deserialize)]
struct Segment {
    start: u64,
    end: u64,
    file: String,
    content: Fingerprint,
    encoding: String,
    checkpoint_file: String,
    checkpoint: Fingerprint,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
    task: Uuid,
    config_blake3: String,
    range: Option<Range>,
    render_backend: Option<String>,
    segments: Vec<Segment>,
    // Written before publishing the validated output. Only a matching destination is complete.
    publication: Option<Fingerprint>,
    intermediates_removed: bool,
}

pub struct Workspace {
    dir: PathBuf,
    config: Config,
    options: RenderOptions,
    manifest: Manifest,
    archived: bool,
    attempt: Uuid,
    _lock: File,
}

/// Keep both handles open across the SQLite commit. The workspace lock guards the
/// manifest, and the output handle prevents its file identity from being recycled.
pub(crate) struct VerifiedPublication {
    work: Workspace,
    file: PublishedFile,
}

pub(crate) enum RecoveredWorkspace {
    Published(VerifiedPublication),
    Unpublished(Workspace),
}

struct PublishedFile {
    _handle: File,
    stamp: FileStamp,
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    identity: (u64, u64),
    size: u64,
    modified: std::time::SystemTime,
}

#[cfg(unix)]
fn unix_identity(meta: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(unix)]
pub(crate) fn file_identity(file: &File) -> Result<(u64, u64)> {
    Ok(unix_identity(&file.metadata()?))
}

#[cfg(windows)]
pub(crate) fn file_identity(file: &File) -> Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }?;
    let index = u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow);
    Ok((u64::from(info.dwVolumeSerialNumber), index))
}

impl FileStamp {
    fn read(file: &File) -> Result<Self> {
        let meta = file.metadata()?;
        ensure!(meta.is_file(), "published output is not a regular file");
        #[cfg(unix)]
        let identity = unix_identity(&meta);
        #[cfg(windows)]
        let identity = file_identity(file)?;
        Ok(Self { identity, size: meta.len(), modified: meta.modified()? })
    }

    fn still_at(&self, path: &Path) -> Result<bool> {
        Ok(fs::symlink_metadata(path)?.is_file() && *self == Self::read(&File::open(path)?)?)
    }
}

impl VerifiedPublication {
    pub(crate) fn total(&self) -> u64 { self.work.total() }

    pub(crate) fn size(&self) -> u64 { self.file.stamp.size }

    pub(crate) fn cleanup(mut self) -> Result<()> {
        if !self.file.stamp.still_at(&self.work.options.output)? {
            ensure!(self.work.verified_file()?.is_some(), "cannot clean before output publication");
        }
        self.work.archive_verified()
    }
}

#[cfg(test)]
pub(crate) mod publication_probe {
    use std::{collections::HashMap, path::{Path, PathBuf}, sync::{LazyLock, Mutex, mpsc}, time::Duration};

    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Phase { Hash, Archive }

    type Event = (Phase, mpsc::Sender<()>);
    static PROBES: LazyLock<Mutex<HashMap<PathBuf, mpsc::Sender<Event>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(crate) struct Probe(PathBuf);

    impl Probe {
        pub(crate) fn install(path: &Path) -> (Self, mpsc::Receiver<Event>) {
            let (send, receive) = mpsc::channel();
            PROBES.lock().unwrap().insert(path.to_owned(), send);
            (Self(path.to_owned()), receive)
        }
    }

    impl Drop for Probe {
        fn drop(&mut self) { PROBES.lock().unwrap().remove(&self.0); }
    }

    pub(super) fn visit(path: &Path, phase: Phase) {
        let hook = PROBES.lock().unwrap().get(path).cloned();
        if let Some(hook) = hook {
            let (go, wait) = mpsc::channel();
            hook.send((phase, go)).unwrap();
            wait.recv_timeout(Duration::from_secs(5)).expect("publication probe was not released");
        }
    }
}

fn segment_name(start: u64) -> String {
    format!("{start:020}.mp4")
}
fn checkpoint_name(end: u64) -> String {
    format!("{end:020}.json")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetRole {
    Audio,
    Lyrics,
    Background,
    Cover,
}

impl AssetRole {
    // Preserve the on-disk order of the version 1 config's input list.
    pub const ALL: [Self; 4] = [Self::Audio, Self::Lyrics, Self::Background, Self::Cover];

    pub(crate) fn path(self, options: &RenderOptions) -> Option<&Path> {
        match self {
            Self::Audio => Some(&options.song),
            Self::Lyrics => options.lyrics.as_deref(),
            Self::Background => options.background.as_deref(),
            Self::Cover => options.cover.as_deref(),
        }
    }
}

pub(crate) type InputFingerprints = [Option<Fingerprint>; AssetRole::ALL.len()];

fn input_assets(options: &RenderOptions) -> impl Iterator<Item = (AssetRole, &Path)> {
    AssetRole::ALL.into_iter().filter_map(move |role| role.path(options).map(|path| (role, path)))
}

fn resolve(options: &RenderOptions, cwd: &Path) -> Result<RenderOptions> {
    ensure!(cwd.is_absolute(), "invalid workspace creation directory");
    let mut resolved = options.clone();
    for path in std::iter::once(&mut resolved.song)
        .chain(resolved.lyrics.iter_mut())
        .chain(resolved.background.iter_mut())
        .chain(resolved.cover.iter_mut())
        .chain(std::iter::once(&mut resolved.output))
    {
        ensure!(!path.as_os_str().is_empty(), "empty render path");
        if !path.is_absolute() {
            *path = cwd.join(&*path);
        }
        ensure!(
            path.is_absolute(),
            "cannot resolve render path {}",
            path.display()
        );
    }
    Ok(resolved)
}

fn workspace_id(dir: &Path) -> Result<Uuid> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("workspace needs a task UUID directory name")?;
    Uuid::parse_str(name).context("invalid task UUID directory name")
}

pub(crate) fn directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)
        .with_context(|| format!("inspect directory {}", path.display()))?;
    ensure!(
        meta.is_dir(),
        "invalid workspace directory {}",
        path.display()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            meta.file_attributes() & 0x400 == 0,
            "unexpected reparse point {}",
            path.display()
        );
    }
    Ok(())
}

pub(crate) fn lock(dir: &Path, create: bool) -> Result<File> {
    directory(dir)?;
    let path = dir.join("worker.lock");
    match fs::symlink_metadata(&path) {
        Ok(meta) => ensure!(meta.is_file(), "invalid workspace lock file"),
        Err(err) if create && err.kind() == io::ErrorKind::NotFound => (),
        Err(err) => return Err(err).context("missing or inaccessible workspace lock"),
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(&path)?;
    lock.try_lock().context("acquire workspace worker lock")?;
    Ok(lock)
}

fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

impl Workspace {
    pub fn create(dir: &Path, options: &RenderOptions, segment_seconds: u32) -> Result<Self> {
        Self::create_checked(dir, options, segment_seconds, None)
    }

    pub(crate) fn create_checked(
        dir: &Path,
        options: &RenderOptions,
        segment_seconds: u32,
        expected: Option<&InputFingerprints>,
    ) -> Result<Self> {
        let task = workspace_id(dir)?;
        ensure!(
            options.fps > 0 && segment_seconds > 0,
            "frame rate and segment length must be positive"
        );
        let dir = std::path::absolute(dir)?;
        fs::create_dir_all(&dir).with_context(|| format!("create workspace {}", dir.display()))?;
        let guard = lock(&dir, true)?;
        for entry in fs::read_dir(&dir)? {
            ensure!(
                entry?.file_name() == "worker.lock",
                "workspace already contains data: {}",
                dir.display()
            );
        }
        let cwd = std::env::current_dir()?;
        let resolved = resolve(options, &cwd)?;
        let inputs = input_assets(&resolved)
            .map(|(role, path)| {
                let content = Fingerprint::read(path)?;
                if let Some(expected) = expected {
                    ensure!(
                        expected[role as usize].as_ref() == Some(&content),
                        "创建任务时素材发生变化，请重新检查：{}", path.display()
                    );
                }
                Ok(Asset { path: path.to_owned(), content: Some(content) })
            })
            .collect::<Result<Vec<_>>>()?;
        let config = Config {
            schema_version: MANIFEST_VERSION,
            renderer: RENDERER.into(),
            task,
            platform: platform(),
            working_directory: cwd,
            options: options.clone(),
            segment_seconds,
            inputs,
        };
        let bytes = serde_json::to_vec_pretty(&config)?;
        let manifest = Manifest {
            schema_version: MANIFEST_VERSION,
            task,
            config_blake3: blake3::hash(&bytes).to_hex().to_string(),
            range: None,
            render_backend: None,
            segments: Vec::new(),
            publication: None,
            intermediates_removed: false,
        };
        for child in OWNED_DIRS {
            fs::create_dir(dir.join(child))?;
        }
        atomic_write(&dir.join("config.json"), &bytes)?;
        let work = Self {
            dir,
            config,
            options: resolved,
            manifest,
            archived: false,
            attempt: Uuid::new_v4(),
            _lock: guard,
        };
        work.save()?;
        sync_dir(
            work.dir
                .parent()
                .context("missing tasks parent directory")?,
        )?;
        Ok(work)
    }

    fn load(dir: &Path) -> Result<Self> {
        let task = workspace_id(dir)?;
        let dir = std::path::absolute(dir)?;
        let guard = lock(&dir, false)?;
        for name in OWNED_DIRS {
            directory(&dir.join(name))?;
        }
        let active = dir.join("manifest.json");
        let archived = !active.try_exists()?;
        let bytes = if archived {
            regular_bytes(&dir.join(ARCHIVED_MANIFEST))?
        } else {
            regular_bytes(&active)?
        };
        let manifest: Manifest =
            serde_json::from_slice(&bytes).context("read workspace manifest")?;
        ensure!(!archived || manifest.intermediates_removed, "incomplete archived manifest");
        ensure!(
            manifest.schema_version == MANIFEST_VERSION && manifest.task == task,
            "incompatible manifest schema or task identity"
        );
        let bytes = regular_bytes(&dir.join("config.json"))?;
        ensure!(
            manifest.config_blake3 == blake3::hash(&bytes).to_hex().as_str(),
            "immutable workspace configuration hash mismatch"
        );
        let config: Config =
            serde_json::from_slice(&bytes).context("read immutable render configuration")?;
        ensure!(
            config.task == task
                && config.schema_version == MANIFEST_VERSION
                && config.renderer == RENDERER
                && config.platform == platform(),
            "incompatible workspace configuration, renderer or platform"
        );
        ensure!(
            config.segment_seconds > 0 && config.options.fps > 0,
            "invalid saved segmentation policy"
        );
        let options = resolve(&config.options, &config.working_directory)?;
        ensure!(
            input_assets(&options).map(|(_, path)| path).eq(config.inputs.iter().map(|a| a.path.as_path()))
                && config.inputs.iter().all(|a| a.content.is_some()),
            "workspace input list mismatch"
        );
        ensure!(
            manifest.segments.is_empty()
                || manifest
                    .render_backend
                    .as_deref()
                    .is_some_and(|s| !s.is_empty()),
            "committed segments lack their renderer backend"
        );
        if let Some(range) = manifest.range {
            validate_range(range, options.fps)?;
        }
        if manifest.publication.is_some() || manifest.intermediates_removed {
            let mut start = 0;
            let range = manifest.range.context("completion without a task range")?;
            for s in &manifest.segments {
                ensure!(
                    s.start == start && s.start < s.end && s.end <= range.total,
                    "invalid completion segment ranges"
                );
                start = s.end;
            }
            ensure!(
                start == range.total && manifest.publication.is_some(),
                "incomplete publication record"
            );
        }
        Ok(Self {
            dir,
            config,
            options,
            manifest,
            archived,
            attempt: Uuid::new_v4(),
            _lock: guard,
        })
    }

    pub fn open(dir: &Path) -> Result<Self> {
        match Self::inspect_recovery(dir)? {
            RecoveredWorkspace::Published(publication) => Ok(publication.work),
            RecoveredWorkspace::Unpublished(work) => Ok(work),
        }
    }

    pub(crate) fn inspect_recovery(dir: &Path) -> Result<RecoveredWorkspace> {
        let mut work = Self::load(dir)?;
        if let Some(file) = work.verified_file()? {
            work.clean_published_staging();
            return Ok(RecoveredWorkspace::Published(VerifiedPublication { work, file }));
        }
        ensure!(
            !work.manifest.intermediates_removed,
            "published output is missing and intermediates were cleaned"
        );
        work.verify_inputs()?;
        let mut valid = 0;
        let mut start = 0;
        for segment in &work.manifest.segments {
            if !work.valid_entry(segment, start)
                || !segment.content.matches(&work.segment_file(start))?
                || work.read_checkpoint(segment)?.is_none()
            {
                break;
            }
            start = segment.end;
            valid += 1;
        }
        if valid != work.manifest.segments.len() {
            work.manifest.segments.truncate(valid);
            work.manifest.publication = None;
            work.save()?;
        }
        work.clean_orphans()?;
        Ok(RecoveredWorkspace::Unpublished(work))
    }

    pub fn verify_inputs(&self) -> Result<()> {
        for asset in &self.config.inputs {
            ensure!(
                asset.content.as_ref() == Some(&Fingerprint::read(&asset.path)?),
                "input asset changed: {}",
                asset.path.display()
            );
        }
        Ok(())
    }

    /// Read only immutable configuration and the atomically published time range. A redo can
    /// start while the source worker owns its lock, including after its segments are archived.
    pub(crate) fn redo_inputs(dir: &Path, options: &mut RenderOptions) -> Result<InputFingerprints> {
        #[derive(Deserialize)]
        struct Provenance {
            schema_version: u32,
            task: Uuid,
            config_blake3: String,
            range: Option<Range>,
        }
        let mut original = InputFingerprints::default();
        if !dir.try_exists()? {
            return Ok(original);
        }
        directory(dir)?;
        let manifest = match optional_regular_bytes(&dir.join("manifest.json"))? {
            Some(bytes) => bytes,
            None => match optional_regular_bytes(&dir.join(ARCHIVED_MANIFEST))? {
                Some(bytes) => bytes,
                None => return Ok(original),
            },
        };
        let task = workspace_id(dir)?;
        let manifest: Provenance = serde_json::from_slice(&manifest).context("读取历史任务清单")?;
        ensure!(manifest.schema_version == MANIFEST_VERSION && manifest.task == task, "历史任务清单不匹配");
        if let Some(bytes) = optional_regular_bytes(&dir.join("config.json"))? {
            ensure!(manifest.config_blake3 == blake3::hash(&bytes).to_hex().as_str(), "历史配置校验失败");
            let config: Config = serde_json::from_slice(&bytes).context("读取历史任务配置")?;
            ensure!(config.task == task && config.schema_version == MANIFEST_VERSION, "历史任务配置不匹配");
            let resolved = resolve(&config.options, &config.working_directory)?;
            ensure!(resolved.output == options.output, "工作目录与历史输出路径不匹配");
            let mut assets = config.inputs.into_iter().peekable();
            let sparse = assets.len() < input_assets(&resolved).count();
            for (role, path) in input_assets(&resolved) {
                if let Some(asset) = assets.next_if(|asset| asset.path == path) {
                    // Older lists identify inputs by path. If entries are missing, a repeated
                    // path cannot establish which role was fingerprinted.
                    if !sparse || !input_assets(&resolved).any(|(other, p)| other != role && p == path) {
                        original[role as usize] = asset.content;
                    }
                }
            }
            ensure!(assets.next().is_none(), "历史输入列表不匹配");
            *options = resolved;
        }
        if let Some(range) = manifest.range {
            validate_range(range, options.fps)?;
            if options.sample_start_ms.is_some() || options.sample_duration_ms.is_some() {
                options.sample_start_ms = Some(range.start_ms);
                options.sample_duration_ms = Some(range.duration_ms);
            }
        }
        Ok(original)
    }

    pub(crate) fn id(&self) -> Uuid {
        self.config.task
    }
    pub(crate) fn stage_records(&self) -> PathBuf {
        self.dir.join("output")
    }
    pub fn options(&self) -> &RenderOptions {
        &self.options
    }
    pub fn segment_frames(&self) -> u64 {
        u64::from(self.config.segment_seconds) * u64::from(self.options.fps)
    }
    pub fn committed(&self) -> u64 {
        self.manifest.segments.last().map_or(0, |s| s.end)
    }
    pub fn total(&self) -> u64 {
        self.manifest.range.map_or(0, |r| r.total)
    }
    pub fn range(&self) -> Option<(u64, u64, u64)> {
        self.manifest
            .range
            .map(|r| (r.start_ms, r.duration_ms, r.total))
    }
    pub fn encoding(&self) -> Option<&str> {
        self.manifest.segments.first().map(|s| s.encoding.as_str())
    }

    pub fn set_render_backend(&mut self, name: &str) -> Result<()> {
        ensure!(!name.is_empty(), "empty renderer backend identifier");
        if let Some(saved) = &self.manifest.render_backend {
            ensure!(
                saved == name,
                "renderer backend changed from {saved} to {name}"
            );
            return Ok(());
        }
        self.manifest.render_backend = Some(name.to_owned());
        if let Err(error) = self.save() {
            self.manifest.render_backend = None;
            return Err(error);
        }
        Ok(())
    }

    pub fn set_range(&mut self, start_ms: u64, duration_ms: u64, total: u64) -> Result<()> {
        let range = Range {
            start_ms,
            duration_ms,
            total,
        };
        validate_range(range, self.options.fps)?;
        if let Some(saved) = self.manifest.range {
            ensure!(
                saved == range,
                "render time range differs from persisted task"
            );
            return Ok(());
        }
        self.manifest.range = Some(range);
        if let Err(error) = self.save() {
            self.manifest.range = None;
            return Err(error);
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        atomic_write(
            &self.dir.join("manifest.json"),
            &serde_json::to_vec_pretty(&self.manifest)?,
        )
    }

    pub fn segment_path(&self, start: u64) -> PathBuf {
        // A pipe encoder can outlive a killed Worker while draining its last input.
        self.dir
            .join("segments")
            .join(format!("pending-{}-{start:020}.mp4", self.attempt))
    }

    fn segment_file(&self, start: u64) -> PathBuf {
        self.dir.join("segments").join(segment_name(start))
    }
    fn checkpoint_file(&self, end: u64) -> PathBuf {
        self.dir.join("checkpoints").join(checkpoint_name(end))
    }

    fn valid_entry(&self, s: &Segment, start: u64) -> bool {
        s.start == start
            && start < s.end
            && s.end
                == start
                    .saturating_add(self.segment_frames())
                    .min(self.total())
            && s.file == segment_name(start)
            && s.checkpoint_file == checkpoint_name(s.end)
            && s.content.bytes > 0
            && !s.encoding.is_empty()
            && Some(s.encoding.as_str()) == self.encoding()
    }

    fn read_checkpoint(&self, segment: &Segment) -> Result<Option<LayoutCheckpoint>> {
        let path = self.checkpoint_file(segment.end);
        match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() || meta.len() != segment.checkpoint.bytes => {
                return Ok(None);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).context("inspect animation checkpoint"),
            Ok(_) => (),
        }
        let bytes =
            fs::read(&path).with_context(|| format!("read checkpoint {}", path.display()))?;
        if Fingerprint::from_bytes(&bytes) != segment.checkpoint {
            return Ok(None);
        }
        let cp: LayoutCheckpoint = serde_json::from_slice(&bytes)
            .context("unreadable hash-verified animation checkpoint")?;
        cp.validate_schema()?;
        Ok(Some(cp))
    }

    pub fn checkpoint(&self) -> Result<Option<LayoutCheckpoint>> {
        self.manifest
            .segments
            .last()
            .map(|s| {
                self.read_checkpoint(s)?
                    .context("committed checkpoint missing or corrupt")
            })
            .transpose()
    }

    /// The caller has finished the encoder, closed the muxer and decoded every pending frame.
    pub fn commit(
        &mut self,
        start: u64,
        end: u64,
        checkpoint: &LayoutCheckpoint,
        encoding: &str,
    ) -> Result<()> {
        ensure!(
            self.manifest.render_backend.is_some(),
            "record renderer backend before committing a segment"
        );
        ensure!(
            self.manifest.publication.is_none(),
            "workspace already finalized"
        );
        ensure!(
            start == self.committed()
                && end > start
                && end
                    == start
                        .saturating_add(self.segment_frames())
                        .min(self.total()),
            "unexpected committed frame range [{start},{end})"
        );
        ensure!(
            !encoding.is_empty() && self.encoding().is_none_or(|previous| previous == encoding),
            "incompatible segment encoding"
        );
        checkpoint.validate_schema()?;
        let pending = self.segment_path(start);
        sync_regular(&pending)?;
        let content = Fingerprint::read(&pending)?;
        ensure!(content.bytes > 0, "empty pending segment");
        rename_durable(&pending, &self.segment_file(start))?;
        let bytes = serde_json::to_vec(checkpoint)?;
        atomic_write(&self.checkpoint_file(end), &bytes)?;
        let entry = Segment {
            start,
            end,
            file: segment_name(start),
            content,
            encoding: encoding.to_owned(),
            checkpoint_file: checkpoint_name(end),
            checkpoint: Fingerprint::from_bytes(&bytes),
        };
        self.manifest.segments.push(entry);
        if let Err(error) = self.save() {
            self.manifest.segments.pop();
            return Err(error);
        }
        Ok(())
    }

    pub fn segments(&self) -> Vec<PathBuf> {
        self.manifest
            .segments
            .iter()
            .map(|s| self.segment_file(s.start))
            .collect()
    }

    pub fn verified_segments(&self) -> Result<Vec<(PathBuf, u64)>> {
        let mut start = 0;
        self.manifest
            .segments
            .iter()
            .map(|s| {
                let path = self.segment_file(s.start);
                ensure!(
                    self.valid_entry(s, start) && s.content.matches(&path)?,
                    "invalid committed segment {}",
                    path.display()
                );
                start = s.end;
                Ok((path, s.end - s.start))
            })
            .collect()
    }

    pub fn prepare_publication(&mut self, validated_stage: &Path) -> Result<()> {
        ensure!(
            self.total() > 0 && self.committed() == self.total(),
            "cannot publish an incomplete render"
        );
        sync_regular(validated_stage)?;
        let fingerprint = Fingerprint::read(validated_stage)?;
        ensure!(fingerprint.bytes > 0, "empty staged output");
        let old = self.manifest.publication.replace(fingerprint);
        if let Err(error) = self.save() {
            self.manifest.publication = old;
            return Err(error);
        }
        Ok(())
    }

    pub fn published_size(&self) -> Result<Option<u64>> {
        Ok(self.verified_file()?.map(|file| file.stamp.size))
    }

    fn verified_file(&self) -> Result<Option<PublishedFile>> {
        let Some(receipt) = &self.manifest.publication else {
            return Ok(None);
        };
        let output = &self.options.output;
        match fs::symlink_metadata(output) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("inspect publication {}", output.display()));
            }
            Ok(meta) => ensure!(meta.is_file(), "published output is not a regular file: {}", output.display()),
        }
        let file = File::open(output)?;
        let stamp = FileStamp::read(&file)?;
        #[cfg(test)]
        publication_probe::visit(output, publication_probe::Phase::Hash);
        ensure!(
            stamp.size == receipt.bytes && Fingerprint::hash_file(&file, output)? == *receipt && stamp.still_at(output)?,
            "published output differs from the verified receipt: {}",
            output.display()
        );
        sync_dir(output.parent().context("output has no parent directory")?)?;
        Ok(Some(PublishedFile { _handle: file, stamp }))
    }

    /// A receipt plus matching output closes the crash window before the history update.
    pub fn recover_published(dir: &Path) -> Result<Option<(u64, u64)>> {
        Ok(Self::verify_publication(dir)?.map(|verified| (verified.total(), verified.size())))
    }

    pub(crate) fn verify_publication(dir: &Path) -> Result<Option<VerifiedPublication>> {
        let work = Self::load(dir)?;
        let Some(file) = work.verified_file()? else { return Ok(None) };
        work.clean_published_staging();
        Ok(Some(VerifiedPublication { work, file }))
    }

    pub(crate) fn clean_published_staging(&self) {
        if let Err(error) = crate::media::clean_staging(self) {
            // A housekeeping failure cannot invalidate a verified, durable final movie.
            eprintln!(
                "clean staging after publication for {}: {error:#}",
                self.id()
            );
        }
    }

    pub fn cleanup_published(dir: &Path) -> Result<()> {
        Self::load(dir)?.cleanup_completed()
    }

    /// Called after completion has also been recorded in SQLite. Retains provenance and receipt.
    pub fn cleanup_completed(&mut self) -> Result<()> {
        if self.archived {
            return Ok(());
        }
        ensure!(
            self.published_size()?.is_some(),
            "cannot clean before output publication"
        );
        self.archive_verified()
    }

    fn archive_verified(&mut self) -> Result<()> {
        if self.archived { return Ok(()) }
        #[cfg(test)]
        publication_probe::visit(&self.options.output, publication_probe::Phase::Archive);
        if !self.manifest.intermediates_removed {
            self.manifest.intermediates_removed = true;
            self.save()?;
        }
        for s in &self.manifest.segments {
            remove_if_present(&self.segment_file(s.start))?;
            remove_if_present(&self.checkpoint_file(s.end))?;
        }
        sync_dir(&self.dir.join("segments"))?;
        sync_dir(&self.dir.join("checkpoints"))?;
        // Keep the active manifest until both directories are durably cleaned.
        rename_durable(&self.dir.join("manifest.json"), &self.dir.join(ARCHIVED_MANIFEST))?;
        self.archived = true;
        Ok(())
    }

    fn clean_orphans(&self) -> Result<()> {
        let videos: HashSet<_> = self
            .manifest
            .segments
            .iter()
            .map(|s| s.file.as_str())
            .collect();
        let checkpoints: HashSet<_> = self
            .manifest
            .segments
            .iter()
            .map(|s| s.checkpoint_file.as_str())
            .collect();
        for (subdir, suffix, keep) in [
            ("segments", ".mp4", &videos),
            ("checkpoints", ".json", &checkpoints),
        ] {
            let dir = self.dir.join(subdir);
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let owned = name.strip_suffix(suffix).is_some_and(frame_number);
                if subdir == "segments" && temporary_segment(name) {
                    if let Err(error) = remove_if_present(&entry.path()) {
                        // A surviving encoder/muxer may still hold this attempt's file on Windows.
                        #[cfg(windows)]
                        if error
                            .downcast_ref::<io::Error>()
                            .is_some_and(|e| matches!(e.raw_os_error(), Some(32 | 33)))
                        {
                            continue;
                        }
                        return Err(error);
                    }
                } else if (owned && !keep.contains(name)) || atomic_temp_name(name) {
                    remove_if_present(&entry.path())?;
                }
            }
            sync_dir(&dir)?;
        }
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.file_name().to_str().is_some_and(atomic_temp_name) {
                remove_if_present(&entry.path())?;
            }
        }
        sync_dir(&self.dir)
    }
}

pub(crate) fn temporary_segment(name: &str) -> bool {
    name.strip_prefix("pending-")
        .and_then(|s| s.strip_suffix(".mp4"))
        .and_then(|s| s.rsplit_once('-'))
        .is_some_and(|(id, frame)| {
            id.len() == 36 && Uuid::parse_str(id).is_ok() && frame_number(frame)
        })
        || name.strip_prefix(".goosy-concat-")
            .and_then(|s| s.strip_suffix(".ffconcat"))
            .is_some_and(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_alphanumeric()))
}

pub(crate) fn frame_number(name: &str) -> bool {
    name.len() == 20 && name.bytes().all(|b| b.is_ascii_digit()) && name.parse::<u64>().is_ok()
}

fn validate_range(range: Range, fps: u32) -> Result<()> {
    let expected = (u128::from(range.duration_ms) * u128::from(fps)).div_ceil(1_000);
    ensure!(
        range.duration_ms > 0
            && u128::from(range.total) == expected
            && range.start_ms.checked_add(range.duration_ms).is_some(),
        "invalid task range"
    );
    Ok(())
}

fn regular_bytes(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "invalid workspace file {}",
        path.display()
    );
    fs::read(path).with_context(|| format!("read {}", path.display()))
}

fn optional_regular_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match regular_bytes(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::NotFound) => Ok(None),
        Err(err) => Err(err),
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result.with_context(|| format!("remove workspace artifact {}", path.display())),
    }
}

fn sync_regular(path: &Path) -> Result<()> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "invalid staged file {}",
        path.display()
    );
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()
        .with_context(|| format!("flush {}", path.display()))
}

pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    // Rust uses F_FULLFSYNC for regular files on macOS. Directories use fsync(2).
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn fsync(fd: std::ffi::c_int) -> std::ffi::c_int;
        }
        let directory = File::open(dir)?;
        loop {
            if unsafe { fsync(directory.as_raw_fd()) } == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err).with_context(|| format!("sync directory {}", dir.display()));
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    File::open(dir)?
        .sync_all()
        .with_context(|| format!("sync directory {}", dir.display()))?;
    // Windows has no general directory fsync; renames use MOVEFILE_WRITE_THROUGH below.
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(windows)]
fn windows_move(from: &Path, to: &Path, replace: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;
    let wide = |p: &Path| -> Result<Vec<u16>> {
        // Verbatim absolute paths support long Windows user-data paths as std::fs does.
        let parent = p.parent().context("rename has no parent")?.canonicalize()?;
        let p = parent.join(p.file_name().context("rename has no filename")?);
        let mut name: Vec<_> = p.as_os_str().encode_wide().collect();
        ensure!(!name.contains(&0), "invalid NUL in path");
        name.push(0);
        Ok(name)
    };
    let source = wide(from)?;
    let target = wide(to)?;
    let flags = if replace {
        MOVEFILE_WRITE_THROUGH | MOVEFILE_REPLACE_EXISTING
    } else {
        MOVEFILE_WRITE_THROUGH
    };
    unsafe { MoveFileExW(PCWSTR(source.as_ptr()), PCWSTR(target.as_ptr()), flags) }
        .with_context(|| format!("publish {}", to.display()))
}

/// Move a synced file into place without replacing any existing directory entry.
pub(crate) fn publish_noclobber(from: &Path, to: &Path) -> Result<()> {
    let source = from.parent().context("source has no parent")?;
    let target = to.parent().context("output has no parent")?;
    #[cfg(windows)]
    windows_move(from, to, false)?;
    #[cfg(unix)]
    {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let needs_link = {
            use rustix::fs::{CWD, RenameFlags, renameat_with};
            use rustix::io::Errno;
            match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
                Ok(()) => false,
                // Older kernels or filesystems may not implement the exclusive flag.
                Err(Errno::NOSYS | Errno::OPNOTSUPP | Errno::INVAL) => true,
                Err(error) => return Err(io::Error::from(error))
                    .with_context(|| format!("publish {}", to.display())),
            }
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let needs_link = true;
        if needs_link {
            fs::hard_link(from, to)
                .with_context(|| format!("cannot publish atomically on output volume: {}", to.display()))?;
            fs::remove_file(from).with_context(|| format!("unlink {}", from.display()))?;
        }
    }
    sync_dir(target)?;
    if source != target {
        sync_dir(source)?;
    }
    Ok(())
}

fn rename_durable(from: &Path, to: &Path) -> Result<()> {
    #[cfg(windows)]
    windows_move(from, to, true)?;
    #[cfg(not(windows))]
    fs::rename(from, to).with_context(|| format!("publish {}", to.display()))?;
    sync_dir(to.parent().context("rename has no parent")?)
}

pub(crate) fn atomic_temp_name(name: &str) -> bool {
    name.strip_prefix(".goosy-write-")
        .is_some_and(|suffix| suffix.len() == 36 && Uuid::parse_str(suffix).is_ok())
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .context("atomic write needs a parent directory")?;
    let temp = dir.join(format!(".goosy-write-{}", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .with_context(|| format!("create temporary file for {}", path.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        rename_durable(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.with_context(|| format!("atomically write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn publish_on_selected_volume() {
        use std::sync::Barrier;
        let volume = std::env::var_os("GOOSY_TEST_OUTPUT_DIR");
        let tmp = match volume {
            Some(volume) => tempfile::tempdir_in(volume),
            None => tempfile::tempdir(),
        }.unwrap();
        let marker = tmp.path().join(".movie.mp4.goosy-lock");
        let token = tmp.path().join("reservation");
        let id = Uuid::new_v4();
        fs::write(&token, id.as_bytes()).unwrap();
        publish_noclobber(&token, &marker).unwrap();
        assert!(!token.exists());
        assert_eq!(fs::read(marker).unwrap(), id.as_bytes());

        let output = tmp.path().join("movie.mp4");
        let barrier = Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let publish = |index: u8| {
                let stage = tmp.path().join(index.to_string());
                fs::create_dir(&stage).unwrap();
                let source = stage.join("movie.mp4");
                fs::write(&source, [index]).unwrap();
                barrier.wait();
                let result = publish_noclobber(&source, &output);
                (source, result)
            };
            let first = scope.spawn(move || publish(0));
            [publish(1), first.join().unwrap()]
        });
        let bytes = fs::read(&output).unwrap();
        assert_eq!(results.iter().filter(|(_, r)| r.is_ok()).count(), 1);
        for (index, (source, result)) in [1, 0].into_iter().zip(results) {
            if result.is_ok() {
                assert_eq!(bytes, [index]);
                assert!(!source.exists());
            } else {
                assert_eq!(fs::read(source).unwrap(), [index]);
            }
        }

    }

    #[cfg(unix)]
    #[test]
    fn publication_preserves_dangling_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let movie = tmp.path().join("movie.mp4");
        let dangling = tmp.path().join("dangling.mp4");
        fs::write(&movie, b"validated movie").unwrap();
        std::os::unix::fs::symlink("missing", &dangling).unwrap();
        assert!(publish_noclobber(&movie, &dangling).is_err());
        assert_eq!(fs::read_link(dangling).unwrap(), Path::new("missing"));
        assert_eq!(fs::read(movie).unwrap(), b"validated movie");
    }

    fn task() -> (TempDir, PathBuf, Workspace) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(Uuid::new_v4().to_string());
        let mut options = RenderOptions::new(tmp.path().join("song"), tmp.path().join("movie.mp4"));
        options.fps = 2;
        fs::write(&options.song, b"audio input").unwrap();
        let work = Workspace::create(&dir, &options, 1).unwrap();
        (tmp, dir, work)
    }

    // Media decoding is tested by the media layer. These bytes exercise the subsequent
    // durability protocol with an actual serialized animation checkpoint and real files.
    fn commit_next(work: &mut Workspace) {
        work.set_render_backend("raster").unwrap();
        let start = work.committed();
        let end = (start + work.segment_frames()).min(work.total());
        let lines = crate::parse_lyrics("[00:00.00]test", crate::LyricFormat::Lrc).unwrap();
        let mut layout = crate::layout::Layout::new(&lines, 64.0, &[10.0], 3.0, 6.0);
        layout.update(&lines, (end - 1) * 500, 2);
        fs::write(work.segment_path(start), start.to_le_bytes()).unwrap();
        work.commit(start, end, &layout.save_checkpoint(), "stream")
            .unwrap();
    }

    #[test]
    fn checked_workspace_rejects_input_changed_after_redo_preflight() {
        let (tmp, _, source) = task();
        let mut expected = InputFingerprints::default();
        expected[AssetRole::Audio as usize] = Some(Fingerprint::read(&source.options().song).unwrap());
        fs::write(&source.options().song, b"other input").unwrap();
        let target = tmp.path().join(Uuid::new_v4().to_string());
        let result = Workspace::create_checked(&target, source.options(), 1, Some(&expected));
        assert!(result.is_err());
        assert!(!target.join("config.json").exists());
        assert!(!target.join("manifest.json").exists());
    }

    #[test]
    fn workspace_persists_snapshot_range_and_exclusive_lock() {
        let (_tmp, dir, mut work) = task();
        let locked = Workspace::open(&dir).err().unwrap();
        assert!(matches!(
            locked.downcast_ref::<std::fs::TryLockError>(),
            Some(std::fs::TryLockError::WouldBlock)
        ));
        assert!(work.set_range(10, 2000, 3).is_err());
        work.set_range(10, 2000, 4).unwrap();
        work.set_render_backend("raster").unwrap();
        let original = serde_json::to_value(work.options()).unwrap();
        drop(work);
        let mut reopened = Workspace::open(&dir).unwrap();
        assert_eq!(reopened.range(), Some((10, 2000, 4)));
        assert!(reopened.set_render_backend("different").is_err());
        reopened.set_render_backend("raster").unwrap();
        assert_eq!(serde_json::to_value(reopened.options()).unwrap(), original);
        assert_eq!(reopened.segment_frames(), 2);
        assert!(reopened.set_range(11, 2000, 4).is_err());
        assert!(Workspace::create(&dir, reopened.options(), 1).is_err());
    }

    #[test]
    fn recovery_discards_interrupted_attempt_and_uncommitted_renames_only() {
        let (_tmp, dir, mut work) = task();
        work.set_range(0, 3000, 6).unwrap();
        commit_next(&mut work);
        let pending = work.segment_path(2);
        let orphan = work.segment_file(2);
        let checkpoint = work.checkpoint_file(4);
        let unrelated = dir.join("segments").join("notes.mp4");
        for path in [&pending, &orphan, &checkpoint, &unrelated] {
            fs::write(path, b"uncommitted").unwrap();
        }
        drop(work);
        let reopened = Workspace::open(&dir).unwrap();
        assert_eq!(reopened.committed(), 2);
        assert_ne!(pending, reopened.segment_path(2));
        assert!(!pending.exists() && !orphan.exists() && !checkpoint.exists());
        assert!(unrelated.exists());
        assert!(reopened.checkpoint().unwrap().is_some());
    }

    #[test]
    fn corruption_truncates_the_entire_tail_and_preserves_the_last_good_checkpoint() {
        for break_checkpoint in [false, true] {
            let (_tmp, dir, mut work) = task();
            work.set_range(0, 3000, 6).unwrap();
            commit_next(&mut work);
            let expected = serde_json::to_value(work.checkpoint().unwrap().unwrap()).unwrap();
            commit_next(&mut work);
            commit_next(&mut work);
            let corrupt = if break_checkpoint {
                work.checkpoint_file(4)
            } else {
                work.segment_file(2)
            };
            let mut bytes = fs::read(&corrupt).unwrap();
            bytes[0] ^= 1;
            fs::write(corrupt, bytes).unwrap();
            drop(work);
            let reopened = Workspace::open(&dir).unwrap();
            assert_eq!(reopened.committed(), 2);
            assert!(!reopened.segment_file(4).exists());
            assert_eq!(
                expected,
                serde_json::to_value(reopened.checkpoint().unwrap().unwrap()).unwrap()
            );
        }
    }

    #[test]
    fn incompatible_checkpoint_schema_is_rejected_without_removing_segments() {
        let (_tmp, dir, mut work) = task();
        work.set_range(0, 1000, 2).unwrap();
        commit_next(&mut work);
        let path = work.checkpoint_file(2);
        let mut checkpoint: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        checkpoint["schema_version"] = serde_json::json!(999);
        let bytes = serde_json::to_vec(&checkpoint).unwrap();
        fs::write(path, &bytes).unwrap();
        work.manifest.segments[0].checkpoint = Fingerprint::from_bytes(&bytes);
        work.save().unwrap();
        let segment = work.segment_file(0);
        drop(work);
        assert!(Workspace::open(&dir).is_err());
        assert!(segment.exists());
    }

    #[test]
    fn changing_input_or_configuration_blocks_recovery() {
        let (_tmp, dir, work) = task();
        let song = work.options().song.clone();
        drop(work);
        fs::write(&song, b"changed file").unwrap();
        assert!(
            Workspace::open(&dir)
                .err()
                .unwrap()
                .to_string()
                .contains("input asset changed")
        );
        fs::write(&song, b"audio input").unwrap();
        let config = dir.join("config.json");
        OpenOptions::new()
            .append(true)
            .open(config)
            .unwrap()
            .write_all(b" ")
            .unwrap();
        assert!(
            Workspace::open(&dir)
                .err()
                .unwrap()
                .to_string()
                .contains("hash mismatch")
        );
    }

    #[test]
    fn receipt_recovers_durable_publication_even_after_input_removal() {
        let (tmp, dir, mut work) = task();
        work.set_range(0, 1000, 2).unwrap();
        commit_next(&mut work);
        let staging = tmp.path().join("verified.mp4");
        fs::write(&staging, b"validated movie").unwrap();
        work.prepare_publication(&staging).unwrap();
        let output = work.options().output.clone();
        let song = work.options().song.clone();
        drop(work);
        assert_eq!(Workspace::recover_published(&dir).unwrap(), None);
        fs::rename(&staging, &output).unwrap();
        fs::remove_file(song).unwrap();
        assert_eq!(Workspace::recover_published(&dir).unwrap(), Some((2, 15)));
        let mut completed = Workspace::open(&dir).unwrap();
        completed.manifest.intermediates_removed = true;
        completed.save().unwrap();
        let segment = completed.segment_file(0);
        let checkpoint = completed.checkpoint_file(2);
        fs::remove_file(&segment).unwrap();
        drop(completed);
        Workspace::cleanup_published(&dir).unwrap();
        assert!(!segment.exists() && !checkpoint.exists());
        assert!(!dir.join("manifest.json").exists());
        assert!(dir.join(ARCHIVED_MANIFEST).is_file());
        Workspace::cleanup_published(&dir).unwrap();
        assert_eq!(Workspace::recover_published(&dir).unwrap(), Some((2, 15)));
        fs::write(output, b"foreign content").unwrap();
        assert!(Workspace::recover_published(&dir).is_err());
    }

    #[test]
    fn replacement_after_verification_keeps_intermediates() {
        let (tmp, dir, mut work) = task();
        work.set_range(0, 1000, 2).unwrap();
        commit_next(&mut work);
        let segment = work.segment_file(0);
        let stage = tmp.path().join("verified.mp4");
        fs::write(&stage, b"good").unwrap();
        work.prepare_publication(&stage).unwrap();
        let output = work.options().output.clone();
        fs::rename(stage, &output).unwrap();
        drop(work);
        let verified = Workspace::verify_publication(&dir).unwrap().unwrap();
        fs::rename(&output, tmp.path().join("original.mp4")).unwrap();
        fs::write(&output, b"fake").unwrap();
        assert!(verified.cleanup().is_err());
        assert!(segment.is_file());
        assert!(dir.join("manifest.json").is_file());
        assert_eq!(fs::read(output).unwrap(), b"fake");
    }

    #[test]
    fn creation_does_not_replace_occupied_paths() {
        let (tmp, _, work) = task();
        let dir = tmp.path().join(Uuid::new_v4().to_string());
        fs::write(&dir, b"existing").unwrap();
        assert!(Workspace::create(&dir, work.options(), 1).is_err());
        assert_eq!(fs::read(dir).unwrap(), b"existing");
    }
}
