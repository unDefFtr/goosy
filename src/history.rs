//! User-level settings and searchable task history. Media and recovery records live in the workspace.
use crate::{
    RenderOptions,
    workspace::{self, atomic_write, sync_dir},
};
use anyhow::{Context, Result, ensure};
use directories::BaseDirs;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const SCHEMA_VERSION: u32 = 3;
pub(crate) const DELETING_RESERVED: &str = "deleting_reserved";

pub(crate) fn thumbnail_path(root: &Path, id: Uuid) -> PathBuf {
    root.join("thumbnails").join(format!("{id}.png"))
}

pub(crate) fn save_thumbnail(
    root: &Path,
    id: Uuid,
    frame: &mut crate::task_manager::RgbaFrame,
) -> Result<PathBuf> {
    use skia_safe::{AlphaType, ColorType, EncodedImageFormat, ImageInfo, Pixmap};
    let info = ImageInfo::new(
        (i32::try_from(frame.width)?, i32::try_from(frame.height)?),
        ColorType::RGBA8888,
        AlphaType::Unpremul,
        None,
    );
    let expected = info.compute_byte_size(info.min_row_bytes());
    ensure!(frame.pixels.len() == expected, "invalid preview dimensions");
    let pixmap = Pixmap::new(&info, &mut frame.pixels, info.min_row_bytes())
        .context("invalid preview frame")?;
    let png = pixmap
        .encode(EncodedImageFormat::PNG, 100)
        .context("encode history thumbnail")?;
    let path = thumbnail_path(root, id);
    atomic_write(&path, &png)?;
    Ok(path)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub segment_seconds: u32,
    pub keep_segments: bool,
    pub max_concurrent: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            segment_seconds: 5,
            keep_segments: false,
            max_concurrent: 1,
        }
    }
}

pub(crate) struct Record {
    pub id: Uuid,
    pub state: String,
    pub options: RenderOptions,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub elapsed: f64,
    pub committed: u64,
    pub total: u64,
    pub output_size: u64,
    pub error: String,
    pub interval: u32,
}

/// Path identities from every creation snapshot, including rows still being enqueued. An input
/// may be a symlink outside the workspace pointing back into a directory we are about to clean.
pub(crate) struct ProtectedFiles(HashSet<Vec<u8>>);

impl ProtectedFiles {
    pub fn contains(&self, path: &Path) -> Result<bool> {
        if self.0.contains(path.as_os_str().as_encoded_bytes()) {
            return Ok(true);
        }
        match path.canonicalize() {
            Ok(real) => Ok(self.0.contains(real.as_os_str().as_encoded_bytes())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| format!("检查文件身份 {}", path.display())),
        }
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        let Some(metadata) = metadata_if_present(path)? else {
            return Ok(());
        };
        // A link here may itself be a user asset. Never descend through it or remove its target.
        if metadata.is_symlink() || self.contains(path)? {
            return Ok(());
        }
        ensure!(metadata.is_file(), "工作文件类型异常：{}", path.display());
        fs::remove_file(path).with_context(|| format!("删除工作文件 {}", path.display()))
    }
}

// Only paths inside another task's managed namespace need protection after forgetting their
// source row. The exemption belongs to the containing task and disappears with its history.
fn managed_owner(tasks: &Path, thumbnails: &Path, path: &Path) -> Option<Uuid> {
    if let Some(owner) = path.ancestors().filter_map(Path::file_name).find_map(crate::media::private_stage_owner) {
        return Some(owner);
    }
    if let Ok(relative) = path.strip_prefix(tasks) {
        return relative.iter().next()?.to_str()?.parse().ok();
    }
    let name = path.file_name()?.to_str()?;
    if path.parent() == Some(thumbnails) {
        return name.strip_suffix(".png")?.parse().ok();
    }
    None
}

fn metadata_if_present(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("检查 {}", path.display())),
    }
}

fn remove_empty_dir(path: &Path) -> Result<()> {
    match fs::remove_dir(path) {
        // Unrecognized files (e.g. a user exporting into the workspace) stay at their paths.
        Err(e) if matches!(e.kind(), io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound) => Ok(()),
        result => result.with_context(|| format!("删除工作目录 {}", path.display())),
    }
}

