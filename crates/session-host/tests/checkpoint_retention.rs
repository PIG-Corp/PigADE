//! Retention keeps three valid checkpoints and preserves damaged/interrupted files.
use anyhow::Result;
use pigade_session_host::checkpoint;
use pigade_storage::{EventKind, Journal, JournalHeader};
use pigade_terminal::{CORE_PROFILE, Geometry, Terminal};
use std::{fs, path::Path};
use tempfile::TempDir;
use uuid::Uuid;

fn header() -> JournalHeader {
    JournalHeader {
        session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        profile: CORE_PROFILE.into(),
        initial_rows: 8,
        initial_cols: 40,
    }
}

fn checkpoint_files(root: &Path, generation: Uuid) -> Result<Vec<String>> {
    let mut names = fs::read_dir(root)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            (name.starts_with(&format!("{generation}.")) && name.ends_with(".checkpoint"))
                .then_some(name)
        })
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

fn sequence_name(generation: Uuid, sequence: u64) -> String {
    format!("{generation}.{sequence:020}.checkpoint")
}

#[test]
fn publication_retains_the_last_three_valid_checkpoints() -> Result<()> {
    let root = tempfile::tempdir()?;
    let header = header();
    let mut journal = Journal::create(root.path().join("session.journal"), header.clone())?;
    let terminal = Terminal::new(Geometry::new(8, 40)?, false);

    for sequence in 1..=5 {
        let receipt = journal.append(EventKind::Output, b"")?;
        assert_eq!(receipt.sequence, sequence);
        checkpoint::publish(root.path(), &header, receipt, &terminal)?;
        let names = checkpoint_files(root.path(), header.generation)?;
        let first_retained = sequence.saturating_sub(2).max(1);
        let expected = (first_retained..=sequence)
            .map(|n| sequence_name(header.generation, n))
            .collect::<Vec<_>>();
        assert_eq!(names, expected);
    }

    let selected = checkpoint::latest(root.path(), &header, u64::MAX)?;
    assert_eq!(selected.loaded.expect("latest checkpoint").sequence, 5);
    Ok(())
}

#[test]
fn corrupted_cached_checkpoint_does_not_count_toward_retention() -> Result<()> {
    let root = tempfile::tempdir()?;
    let header = header();
    let mut journal = Journal::create(root.path().join("session.journal"), header.clone())?;
    let terminal = Terminal::new(Geometry::new(8, 40)?, false);
    let mut receipts = Vec::new();
    for _ in 0..3 {
        let receipt = journal.append(EventKind::Output, b"")?;
        checkpoint::publish(root.path(), &header, receipt, &terminal)?;
        receipts.push(receipt);
    }

    // Change a previously validated file in place while preserving its length.
    let damaged = root
        .path()
        .join(sequence_name(header.generation, receipts[0].sequence));
    let mut bytes = fs::read(&damaged)?;
    bytes[0] ^= 0xff;
    fs::write(&damaged, bytes)?;

    let fourth = journal.append(EventKind::Output, b"")?;
    checkpoint::publish(root.path(), &header, fourth, &terminal)?;

    assert!(damaged.is_file(), "damaged checkpoint must be preserved");
    assert_eq!(
        checkpoint_files(root.path(), header.generation)?,
        (1..=4)
            .map(|n| sequence_name(header.generation, n))
            .collect::<Vec<_>>()
    );
    let selected = checkpoint::latest(root.path(), &header, u64::MAX)?;
    assert_eq!(
        selected.loaded.expect("latest valid checkpoint").sequence,
        4
    );
    Ok(())
}

#[test]
fn generation_and_profile_mismatches_do_not_reuse_cached_validation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let header = header();
    let mut journal = Journal::create(root.path().join("session.journal"), header.clone())?;
    let terminal = Terminal::new(Geometry::new(8, 40)?, false);
    for _ in 0..3 {
        let receipt = journal.append(EventKind::Output, b"")?;
        checkpoint::publish(root.path(), &header, receipt, &terminal)?;
    }

    let mut wrong_generation = header.clone();
    wrong_generation.generation = Uuid::new_v4();
    assert!(
        checkpoint::latest(root.path(), &wrong_generation, u64::MAX)?
            .loaded
            .is_none()
    );

    // The same on-disk identity with a different profile must force a fresh
    // validation rather than trusting the previous cache entry.
    let damaged = root.path().join(sequence_name(header.generation, 1));
    let mut bytes = fs::read(&damaged)?;
    bytes[0] ^= 0xff;
    fs::write(&damaged, bytes)?;
    let mut wrong_profile = header.clone();
    wrong_profile.profile.push_str(";different-profile");
    let next = journal.append(EventKind::Output, b"")?;
    checkpoint::publish(root.path(), &wrong_profile, next, &terminal)?;

    assert!(damaged.is_file());
    assert_eq!(checkpoint_files(root.path(), header.generation)?.len(), 4);
    Ok(())
}

#[test]
fn interrupted_temporary_file_is_ignored_and_preserved() -> Result<()> {
    let root: TempDir = tempfile::tempdir()?;
    let header = header();
    let mut journal = Journal::create(root.path().join("session.journal"), header.clone())?;
    let terminal = Terminal::new(Geometry::new(8, 40)?, false);
    let first = journal.append(EventKind::Output, b"")?;
    checkpoint::publish(root.path(), &header, first, &terminal)?;

    let interrupted = root
        .path()
        .join(format!("{}.interrupted.tmp", header.generation));
    fs::write(&interrupted, b"partial checkpoint write")?;
    let second = journal.append(EventKind::Output, b"")?;
    checkpoint::publish(root.path(), &header, second, &terminal)?;

    assert!(interrupted.is_file());
    assert_eq!(
        checkpoint::latest(root.path(), &header, u64::MAX)?
            .loaded
            .expect("temporary file must not hide valid checkpoint")
            .sequence,
        2
    );
    Ok(())
}
