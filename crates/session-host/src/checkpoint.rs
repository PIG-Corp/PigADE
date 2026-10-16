//! Atomic generation-bound checkpoint files. Raw journal is never compacted here.
use anyhow::{Result, ensure};
use pigade_storage::{JournalHeader, Receipt};
use pigade_terminal::Terminal;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use uuid::Uuid;
const MAGIC: &[u8; 8] = b"PIGCP001";
const LIMIT: usize = 128 * 1024 * 1024;

pub struct Loaded {
    pub terminal: Terminal,
    pub sequence: u64,
    pub offset: u64,
}
pub fn publish(
    root: &Path,
    header: &JournalHeader,
    receipt: Receipt,
    terminal: &Terminal,
) -> Result<()> {
    let bytes = terminal.checkpoint()?;
    ensure!(
        bytes.len() <= LIMIT,
        "checkpoint exceeds encoded byte budget"
    );
    let mut prefix = Vec::with_capacity(64);
    prefix.extend_from_slice(MAGIC);
    prefix.extend_from_slice(header.session_id.as_bytes());
    prefix.extend_from_slice(header.generation.as_bytes());
    prefix.extend_from_slice(&receipt.sequence.to_le_bytes());
    prefix.extend_from_slice(&receipt.end_offset.to_le_bytes());
    prefix.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    let mut crc = crc32fast::Hasher::new();
    crc.update(&prefix);
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
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, &destination)?;
    File::open(root)?.sync_all()?;
    // Keep the last three files. Old raw journal remains the full replay oracle.
    let mut checkpoints = candidates(root, header.generation)?;
    checkpoints.sort();
    let mut valid = 0;
    for path in checkpoints.into_iter().rev() {
        if load(&path, header).is_ok() {
            valid += 1;
            if valid > 3 {
                fs::remove_file(path)?;
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
        &prefix[..8] == MAGIC
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
    Ok(Loaded {
        terminal: Terminal::from_checkpoint(&bytes, false)?,
        sequence,
        offset,
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
