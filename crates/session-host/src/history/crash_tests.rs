//! Process-kill recovery checks at archive durability boundaries.
//!
//! These exercise SIGKILL/reopen behavior on the current OS and filesystem. They
//! do not simulate power loss, kernel cache loss, or a VM reboot.
use super::*;
use pigade_storage::{EventKind, Journal, Receipt};
use pigade_terminal::{CORE_PROFILE, Geometry, Terminal};
use std::{
    collections::BTreeMap,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

const CHILD_STAGE: &str = "PIGADE_CRASH_TEST_STAGE";
const CHILD_ROOT: &str = "PIGADE_CRASH_TEST_ROOT";
const CHILD_MARKER: &str = "PIGADE_CRASH_TEST_MARKER";
const PARENT_WAIT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug)]
enum Cut {
    RawFsyncBeforeModel,
    ArchiveIndexAppendBeforeCommit,
    ArchiveFsyncBeforeIndexSync,
    HistoryCommitBeforeCheckpoint,
}

impl Cut {
    fn name(self) -> &'static str {
        match self {
            Self::RawFsyncBeforeModel => "raw_fsync_before_model",
            Self::ArchiveIndexAppendBeforeCommit => "archive_index_append_before_commit",
            Self::ArchiveFsyncBeforeIndexSync => "archive_fsync_before_index_sync",
            Self::HistoryCommitBeforeCheckpoint => "history_commit_before_checkpoint",
        }
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn launch_and_cut(cut: Cut) -> Result<tempfile::TempDir> {
    let root = tempfile::tempdir()?;
    let marker = root.path().join("cut.marker");
    let executable = std::env::current_exe()?;
    let child = Command::new(executable)
        .args([
            "--exact",
            "history::crash_tests::crash_writer_child",
            "--nocapture",
        ])
        .env(CHILD_STAGE, cut.name())
        .env(CHILD_ROOT, root.path())
        .env(CHILD_MARKER, &marker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut child = ChildGuard(child);
    let started = Instant::now();
    loop {
        if marker.is_file() {
            break;
        }
        if let Some(status) = child.0.try_wait()? {
            anyhow::bail!(
                "crash writer exited before marker for {}: {status}",
                cut.name()
            );
        }
        if started.elapsed() >= PARENT_WAIT {
            anyhow::bail!(
                "timed out waiting for crash writer marker at {}",
                cut.name()
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    // Kill only the exact child started above. SIGKILL is intentional: no Rust
    // destructors run in the writer process at the chosen durability boundary.
    child.0.kill()?;
    let status = child.0.wait()?;
    anyhow::ensure!(!status.success(), "SIGKILL child unexpectedly succeeded");
    Ok(root)
}

fn source() -> JournalHeader {
    JournalHeader {
        session_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        profile: CORE_PROFILE.into(),
        initial_rows: 8,
        initial_cols: 40,
    }
}

fn apply(
    journal: &mut Journal,
    terminal: &mut Terminal,
    history: &History,
    kind: EventKind,
    bytes: &[u8],
) -> Result<Receipt> {
    let receipt = journal.append(kind, bytes)?;
    history.begin(receipt.sequence)?;
    match kind {
        EventKind::Output => terminal.output(bytes),
        EventKind::Resize => terminal.resize(serde_json::from_slice(bytes)?),
        EventKind::SyncTimeout => terminal.sync_timeout(),
        EventKind::Exit => (),
    }
    history.commit()?;
    Ok(receipt)
}

/// Child process entry point. Parent invokes this exact test with stage
/// variables; an ordinary test-harness invocation is a harmless no-op.
#[test]
fn crash_writer_child() -> Result<()> {
    let (Some(stage), Some(root), Some(marker)) = (
        std::env::var_os(CHILD_STAGE),
        std::env::var_os(CHILD_ROOT),
        std::env::var_os(CHILD_MARKER),
    ) else {
        return Ok(());
    };
    let stage = stage.to_string_lossy();
    let root = PathBuf::from(root);
    let marker = PathBuf::from(marker);
    let source = source();
    let journal_path = root.join(format!("{}.journal", source.generation));
    let mut journal = Journal::create(&journal_path, source.clone())?;
    let history = History::live(&root, &source)?;
    let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
    terminal.set_history_observer(Some(history.clone()));

    // Make the checkpoint contain durable cold rows so all cut points recover
    // from a real committed base plus the second output event.
    let first = apply(
        &mut journal,
        &mut terminal,
        &history,
        EventKind::Output,
        b"BASE\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9\r\n10\r\n11\r\n12\r\n13\r\n14\r\n15\r\n16\r\n17\r\n18\r\n19\r\n20\r\n",
    )?;
    crate::checkpoint::publish_with_history(
        &root,
        &source,
        first,
        &terminal,
        Some(history.checkpoint()?),
    )?;

    let payload = b"AFTER\x1b[3J\x1bcFINAL";
    let receipt = journal.append(EventKind::Output, payload)?;
    if stage == Cut::RawFsyncBeforeModel.name() {
        park_at_cut(&marker)?;
        return Ok(());
    }
    history.begin(receipt.sequence)?;
    terminal.output(payload);
    if stage == Cut::ArchiveIndexAppendBeforeCommit.name() {
        park_at_cut(&marker)?;
        return Ok(());
    }
    if stage == Cut::ArchiveFsyncBeforeIndexSync.name() {
        let mut inner = history.inner.lock().expect("history lock");
        inner
            .store
            .writer
            .as_mut()
            .context("expected appended cold records")?
            .commit()?;
        drop(inner);
        park_at_cut(&marker)?;
        return Ok(());
    }
    if stage == Cut::HistoryCommitBeforeCheckpoint.name() {
        history.commit()?;
        park_at_cut(&marker)?;
        return Ok(());
    }
    anyhow::bail!("unknown crash test stage: {stage}")
}

fn park_at_cut(marker: &Path) -> Result<()> {
    let mut file = File::create(marker)?;
    file.write_all(b"ready")?;
    file.sync_all()?;
    // A deadline keeps a broken parent from leaving a parked child behind.
    thread::sleep(Duration::from_secs(60));
    anyhow::bail!("parent did not SIGKILL child at durability cut")
}

fn snapshot_files(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
        for item in fs::read_dir(current)? {
            let item = item?;
            let path = item.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                visit(root, &path, files)?;
            } else if metadata.is_file() {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                let source_artifact = name == "rows.index"
                    || ["journal", "checkpoint", "archive"]
                        .iter()
                        .any(|extension| path.extension().is_some_and(|got| got == *extension));
                if source_artifact {
                    files.insert(path.strip_prefix(root)?.to_path_buf(), fs::read(path)?);
                }
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

fn assert_crash_recovery(cut: Cut) -> Result<()> {
    let root = launch_and_cut(cut)?;
    let journal = fs::read_dir(root.path())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "journal")
        })
        .context("child did not create journal")?;
    let originals_before = snapshot_files(root.path())?;

    let (view, recovered, recovered_history) = crate::restore_model(&journal, true)?;
    anyhow::ensure!(
        view.storage_error.is_none(),
        "recovery reported storage error"
    );
    let recovered_history = recovered_history.context("recovery omitted cold history")?;
    let (full_view, full, full_history) = crate::restore_model(&journal, false)?;
    anyhow::ensure!(
        full_view.storage_error.is_none(),
        "full replay reported storage error"
    );
    anyhow::ensure!(
        recovered.checkpoint()? == full.checkpoint()?,
        "crash recovery terminal/parser state differs from full replay at {}",
        cut.name()
    );
    let full_history = full_history.context("full replay omitted cold history")?;
    anyhow::ensure!(
        recovered_history.state() == full_history.state(),
        "crash recovery cold state differs from full replay at {}",
        cut.name()
    );
    anyhow::ensure!(
        full_history.summary().records > 0,
        "fixture has no cold rows"
    );
    for ordinal in 0..full_history.summary().records {
        anyhow::ensure!(
            recovered_history.record(ordinal)? == full_history.record(ordinal)?,
            "cold record {ordinal} differs at {}",
            cut.name()
        );
    }
    let proof = crate::verify_recovery(&journal)?;
    anyhow::ensure!(
        proof.complete_state_equal && proof.frame_equal,
        "recovery oracle mismatch"
    );
    anyhow::ensure!(proof.cold_history_equal, "cold-history oracle mismatch");
    drop(recovered_history);
    drop(full_history);
    drop(recovered);
    drop(full);
    let originals_after = snapshot_files(root.path())?;
    anyhow::ensure!(
        originals_after == originals_before,
        "recovery changed originals at {}: before={:?}, after={:?}",
        cut.name(),
        originals_before.keys().collect::<Vec<_>>(),
        originals_after.keys().collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn sigkill_after_raw_fsync_before_model_recovers() -> Result<()> {
    assert_crash_recovery(Cut::RawFsyncBeforeModel)
}

#[test]
fn sigkill_after_archive_and_index_append_before_commit_recovers() -> Result<()> {
    assert_crash_recovery(Cut::ArchiveIndexAppendBeforeCommit)
}

#[test]
fn sigkill_after_archive_data_fsync_before_index_sync_recovers() -> Result<()> {
    assert_crash_recovery(Cut::ArchiveFsyncBeforeIndexSync)
}

#[test]
fn sigkill_after_history_commit_before_checkpoint_recovers() -> Result<()> {
    assert_crash_recovery(Cut::HistoryCommitBeforeCheckpoint)
}
