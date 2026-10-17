//! Atomic generation-bound checkpoint files. Raw journal is never compacted here.
use anyhow::{Context, Result, ensure};
use pigade_storage::{JournalHeader, Receipt};
use pigade_terminal::Terminal;
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Condvar, Mutex, OnceLock, mpsc},
};
use uuid::Uuid;
const MAGIC: &[u8; 8] = b"PIGCP001";
const MAGIC_WITH_HISTORY: &[u8; 8] = b"PIGCP002";
const LIMIT: usize = 128 * 1024 * 1024;
const COMPLETION_ERROR_LIMIT: usize = 1024;
// Retention validation/import cache; the separate singleflight permit guards
// actor-side capture through async persistence. Checkpoint work is bounded, but
// this cache alone is not proof of the total heap budget.
static CHECKPOINT_WORK: Mutex<Vec<Validated>> = Mutex::new(Vec::new());

// A checkpoint is captured on the actor thread, but the potentially slow
// filesystem durability work is performed by one process-wide worker.
static CHECKPOINT_PERMIT: OnceLock<Mutex<bool>> = OnceLock::new();
static CHECKPOINT_PERMIT_CHANGED: Condvar = Condvar::new();
static CHECKPOINT_WORKER: OnceLock<std::result::Result<mpsc::SyncSender<Job>, String>> =
    OnceLock::new();
#[cfg(test)]
type TestPause = (Uuid, mpsc::SyncSender<()>, mpsc::Receiver<()>);
#[cfg(test)]
static TEST_PAUSE: OnceLock<Mutex<Option<TestPause>>> = OnceLock::new();

struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut busy) = permit_lock().lock() {
            *busy = false;
            CHECKPOINT_PERMIT_CHANGED.notify_one();
        }
    }
}
fn permit_lock() -> &'static Mutex<bool> {
    CHECKPOINT_PERMIT.get_or_init(|| Mutex::new(false))
}
fn try_permit() -> Result<Option<Permit>> {
    let mut busy = permit_lock()
        .lock()
        .map_err(|_| anyhow::anyhow!("checkpoint scheduler poisoned"))?;
    if *busy {
        return Ok(None);
    }
    *busy = true;
    Ok(Some(Permit))
}
fn wait_permit() -> Result<Permit> {
    let mut busy = permit_lock()
        .lock()
        .map_err(|_| anyhow::anyhow!("checkpoint scheduler poisoned"))?;
    while *busy {
        busy = CHECKPOINT_PERMIT_CHANGED
            .wait(busy)
            .map_err(|_| anyhow::anyhow!("checkpoint scheduler poisoned"))?;
    }
    *busy = true;
    Ok(Permit)
}

struct BoundedError(String);
impl fmt::Write for BoundedError {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.0.len() + value.len() <= COMPLETION_ERROR_LIMIT {
            self.0.push_str(value);
            return Ok(());
        }
        let marker = "…";
        let budget = COMPLETION_ERROR_LIMIT.saturating_sub(marker.len());
        if self.0.len() > budget {
            let mut end = budget;
            while !self.0.is_char_boundary(end) {
                end -= 1;
            }
            self.0.truncate(end);
        }
        let room = budget - self.0.len();
        let mut end = room.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        self.0.push_str(&value[..end]);
        self.0.push_str(marker);
        Err(fmt::Error)
    }
}
fn bounded_error(error: &anyhow::Error) -> String {
    let mut bounded = BoundedError(String::with_capacity(COMPLETION_ERROR_LIMIT));
    let _ = fmt::Write::write_fmt(&mut bounded, format_args!("{error:#}"));
    bounded.0
}

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

struct Job {
    root: PathBuf,
    header: JournalHeader,
    receipt: Receipt,
    terminal_bytes: Vec<u8>,
    history: Option<crate::history::Checkpoint>,
    completion: mpsc::SyncSender<std::result::Result<(), String>>,
    _permit: Permit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    Pending,
    Succeeded,
    Failed(String),
}

pub struct Pending {
    completion: mpsc::Receiver<std::result::Result<(), String>>,
    cached: Mutex<Option<Completion>>,
}

