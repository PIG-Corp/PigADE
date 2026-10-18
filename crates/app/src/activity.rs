//! Bounded unread activity metadata and persistence for hidden panels.
//!
//! This module intentionally stores counters and state only. It never persists
//! terminal text, filesystem labels, or credentials.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::ffi::OsStrExt,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_SESSIONS: usize = 32;
const MAX_FILE_BYTES: usize = 32 * 1024;
const SAVED_SCHEMA: u32 = 1;
const MAX_SOCKET_PATH_BYTES: usize = 108;
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema: u32,
    pub host_instance: Uuid,
    pub revision: u64,
    pub sessions: Vec<Session>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub session: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub output_bytes: u64,
    pub state: String,
    pub storage_error: bool,
}

/// Persisted acknowledgement watermarks. Entries describe only the latest
/// visible host summary for each current session, never historical sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub schema: u32,
    #[serde(default)]
    pub revision: u64,
    pub host_instance: Option<Uuid>,
    pub seen: Vec<Seen>,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            schema: SAVED_SCHEMA,
            revision: 0,
            host_instance: None,
            seen: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seen {
    pub session: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub output_bytes: u64,
    pub state: String,
    pub storage_error: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub output: bool,
    pub ended: bool,
    pub error: bool,
}

impl Flags {
    pub fn any(self) -> bool {
        self.output || self.ended || self.error
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    session: Uuid,
    generation: Uuid,
}

pub struct Tracker {
    saved: Saved,
    latest: Option<Snapshot>,
    initialized: bool,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new(Saved::default())
    }
}

impl Tracker {
    pub fn new(mut saved: Saved) -> Self {
        if validate_saved(&saved).is_err() {
            saved = Saved::default();
        }
        Self {
            initialized: saved.host_instance.is_some(),
            saved,
            latest: None,
        }
    }

    pub fn observe(&mut self, snapshot: Snapshot) -> Result<bool, String> {
        validate_snapshot(&snapshot)?;

        if let Some(current) = &self.latest {
            if current.host_instance == snapshot.host_instance {
                if snapshot.revision < current.revision {
                    return Err("activity revision regressed".into());
                }
                if snapshot.revision == current.revision && *current == snapshot {
                    return Ok(false);
                }
                check_regressions(current, &snapshot)?;
            }
        } else if self.saved.host_instance == Some(snapshot.host_instance) {
            check_saved_regressions(&self.saved, &snapshot)?;
        }

        let host_changed = self.saved.host_instance != Some(snapshot.host_instance);
        let first_baseline = !self.initialized || host_changed;
        let prior = index_seen(&self.saved);
        let sessions = &snapshot.sessions;
        let seen: Vec<_> = sessions
            .iter()
            .map(|session| {
                let old = prior.get(&Key {
                    session: session.session,
                    generation: session.generation,
                });
                if first_baseline {
                    baseline(session)
                } else if old.is_none() {
                    newly_observed_baseline(session)
                } else {
                    (*old.expect("checked above")).clone()
                }
            })
            .collect();
        if first_baseline || seen != self.saved.seen {
            self.saved.revision = snapshot.revision;
        }
        self.saved.seen = seen;
        self.saved.host_instance = Some(snapshot.host_instance);
        self.saved.schema = SAVED_SCHEMA;
        self.latest = Some(snapshot);
        self.initialized = true;
        Ok(true)
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.latest.as_ref()
    }

    pub fn flags(&self, session: Uuid) -> Flags {
        let Some(latest) = &self.latest else {
            return Flags::default();
        };
        let Some(current) = latest
            .sessions
            .iter()
            .find(|entry| entry.session == session)
        else {
            return Flags::default();
        };
        let seen = self.saved.seen.iter().find(|entry| {
            entry.session == current.session && entry.generation == current.generation
        });
        let Some(seen) = seen else {
            return Flags::default();
        };
        Flags {
            output: current.output_bytes > seen.output_bytes,
            ended: current.state == "exited" && seen.state != "exited",
            error: current.storage_error && !seen.storage_error,
        }
    }

