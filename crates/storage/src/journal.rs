use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
use uuid::Uuid;

const MAGIC: &[u8; 8] = b"PIGJNL01";
const MAX_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;
const FRAME_HEADER_BYTES: usize = 33;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalHeader {
    pub session_id: Uuid,
    pub generation: Uuid,
    /// Immutable core/version, Unicode width and font profile descriptor.
    pub profile: String,
    pub initial_rows: u16,
    pub initial_cols: u16,
}

impl JournalHeader {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.initial_rows > 0 && self.initial_cols > 0,
            "zero terminal geometry"
        );
        ensure!(!self.profile.is_empty(), "empty terminal profile");
        ensure!(
            self.profile.len() <= MAX_HEADER_BYTES - 256,
            "terminal profile too large"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum EventKind {
    Output = 1,
    Resize = 2,
    SyncTimeout = 3,
    Exit = 4,
}

impl EventKind {
    fn decode(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Output),
            2 => Some(Self::Resize),
            3 => Some(Self::SyncTimeout),
            4 => Some(Self::Exit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEvent {
    pub sequence: u64,
    pub kind: EventKind,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    pub sequence: u64,
    pub end_offset: u64,
    pub checksum: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorruptionKind {
    TruncatedHeader,
    InvalidHeader,
    UnsupportedVersion,
    TruncatedFrame,
    OversizeEvent,
    Sequence,
    Generation,
    UnknownEvent,
    Checksum,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corruption {
    pub offset: u64,
    pub kind: CorruptionKind,
}
impl fmt::Display for Corruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "journal corruption at byte {}: {:?}",
            self.offset, self.kind
        )
    }
}
impl std::error::Error for Corruption {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    pub valid_events: u64,
    pub valid_bytes: u64,
    pub corruption: Option<Corruption>,
}

fn corrupt(offset: u64, kind: CorruptionKind) -> anyhow::Error {
    Corruption { offset, kind }.into()
}

fn read_header(file: &mut File) -> Result<JournalHeader> {
    let mut prefix = [0u8; 12];
    file.read_exact(&mut prefix).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            corrupt(0, CorruptionKind::TruncatedHeader)
        } else {
            e.into()
        }
    })?;
    if &prefix[..8] != MAGIC {
        return Err(corrupt(0, CorruptionKind::UnsupportedVersion));
    }
    let len = u32::from_le_bytes(prefix[8..12].try_into().unwrap()) as usize;
    if len > MAX_HEADER_BYTES {
        return Err(corrupt(8, CorruptionKind::InvalidHeader));
    }
    let mut bytes = vec![0; len];
    let mut checksum = [0; 4];
    file.read_exact(&mut bytes)
        .and_then(|_| file.read_exact(&mut checksum))
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                corrupt(12, CorruptionKind::TruncatedHeader)
            } else {
                e.into()
            }
        })?;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&prefix);
    hasher.update(&bytes);
    if hasher.finalize() != u32::from_le_bytes(checksum) {
        return Err(corrupt(12, CorruptionKind::Checksum));
    }
    let header: JournalHeader =
        serde_json::from_slice(&bytes).map_err(|_| corrupt(12, CorruptionKind::InvalidHeader))?;
    header
        .validate()
        .map_err(|_| corrupt(12, CorruptionKind::InvalidHeader))?;
    Ok(header)
}