impl Pending {
    pub fn poll(&self) -> Completion {
        let mut cached = match self.cached.lock() {
            Ok(cached) => cached,
            Err(_) => return Completion::Failed("checkpoint completion poisoned".into()),
        };
        if let Some(result) = cached.as_ref() {
            return result.clone();
        }
        let result = match self.completion.try_recv() {
            Ok(Ok(())) => Completion::Succeeded,
            Ok(Err(message)) => Completion::Failed(message),
            Err(mpsc::TryRecvError::Empty) => return Completion::Pending,
            Err(mpsc::TryRecvError::Disconnected) => {
                Completion::Failed("checkpoint worker disconnected before completion".into())
            }
        };
        *cached = Some(result.clone());
        result
    }

    pub fn wait(self) -> Result<()> {
        if let Some(result) = self
            .cached
            .into_inner()
            .map_err(|_| anyhow::anyhow!("checkpoint completion poisoned"))?
        {
            return completion_result(result);
        }
        match self.completion.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(anyhow::anyhow!(message)),
            Err(_) => Err(anyhow::anyhow!(
                "checkpoint worker disconnected before completion"
            )),
        }
    }
}

fn completion_result(completion: Completion) -> Result<()> {
    match completion {
        Completion::Pending => unreachable!("pending completion is not cached"),
        Completion::Succeeded => Ok(()),
        Completion::Failed(message) => Err(anyhow::anyhow!(message)),
    }
}

fn worker_sender() -> Result<&'static mpsc::SyncSender<Job>> {
    let worker = CHECKPOINT_WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<Job>(1);
        match std::thread::Builder::new()
            .name("pigade-checkpoint-writer".into())
            .spawn(move || {
                while let Ok(mut job) = receiver.recv() {
                    #[cfg(test)]
                    if let Ok(mut pause) = TEST_PAUSE.get_or_init(|| Mutex::new(None)).lock()
                        && pause
                            .as_ref()
                            .is_some_and(|(generation, _, _)| *generation == job.header.generation)
                        && let Some((_, started, resume)) = pause.take()
                    {
                        let _ = started.send(());
                        let _ = resume.recv_timeout(std::time::Duration::from_secs(20));
                    }
                    let result = persist_snapshot(
                        &job.root,
                        &job.header,
                        job.receipt,
                        &job.terminal_bytes,
                        job.history.take(),
                    )
                    .map_err(|error| bounded_error(&error));
                    let completion = job.completion.clone();
                    // Job field order drops captured bytes before its permit,
                    // including on unwind. A new capture cannot overlap them.
                    drop(job);
                    let _ = completion.send(result);
                }
            }) {
            Ok(_) => Ok(sender),
            Err(error) => Err(format!("cannot start checkpoint worker: {error}")),
        }
    });
    worker.as_ref().map_err(|message| anyhow::anyhow!(message))
}

/// Capture an immutable checkpoint after its journal/history receipt is durable,
/// then persist it on the bounded process-wide checkpoint worker. `None` means
/// another generation owns the single snapshot/persist permit; no terminal
/// export or history clone has occurred in that case.
pub fn schedule(
    root: &Path,
    header: &JournalHeader,
    receipt: Receipt,
    terminal: &Terminal,
    history: Option<crate::history::Checkpoint>,
) -> Result<Option<Pending>> {
    schedule_with_capture(root, header, receipt, history, || terminal.checkpoint())
}

fn schedule_with_capture<F>(
    root: &Path,
    header: &JournalHeader,
    receipt: Receipt,
    history: Option<crate::history::Checkpoint>,
    capture: F,
) -> Result<Option<Pending>>
where
    F: FnOnce() -> Result<Vec<u8>>,
{
    validate_receipt(receipt)?;
    let Some(permit) = try_permit()? else {
        return Ok(None);
    };
    let sender = worker_sender()?;
    let terminal_bytes = capture()?;
    validate_history(&history, receipt)?;
    let (completion_tx, completion_rx) = mpsc::sync_channel(1);
    let job = Job {
        root: root.to_owned(),
        header: header.clone(),
        receipt,
        terminal_bytes,
        history,
        completion: completion_tx,
        _permit: permit,
    };
    sender
        .try_send(job)
        .map_err(|error| anyhow::anyhow!("checkpoint worker unavailable: {error}"))?;
    Ok(Some(Pending {
        completion: completion_rx,
        cached: Mutex::new(None),
    }))
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
    validate_receipt(receipt)?;
    let _permit = wait_permit()?;
    let bytes = terminal.checkpoint()?;
    persist_snapshot(root, header, receipt, &bytes, history)
}