fn clean_workspace_directory(dir: &Path, subdir: &str, protected: &ProtectedFiles) -> Result<()> {
    workspace::directory(dir)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue; };
        let owned = match subdir {
            "" => matches!(name, "config.json" | "manifest.json" | "manifest.completed.json")
                || workspace::atomic_temp_name(name),
            "segments" => name.strip_suffix(".mp4").is_some_and(workspace::frame_number)
                || workspace::temporary_segment(name) || workspace::atomic_temp_name(name),
            "checkpoints" => name.strip_suffix(".json").is_some_and(workspace::frame_number)
                || workspace::atomic_temp_name(name),
            "output" => workspace::atomic_temp_name(name),
            "preview-cache" => name.strip_suffix(".rgba").is_some_and(|s| s.parse::<u64>().is_ok()),
            _ => false,
        };
        if owned {
            protected.remove_file(&entry.path())?;
        }
    }
    sync_dir(dir)
}

/// Pin the output directory while deleting and detect a disk swap before forgetting ownership.
pub(crate) struct DeletionParent<'a> {
    path: &'a Path,
    _handle: File,
    identity: (u64, u64),
}

impl<'a> DeletionParent<'a> {
    pub fn open(path: &'a Path) -> Result<Self> {
        let mut options = File::options();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(rustix::fs::OFlags::DIRECTORY.bits() as i32);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
            options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0);
        }
        let context = || format!("输出目录无法访问：{}", path.display());
        let handle = options.open(path).with_context(context)?;
        #[cfg(not(unix))]
        ensure!(handle.metadata().with_context(context)?.is_dir(), "输出路径的父级不是目录：{}", path.display());
        let identity = workspace::file_identity(&handle).with_context(context)?;
        Ok(Self { path, _handle: handle, identity })
    }

    pub fn recheck(&self) -> Result<()> {
        ensure!(Self::open(self.path)?.identity == self.identity,
            "删除期间输出目录已更换：{}", self.path.display());
        Ok(())
    }
}

pub(crate) struct History {
    db: Connection,
    root: PathBuf,
    pub settings: Settings,
    // Keep this handle (and its stable inode) until the SQLite connection has closed.
    _owner: File,
}