/// Streaming reader. On corruption it becomes poisoned; reopening is required.
/// One decoded event is allocated at a time, bounded by MAX_EVENT_BYTES.
pub struct JournalReader {
    file: File,
    header: JournalHeader,
    sequence: u64,
    valid_bytes: u64,
    poisoned: bool,
}
impl JournalReader {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_file(File::open(path)?)
    }
    fn from_file(mut file: File) -> Result<Self> {
        file.seek(SeekFrom::Start(0))?;
        let header = read_header(&mut file)?;
        let valid_bytes = file.stream_position()?;
        Ok(Self {
            file,
            header,
            sequence: 0,
            valid_bytes,
            poisoned: false,
        })
    }
    pub fn header(&self) -> &JournalHeader {
        &self.header
    }
    pub fn next_event(&mut self) -> Result<Option<JournalEvent>> {
        ensure!(!self.poisoned, "reader poisoned by prior corruption");
        match self.read_event() {
            Ok(event) => Ok(event),
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }
    fn read_event(&mut self) -> Result<Option<JournalEvent>> {
        let offset = self.valid_bytes;
        let mut frame = [0u8; FRAME_HEADER_BYTES];
        // Distinguish a clean boundary from a partial fixed header.
        let count = self.file.read(&mut frame[..1])?;
        if count == 0 {
            return Ok(None);
        }
        self.file.read_exact(&mut frame[1..]).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                corrupt(offset, CorruptionKind::TruncatedFrame)
            } else {
                e.into()
            }
        })?;
        let sequence = u64::from_le_bytes(frame[..8].try_into().unwrap());
        let generation = Uuid::from_slice(&frame[8..24])?;
        let kind = EventKind::decode(frame[24])
            .ok_or_else(|| corrupt(offset, CorruptionKind::UnknownEvent))?;
        let len = u32::from_le_bytes(frame[25..29].try_into().unwrap()) as usize;
        if len > MAX_EVENT_BYTES {
            return Err(corrupt(offset, CorruptionKind::OversizeEvent));
        }
        let expected = u32::from_le_bytes(frame[29..33].try_into().unwrap());
        let mut payload = vec![0; len];
        self.file.read_exact(&mut payload).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                corrupt(offset, CorruptionKind::TruncatedFrame)
            } else {
                e.into()
            }
        })?;
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&frame[..29]);
        hasher.update(&payload);
        if hasher.finalize() != expected {
            return Err(corrupt(offset, CorruptionKind::Checksum));
        }
        if sequence != self.sequence.checked_add(1).context("sequence exhausted")? {
            return Err(corrupt(offset, CorruptionKind::Sequence));
        }
        if generation != self.header.generation {
            return Err(corrupt(offset, CorruptionKind::Generation));
        }
        self.sequence = sequence;
        self.valid_bytes = self.file.stream_position()?;
        Ok(Some(JournalEvent {
            sequence,
            kind,
            payload,
        }))
    }
    /// Resume only from an authenticated checkpoint watermark. This intentionally
    /// does not scan the prefix; the snapshot checksum/profile binds that prefix.
    pub fn resume_after_checkpoint(mut self, sequence: u64, offset: u64) -> Result<Self> {
        ensure!(
            offset >= self.valid_bytes && offset <= self.file.metadata()?.len(),
            "checkpoint journal offset outside file"
        );
        ensure!(
            sequence > 0 || offset == self.valid_bytes,
            "invalid initial checkpoint watermark"
        );
        self.file.seek(SeekFrom::Start(offset))?;
        self.sequence = sequence;
        self.valid_bytes = offset;
        Ok(self)
    }
    pub fn valid_offset(&self) -> u64 {
        self.valid_bytes
    }

    fn scan(&mut self) -> Result<RecoveryReport> {
        loop {
            match self.next_event() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return Ok(RecoveryReport {
                        valid_events: self.sequence,
                        valid_bytes: self.valid_bytes,
                        corruption: None,
                    });
                }
                Err(error) => {
                    if let Some(corruption) = error.downcast_ref::<Corruption>() {
                        return Ok(RecoveryReport {
                            valid_events: self.sequence,
                            valid_bytes: self.valid_bytes,
                            corruption: Some(corruption.clone()),
                        });
                    }
                    return Err(error);
                }
            }
        }
    }
}

pub fn scan_valid_prefix(path: impl AsRef<Path>) -> Result<RecoveryReport> {
    JournalReader::open(path)?.scan()
}

