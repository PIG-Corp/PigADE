//! Atomic generation-bound checkpoint files. Raw journal is never compacted here.
use anyhow::{Context, Result, ensure};
use pigade_storage::{JournalHeader, Receipt};
use pigade_terminal::Terminal;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;
const MAGIC: &[u8; 8] = b"PIGCP001";
const MAGIC_WITH_HISTORY: &[u8; 8] = b"PIGCP002";
const LIMIT: usize = 128 * 1024 * 1024;
// Export/import/pruning can temporarily hold a second hot grid. Serialize
// checkpoint work across actors; bounded PTY queues provide backpressure while
// a generation waits. This is one part of, not proof of, the total heap budget.
static CHECKPOINT_WORK: Mutex<Vec<Validated>> = Mutex::new(Vec::new());

#[derive(PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn stamp(path: &Path) -> Result<Stamp> {
    let m = fs::metadata(path)?;
    Ok(Stamp {
        device: m.dev(),
        inode: m.ino(),
        length: m.len(),
        modified: (m.mtime(), m.mtime_nsec()),
        changed: (m.ctime(), m.ctime_nsec()),
    })
}
struct Validated {
    path: PathBuf,
    stamp: Stamp,
    session: Uuid,
    generation: Uuid,
    profile: String,
    history: Option<crate::history::Checkpoint>,
}
fn remember(
    cache: &mut Vec<Validated>,
    path: &Path,
    header: &JournalHeader,
    history: Option<crate::history::Checkpoint>,
) -> Result<()> {
    if cache.len() >= 256 {
        cache.clear();
    }
    cache.retain(|entry| entry.path != path);
    cache.push(Validated {
        path: path.into(),
        stamp: stamp(path)?,
        session: header.session_id,
        generation: header.generation,
        profile: header.profile.clone(),
        history,
    });
    Ok(())
}
fn valid_for_retention(
    cache: &mut Vec<Validated>,
    path: &Path,
    header: &JournalHeader,
) -> Result<()> {
    let current = stamp(path)?;
    if let Some(known) = cache.iter().find(|entry| {
        entry.path == path
            && entry.stamp == current
            && entry.session == header.session_id
            && entry.generation == header.generation
            && entry.profile == header.profile
    }) {
        // File bytes were already exported+validated, or fully imported, in this
        // process. Still validate the bounded external cold-root dependency.
        if let Some(history) = &known.history {
            crate::history::validate_checkpoint(
                path.parent().context("checkpoint directory")?,
                header,
                history,
            )?;
        }
        return Ok(());
    }
    let loaded = load(path, header)?;
    remember(cache, path, header, loaded.history)
}