    /// Acknowledge only a summary that was actually displayed. The sequence
    /// check prevents a delayed paint/ack from clearing newer activity.
    pub fn acknowledge(&mut self, session: Uuid, generation: Uuid, through_sequence: u64) -> bool {
        let Some(current) = self.latest.as_ref().and_then(|snapshot| {
            snapshot
                .sessions
                .iter()
                .find(|entry| entry.session == session && entry.generation == generation)
        }) else {
            return false;
        };
        if current.sequence > through_sequence {
            return false;
        }
        if let Some(seen) = self
            .saved
            .seen
            .iter_mut()
            .find(|entry| entry.session == session && entry.generation == generation)
        {
            if seen.output_bytes == current.output_bytes
                && seen.state == current.state
                && seen.storage_error == current.storage_error
            {
                return false;
            }
            let next = baseline(current);
            if *seen == next {
                return false;
            }
            *seen = next;
            self.saved.revision = self.latest.as_ref().expect("current summary").revision;
            true
        } else {
            false
        }
    }

    pub fn acknowledge_all(&mut self) -> bool {
        let Some(snapshot) = self.latest.clone() else {
            return false;
        };
        let mut changed = false;
        for current in &snapshot.sessions {
            if let Some(seen) = self.saved.seen.iter_mut().find(|entry| {
                entry.session == current.session && entry.generation == current.generation
            }) {
                if seen.output_bytes == current.output_bytes
                    && seen.state == current.state
                    && seen.storage_error == current.storage_error
                {
                    continue;
                }
                let next = baseline(current);
                changed |= *seen != next;
                *seen = next;
            }
        }
        if changed {
            self.saved.revision = snapshot.revision;
        }
        changed
    }

    pub fn saved(&self) -> Saved {
        self.saved.clone()
    }
}

fn baseline(session: &Session) -> Seen {
    Seen {
        session: session.session,
        generation: session.generation,
        sequence: session.sequence,
        output_bytes: session.output_bytes,
        state: session.state.clone(),
        storage_error: session.storage_error,
    }
}

fn newly_observed_baseline(session: &Session) -> Seen {
    Seen {
        session: session.session,
        generation: session.generation,
        sequence: 0,
        output_bytes: 0,
        state: "running".into(),
        storage_error: false,
    }
}

fn index_seen(saved: &Saved) -> HashMap<Key, &Seen> {
    saved
        .seen
        .iter()
        .map(|entry| {
            (
                Key {
                    session: entry.session,
                    generation: entry.generation,
                },
                entry,
            )
        })
        .collect()
}

