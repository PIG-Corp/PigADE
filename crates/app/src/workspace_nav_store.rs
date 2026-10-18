//! Small, per-host navigation preferences. This file contains UUID metadata
//! only; workspace names, paths, and provider content never enter the store.

use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

const SCHEMA: u32 = 1;
const MAX_FILE_BYTES: usize = 32 * 1024;
const MAX_SOCKET_PATH_BYTES: usize = 108;
const MAX_RECENT: usize = 32;
const MAX_COLLAPSED: usize = 128;
const MAX_REVISION: u64 = i64::MAX as u64;
const LOCK_WAIT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) schema: u32,
    pub(crate) revision: u64,
    pub(crate) recent: Vec<Visit>,
    pub(crate) collapsed_projects: Vec<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Visit {
    pub(crate) workspace: Uuid,
    pub(crate) tab: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Visit(Visit),
    SetCollapsed { project: Uuid, collapsed: bool },
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            revision: 0,
            recent: Vec::new(),
            collapsed_projects: Vec::new(),
        }
    }
}

impl Config {
    /// Apply one semantic operation. Returns true only if persisted state changes.
    pub(crate) fn apply(&mut self, op: &Op) -> bool {
        match op {
            Op::Visit(visit) => {
                if self.recent.first() == Some(visit) {
                    return false;
                }
                self.recent
                    .retain(|entry| entry.workspace != visit.workspace);
                self.recent.insert(0, visit.clone());
                self.recent.truncate(MAX_RECENT);
                true
            }
            Op::SetCollapsed { project, collapsed } => {
                let found = self.collapsed_projects.iter().position(|id| id == project);
                if *collapsed {
                    if found.is_some() {
                        return false;
                    }
                    if self.collapsed_projects.len() == MAX_COLLAPSED {
                        self.collapsed_projects.remove(0);
                    }
                    self.collapsed_projects.push(*project);
                    true
                } else if let Some(index) = found {
                    self.collapsed_projects.remove(index);
                    true
                } else {
                    false
                }
            }
        }
    }
}

/// Return a private navigation file path keyed by the exact socket bytes.
pub(crate) fn path_for_socket(socket: &Path) -> Result<PathBuf, String> {
    let bytes = socket.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SOCKET_PATH_BYTES {
        return Err("host socket path must contain 1..=108 bytes".into());
    }
    let mut filename = String::with_capacity(4 + bytes.len() * 2 + 5);
    filename.push_str("nav-");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut filename, "{byte:02x}").expect("writing to String cannot fail");
    }
    filename.push_str(".json");
    Ok(config_directory()?
        .join("workspace-navigation")
        .join(filename))
}

pub(crate) fn load_from(path: &Path) -> Result<Config, String> {
    let parent = parent_dir(path);
    ensure_private_directory(parent)?;
    let _lock = lock(path)?;
    read_unlocked(path)
}

/// Merge one operation under a per-host lock so independent windows cannot
/// overwrite one another's navigation changes.
pub(crate) fn apply_to(path: &Path, op: &Op) -> Result<Config, String> {
    let parent = parent_dir(path);
    ensure_private_directory(parent)?;
    let _lock = lock(path)?;
    let mut config = read_unlocked(path)?;
    if !config.apply(op) {
        return Ok(config);
    }
    validate_config(&config)?;
    config.revision = config
        .revision
        .checked_add(1)
        .filter(|revision| *revision <= MAX_REVISION)
        .ok_or_else(|| "workspace navigation revision exhausted".to_string())?;
    let bytes = serde_json::to_vec(&config)
        .map_err(|error| format!("encode workspace navigation: {error}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("workspace navigation file exceeds 32 KiB".into());
    }
    atomic_write(path, parent, &bytes)?;
    Ok(config)
}

fn config_directory() -> Result<PathBuf, String> {
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(config);
        if !path.is_absolute() {
            return Err("XDG_CONFIG_HOME must be absolute".into());
        }
        return Ok(path.join("pigade"));
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "neither XDG_CONFIG_HOME nor HOME is set".to_string())?;
    let path = PathBuf::from(home);
    if !path.is_absolute() {
        return Err("HOME must be absolute".into());
    }
    Ok(path.join(".config/pigade"))
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn current_uid() -> Result<u32, String> {
    fs::metadata("/proc/self")
        .map(|metadata| metadata.uid())
        .map_err(|error| format!("read current user identity: {error}"))
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("create navigation parent: {error}"))?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(path) {
                Ok(()) => validate_directory(
                    &fs::symlink_metadata(path)
                        .map_err(|error| format!("inspect navigation directory: {error}"))?,
                ),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    validate_directory(
                        &fs::symlink_metadata(path)
                            .map_err(|error| format!("inspect navigation directory: {error}"))?,
                    )
                }
                Err(error) => Err(format!("create private navigation directory: {error}")),
            }
        }
        Err(error) => Err(format!("inspect navigation directory: {error}")),
    }
}