fn validate_receipt(receipt: Receipt) -> Result<()> {
    ensure!(
        receipt.kind != pigade_storage::EventKind::OutputRejectedResource,
        "resource-refused output cannot be a checkpoint watermark"
    );
    Ok(())
}

fn validate_history(history: &Option<crate::history::Checkpoint>, receipt: Receipt) -> Result<()> {
    validate_receipt(receipt)?;
    if let Some(history) = history {
        ensure!(
            history.state.applied_sequence == receipt.sequence,
            "history/checkpoint sequence mismatch"
        );
        ensure!(
            serde_json::to_vec(history)?.len() <= 32768,
            "history checkpoint metadata exceeds budget"
        );
    }
    Ok(())
}

fn persist_snapshot(
    root: &Path,
    header: &JournalHeader,
    receipt: Receipt,
    bytes: &[u8],
    history: Option<crate::history::Checkpoint>,
) -> Result<()> {
    validate_history(&history, receipt)?;
    let mut work = CHECKPOINT_WORK
        .lock()
        .map_err(|_| anyhow::anyhow!("checkpoint scheduler poisoned"))?;
    let mut history_bytes = Vec::new();
    if let Some(history) = &history {
        let metadata = serde_json::to_vec(history)?;
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
    crc.update(bytes);
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
    file.write_all(bytes)?;
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
    use std::sync::atomic::{AtomicBool, Ordering};
    static ASYNC_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct ActorCleanup {
        release: Option<mpsc::SyncSender<()>>,
        control: mpsc::SyncSender<crate::Control>,
        actions: mpsc::SyncSender<crate::Action>,
        actor: Option<std::thread::JoinHandle<()>>,
        reader: Option<std::thread::JoinHandle<()>>,
        finished: bool,
    }

    impl ActorCleanup {
        fn release_worker(&mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
        }

        fn join(&mut self) {
            self.release_worker();
            if let Some(actor) = self.actor.take() {
                actor.join().expect("test actor thread panicked");
            }
            if let Some(reader) = self.reader.take() {
                reader.join().expect("test PTY reader thread panicked");
            }
            self.finished = true;
        }
    }

    impl Drop for ActorCleanup {
        fn drop(&mut self) {
            if self.finished {
                return;
            }
            self.release_worker();
            let _ = self.control.try_send(crate::Control::Stop);
            let _ = self.actions.try_send(crate::Action::Wake);
            if let Some(actor) = self.actor.take() {
                let _ = actor.join();
            }
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }

    fn header() -> JournalHeader {
        JournalHeader {
            session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            profile: CORE_PROFILE.into(),
            initial_rows: 8,
            initial_cols: 40,
        }
    }

    #[test]
    fn refused_receipt_is_rejected_before_busy_permit_or_capture() -> Result<()> {
        let _serial = ASYNC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir()?;
        let header = header();
        let receipt = Receipt {
            kind: pigade_storage::EventKind::OutputRejectedResource,
            sequence: 1,
            end_offset: 100,
            checksum: 0,
        };
        let _permit = wait_permit()?;
        assert!(
            schedule_with_capture(root.path(), &header, receipt, None, || panic!(
                "refused event must not capture model"
            ))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn async_capture_is_immutable_and_busy_does_not_capture() -> Result<()> {
        let _serial = ASYNC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir()?;
        let header = header();
        let history = crate::history::History::live(root.path(), &header)?;
        let history_checkpoint = history.checkpoint()?;
        let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
        terminal.output(b"captured");
        let expected = terminal.checkpoint()?;
        let receipt = Receipt {
            kind: pigade_storage::EventKind::Output,
            sequence: 0,
            end_offset: 0,
            checksum: 0,
        };
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        *TEST_PAUSE.get_or_init(|| Mutex::new(None)).lock().unwrap() =
            Some((header.generation, started_tx, resume_rx));
        let pending = loop {
            if let Some(pending) = schedule(
                root.path(),
                &header,
                receipt,
                &terminal,
                Some(history_checkpoint.clone()),
            )? {
                break pending;
            }
            std::thread::yield_now();
        };
        started_rx.recv().unwrap();
        let captured_on_busy = AtomicBool::new(false);
        assert!(
            schedule_with_capture(root.path(), &header, receipt, None, || {
                captured_on_busy.store(true, Ordering::SeqCst);
                Ok(Vec::new())
            })?
            .is_none()
        );
        assert!(!captured_on_busy.load(Ordering::SeqCst));
        terminal.output(b"changed after capture");
        assert_eq!(pending.poll(), Completion::Pending);
        resume_tx.send(()).unwrap();
        pending.wait()?;
        let loaded = latest(root.path(), &header, u64::MAX)?.loaded.unwrap();
        assert_eq!(loaded.sequence, receipt.sequence);
        assert_eq!(loaded.terminal.checkpoint()?, expected);
        assert_eq!(loaded.history, Some(history_checkpoint));
        Ok(())
    }

    #[test]
    fn async_checkpoint_keeps_cold_history_prefix_immutable_during_growth() -> Result<()> {
        let _serial = ASYNC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir()?;
        let header = header();
        let journal_path = root.path().join(format!("{}.journal", header.generation));
        let mut journal = Journal::create(&journal_path, header.clone())?;
        let history = crate::history::History::live(root.path(), journal.header())?;
        let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
        terminal.set_history_observer(Some(history.clone()));

        // Cross the cold-retirement boundary with enough real rows to make the
        // snapshot non-vacuous, while keeping each journal event below limits.
        let first_output = (0..10_020)
            .map(|row| format!("{row:05} history growth\r\n"))
            .collect::<String>();
        let mut first_receipt = None;
        for payload in first_output.as_bytes().chunks(16 * 1024) {
            let receipt = journal.append(EventKind::Output, payload)?;
            history.begin(receipt.sequence)?;
            terminal.output(payload);
            history.commit()?;
            first_receipt = Some(receipt);
        }
        let first_receipt = first_receipt.context("no initial output receipt")?;
        let captured_terminal = terminal.checkpoint()?;
        let captured_history = history.checkpoint()?;
        let captured_records = captured_history.state.next_entry;
        let captured_retired = captured_history.state.retired_rows;
        assert!(
            captured_retired > 0,
            "fixture must retire rows into cold history"
        );
        assert!(captured_records > 0, "fixture must contain cold records");

        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        *TEST_PAUSE.get_or_init(|| Mutex::new(None)).lock().unwrap() =
            Some((header.generation, started_tx, resume_rx));
        let pending = loop {
            if let Some(pending) = schedule(
                root.path(),
                &header,
                first_receipt,
                &terminal,
                Some(captured_history.clone()),
            )? {
                break pending;
            }
            std::thread::yield_now();
        };
        started_rx.recv_timeout(std::time::Duration::from_secs(8))?;

        let later = (0..64)
            .map(|row| format!("later {row:03} cold rows\r\n"))
            .collect::<String>();
        let later_receipt = journal.append(EventKind::Output, later.as_bytes())?;
        history.begin(later_receipt.sequence)?;
        terminal.output(later.as_bytes());
        history.commit()?;
        assert!(history.summary().retired_rows > captured_retired);
        assert!(history.summary().records > captured_records);

        let exit = journal.append(EventKind::Exit, b"")?;
        history.begin(exit.sequence)?;
        history.commit()?;
        resume_tx.send(()).unwrap();
        pending.wait()?;

        crate::history::validate_checkpoint(root.path(), &header, &captured_history)?;
        let saved = latest(root.path(), &header, exit.end_offset)?
            .loaded
            .context("missing asynchronous checkpoint")?;
        assert_eq!(saved.sequence, first_receipt.sequence);
        assert_eq!(saved.offset, first_receipt.end_offset);
        assert_eq!(saved.terminal.checkpoint()?, captured_terminal);
        assert_eq!(saved.history.as_ref(), Some(&captured_history));

        let raw_before_recovery = fs::read(&journal_path)?;
        let verification = crate::verify_recovery_against_live(&journal_path, &terminal, &history)?;
        assert_eq!(verification.sequence, exit.sequence);
        assert_eq!(verification.recovery_path, "checkpoint-tail");
        assert!(verification.complete_state_equal);
        assert!(verification.frame_equal);
        assert!(verification.cold_history_equal);
        assert_eq!(verification.live_state_equal, Some(true));
        assert_eq!(verification.live_cold_history_equal, Some(true));
        assert_eq!(verification.cold_history_records, history.summary().records);
        assert!(verification.cold_history_records > captured_records);
        assert_eq!(fs::read(&journal_path)?, raw_before_recovery);
        Ok(())
    }

    #[test]
    fn async_failure_is_reported_and_releases_global_permit() -> Result<()> {
        let _serial = ASYNC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir()?;
        let missing = root.path().join("missing-directory");
        let header = header();
        let receipt = Receipt {
            kind: pigade_storage::EventKind::Output,
            sequence: 0,
            end_offset: 0,
            checksum: 0,
        };
        let terminal = Terminal::new(Geometry::new(8, 40)?, false);
        let first = loop {
            if let Some(pending) = schedule(&missing, &header, receipt, &terminal, None)? {
                break pending;
            }
            std::thread::yield_now();
        };
        let error = first.wait().unwrap_err().to_string();
        assert!(!error.is_empty());
        let second = loop {
            if let Some(pending) = schedule(&missing, &header, receipt, &terminal, None)? {
                break pending;
            }
            std::thread::yield_now();
        };
        assert!(second.wait().is_err());

        let pending = Pending {
            completion: {
                let (tx, rx) = mpsc::sync_channel(1);
                drop(tx);
                rx
            },
            cached: Mutex::new(None),
        };
        assert!(matches!(pending.poll(), Completion::Failed(_)));
        Ok(())
    }

    #[test]
    fn completion_error_text_is_utf8_bounded() {
        let error = anyhow::anyhow!("界".repeat(COMPLETION_ERROR_LIMIT));
        let message = bounded_error(&error);
        assert!(message.len() <= COMPLETION_ERROR_LIMIT);
        assert!(message.ends_with('…'));
        assert!(std::str::from_utf8(message.as_bytes()).is_ok());

        let mut segmented = BoundedError(String::new());
        fmt::Write::write_str(&mut segmented, &"a".repeat(COMPLETION_ERROR_LIMIT)).unwrap();
        assert!(fmt::Write::write_str(&mut segmented, "next").is_err());
        assert!(segmented.0.len() <= COMPLETION_ERROR_LIMIT);
        assert!(segmented.0.ends_with('…'));

        let mut multibyte = BoundedError(String::new());
        fmt::Write::write_str(&mut multibyte, &"界".repeat(340)).unwrap();
        assert!(fmt::Write::write_str(&mut multibyte, "界界").is_err());
        assert!(multibyte.0.len() <= COMPLETION_ERROR_LIMIT);
        assert!(std::str::from_utf8(multibyte.0.as_bytes()).is_ok());
        assert!(multibyte.0.ends_with('…'));
    }

    #[test]
    fn actor_keeps_committing_pty_output_while_checkpoint_persistence_is_paused() -> Result<()> {
        use crate::{Action, ActorConfig, View, actor};
        use portable_pty::{CommandBuilder, native_pty_system};
        use std::{
            io::Read,
            sync::Arc,
            time::{Duration, Instant},
        };

        let _serial = ASYNC_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir()?;
        let header = header();
        let geometry = Geometry::new(8, 40)?;
        let journal_path = root.path().join(format!("{}.journal", header.generation));
        let journal = Journal::create(&journal_path, header.clone())?;
        let history = crate::history::History::live(root.path(), journal.header())?;
        let actor_history = history.clone();

        let pair = native_pty_system().openpty(crate::pty_size(geometry))?;
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "printf 'FIRST\\n'; IFS= read -r line; printf 'SECOND\\n'; sleep 0.2",
        ]);
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = crate::input::InputWriter::new(pair.master.take_writer()?);
        let (action_tx, action_rx) = mpsc::sync_channel(8);
        let reader_tx = action_tx.clone();
        let reader_thread = std::thread::spawn(move || {
            loop {
                let mut bytes = vec![0; crate::OUTPUT_CHUNK];
                match reader.read(&mut bytes) {
                    Ok(0) => {
                        let _ = reader_tx.send(Action::Eof);
                        break;
                    }
                    Ok(count) => {
                        bytes.truncate(count);
                        if reader_tx.send(Action::Output(bytes)).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = reader_tx.send(Action::ReadError(error.to_string()));
                        break;
                    }
                }
            }
        });
        let view = Arc::new(Mutex::new(View {
            session: header.session_id,
            generation: header.generation,
            sequence: 0,
            state: "running".into(),
            recovery_path: "live".into(),
            pid: None,
            storage_error: None,
            frame: Terminal::new(geometry, false).frame(),
            history: Some(history.summary()),
            recovery_journal_bytes_read: 0,
            metrics: crate::metrics::SessionMetrics::default(),
            actor_diagnostic: None,
        }));
        let probe = Arc::new(crate::metrics::ActorProbe::default());
        let (control_tx, control_rx) = mpsc::sync_channel(1);

        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (resume_tx, resume_rx) = mpsc::sync_channel(1);
        *TEST_PAUSE.get_or_init(|| Mutex::new(None)).lock().unwrap() =
            Some((header.generation, started_tx, resume_rx));

        let actor_view = view.clone();
        let actor_probe = probe.clone();
        let actor_writer = writer.clone();
        let actor_root = root.path().to_owned();
        let actor_thread = std::thread::spawn(move || {
            actor(
                action_rx,
                pair.master,
                actor_writer,
                child,
                journal,
                actor_view,
                ActorConfig {
                    geometry,
                    root: actor_root,
                    control: control_rx,
                    history: actor_history,
                    probe: actor_probe,
                    changes: crate::changes::ChangeHub::new(),
                },
            );
        });
        let mut cleanup = ActorCleanup {
            release: Some(resume_tx),
            control: control_tx.clone(),
            actions: action_tx.clone(),
            actor: Some(actor_thread),
            reader: Some(reader_thread),
            finished: false,
        };

        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let snapshot = view.lock().unwrap().clone();
            if snapshot.frame.text().contains("FIRST") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "PTY did not publish initial marker: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        started_rx.recv_timeout(Duration::from_secs(8))?;
        writer
            .try_write(b"\n".to_vec())
            .map_err(anyhow::Error::msg)?;
        let prefix_view = loop {
            let snapshot = view.lock().unwrap().clone();
            if snapshot.frame.text().contains("SECOND") {
                break snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "PTY output was not published while checkpoint was paused: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            prefix_view.sequence, 2,
            "first and second output events must both publish"
        );
        assert_eq!(history.state().applied_sequence, prefix_view.sequence);
        let mut reader = pigade_storage::JournalReader::open(&journal_path)?;
        let mut second_event_offset = None;
        let mut event_count = 0;
        while let Some(event) = reader.next_event()? {
            event_count += 1;
            if event.kind == EventKind::Output && event.payload.windows(6).any(|w| w == b"SECOND") {
                second_event_offset = Some(reader.valid_offset());
            }
        }
        assert_eq!(
            event_count, 2,
            "output prefix should be durably journaled before shell exit"
        );
        let second_event_offset =
            second_event_offset.context("missing second output journal event")?;

        cleanup.release_worker();
        loop {
            let snapshot = view.lock().unwrap().clone();
            if snapshot.state == "exited" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "actor did not durably process Exit: {snapshot:?}"
            );
            if let Some(error) = snapshot.storage_error {
                anyhow::bail!("actor failed while completing checkpoint: {error}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        cleanup.join();
        drop(writer);
        drop(control_tx);

        let full = crate::restore_model(&journal_path, false)?;
        assert_eq!(
            full.0.sequence, 3,
            "expected first output, second output, and Exit"
        );
        assert_eq!(full.0.state, "history");
        let full_raw = fs::read(&journal_path)?;
        let mut full_reader = pigade_storage::JournalReader::open(&journal_path)?;
        let mut full_event_count = 0;
        while full_reader.next_event()?.is_some() {
            full_event_count += 1;
        }
        assert_eq!(full_event_count, 3);
        assert_eq!(full_raw.len() as u64, full_reader.valid_offset());

        // Truncate the final Exit after preserving the complete second output.
        // Recovery must use the durable checkpoint plus its valid journal tail
        // to reconstruct the exact published prefix view.
        let file = OpenOptions::new().write(true).open(&journal_path)?;
        file.set_len(second_event_offset)?;
        file.sync_all()?;
        let recovered = crate::recover_journal(&journal_path)?;
        assert_eq!(recovered.sequence, prefix_view.sequence);
        assert_eq!(recovered.recovery_path, "checkpoint-tail");
        assert_eq!(recovered.frame, prefix_view.frame);
        assert_eq!(recovered.history, prefix_view.history);
        assert!(recovered.storage_error.is_some());
        assert_eq!(
            fs::read(&journal_path)?,
            full_raw[..second_event_offset as usize]
        );
        Ok(())
    }
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