pub struct Loaded {
    pub terminal: Terminal,
    pub sequence: u64,
    pub offset: u64,
    pub history: Option<crate::history::Checkpoint>,
}
pub fn publish(
    root: &Path,
    header: &JournalHeader,
    receipt: Receipt,
    terminal: &Terminal,
) -> Result<()> {
    publish_with_history(root, header, receipt, terminal, None)
}
pub fn publish_with_history(
    root: &Path,
    header: &JournalHeader,
    receipt: Receipt,
    terminal: &Terminal,
    history: Option<crate::history::Checkpoint>,
) -> Result<()> {
    let mut work = CHECKPOINT_WORK
        .lock()
        .map_err(|_| anyhow::anyhow!("checkpoint scheduler poisoned"))?;
    let bytes = terminal.checkpoint()?;
    let mut history_bytes = Vec::new();
    if let Some(history) = &history {
        ensure!(
            history.state.applied_sequence == receipt.sequence,
            "history/checkpoint sequence mismatch"
        );
        let metadata = serde_json::to_vec(history)?;
        ensure!(
            metadata.len() <= 32768,
            "history checkpoint metadata exceeds budget"
        );
        history_bytes.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        history_bytes.extend_from_slice(&metadata);
    }
    let length = bytes.len() + history_bytes.len();
    ensure!(length <= LIMIT, "checkpoint exceeds encoded byte budget");
    let mut prefix = Vec::with_capacity(64);
    prefix.extend_from_slice(if history.is_some() {
        MAGIC_WITH_HISTORY
    } else {
        MAGIC
    });
    prefix.extend_from_slice(header.session_id.as_bytes());
    prefix.extend_from_slice(header.generation.as_bytes());
    prefix.extend_from_slice(&receipt.sequence.to_le_bytes());
    prefix.extend_from_slice(&receipt.end_offset.to_le_bytes());
    prefix.extend_from_slice(&(length as u32).to_le_bytes());
    let mut crc = crc32fast::Hasher::new();
    crc.update(&prefix);
    crc.update(&history_bytes);
    crc.update(&bytes);
    prefix.extend_from_slice(&crc.finalize().to_le_bytes());
    let temporary = root.join(format!("{}.{}.tmp", header.generation, Uuid::new_v4()));
    let destination = root.join(format!(
        "{}.{:020}.checkpoint",
        header.generation, receipt.sequence
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&prefix)?;
    file.write_all(&history_bytes)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, &destination)?;
    File::open(root)?.sync_all()?;
    // Export validated the complete semantic DTO before this atomic write.
    // Avoid reconstructing all three unchanged hot grids on every publication.
    remember(&mut work, &destination, header, history)?;
    // Keep the last three files. Old raw journal remains the full replay oracle.
    let mut checkpoints = candidates(root, header.generation)?;
    checkpoints.sort();
    let mut valid = 0;
    for path in checkpoints.into_iter().rev() {
        if valid_for_retention(&mut work, &path, header).is_ok() {
            valid += 1;
            if valid > 3 {
                fs::remove_file(&path)?;
                work.retain(|entry| entry.path != path);
            }
        }
    }
    File::open(root)?.sync_all()?;
    Ok(())
}
fn candidates(root: &Path, generation: Uuid) -> Result<Vec<PathBuf>> {
    let prefix = format!("{generation}.");
    let mut paths = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with(&prefix) && s.ends_with(".checkpoint"))
        {
            ensure!(
                paths.len() < 4096,
                "checkpoint directory exceeds bounded candidate enumeration; archive inspection required"
            );
            paths.push(path);
        }
    }
    Ok(paths)
}
pub struct Selection {
    pub loaded: Option<Loaded>,
    pub warnings: Vec<String>,
}
pub fn latest(root: &Path, header: &JournalHeader, max_offset: u64) -> Result<Selection> {
    let mut warnings = Vec::new();
    let mut paths = candidates(root, header.generation)?;
    paths.sort();
    for path in paths.into_iter().rev() {
        match load(&path, header) {
            Ok(snapshot) if snapshot.offset <= max_offset => {
                return Ok(Selection {
                    loaded: Some(snapshot),
                    warnings,
                });
            }
            Ok(_) => warnings.push(format!(
                "checkpoint watermark exceeds journal; preserved {}",
                path.display()
            )),
            Err(error) => warnings.push(format!(
                "checkpoint rejected, preserved {}: {error:#}",
                path.display()
            )),
        }
    }
    Ok(Selection {
        loaded: None,
        warnings,
    })
}
fn load(path: &Path, header: &JournalHeader) -> Result<Loaded> {
    let mut file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= LIMIT as u64 + 64,
        "oversize checkpoint file"
    );
    let mut prefix = [0; 64];
    file.read_exact(&mut prefix)?;
    ensure!(
        (&prefix[..8] == MAGIC || &prefix[..8] == MAGIC_WITH_HISTORY)
            && &prefix[8..24] == header.session_id.as_bytes()
            && &prefix[24..40] == header.generation.as_bytes(),
        "checkpoint identity/schema mismatch"
    );
    let sequence = u64::from_le_bytes(prefix[40..48].try_into()?);
    let offset = u64::from_le_bytes(prefix[48..56].try_into()?);
    let length = u32::from_le_bytes(prefix[56..60].try_into()?) as usize;
    ensure!(
        length <= LIMIT && file.metadata()?.len() == 64 + length as u64,
        "checkpoint size mismatch"
    );
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    let mut crc = crc32fast::Hasher::new();
    crc.update(&prefix[..60]);
    crc.update(&bytes);
    ensure!(
        crc.finalize() == u32::from_le_bytes(prefix[60..64].try_into()?),
        "checkpoint checksum mismatch"
    );
    let (history, model_bytes) = if &prefix[..8] == MAGIC_WITH_HISTORY {
        ensure!(bytes.len() >= 4, "missing history checkpoint metadata");
        let length = u32::from_le_bytes(bytes[..4].try_into()?) as usize;
        ensure!(
            length <= 32768 && length <= bytes.len() - 4,
            "invalid history checkpoint metadata length"
        );
        let history: crate::history::Checkpoint = serde_json::from_slice(&bytes[4..4 + length])?;
        ensure!(
            history.state.applied_sequence == sequence,
            "history/checkpoint source sequence mismatch"
        );
        crate::history::validate_checkpoint(
            path.parent().context("checkpoint directory")?,
            header,
            &history,
        )?;
        (Some(history), &bytes[4 + length..])
    } else {
        (None, bytes.as_slice())
    };
    Ok(Loaded {
        terminal: Terminal::from_checkpoint(model_bytes, false)?,
        sequence,
        offset,
        history,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use pigade_storage::{EventKind, Journal};
    use pigade_terminal::{CORE_PROFILE, Geometry};
    #[test]
    fn older_valid_checkpoint_survives_newer_watermark_beyond_torn_journal() -> Result<()> {
        use std::io::{Seek, SeekFrom};
        let root = tempfile::tempdir()?;
        let header = JournalHeader {
            session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            profile: CORE_PROFILE.into(),
            initial_rows: 8,
            initial_cols: 40,
        };
        let path = root.path().join("generation.journal");
        let mut journal = Journal::create(&path, header.clone())?;
        let initial_offset = pigade_storage::JournalReader::open(&path)?.valid_offset();
        let mut t = Terminal::new(Geometry::new(8, 40)?, false);
        let first = journal.append(EventKind::Output, b"FIRST")?;
        t.output(b"FIRST");
        publish(root.path(), &header, first, &t)?;
        let second = journal.append(EventKind::Output, b"SECOND")?;
        t.output(b"SECOND");
        publish(root.path(), &header, second, &t)?;
        drop(journal);
        let mut file = OpenOptions::new().write(true).open(&path)?;
        file.set_len(first.end_offset)?;
        file.seek(SeekFrom::Start(initial_offset + 33))?;
        file.write_all(b"!")?;
        file.sync_all()?;
        let recovered = crate::recover_journal(&path)?;
        assert_eq!(recovered.sequence, first.sequence);
        assert_eq!(recovered.recovery_path, "checkpoint-tail");
        assert!(recovered.frame.text().starts_with("FIRST"));
        assert!(recovered.storage_error.is_some());
        assert_eq!(crate::replay_journal(&path)?.sequence, 0);
        Ok(())
    }
    #[test]
    fn generation_bound_atomic_files_reject_damage_and_fall_back() -> Result<()> {
        let root = tempfile::tempdir()?;
        let header = JournalHeader {
            session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            profile: CORE_PROFILE.into(),
            initial_rows: 8,
            initial_cols: 40,
        };
        let mut journal = Journal::create(root.path().join("test.journal"), header.clone())?;
        let mut t = Terminal::new(Geometry::new(8, 40)?, false);
        let first = journal.append(EventKind::Output, b"first")?;
        t.output(b"first");
        publish(root.path(), &header, first, &t)?;
        let second = journal.append(EventKind::Output, b"second")?;
        t.output(b"second");
        publish(root.path(), &header, second, &t)?;
        let restored = latest(root.path(), &header, u64::MAX)?.loaded.unwrap();
        assert_eq!(restored.sequence, second.sequence);
        assert_eq!(restored.terminal.checkpoint()?, t.checkpoint()?);
        let mut paths = candidates(root.path(), header.generation)?;
        paths.sort();
        let path = paths.last().unwrap();
        let mut bytes = fs::read(path)?;
        bytes[64] ^= 1;
        fs::write(path, bytes)?;
        assert_eq!(
            latest(root.path(), &header, u64::MAX)?
                .loaded
                .unwrap()
                .sequence,
            first.sequence
        );
        let mut wrong = header.clone();
        wrong.generation = Uuid::new_v4();
        assert!(load(paths.first().unwrap(), &wrong).is_err());
        fs::write(root.path().join("interrupted.tmp"), b"partial")?;
        assert_eq!(
            latest(root.path(), &header, u64::MAX)?
                .loaded
                .unwrap()
                .sequence,
            first.sequence
        );
        Ok(())
    }
}