fn validate_directory(metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != current_uid()?
        || metadata.mode() & 0o777 != 0o700
    {
        return Err("workspace navigation directory must be an owned real 0700 directory".into());
    }
    Ok(())
}

fn lock(path: &Path) -> Result<File, String> {
    let lock_path = suffix_path(path, ".lock");
    let file = open_regular(&lock_path, true, true)
        .map_err(|error| format!("open navigation lock: {error}"))?;
    let deadline = Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err("timed out waiting 2 seconds for workspace navigation lock".into());
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("lock workspace navigation: {error}"));
            }
        }
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("secure navigation lock: {error}"))?;
    Ok(file)
}

fn open_regular(path: &Path, writable: bool, create: bool) -> std::io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_file_metadata(&metadata).map_err(invalid_data)?,
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    if create {
        options.write(true).create(true).mode(0o600);
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    validate_file_metadata(&opened).map_err(invalid_data)?;
    if opened.dev() != named.dev()
        || opened.ino() != named.ino()
        || !named.is_file()
        || named.file_type().is_symlink()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "navigation path changed while opening",
        ));
    }
    Ok(file)
}

fn validate_file_metadata(metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file()
        || metadata.uid() != current_uid().map_err(|error| error.to_string())?
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
    {
        return Err("workspace navigation file must be an owned regular 0600 file".into());
    }
    Ok(())
}

