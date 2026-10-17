//! Private derived replay caches and conservative collection of their orphans.
//!
//! Source journals and generation archives never use this directory namespace.
//! A directory is collectible only when its bounded owner lease is intact and
//! the lease can be locked exclusively without waiting.

use anyhow::{Context, Result, ensure};
use pigade_storage::JournalHeader;
use std::ffi::CString;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::ffi::OsStrExt,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::Path,
};
use uuid::Uuid;

const PREFIX: &str = ".history-replay-";
const LEASE_NAME: &str = ".pigade-replay-owner";
const MAGIC: &[u8; 8] = b"PIGRC001";
const SCHEMA: u32 = 1;
const MAX_PROFILE: usize = 2048;
const MAX_MARKER: usize = 4096;
const MAX_SCAN_ENTRIES: usize = 256;
const MAX_CACHE_FILES: usize = 512;
const MAX_WARNINGS: usize = 16;

/// Owns one private replay cache. The lease lock is held until just before the
/// TempDir is dropped, so a concurrent host's collector cannot remove it.
pub(crate) struct ReplayCache {
    _lease: File,
    temporary: tempfile::TempDir,
}

impl ReplayCache {
    pub(crate) fn create(parent: &Path, source: &JournalHeader) -> Result<Self> {
        validate_parent(parent)?;
        ensure!(
            crate::journal_policy::Policy::from_profile(&source.profile).is_some(),
            "replay source profile is unsupported"
        );
        let profile = format!("{};cold={}", source.profile, crate::history::ARCHIVE_FORMAT);
        ensure!(profile.len() <= MAX_PROFILE, "replay profile too large");
        let temporary = tempfile::Builder::new()
            .prefix(PREFIX)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(parent)?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
        let dir_meta = fs::symlink_metadata(temporary.path())?;
        let marker = encode_marker(
            source.session_id,
            source.generation,
            profile.as_bytes(),
            dir_meta.dev(),
            dir_meta.ino(),
        )?;
        let lease_path = temporary.path().join(LEASE_NAME);
        let mut lease = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(lease_path)?;
        lock_exclusive(&lease, false)?;
        lease.write_all(&marker)?;
        lease.sync_all()?;
        File::open(temporary.path())?.sync_all()?;
        File::open(parent)?.sync_all()?;
        Ok(Self {
            _lease: lease,
            temporary,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        self.temporary.path()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CleanupReport {
    pub(crate) entries_scanned: usize,
    pub(crate) candidates: usize,
    pub(crate) collected: usize,
    pub(crate) preserved: usize,
    pub(crate) warnings: Vec<String>,
}

/// Collects only marked, schema-current, source-bound replay directories in
/// `parent`. Unknown entries and every validation/race failure are retained.
pub(crate) fn cleanup(parent: &Path) -> CleanupReport {
    let mut report = CleanupReport::default();
    if let Err(error) = validate_parent(parent) {
        warn(
            &mut report,
            format!("unsafe replay-cache parent: {error:#}"),
        );
        return report;
    }
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) => {
            warn(
                &mut report,
                format!("cannot scan replay-cache parent: {error}"),
            );
            return report;
        }
    };
    for entry in entries.take(MAX_SCAN_ENTRIES) {
        report.entries_scanned += 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warn(
                    &mut report,
                    format!("cannot read replay-cache entry: {error}"),
                );
                continue;
            }
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(PREFIX) {
            continue;
        }
        report.candidates += 1;
        match collect_candidate(parent, &name) {
            Ok(true) => report.collected += 1,
            Ok(false) => report.preserved += 1,
            Err(error) => {
                report.preserved += 1;
                warn(
                    &mut report,
                    format!("replay-cache candidate preserved: {error:#}"),
                );
            }
        }
    }
    if report.entries_scanned == MAX_SCAN_ENTRIES {
        warn(&mut report, "replay-cache scan entry limit reached".into());
    }
    report
}

fn collect_candidate(parent: &Path, name: &str) -> Result<bool> {
    let candidate = parent.join(name);
    let before = fs::symlink_metadata(&candidate)?;
    ensure!(
        safe_directory(&before),
        "candidate directory type, owner, or mode rejected (dir={}, uid={}, mode={:o}, current_uid={})",
        before.is_dir(),
        before.uid(),
        before.mode() & 0o777,
        unsafe { libc::getuid() }
    );
    let lease_path = candidate.join(LEASE_NAME);
    let lease_path_meta = fs::symlink_metadata(&lease_path).context("owner lease missing")?;
    ensure!(
        safe_lease(&lease_path_meta),
        "owner lease type, owner, mode, link count, or size rejected"
    );
    let mut lease = match OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&lease_path)
    {
        Ok(file) => file,
        Err(error) => return Err(error).context("owner lease open failed"),
    };
    ensure!(
        same_inode(&lease_path_meta, &lease.metadata()?) && safe_lease(&lease.metadata()?),
        "owner lease changed during open"
    );
    lock_exclusive(&lease, true).context("owner lease is locked")?;
    let mut bytes = Vec::with_capacity(MAX_MARKER.min(256));
    (&mut lease)
        .take((MAX_MARKER + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_MARKER, "owner marker too large");
    ensure!(
        valid_marker(&bytes, before.dev(), before.ino()),
        "owner marker invalid or foreign"
    );
    ensure!(
        same_inode(&before, &fs::symlink_metadata(&candidate)?),
        "candidate changed before quarantine"
    );
    ensure!(
        same_inode(&lease_path_meta, &fs::symlink_metadata(&lease_path)?),
        "owner lease changed before quarantine"
    );

    // Keep the quarantine under the same bounded namespace, so interruption
    // leaves a discoverable, still-marked orphan for a later pass.
    let quarantine = parent.join(format!("{PREFIX}reap-{}", Uuid::new_v4()));
    rename_noreplace(&candidate, &quarantine).context("quarantine rename failed")?;
    let moved = fs::symlink_metadata(&quarantine)?;
    ensure!(
        same_inode(&before, &moved) && safe_directory(&moved),
        "quarantined object identity changed; object preserved"
    );
    ensure!(
        valid_marker(
            &read_marker_from_dir(&quarantine)?,
            moved.dev(),
            moved.ino()
        ),
        "quarantined owner marker changed; object preserved"
    );
    File::open(parent)?.sync_all()?;
    remove_cache_contents(&quarantine, &lease_path_meta)?;
    File::open(parent)?.sync_all()?;
    Ok(true)
}

/// Replay stores are flat: an index, archive segment files, and the lease.
/// Remove data files first and unlink the locked marker last. If the collector
/// is killed midway, the remaining directory remains recognizable next time.
fn remove_cache_contents(dir: &Path, expected_lease: &fs::Metadata) -> Result<()> {
    let entries = fs::read_dir(dir)?;
    let mut removable = Vec::new();
    for entry in entries.take(MAX_CACHE_FILES) {
        let entry = entry?;
        if entry.file_name() == LEASE_NAME {
            continue;
        }
        let name = entry.file_name();
        let name = name
            .to_str()
            .context("non-UTF8 replay-cache child preserved")?;
        let segment = name.strip_suffix(".archive").is_some_and(|number| {
            number.len() == 20 && number.bytes().all(|byte| byte.is_ascii_digit())
        });
        ensure!(
            name == "rows.index" || segment,
            "unknown replay-cache child preserved"
        );
        let meta = fs::symlink_metadata(entry.path())?;
        ensure!(
            meta.file_type().is_file()
                && meta.uid() == unsafe { libc::getuid() }
                && meta.mode() & 0o777 == 0o600
                && meta.nlink() == 1,
            "unexpected replay-cache child preserved"
        );
        removable.push(entry.path());
    }
    for path in removable {
        fs::remove_file(path)?;
    }
    ensure!(
        fs::read_dir(dir)?.take(2).count() <= 1,
        "replay-cache child limit reached; cache preserved"
    );
    let lease_path = dir.join(LEASE_NAME);
    let current_lease = fs::symlink_metadata(&lease_path)?;
    ensure!(
        safe_lease(&current_lease) && same_inode(expected_lease, &current_lease),
        "replay-cache lease changed before removal"
    );
    fs::remove_file(&lease_path)?;
    File::open(dir)?.sync_all()?;
    fs::remove_dir(dir)?;
    Ok(())
}

fn read_marker_from_dir(dir: &Path) -> Result<Vec<u8>> {
    let path = dir.join(LEASE_NAME);
    let meta = fs::symlink_metadata(&path)?;
    ensure!(safe_lease(&meta), "unsafe replay-cache lease");
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        same_inode(&meta, &file.metadata()?),
        "replay-cache lease changed"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take((MAX_MARKER + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_MARKER, "replay-cache lease too large");
    Ok(bytes)
}

fn validate_parent(parent: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(parent)
        .with_context(|| format!("replay-cache parent {}", parent.display()))?;
    ensure!(
        meta.file_type().is_dir()
            && meta.uid() == unsafe { libc::getuid() }
            && meta.mode() & 0o022 == 0,
        "replay-cache parent is not an owned directory"
    );
    Ok(())
}

fn safe_directory(meta: &fs::Metadata) -> bool {
    meta.file_type().is_dir()
        && meta.uid() == unsafe { libc::getuid() }
        && meta.mode() & 0o777 == 0o700
}

fn safe_lease(meta: &fs::Metadata) -> bool {
    meta.file_type().is_file()
        && meta.uid() == unsafe { libc::getuid() }
        && meta.mode() & 0o777 == 0o600
        && meta.nlink() == 1
        && meta.len() as usize <= MAX_MARKER
}

fn same_inode(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn lock_exclusive(file: &File, nonblocking: bool) -> std::io::Result<()> {
    let operation = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
    // SAFETY: flock receives a valid fd and an operation flag.
    if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("NUL in replay-cache path"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("NUL in replay-cache path"))?;
    // SAFETY: both paths are NUL-terminated and RENAME_NOREPLACE prevents
    // replacing any destination that appeared after the unique-name choice.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn encode_marker(
    session: Uuid,
    generation: Uuid,
    profile: &[u8],
    device: u64,
    inode: u64,
) -> Result<Vec<u8>> {
    ensure!(profile.len() <= MAX_PROFILE, "replay profile too large");
    let mut bytes = Vec::with_capacity(8 + 4 + 16 + 16 + 2 + profile.len() + 16 + 4);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&SCHEMA.to_le_bytes());
    bytes.extend_from_slice(session.as_bytes());
    bytes.extend_from_slice(generation.as_bytes());
    bytes.extend_from_slice(&(profile.len() as u16).to_le_bytes());
    bytes.extend_from_slice(profile);
    bytes.extend_from_slice(&device.to_le_bytes());
    bytes.extend_from_slice(&inode.to_le_bytes());
    let crc = crc32fast::hash(&bytes);
    bytes.extend_from_slice(&crc.to_le_bytes());
    ensure!(bytes.len() <= MAX_MARKER, "replay marker too large");
    Ok(bytes)
}

fn valid_marker(bytes: &[u8], device: u64, inode: u64) -> bool {
    if bytes.len() < 8 + 4 + 16 + 16 + 2 + 16 + 4 || bytes.len() > MAX_MARKER {
        return false;
    }
    if &bytes[..8] != MAGIC || u32::from_le_bytes(bytes[8..12].try_into().unwrap()) != SCHEMA {
        return false;
    }
    let profile_len = u16::from_le_bytes(bytes[44..46].try_into().unwrap()) as usize;
    if profile_len == 0 || profile_len > MAX_PROFILE {
        return false;
    }
    let tail = 46 + profile_len;
    if bytes.len() != tail + 16 + 4 {
        return false;
    }
    let Ok(profile) = std::str::from_utf8(&bytes[46..tail]) else {
        return false;
    };
    let suffix = format!(";cold={}", crate::history::ARCHIVE_FORMAT);
    if !profile
        .strip_suffix(&suffix)
        .is_some_and(|source| crate::journal_policy::Policy::from_profile(source).is_some())
    {
        return false;
    }
    if Uuid::from_slice(&bytes[12..28]).is_err() || Uuid::from_slice(&bytes[28..44]).is_err() {
        return false;
    }
    let marker_device = u64::from_le_bytes(bytes[tail..tail + 8].try_into().unwrap());
    let marker_inode = u64::from_le_bytes(bytes[tail + 8..tail + 16].try_into().unwrap());
    let crc = u32::from_le_bytes(bytes[tail + 16..tail + 20].try_into().unwrap());
    marker_device == device && marker_inode == inode && crc32fast::hash(&bytes[..tail + 16]) == crc
}

fn warn(report: &mut CleanupReport, message: String) {
    if report.warnings.len() < MAX_WARNINGS {
        report.warnings.push(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::{MetadataExt, symlink},
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    const CHILD_ENV: &str = "PIGADE_REPLAY_CACHE_ORPHAN_CHILD";

    fn source() -> JournalHeader {
        JournalHeader {
            session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            profile: pigade_terminal::CORE_PROFILE.into(),
            initial_rows: 24,
            initial_cols: 80,
        }
    }

    #[test]
    fn current_lease_and_unknown_objects_are_preserved() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let cache = ReplayCache::create(parent.path(), &source())?;
        let cache_path = cache.path().to_path_buf();
        let report = cleanup(parent.path());
        assert_eq!(report.collected, 0);
        assert_eq!(report.preserved, 1);
        assert!(cache_path.is_dir());

        let source_archive = parent.path().join(format!("{}.history", Uuid::new_v4()));
        fs::create_dir(&source_archive)?;
        fs::write(source_archive.join("rows.index"), b"source")?;

        let unmarked = parent.path().join(format!("{PREFIX}unmarked"));
        fs::create_dir(&unmarked)?;
        fs::write(unmarked.join("keep"), b"owned source-like data")?;
        let link = parent.path().join(format!("{PREFIX}symlink"));
        symlink(&source_archive, &link)?;

        let report = cleanup(parent.path());
        assert_eq!(report.collected, 0);
        assert_eq!(report.preserved, 3);
        assert!(source_archive.join("rows.index").exists());
        assert!(unmarked.join("keep").exists());
        assert!(fs::symlink_metadata(&link)?.file_type().is_symlink());
        Ok(())
    }

    #[test]
    fn orphan_child_leaves_cache_for_collector() -> Result<()> {
        if let Some(parent) = std::env::var_os(CHILD_ENV) {
            let parent = Path::new(&parent);
            let _cache = ReplayCache::create(parent, &source())?;
            fs::write(parent.join("child-ready"), b"ready")?;
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }

        let parent = tempfile::tempdir()?;
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "replay_cache::tests::orphan_child_leaves_cache_for_collector",
            ])
            .env(CHILD_ENV, parent.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let ready = parent.path().join("child-ready");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                anyhow::bail!("orphan child exited before creating cache: {status}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        if !ready.exists() {
            child.kill()?;
            let _ = child.wait();
            anyhow::bail!("orphan child did not create replay cache in time");
        }
        child.kill()?;
        let _ = child.wait()?;
        fs::remove_file(ready)?;
        let report = cleanup(parent.path());
        assert_eq!(report.collected, 1, "report: {report:?}");
        assert_eq!(report.preserved, 0);
        assert_eq!(fs::read_dir(parent.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn invalid_marker_and_foreign_schema_are_retained() -> Result<()> {
        let parent = tempfile::tempdir()?;
        if let Some(parent_arg) = std::env::var_os(CHILD_ENV) {
            let cache = ReplayCache::create(Path::new(&parent_arg), &source())?;
            let marker_path = cache.path().join(LEASE_NAME);
            let mut marker = fs::read(&marker_path)?;
            match std::env::var("PIGADE_REPLAY_CACHE_MARKER_MODE").as_deref() {
                Ok("bad") => marker[0] ^= 0xff,
                Ok("foreign") => {
                    marker[8..12].copy_from_slice(&(SCHEMA + 1).to_le_bytes());
                    let crc_at = marker.len() - 4;
                    let crc = crc32fast::hash(&marker[..crc_at]);
                    marker[crc_at..].copy_from_slice(&crc.to_le_bytes());
                }
                Ok("foreign-profile") => {
                    marker[46] ^= 1;
                    let crc_at = marker.len() - 4;
                    let crc = crc32fast::hash(&marker[..crc_at]);
                    marker[crc_at..].copy_from_slice(&crc.to_le_bytes());
                }
                _ => anyhow::bail!("unknown child marker mode"),
            }
            fs::write(marker_path, marker)?;
            std::mem::forget(cache);
            return Ok(());
        }

        for mode in ["bad", "foreign", "foreign-profile"] {
            let status = Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "replay_cache::tests::invalid_marker_and_foreign_schema_are_retained",
                ])
                .env(CHILD_ENV, parent.path())
                .env("PIGADE_REPLAY_CACHE_MARKER_MODE", mode)
                .status()?;
            assert!(status.success());
        }
        let report = cleanup(parent.path());
        assert_eq!(report.collected, 0);
        assert_eq!(report.preserved, 3);
        assert_eq!(fs::read_dir(parent.path())?.count(), 3);
        Ok(())
    }

    #[test]
    fn marker_is_bound_to_exact_cache_inode_and_source_profile() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let source = source();
        let cache = ReplayCache::create(parent.path(), &source)?;
        let meta = fs::symlink_metadata(cache.path())?;
        let marker = fs::read(cache.path().join(LEASE_NAME))?;
        assert!(valid_marker(&marker, meta.dev(), meta.ino()));
        assert!(!valid_marker(
            &marker,
            meta.dev(),
            meta.ino().wrapping_add(1)
        ));
        assert_eq!(&marker[12..28], source.session_id.as_bytes());
        assert_eq!(&marker[28..44], source.generation.as_bytes());
        let profile_len = u16::from_le_bytes(marker[44..46].try_into()?) as usize;
        assert_eq!(
            &marker[46..46 + profile_len],
            format!(
                "{};cold={}",
                pigade_terminal::CORE_PROFILE,
                crate::history::ARCHIVE_FORMAT
            )
            .as_bytes()
        );
        Ok(())
    }

    #[test]
    fn resource_refusal_policy_marker_is_exact_and_orphan_cleanup_stays_leased() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let mut source = source();
        source.profile = crate::journal_policy::resource_refusal_profile();
        let cache = ReplayCache::create(parent.path(), &source)?;
        let metadata = fs::metadata(cache.path())?;
        let marker = fs::read(cache.path().join(LEASE_NAME))?;
        assert!(valid_marker(&marker, metadata.dev(), metadata.ino()));
        assert_eq!(
            cleanup(parent.path()).collected,
            0,
            "live lease must protect cache"
        );
        let ReplayCache { _lease, temporary } = cache;
        let retained = temporary.keep();
        drop(_lease);
        assert_eq!(cleanup(parent.path()).collected, 1);
        assert!(!retained.exists());
        source.profile.push_str(";unknown");
        assert!(ReplayCache::create(parent.path(), &source).is_err());
        assert_eq!(fs::read_dir(parent.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn marked_orphan_with_unknown_source_like_child_is_preserved() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let cache = ReplayCache::create(parent.path(), &source())?;
        let original_inode = fs::metadata(cache.path())?.ino();
        fs::write(
            cache.path().join("original.journal"),
            b"preserve raw source",
        )?;
        let ReplayCache { _lease, temporary } = cache;
        let _retained = temporary.keep();
        drop(_lease);
        let report = cleanup(parent.path());
        assert_eq!(report.collected, 0);
        assert_eq!(report.preserved, 1);
        let retained = fs::read_dir(parent.path())?.next().unwrap()?.path();
        assert_eq!(fs::metadata(&retained)?.ino(), original_inode);
        assert_eq!(
            fs::read(retained.join("original.journal"))?,
            b"preserve raw source"
        );
        assert!(retained.join(LEASE_NAME).exists());
        Ok(())
    }

    #[test]
    fn unsafe_parent_is_reported_without_scanning_through_symlink() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let target = tempfile::tempdir()?;
        let candidate = target.path().join(format!("{PREFIX}keep"));
        fs::create_dir(&candidate)?;
        fs::write(candidate.join("payload"), b"preserve")?;
        let link = parent.path().join("parent-link");
        symlink(target.path(), &link)?;

        let report = cleanup(&link);
        assert_eq!(report.entries_scanned, 0);
        assert_eq!(report.collected, 0);
        assert!(!report.warnings.is_empty());
        assert_eq!(fs::read(candidate.join("payload"))?, b"preserve");
        Ok(())
    }
}