/// Exclusive append writer. Any I/O failure poisons it: no later append can
/// mistake an uncertain fsync outcome for a committed event.
pub struct Journal {
    file: File,
    header: JournalHeader,
    sequence: u64,
    poisoned: bool,
}
impl Journal {
    pub fn create(path: impl AsRef<Path>, header: JournalHeader) -> Result<Self> {
        header.validate()?;
        let bytes = serde_json::to_vec(&header)?;
        ensure!(
            bytes.len() <= MAX_HEADER_BYTES,
            "journal profile exceeds header limit"
        );
        let path = path.as_ref();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        super::lock_writer(&file)?;
        let mut prefix = Vec::with_capacity(12 + bytes.len());
        prefix.extend_from_slice(MAGIC);
        prefix.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        prefix.extend_from_slice(&bytes);
        file.write_all(&prefix)?;
        file.write_all(&crc32fast::hash(&prefix).to_le_bytes())?;
        file.sync_all()?;
        File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )?
        .sync_all()?;
        Ok(Self {
            file,
            header,
            sequence: 0,
            poisoned: false,
        })
    }
    pub fn open(path: impl AsRef<Path>, expected_header: &JournalHeader) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        super::lock_writer(&file)?;
        let mut reader = JournalReader::from_file(file)?;
        ensure!(
            &reader.header == expected_header,
            "journal generation/profile mismatch"
        );
        let report = reader.scan()?;
        if let Some(corruption) = report.corruption {
            return Err(corruption.into());
        }
        Ok(Self {
            file: reader.file,
            header: reader.header,
            sequence: report.valid_events,
            poisoned: false,
        })
    }
    pub fn header(&self) -> &JournalHeader {
        &self.header
    }
    pub fn durable_sequence(&self) -> u64 {
        self.sequence
    }
    pub fn append(&mut self, kind: EventKind, payload: &[u8]) -> Result<Receipt> {
        self.append_impl(kind, payload, None)
    }
    fn append_impl(
        &mut self,
        kind: EventKind,
        payload: &[u8],
        fault: Option<WriteFault>,
    ) -> Result<Receipt> {
        ensure!(
            !self.poisoned,
            "journal writer poisoned by uncertain write; reopen and recover"
        );
        ensure!(
            payload.len() <= MAX_EVENT_BYTES,
            "event exceeds bounded size"
        );
        let sequence = self.sequence.checked_add(1).context("sequence exhausted")?;
        let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
        frame.extend_from_slice(&sequence.to_le_bytes());
        frame.extend_from_slice(self.header.generation.as_bytes());
        frame.push(kind as u8);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&frame);
        hasher.update(payload);
        let checksum = hasher.finalize();
        frame.extend_from_slice(&checksum.to_le_bytes());
        frame.extend_from_slice(payload);
        self.poisoned = true;
        match fault {
            #[cfg(test)]
            Some(WriteFault::AfterBytes(limit)) => {
                self.file.write_all(&frame[..limit.min(frame.len())])?;
                anyhow::bail!("injected partial write");
            }
            #[cfg(test)]
            Some(WriteFault::BeforeSync) => {
                self.file.write_all(&frame)?;
                anyhow::bail!("injected fsync failure");
            }
            None => self.file.write_all(&frame)?,
        }
        self.file.sync_all()?;
        let end_offset = self.file.stream_position()?;
        self.sequence = sequence;
        self.poisoned = false;
        Ok(Receipt {
            sequence,
            end_offset,
            checksum,
        })
    }
}