fn invalid_data(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn read_unlocked(path: &Path) -> Result<Config, String> {
    let file = match open_regular(path, false, false) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => return Err(format!("open workspace navigation: {error}")),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect workspace navigation: {error}"))?;
    if metadata.len() > MAX_FILE_BYTES as u64 {
        return Err("workspace navigation file exceeds 32 KiB".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read workspace navigation: {error}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("workspace navigation file exceeds 32 KiB".into());
    }
    let config: Config = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse workspace navigation: {error}"))?;
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &Config) -> Result<(), String> {
    if config.schema != SCHEMA {
        return Err(format!(
            "unsupported workspace navigation schema {}",
            config.schema
        ));
    }
    if config.revision > MAX_REVISION {
        return Err("workspace navigation revision is out of range".into());
    }
    if config.recent.len() > MAX_RECENT
        || config
            .recent
            .iter()
            .map(|visit| visit.workspace)
            .collect::<std::collections::HashSet<_>>()
            .len()
            != config.recent.len()
    {
        return Err("workspace navigation recent list is invalid".into());
    }
    if config.collapsed_projects.len() > MAX_COLLAPSED
        || config
            .collapsed_projects
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != config.collapsed_projects.len()
    {
        return Err("workspace navigation collapsed list is invalid".into());
    }
    Ok(())
}

fn atomic_write(path: &Path, parent: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        validate_file_metadata(&metadata)?;
    }
    let temp = parent.join(format!(".nav-{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|error| format!("create navigation temp file: {error}"))?;
    let outcome = (|| {
        file.write_all(bytes)
            .map_err(|error| format!("write workspace navigation: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync workspace navigation: {error}"))?;
        let opened = file
            .metadata()
            .map_err(|error| format!("inspect navigation temp file: {error}"))?;
        let named = fs::symlink_metadata(&temp)
            .map_err(|error| format!("inspect navigation temp path: {error}"))?;
        if !named.is_file()
            || named.file_type().is_symlink()
            || opened.dev() != named.dev()
            || opened.ino() != named.ino()
            || opened.uid() != current_uid()?
            || opened.mode() & 0o777 != 0o600
        {
            return Err("navigation temp path changed or is not a private regular file".into());
        }
        drop(file);
        fs::rename(&temp, path)
            .map_err(|error| format!("replace workspace navigation: {error}"))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync navigation directory: {error}"))?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&temp);
    }
    outcome
}

fn suffix_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "pigade-nav-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            ));
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(&path).unwrap();
            Self(path)
        }
        fn path(&self, host: &str) -> PathBuf {
            self.0.join(format!("{host}.json"))
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn visits_are_mru_deduplicated_and_remember_tabs_after_reopen() {
        let dir = TestDir::new();
        let path = dir.path("host");
        let workspace = Uuid::new_v4();
        let first_tab = Uuid::new_v4();
        let second_tab = Uuid::new_v4();
        let saved = apply_to(
            &path,
            &Op::Visit(Visit {
                workspace,
                tab: Some(first_tab),
            }),
        )
        .unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(load_from(&path).unwrap().recent[0].tab, Some(first_tab));
        assert_eq!(
            apply_to(
                &path,
                &Op::Visit(Visit {
                    workspace,
                    tab: Some(first_tab)
                })
            )
            .unwrap()
            .revision,
            1
        );
        let changed = apply_to(
            &path,
            &Op::Visit(Visit {
                workspace,
                tab: Some(second_tab),
            }),
        )
        .unwrap();
        assert_eq!(changed.revision, 2);
        assert_eq!(
            load_from(&path).unwrap().recent,
            vec![Visit {
                workspace,
                tab: Some(second_tab)
            }]
        );
        let other = Uuid::new_v4();
        let newest = apply_to(
            &path,
            &Op::Visit(Visit {
                workspace: other,
                tab: None,
            }),
        )
        .unwrap();
        assert_eq!(
            newest
                .recent
                .iter()
                .map(|visit| visit.workspace)
                .collect::<Vec<_>>(),
            vec![other, workspace]
        );
    }

    #[test]
    fn concurrent_operations_merge_and_socket_paths_are_separate() {
        let dir = TestDir::new();
        let host_a = dir.path("host-a");
        let host_b = dir.path("host-b");
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let path = host_a.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                apply_to(
                    &path,
                    &Op::Visit(Visit {
                        workspace: Uuid::new_v4(),
                        tab: None,
                    }),
                )
                .unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let merged = load_from(&host_a).unwrap();
        assert_eq!(merged.revision, 8);
        assert_eq!(merged.recent.len(), 8);
        let isolated = apply_to(
            &host_b,
            &Op::SetCollapsed {
                project: Uuid::new_v4(),
                collapsed: true,
            },
        )
        .unwrap();
        assert_eq!(isolated.revision, 1);
        assert_eq!(load_from(&host_a).unwrap(), merged);
    }

    #[test]
    fn malformed_and_symlink_files_are_preserved() {
        let dir = TestDir::new();
        let malformed = dir.path("malformed");
        fs::write(
            &malformed,
            br#"{"schema":2,"revision":1,"recent":[],"collapsed_projects":[]}"#,
        )
        .unwrap();
        let original = fs::read(&malformed).unwrap();
        assert!(
            apply_to(
                &malformed,
                &Op::Visit(Visit {
                    workspace: Uuid::new_v4(),
                    tab: None
                })
            )
            .is_err()
        );
        assert_eq!(fs::read(&malformed).unwrap(), original);

        let target = dir.path("target");
        fs::write(&target, b"untouched").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let alias = dir.path("alias");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert!(
            apply_to(
                &alias,
                &Op::SetCollapsed {
                    project: Uuid::new_v4(),
                    collapsed: true
                }
            )
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"untouched");

        let exposed = dir.path("exposed");
        fs::write(
            &exposed,
            br#"{"schema":1,"revision":1,"recent":[],"collapsed_projects":[]}"#,
        )
        .unwrap();
        let exposed_original = fs::read(&exposed).unwrap();
        fs::set_permissions(&exposed, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_from(&exposed).is_err());
        assert_eq!(fs::read(&exposed).unwrap(), exposed_original);

        let oversized = dir.path("oversized");
        fs::write(&oversized, vec![b' '; MAX_FILE_BYTES + 1]).unwrap();
        fs::set_permissions(&oversized, fs::Permissions::from_mode(0o600)).unwrap();
        let oversized_original = fs::read(&oversized).unwrap();
        assert!(load_from(&oversized).is_err());
        assert_eq!(fs::read(&oversized).unwrap(), oversized_original);
    }

    #[test]
    fn lists_are_bounded_and_collapsed_eviction_is_fifo() {
        let mut config = Config::default();
        for _ in 0..MAX_RECENT + 4 {
            config.apply(&Op::Visit(Visit {
                workspace: Uuid::new_v4(),
                tab: None,
            }));
        }
        assert_eq!(config.recent.len(), MAX_RECENT);
        for _ in 0..MAX_COLLAPSED + 1 {
            config.apply(&Op::SetCollapsed {
                project: Uuid::new_v4(),
                collapsed: true,
            });
        }
        assert_eq!(config.collapsed_projects.len(), MAX_COLLAPSED);
        assert!(validate_config(&config).is_ok());
    }
}