impl Snapshot {
    pub fn validate(&self) -> Result<(), String> {
        validate_snapshot(self)
    }
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<(), String> {
    if snapshot.schema != 1 {
        return Err("unsupported activity schema".into());
    }
    if snapshot.host_instance.is_nil() {
        return Err("nil activity host instance".into());
    }
    if snapshot.sessions.len() > MAX_SESSIONS {
        return Err("too many activity sessions".into());
    }
    let mut sessions = HashMap::new();
    let mut generations = HashMap::new();
    for entry in &snapshot.sessions {
        if entry.session.is_nil() || entry.generation.is_nil() {
            return Err("nil activity session or generation".into());
        }
        if entry.state.len() > 32
            || !matches!(
                entry.state.as_str(),
                "running" | "history" | "exited" | "degraded"
            )
        {
            return Err("invalid activity state".into());
        }
        if sessions.insert(entry.session, ()).is_some() {
            return Err("duplicate activity session".into());
        }
        if generations.insert(entry.generation, ()).is_some() {
            return Err("duplicate activity generation".into());
        }
    }
    Ok(())
}

fn validate_saved(saved: &Saved) -> Result<(), String> {
    if saved.schema != SAVED_SCHEMA {
        return Err("unsupported saved activity schema".into());
    }
    if saved.seen.len() > MAX_SESSIONS {
        return Err("too many saved activity sessions".into());
    }
    if saved.host_instance.is_some_and(|host| host.is_nil()) {
        return Err("nil saved activity host".into());
    }
    if saved.host_instance.is_none() && !saved.seen.is_empty() {
        return Err("saved activity has no host".into());
    }
    let mut keys = HashMap::new();
    let mut generations = HashMap::new();
    for entry in &saved.seen {
        if entry.session.is_nil()
            || entry.generation.is_nil()
            || entry.state.len() > 32
            || !matches!(
                entry.state.as_str(),
                "running" | "history" | "exited" | "degraded"
            )
        {
            return Err("invalid saved activity entry".into());
        }
        if keys.insert(entry.session, ()).is_some() {
            return Err("duplicate saved activity session".into());
        }
        if generations.insert(entry.generation, ()).is_some() {
            return Err("duplicate saved activity generation".into());
        }
    }
    Ok(())
}

fn check_regressions(old: &Snapshot, new: &Snapshot) -> Result<(), String> {
    let old_by_key: HashMap<_, _> = old
        .sessions
        .iter()
        .map(|entry| ((entry.session, entry.generation), entry))
        .collect();
    for current in &new.sessions {
        if let Some(previous) = old_by_key.get(&(current.session, current.generation))
            && (current.sequence < previous.sequence
                || current.output_bytes < previous.output_bytes)
        {
            return Err("activity session watermark regressed".into());
        }
    }
    Ok(())
}

fn check_saved_regressions(saved: &Saved, new: &Snapshot) -> Result<(), String> {
    for current in &new.sessions {
        if let Some(previous) = saved.seen.iter().find(|entry| {
            entry.session == current.session && entry.generation == current.generation
        }) && (current.sequence < previous.sequence
            || current.output_bytes < previous.output_bytes)
        {
            return Err("activity session watermark regressed".into());
        }
    }
    Ok(())
}

fn config_directory() -> Result<PathBuf, String> {
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME")
        && !config.is_empty()
    {
        return Ok(PathBuf::from(config).join("pigade"));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/pigade"))
}

/// Return a per-host activity file keyed by the exact Unix socket path bytes.
/// The filename is reversible hex and contains no socket labels or terminal data.
pub fn path_for_socket(socket: &Path) -> Result<PathBuf, String> {
    let bytes = socket.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SOCKET_PATH_BYTES {
        return Err("host socket path must contain 1..=108 bytes".into());
    }
    let mut filename = String::with_capacity("activity-".len() + bytes.len() * 2 + ".json".len());
    filename.push_str("activity-");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut filename, "{byte:02x}").expect("writing to String cannot fail");
    }
    filename.push_str(".json");
    Ok(config_directory()?.join(filename))
}

pub fn load_from(path: &Path) -> Result<Saved, String> {
    let _lock = lock(path)?;
    match open_regular(path, false, false) {
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|e| format!("activity metadata: {e}"))?;
            if metadata.len() as usize > MAX_FILE_BYTES {
                return Err("activity file exceeds 32 KiB".into());
            }
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            file.take((MAX_FILE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("read activity file: {e}"))?;
            if bytes.len() > MAX_FILE_BYTES {
                return Err("activity file exceeds 32 KiB".into());
            }
            let saved: Saved = serde_json::from_slice(&bytes)
                .map_err(|e| format!("invalid activity file: {e}"))?;
            validate_saved(&saved)?;
            Ok(saved)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Saved {
            schema: SAVED_SCHEMA,
            revision: 0,
            ..Saved::default()
        }),
        Err(error) => Err(format!("open activity file: {error}")),
    }
}