#[derive(Clone, Copy)]
enum WriteFault {
    #[cfg(test)]
    AfterBytes(usize),
    #[cfg(test)]
    BeforeSync,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header() -> JournalHeader {
        JournalHeader {
            session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            profile: "alacritty-0.26.0;unicode-profile;test-font".into(),
            initial_rows: 24,
            initial_cols: 80,
        }
    }
    #[test]
    fn durable_roundtrip_and_exclusive_writer() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("events");
        let h = header();
        let mut journal = Journal::create(&path, h.clone())?;
        ensure!(Journal::open(&path, &h).is_err());
        let receipt = journal.append(EventKind::Output, b"hello\x1b[?1049h")?;
        assert_eq!(receipt.sequence, 1);
        assert_eq!(journal.durable_sequence(), 1);
        journal.append(EventKind::Resize, br#"{"rows":36,"cols":120}"#)?;
        drop(journal);
        let mut journal = Journal::open(&path, &h)?;
        assert_eq!(journal.append(EventKind::SyncTimeout, b"")?.sequence, 3);
        drop(journal);
        let mut reader = JournalReader::open(&path)?;
        assert_eq!(reader.header(), &h);
        assert_eq!(reader.next_event()?.unwrap().payload, b"hello\x1b[?1049h");
        assert_eq!(reader.next_event()?.unwrap().kind, EventKind::Resize);
        assert_eq!(reader.next_event()?.unwrap().kind, EventKind::SyncTimeout);
        assert!(reader.next_event()?.is_none());
        Ok(())
    }
    #[test]
    fn every_partial_write_preserves_prefix_and_refuses_append() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        for cut in 1..FRAME_HEADER_BYTES + 20 {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("events");
            let h = header();
            let mut journal = Journal::create(&path, h.clone())?;
            let committed = journal.append(EventKind::Output, b"durable")?;
            assert!(
                journal
                    .append_impl(
                        EventKind::Output,
                        &[42; 20],
                        Some(WriteFault::AfterBytes(cut))
                    )
                    .is_err()
            );
            assert_eq!(journal.durable_sequence(), 1);
            assert!(journal.append(EventKind::Output, b"later").is_err());
            drop(journal);
            let original = std::fs::read(&path)?;
            let report = scan_valid_prefix(&path)?;
            assert_eq!(report.valid_events, 1);
            assert_eq!(report.valid_bytes, committed.end_offset);
            assert_eq!(
                report.corruption.unwrap().kind,
                CorruptionKind::TruncatedFrame
            );
            assert!(Journal::open(&path, &h).is_err());
            assert_eq!(std::fs::read(path)?, original);
        }
        Ok(())
    }
    #[test]
    fn uncertain_fsync_never_produces_receipt_or_allows_more_writes() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("events");
        let h = header();
        let mut journal = Journal::create(&path, h.clone())?;
        assert!(
            journal
                .append_impl(
                    EventKind::Output,
                    b"uncertain",
                    Some(WriteFault::BeforeSync)
                )
                .is_err()
        );
        assert_eq!(journal.durable_sequence(), 0);
        assert!(journal.append(EventKind::Output, b"retry").is_err());
        drop(journal);
        // An unacknowledged complete event may survive. Recovery must preserve
        // it, rather than overwrite it or infer that the caller saw a receipt.
        assert_eq!(scan_valid_prefix(&path)?.valid_events, 1);
        assert_eq!(Journal::open(path, &h)?.durable_sequence(), 1);
        Ok(())
    }
    fn rewrite_frame(path: &Path, edit: impl FnOnce(&mut [u8])) -> Result<()> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        read_header(&mut file)?;
        let start = file.stream_position()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        edit(&mut bytes);
        let mut hash = crc32fast::Hasher::new();
        hash.update(&bytes[..29]);
        hash.update(&bytes[33..]);
        bytes[29..33].copy_from_slice(&hash.finalize().to_le_bytes());
        file.seek(SeekFrom::Start(start))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    }
    #[test]
    fn rejects_sequence_generation_checksum_and_oversize_without_allocation() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        for expected in [
            CorruptionKind::Sequence,
            CorruptionKind::Generation,
            CorruptionKind::Checksum,
            CorruptionKind::OversizeEvent,
            CorruptionKind::UnknownEvent,
        ] {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("events");
            let h = header();
            let mut journal = Journal::create(&path, h)?;
            journal.append(EventKind::Output, b"hello")?;
            drop(journal);
            rewrite_frame(&path, |frame| match expected {
                CorruptionKind::Sequence => frame[..8].copy_from_slice(&99u64.to_le_bytes()),
                CorruptionKind::Generation => {
                    frame[8..24].copy_from_slice(Uuid::new_v4().as_bytes())
                }
                CorruptionKind::Checksum => {}
                CorruptionKind::OversizeEvent => {
                    frame[25..29].copy_from_slice(&u32::MAX.to_le_bytes())
                }
                CorruptionKind::UnknownEvent => frame[24] = 255,
                _ => unreachable!(),
            })?;
            if expected == CorruptionKind::Checksum {
                let mut bytes = std::fs::read(&path)?;
                *bytes.last_mut().unwrap() ^= 1;
                std::fs::write(&path, bytes)?;
            }
            let report = scan_valid_prefix(&path)?;
            assert_eq!(report.valid_events, 0);
            assert_eq!(report.corruption.unwrap().kind, expected);
        }
        Ok(())
    }
    #[test]
    fn bounds_and_header_profile_are_enforced() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("events");
        let h = header();
        let mut journal = Journal::create(&path, h.clone())?;
        assert!(
            journal
                .append(EventKind::Output, &vec![0; MAX_EVENT_BYTES + 1])
                .is_err()
        );
        assert_eq!(journal.append(EventKind::Output, b"valid")?.sequence, 1);
        drop(journal);
        let mut changed = h.clone();
        changed.profile = "wrong".into();
        assert!(Journal::open(&path, &changed).is_err());
        let bytes = std::fs::read(&path)?;
        for cut in 0..12 {
            std::fs::write(&path, &bytes[..cut])?;
            assert!(JournalReader::open(&path).is_err());
        }
        Ok(())
    }
    #[test]
    fn real_sigkill_keeps_durable_prefix_and_detects_torn_tail() -> Result<()> {
        // Isolate fork/exec crash probes: a child can briefly inherit unrelated flock descriptors.
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        const CHILD_PATH: &str = "PIGADE_JOURNAL_CRASH_CHILD_PATH";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let mut journal = Journal::create(&path, header())?;
            journal.append(EventKind::Output, b"durable prefix")?;
            let _ = journal.append_impl(
                EventKind::Output,
                b"partial tail",
                Some(WriteFault::AfterBytes(12)),
            );
            // SAFETY: deliberately terminate only this isolated test child.
            unsafe {
                libc::raise(libc::SIGKILL);
            }
            unreachable!();
        }
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("events");
        let result = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "journal::tests::real_sigkill_keeps_durable_prefix_and_detects_torn_tail",
            ])
            .env(CHILD_PATH, &path)
            .output()?;
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(result.status.signal(), Some(libc::SIGKILL));
        let report = scan_valid_prefix(&path)?;
        assert_eq!(report.valid_events, 1);
        assert_eq!(
            report.corruption.unwrap().kind,
            CorruptionKind::TruncatedFrame
        );
        let mut reader = JournalReader::open(path)?;
        assert_eq!(reader.next_event()?.unwrap().payload, b"durable prefix");
        Ok(())
    }
}