impl History {
    pub fn default_root() -> Result<PathBuf> {
        #[cfg(windows)]
        if let Some(root) = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            return Ok(root.join("Goosy"));
        }
        let dirs = BaseDirs::new().context("cannot locate the user data directory")?;
        Ok(dirs.data_local_dir().join(if cfg!(target_os = "linux") {
            "goosy"
        } else {
            "Goosy"
        }))
    }

    pub fn open(root: &Path) -> Result<Self> {
        fs::create_dir_all(root).with_context(|| format!("create {}", root.display()))?;
        let owner = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("manager.lock"))
            .context("open Goosy data directory lock")?;
        owner.try_lock().with_context(|| {
            format!(
                "cannot exclusively open {} (another Goosy manager may be running)",
                root.display()
            )
        })?;
        for name in ["tasks", "thumbnails", "logs", "cache"] {
            let path = root.join(name);
            fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
        }
        let path = root.join("settings.toml");
        let settings: Settings = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("invalid settings: {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let defaults = Settings::default();
                atomic_write(&path, toml::to_string_pretty(&defaults)?.as_bytes())?;
                defaults
            }
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        ensure!(
            settings.segment_seconds > 0,
            "settings.toml: segment_seconds must be positive"
        );
        ensure!(
            (1..=8).contains(&settings.max_concurrent),
            "settings.toml: max_concurrent must be 1–8"
        );

        let path = root.join("history.sqlite3");
        let mut db = Connection::open(&path).with_context(|| format!("open {}", path.display()))?;
        ensure!(
            !db.is_readonly(rusqlite::MAIN_DB)?,
            "read-only history database: {}",
            path.display()
        );
        let integrity: String = db
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .with_context(|| format!("check SQLite history {}", path.display()))?;
        ensure!(integrity == "ok", "{}: {integrity}", path.display());
        db.execute_batch("PRAGMA synchronous=FULL; PRAGMA fullfsync=ON; PRAGMA foreign_keys=ON;")?;
        let version: u32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        ensure!(
            version <= SCHEMA_VERSION,
            "unsupported history.sqlite3 schema {version}"
        );
        if version == 0 {
            let tx = db.transaction()?;
            tx.execute_batch(
                "CREATE TABLE tasks (
                uuid TEXT PRIMARY KEY NOT NULL,
                created_ms INTEGER NOT NULL,
                updated_ms INTEGER NOT NULL,
                state TEXT NOT NULL,
                options_json TEXT NOT NULL,
                manifest_version INTEGER NOT NULL,
                preview_interval INTEGER NOT NULL,
                committed_frames INTEGER NOT NULL,
                total_frames INTEGER NOT NULL,
                elapsed_seconds REAL NOT NULL,
                output_size INTEGER NOT NULL,
                error TEXT NOT NULL
            ) STRICT;
            CREATE INDEX tasks_recent ON tasks(updated_ms DESC, uuid);",
            )?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
        }
        if version < 2 {
            let tx = db.transaction()?;
            tx.execute_batch("CREATE TABLE preserved_task_paths (
                uuid TEXT NOT NULL REFERENCES tasks(uuid) ON DELETE CASCADE,
                path BLOB NOT NULL,
                PRIMARY KEY (uuid, path)
            ) STRICT;")?;
            tx.pragma_update(None, "user_version", 2)?;
            tx.commit()?;
        }
        if version < 3 {
            let tx = db.transaction()?;
            tx.execute_batch("ALTER TABLE tasks ADD COLUMN deletion_parent BLOB;")?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(Self {
            db,
            root: root.to_owned(),
            settings,
            _owner: owner,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn begin_deletion(&self, record: &mut Record, reserved: bool) -> Result<()> {
        let state = if reserved { DELETING_RESERVED } else { "deleting" };
        let changed = self.db.execute(
            "UPDATE tasks SET state=?2 WHERE uuid=?1",
            params![record.id.to_string(), state],
        )?;
        ensure!(changed == 1, "任务历史不存在");
        state.clone_into(&mut record.state);
        Ok(())
    }

    // Remember the authenticated directory after releasing the last ownership token.
    // A matching token can authenticate a remounted volume whose OS file ID changed.
    pub fn confirm_deletion_parent(
        &self,
        record: &mut Record,
        parent: &DeletionParent<'_>,
        owns_marker: bool,
    ) -> Result<()> {
        let (volume, file) = parent.identity;
        let identity = (u128::from(volume) << 64 | u128::from(file)).to_be_bytes();
        let changed = self.db.execute(
            "UPDATE tasks SET state='deleting', deletion_parent=?3 WHERE uuid=?1 AND state=?2
             AND (deletion_parent IS NULL OR deletion_parent=?3 OR ?4)",
            params![record.id.to_string(), record.state, identity.as_slice(), owns_marker],
        )?;
        ensure!(changed == 1, "输出目录身份与删除记录不一致");
        "deleting".clone_into(&mut record.state);
        Ok(())
    }

    pub fn finish_deletion(&self, id: Uuid) -> Result<()> {
        let changed = self.db.execute("DELETE FROM tasks WHERE uuid=?1 AND state='deleting'", [id.to_string()])?;
        ensure!(changed == 1, "任务删除状态已发生变化");
        Ok(())
    }

    pub fn protected_files(&self, deleting: Uuid) -> Result<ProtectedFiles> {
        let id = deleting.to_string();
        let mut stmt = self.db.prepare("SELECT path FROM preserved_task_paths WHERE uuid=?1")?;
        let mut paths = stmt.query_map([&id], |row| row.get::<_, Vec<u8>>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        let root = self.root.canonicalize()?;
        let tasks = root.join("tasks");
        let thumbnails = root.join("thumbnails");
        let mut preserve = self.db.prepare("INSERT OR IGNORE INTO preserved_task_paths (uuid,path)
            SELECT uuid,?2 FROM tasks WHERE uuid=?1")?;
        let mut stmt = self.db.prepare("SELECT options_json,uuid=?1 FROM tasks")?;
        let mut rows = stmt.query([&id])?;
        while let Some(row) = rows.next()? {
            let options: RenderOptions = serde_json::from_str(row.get_ref(0)?.as_str()?)?;
            let forgetting: bool = row.get(1)?;
            for path in [Some(options.output), Some(options.song), options.lyrics, options.cover, options.background]
                .into_iter().flatten()
            {
                // Keep the parent too: another queued export can target an empty directory.
                for part in std::iter::once(path.as_path()).chain(path.parent()) {
                    let real = match part.canonicalize() {
                        Ok(real) => real,
                        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => continue,
                        Err(e) => return Err(e).with_context(|| format!("检查受保护的素材 {}", part.display())),
                    };
                    let key = real.as_os_str().as_encoded_bytes();
                    if forgetting && let Some(owner) = managed_owner(&tasks, &thumbnails, &real)
                        && owner != deleting
                    {
                        preserve.execute(params![owner.to_string(), key])?;
                    }
                    paths.insert(key.to_vec());
                }
            }
        }
        Ok(ProtectedFiles(paths))
    }

    pub fn deletion_lock(&self, dir: &Path) -> Result<Option<File>> {
        let tasks = self.root.join("tasks");
        if metadata_if_present(&tasks)?.is_none() {
            return Ok(None);
        }
        workspace::directory(&tasks)?;
        if metadata_if_present(dir)?.is_none() {
            return Ok(None);
        }
        workspace::directory(dir)?;
        if metadata_if_present(&dir.join("worker.lock"))?.is_none() {
            // Creation never reached the lock, or a previous cleanup already removed it.
            // Still serialize any surviving staging records before inspecting them.
            if metadata_if_present(&dir.join("output"))?.is_some() {
                return workspace::lock(dir, true).map(Some);
            }
            return Ok(None);
        }
        workspace::lock(dir, false).map(Some)
    }

    pub fn clean_task_files(
        &self,
        record: &Record,
        dir: &Path,
        guard: Option<File>,
        protected: &ProtectedFiles,
        parent: &DeletionParent<'_>,
    ) -> Result<()> {
        let records = dir.join("output");
        if guard.is_some() && metadata_if_present(&records)?.is_some() {
            crate::media::clean_stage_records(&records, record.id, &record.options.output, |path| {
                // The record is the last local reference to a stage on a removable disk.
                parent.recheck()?;
                protected.contains(path)
            })?;
        }
        parent.recheck()?;
        if metadata_if_present(dir)?.is_some() {
            clean_workspace_directory(dir, "", protected)?;
            for subdir in ["segments", "checkpoints", "preview-cache", "audio", "output"] {
                let child = dir.join(subdir);
                if metadata_if_present(&child)?.is_some() {
                    clean_workspace_directory(&child, subdir, protected)?;
                    if !protected.contains(&child)? {
                        remove_empty_dir(&child)?;
                    }
                }
            }
            drop(guard);
            protected.remove_file(&dir.join("worker.lock"))?;
            sync_dir(dir)?;
            if !protected.contains(dir)? {
                remove_empty_dir(dir)?;
            }
        }
        let tasks = self.root.join("tasks");
        if metadata_if_present(&tasks)?.is_some() {
            sync_dir(&tasks)?;
        }
        let thumbnails = self.root.join("thumbnails");
        if metadata_if_present(&thumbnails)?.is_none() {
            return Ok(());
        }
        workspace::directory(&thumbnails)?;
        protected.remove_file(&thumbnail_path(&self.root, record.id))?;
        sync_dir(&thumbnails)
    }

    pub fn set_max_concurrent(&mut self, value: usize) -> Result<()> {
        ensure!((1..=8).contains(&value), "concurrency must be 1–8");
        let settings = Settings {
            max_concurrent: value,
            ..self.settings.clone()
        };
        atomic_write(
            &self.root.join("settings.toml"),
            toml::to_string_pretty(&settings)?.as_bytes(),
        )?;
        self.settings = settings;
        Ok(())
    }

    pub fn save(&self, record: &Record) -> Result<()> {
        let id = record.id.to_string();
        let updated_ms = i64::try_from(record.updated_ms)?;
        let committed = i64::try_from(record.committed)?;
        let total = i64::try_from(record.total)?;
        let size = i64::try_from(record.output_size)?;
        let updated = self.db.prepare_cached("UPDATE tasks SET state=?2, updated_ms=?3,
            committed_frames=?4, total_frames=?5, elapsed_seconds=?6, output_size=?7, error=?8 WHERE uuid=?1")?
            .execute(params![id, record.state, updated_ms, committed, total,
                record.elapsed, size, record.error])?;
        if updated == 0 {
            self.db.execute(
                "INSERT INTO tasks (uuid,created_ms,updated_ms,state,options_json,manifest_version,
                preview_interval,committed_frames,total_frames,elapsed_seconds,output_size,error)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    id,
                    i64::try_from(record.created_ms)?,
                    updated_ms,
                    record.state,
                    serde_json::to_string(&record.options)?,
                    crate::workspace::MANIFEST_VERSION,
                    record.interval,
                    committed,
                    total,
                    record.elapsed,
                    size,
                    record.error
                ],
            )?;
        }
        Ok(())
    }

    pub fn load(&self) -> Result<Vec<Record>> {
        let mut stmt = self.db.prepare("SELECT uuid,state,options_json,created_ms,updated_ms,
            elapsed_seconds,committed_frames,total_frames,output_size,error,preview_interval,manifest_version
            FROM tasks ORDER BY created_ms, rowid")?;
        let mut rows = stmt.query([])?;
        let mut records = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            ensure!(
                row.get::<_, u32>(11)? == crate::workspace::MANIFEST_VERSION,
                "unsupported manifest version for task {id}"
            );
            let json: String = row.get(2)?;
            records.push(Record {
                id: id
                    .parse()
                    .with_context(|| format!("invalid history UUID {id}"))?,
                state: row.get(1)?,
                options: serde_json::from_str(&json)
                    .with_context(|| format!("invalid options for {id}"))?,
                created_ms: u64::try_from(row.get::<_, i64>(3)?)?,
                updated_ms: u64::try_from(row.get::<_, i64>(4)?)?,
                elapsed: row.get(5)?,
                committed: u64::try_from(row.get::<_, i64>(6)?)?,
                total: u64::try_from(row.get::<_, i64>(7)?)?,
                output_size: u64::try_from(row.get::<_, i64>(8)?)?,
                error: row.get(9)?,
                interval: row.get(10)?,
            });
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_survives_restart_with_creation_snapshot() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut history = History::open(dir.path())?;
        let mut options = RenderOptions::new("original.wav", "render.mp4");
        options.sample_start_ms = Some(773);
        let mut record = Record {
            id: Uuid::new_v4(),
            state: "queued".into(),
            options,
            created_ms: 55,
            updated_ms: 55,
            elapsed: 0.,
            committed: 0,
            total: 0,
            output_size: 0,
            error: String::new(),
            interval: 15,
        };
        history.save(&record)?;
        record.options.sample_start_ms = Some(999);
        record.state = "paused".into();
        record.committed = 90;
        record.total = 240;
        history.save(&record)?;
        history.set_max_concurrent(3)?;
        assert!(History::open(dir.path()).is_err());
        drop(history);
        let reopened = History::open(dir.path())?;
        let saved = reopened.load()?.pop().unwrap();
        assert_eq!(
            (
                saved.id,
                saved.state.as_str(),
                saved.committed,
                saved.options.sample_start_ms
            ),
            (record.id, "paused", 90, Some(773))
        );
        assert_eq!(reopened.settings.max_concurrent, 3);
        Ok(())
    }
    #[cfg(windows)]
    #[test]
    fn read_only_database_is_rejected_without_modification() -> Result<()> {
        let dir = tempfile::tempdir()?;
        drop(History::open(dir.path())?);
        let path = dir.path().join("history.sqlite3");
        let original = fs::metadata(&path)?.permissions();
        let mut readonly = original.clone();
        readonly.set_readonly(true);
        fs::set_permissions(&path, readonly)?;
        let result = History::open(dir.path());
        fs::set_permissions(&path, original)?;
        assert!(result.is_err());
        assert!(History::open(dir.path())?.load()?.is_empty());
        Ok(())
    }

    #[test]
    fn unusable_directory_or_corrupt_database_is_reported_and_preserved() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let obstacle = dir.path().join("tasks");
        fs::write(&obstacle, b"user data")?;
        assert!(History::open(dir.path()).is_err());
        assert_eq!(fs::read(&obstacle)?, b"user data");
        fs::remove_file(obstacle)?;
        let db = dir.path().join("history.sqlite3");
        fs::write(&db, b"damaged database")?;
        assert!(History::open(dir.path()).is_err());
        assert_eq!(fs::read(db)?, b"damaged database");
        Ok(())
    }
}