/// Atomically save while holding a per-path advisory lock. Concurrent windows
/// merge watermarks for the same host; on host replacement, the caller's
/// incarnation wins. An older acknowledgement can therefore be preserved,
/// while different host histories intentionally do not coexist.
pub fn save_to(path: &Path, saved: &Saved) -> Result<(), String> {
    validate_saved(saved)?;
    let bytes = serde_json::to_vec(saved).map_err(|e| format!("encode activity: {e}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("activity file exceeds 32 KiB".into());
    }
    let _lock = lock(path)?;
    let merged = match read_unlocked(path) {
        Ok(existing) => merge_saved(existing, saved),
        Err(_) if path_not_found(path) => saved.clone(),
        Err(error) => return Err(error),
    };
    let bytes = serde_json::to_vec(&merged).map_err(|e| format!("encode activity: {e}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("activity file exceeds 32 KiB".into());
    }
    atomic_write(path, &bytes)
}

fn path_not_found(path: &Path) -> bool {
    fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn read_unlocked(path: &Path) -> Result<Saved, String> {
    let file = open_regular(path, false, false).map_err(|e| format!("open activity file: {e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("activity metadata: {e}"))?;
    if metadata.len() as usize > MAX_FILE_BYTES {
        return Err("activity file exceeds 32 KiB".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read activity file: {e}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("activity file exceeds 32 KiB".into());
    }
    let saved: Saved =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid activity file: {e}"))?;
    validate_saved(&saved)?;
    Ok(saved)
}

fn merge_saved(existing: Saved, incoming: &Saved) -> Saved {
    if existing.host_instance != incoming.host_instance {
        return incoming.clone();
    }
    // A stale window may still hold the previous generation. Keep membership
    // from the newer observed head set, merging only exact-generation reads.
    let (mut merged, other) = if existing.revision > incoming.revision {
        (existing.clone(), incoming)
    } else {
        (incoming.clone(), &existing)
    };
    for entry in &mut merged.seen {
        if let Some(old) = other
            .seen
            .iter()
            .find(|old| old.session == entry.session && old.generation == entry.generation)
        {
            entry.sequence = entry.sequence.max(old.sequence);
            entry.output_bytes = entry.output_bytes.max(old.output_bytes);
            entry.storage_error |= old.storage_error;
            if old.state == "exited" {
                entry.state = "exited".into();
            }
        }
    }
    merged
}

fn lock(path: &Path) -> Result<File, String> {
    ensure_parent(path)?;
    let lock_path = suffix_path(path, ".lock");
    let file =
        open_regular(&lock_path, true, true).map_err(|e| format!("open activity lock: {e}"))?;
    // Keep permissions private even if the file predated this implementation.
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("secure activity lock: {e}"))?;
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return Err("timed out waiting 2 seconds for activity lock".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(format!("lock activity file: {error}"));
            }
        }
    }
    Ok(file)
}

fn ensure_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|e| format!("create activity directory: {e}"))?;
    let metadata =
        fs::symlink_metadata(parent).map_err(|e| format!("activity directory metadata: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("activity directory is not a real directory".into());
    }
    Ok(())
}

fn open_regular(path: &Path, writable: bool, create: bool) -> std::io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "symlink is not allowed",
            ));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not a regular file",
            ));
        }
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
    if !opened.is_file()
        || named.file_type().is_symlink()
        || !named.is_file()
        || opened.dev() != named.dev()
        || opened.ino() != named.ino()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "path changed or is not a regular file",
        ));
    }
    Ok(file)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && (meta.file_type().is_symlink() || !meta.is_file())
    {
        return Err("activity target is not a regular file".into());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // A sibling short name keeps the temp component below NAME_MAX even when
    // the socket-encoded target filename is near its 255-byte limit.
    let temp = parent.join(format!(".pigade-activity-{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|e| format!("create activity temp file: {e}"))?;
    let outcome = (|| -> Result<(), String> {
        file.write_all(bytes)
            .map_err(|e| format!("write activity file: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("sync activity file: {e}"))?;
        let opened = file
            .metadata()
            .map_err(|e| format!("activity temp metadata: {e}"))?;
        let named =
            fs::symlink_metadata(&temp).map_err(|e| format!("inspect activity temp: {e}"))?;
        if !named.is_file()
            || named.file_type().is_symlink()
            || opened.dev() != named.dev()
            || opened.ino() != named.ino()
        {
            return Err("activity temp path changed or is not a regular file".into());
        }
        drop(file);
        fs::rename(&temp, path).map_err(|e| format!("replace activity file: {e}"))?;
        File::open(
            path.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|e| format!("sync activity directory: {e}"))?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&temp);
    }
    outcome
}

fn suffix_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(host: Uuid, revision: u64, sessions: Vec<Session>) -> Snapshot {
        Snapshot {
            schema: 1,
            host_instance: host,
            revision,
            sessions,
        }
    }
    fn session(
        id: Uuid,
        generation: Uuid,
        sequence: u64,
        output_bytes: u64,
        state: &str,
        storage_error: bool,
    ) -> Session {
        Session {
            session: id,
            generation,
            sequence,
            output_bytes,
            state: state.into(),
            storage_error,
        }
    }
    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("pigade-activity-{}.json", Uuid::new_v4()))
    }

    #[test]
    fn output_deduplicates_and_resize_only_sequence_is_not_activity() {
        let host = Uuid::new_v4();
        let id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let mut tracker = Tracker::new(Saved::default());
        tracker
            .observe(snap(
                host,
                1,
                vec![session(id, generation, 1, 100, "running", false)],
            ))
            .unwrap();
        assert!(!tracker.flags(id).any());
        let saved_before_resize = tracker.saved();
        let resized = snap(
            host,
            2,
            vec![session(id, generation, 2, 100, "running", false)],
        );
        assert!(tracker.observe(resized.clone()).unwrap());
        assert!(!tracker.flags(id).any());
        assert!(!tracker.acknowledge(id, generation, 2));
        assert!(!tracker.acknowledge_all());
        assert_eq!(
            tracker.saved(),
            saved_before_resize,
            "resize alone must not cause read-receipt writes"
        );
        assert!(!tracker.observe(resized).unwrap());
        tracker
            .observe(snap(
                host,
                3,
                vec![session(id, generation, 3, 120, "running", false)],
            ))
            .unwrap();
        assert!(tracker.flags(id).output);
    }

    #[test]
    fn same_revision_accepts_forward_metadata_but_rejects_regression() {
        let host = Uuid::new_v4();
        let id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let mut tracker = Tracker::default();
        tracker
            .observe(snap(
                host,
                7,
                vec![session(id, generation, 4, 30, "running", false)],
            ))
            .unwrap();

        let sampled_early = snap(
            host,
            7,
            vec![session(id, generation, 5, 48, "running", false)],
        );
        assert!(tracker.observe(sampled_early.clone()).unwrap());
        assert!(tracker.flags(id).output);
        assert!(!tracker.observe(sampled_early).unwrap());

        let saved_before = tracker.saved();
        assert!(
            tracker
                .observe(snap(
                    host,
                    7,
                    vec![session(id, generation, 3, 25, "running", false)],
                ))
                .is_err()
        );
        assert_eq!(tracker.saved(), saved_before);
        assert_eq!(tracker.snapshot().unwrap().sessions[0].sequence, 5);
    }

    #[test]
    fn exit_and_error_are_immediately_unread() {
        let host = Uuid::new_v4();
        let id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let mut tracker = Tracker::new(Saved::default());
        tracker
            .observe(snap(
                host,
                1,
                vec![session(id, generation, 1, 0, "running", false)],
            ))
            .unwrap();
        tracker
            .observe(snap(
                host,
                2,
                vec![session(id, generation, 2, 0, "exited", true)],
            ))
            .unwrap();
        let flags = tracker.flags(id);
        assert!(flags.ended && flags.error && flags.any());
    }

    #[test]
    fn acknowledgements_require_the_displayed_sequence() {
        let host = Uuid::new_v4();
        let id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let mut tracker = Tracker::new(Saved::default());
        tracker
            .observe(snap(
                host,
                1,
                vec![session(id, generation, 1, 0, "running", false)],
            ))
            .unwrap();
        tracker
            .observe(snap(
                host,
                2,
                vec![session(id, generation, 2, 10, "running", false)],
            ))
            .unwrap();
        assert!(!tracker.acknowledge(id, generation, 1));
        assert!(tracker.acknowledge(id, generation, 2));
        assert!(!tracker.flags(id).any());
        assert!(!tracker.acknowledge(id, generation, 2));
    }

    #[test]
    fn generation_and_host_changes_use_fresh_baselines() {
        let host = Uuid::new_v4();
        let id = Uuid::new_v4();
        let gen1 = Uuid::new_v4();
        let gen2 = Uuid::new_v4();
        let mut tracker = Tracker::new(Saved::default());
        tracker
            .observe(snap(
                host,
                1,
                vec![session(id, gen1, 1, 90, "exited", true)],
            ))
            .unwrap();
        assert!(!tracker.flags(id).any());
        tracker
            .observe(snap(
                host,
                2,
                vec![session(id, gen2, 1, 5, "running", false)],
            ))
            .unwrap();
        assert!(tracker.flags(id).output);
        let other_host = Uuid::new_v4();
        tracker
            .observe(snap(
                other_host,
                1,
                vec![session(id, gen2, 1, 5, "running", false)],
            ))
            .unwrap();
        assert!(!tracker.flags(id).any());
    }

    #[test]
    fn persisted_matching_watermarks_survive_restart() {
        let host = Uuid::new_v4();
        let id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let mut first = Tracker::new(Saved::default());
        first
            .observe(snap(
                host,
                1,
                vec![session(id, generation, 1, 4, "running", false)],
            ))
            .unwrap();
        first
            .observe(snap(
                host,
                2,
                vec![session(id, generation, 2, 8, "running", false)],
            ))
            .unwrap();
        let mut restarted = Tracker::new(first.saved());
        restarted
            .observe(snap(
                host,
                3,
                vec![session(id, generation, 3, 12, "running", false)],
            ))
            .unwrap();
        assert!(restarted.flags(id).output);
        assert!(restarted.acknowledge_all());
        assert!(!Tracker::new(restarted.saved()).flags(id).any());
    }

    #[test]
    fn observe_regressions_are_transactional_and_limits_are_enforced() {
        let host = Uuid::new_v4();
        let id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let mut tracker = Tracker::new(Saved::default());
        tracker
            .observe(snap(
                host,
                1,
                vec![session(id, generation, 2, 10, "running", false)],
            ))
            .unwrap();
        let before = tracker.saved();
        assert!(
            tracker
                .observe(snap(
                    host,
                    2,
                    vec![session(id, generation, 1, 9, "running", false)]
                ))
                .is_err()
        );
        assert_eq!(tracker.saved(), before);
        assert_eq!(tracker.snapshot().unwrap().revision, 1);
        let too_many = (0..=MAX_SESSIONS)
            .map(|_| session(Uuid::new_v4(), Uuid::new_v4(), 0, 0, "running", false))
            .collect();
        assert!(validate_snapshot(&snap(host, 2, too_many)).is_err());
    }

    #[test]
    fn storage_roundtrip_rejects_bad_file_without_overwriting() {
        let path = temp_path();
        let saved = Saved {
            schema: SAVED_SCHEMA,
            revision: 0,
            host_instance: Some(Uuid::new_v4()),
            seen: vec![],
        };
        save_to(&path, &saved).unwrap();
        assert_eq!(load_from(&path).unwrap(), saved);
        fs::write(&path, b"not json").unwrap();
        assert!(load_from(&path).is_err());
        let before = fs::read(&path).unwrap();
        assert!(save_to(&path, &saved).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(suffix_path(&path, ".lock"));
    }

    #[test]
    fn storage_rejects_symlink_and_oversized_files() {
        let path = temp_path();
        let target = suffix_path(&path, ".target");
        fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(load_from(&path).is_err());
        assert!(
            save_to(
                &path,
                &Saved {
                    schema: SAVED_SCHEMA,
                    revision: 0,
                    host_instance: None,
                    seen: vec![]
                }
            )
            .is_err()
        );
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&target);
        let _ = fs::remove_file(suffix_path(&path, ".lock"));

        let big = temp_path();
        fs::write(&big, vec![b' '; MAX_FILE_BYTES + 1]).unwrap();
        assert!(load_from(&big).is_err());
        let _ = fs::remove_file(&big);
        let _ = fs::remove_file(suffix_path(&big, ".lock"));
    }

    #[test]
    fn same_host_saves_merge_acknowledgements_for_matching_generation() {
        let host = Uuid::new_v4();
        let id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let older = Saved {
            schema: SAVED_SCHEMA,
            revision: 0,
            host_instance: Some(host),
            seen: vec![Seen {
                session: id,
                generation,
                sequence: 5,
                output_bytes: 42,
                state: "exited".into(),
                storage_error: true,
            }],
        };
        let newer = Saved {
            schema: SAVED_SCHEMA,
            revision: 0,
            host_instance: Some(host),
            seen: vec![Seen {
                session: id,
                generation,
                sequence: 3,
                output_bytes: 32,
                state: "running".into(),
                storage_error: false,
            }],
        };
        let merged = merge_saved(older.clone(), &newer);
        assert_eq!(merged.seen[0].sequence, 5);
        assert_eq!(merged.seen[0].output_bytes, 42);
        assert_eq!(merged.seen[0].state, "exited");
        assert!(merged.seen[0].storage_error);

        let replacement = Saved {
            schema: SAVED_SCHEMA,
            revision: 0,
            host_instance: Some(Uuid::new_v4()),
            seen: vec![],
        };
        assert_eq!(merge_saved(older.clone(), &replacement), replacement);
    }

    #[test]
    fn socket_persistence_paths_are_reversible_and_separate_hosts() {
        let first = path_for_socket(Path::new("/tmp/host-a.sock")).unwrap();
        let second = path_for_socket(Path::new("/tmp/host-b.sock")).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            first.file_name().unwrap(),
            "activity-2f746d702f686f73742d612e736f636b.json"
        );
        let too_long = "x".repeat(MAX_SOCKET_PATH_BYTES + 1);
        assert!(path_for_socket(Path::new(&too_long)).is_err());
    }

    #[test]
    fn older_window_cannot_replace_acknowledged_new_generation() {
        let host = Uuid::new_v4();
        let id = Uuid::new_v4();
        let mut tracker = Tracker::default();
        let first = Uuid::new_v4();
        tracker
            .observe(snap(
                host,
                1,
                vec![session(id, first, 1, 10, "exited", false)],
            ))
            .unwrap();
        let older = tracker.saved();
        let second = Uuid::new_v4();
        tracker
            .observe(snap(
                host,
                2,
                vec![session(id, second, 1, 20, "running", false)],
            ))
            .unwrap();
        tracker.acknowledge_all();
        let newer = tracker.saved();
        assert_eq!(merge_saved(newer.clone(), &older), newer);
        assert_eq!(merge_saved(older, &newer), newer);
    }

    #[test]
    fn long_socket_encoded_names_can_be_saved_atomically() {
        let path = std::env::temp_dir().join(format!(
            "activity-{}.json",
            "ab".repeat(MAX_SOCKET_PATH_BYTES)
        ));
        let saved = Saved::default();
        save_to(&path, &saved).unwrap();
        assert_eq!(load_from(&path).unwrap(), saved);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(suffix_path(&path, ".lock"));
    }
}
