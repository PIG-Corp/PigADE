//! Bounded, append-only archive fragments for retired history rows.
//!
//! This is a derived cache: the source event journal remains authoritative.

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

const MAGIC: &[u8; 8] = b"PIGARC01";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const RECORD_HEADER_BYTES: usize = 49;
const RECORD_PREFIX_BYTES: usize = 45;
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveHeader {
    pub session_id: Uuid,
    pub generation: Uuid,
    pub profile: String,
}

impl ArchiveHeader {
    fn validate(&self) -> Result<()> {
        ensure!(!self.profile.is_empty(), "empty archive profile");
        ensure!(
            self.profile.len() <= MAX_HEADER_BYTES - 128,
            "archive profile too large"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveFragment {
    pub source_event_sequence: u64,
    pub row_ordinal: u64,
    pub fragment_offset: u64,
    pub final_fragment: bool,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveReceipt {
    pub session_id: Uuid,
    pub generation: Uuid,
    pub row_ordinal: u64,
    pub source_event_sequence: u64,
    pub end_offset: u64,
    /// CRC of the final fragment record, not a hash of the whole archive.
    pub last_fragment_checksum: u32,
}

/// A direct byte range for one complete archive row. This is an address, not
/// evidence that the row was fsynced; the host must bind it into a durable
/// committed index/root before treating it as committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveRowLocation {
    pub start_offset: u64,
    pub end_offset: u64,
    pub row_ordinal: u64,
    pub source_event_sequence: u64,
    pub last_fragment_checksum: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveCorruptionKind {
    TruncatedHeader,
    InvalidHeader,
    UnsupportedVersion,
    TruncatedRecord,
    IncompleteRow,
    OversizePayload,
    Checksum,
    Identity,
    Ordering,
    InvalidMarker,
    Receipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveCorruption {
    pub offset: u64,
    pub kind: ArchiveCorruptionKind,
}

impl fmt::Display for ArchiveCorruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "archive corruption at byte {}: {:?}",
            self.offset, self.kind
        )
    }
}
impl std::error::Error for ArchiveCorruption {}

#[derive(Debug, Clone, PartialEq, Eq)]
/// CRC-valid complete rows in a scanned prefix. This is diagnostic only and
/// does not prove the rows crossed an fsync boundary.
pub struct ArchiveScan {
    /// Complete rows found in the valid prefix, not durable commit evidence.
    pub complete_rows: u64,
    pub complete_fragments: u64,
    /// End offset of the last complete row. This is a derived watermark only.
    pub last_complete_row_offset: u64,
    pub corruption: Option<ArchiveCorruption>,
}

fn corruption(offset: u64, kind: ArchiveCorruptionKind) -> anyhow::Error {
    ArchiveCorruption { offset, kind }.into()
}

fn read_header(file: &mut File) -> Result<ArchiveHeader> {
    let mut prefix = [0u8; 12];
    file.read_exact(&mut prefix).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            corruption(0, ArchiveCorruptionKind::TruncatedHeader)
        } else {
            error.into()
        }
    })?;
    if &prefix[..8] != MAGIC {
        return Err(corruption(0, ArchiveCorruptionKind::UnsupportedVersion));
    }
    let length = u32::from_le_bytes(prefix[8..12].try_into().unwrap()) as usize;
    if length > MAX_HEADER_BYTES {
        return Err(corruption(8, ArchiveCorruptionKind::InvalidHeader));
    }
    let mut bytes = vec![0; length];
    let mut checksum = [0; 4];
    file.read_exact(&mut bytes)
        .and_then(|_| file.read_exact(&mut checksum))
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                corruption(0, ArchiveCorruptionKind::TruncatedHeader)
            } else {
                error.into()
            }
        })?;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&prefix);
    hasher.update(&bytes);
    if hasher.finalize() != u32::from_le_bytes(checksum) {
        return Err(corruption(12, ArchiveCorruptionKind::Checksum));
    }
    let header: ArchiveHeader = serde_json::from_slice(&bytes)
        .map_err(|_| corruption(12, ArchiveCorruptionKind::InvalidHeader))?;
    header
        .validate()
        .map_err(|_| corruption(12, ArchiveCorruptionKind::InvalidHeader))?;
    Ok(header)
}

fn read_record(
    file: &mut File,
    header: &ArchiveHeader,
    offset: u64,
    limit: u64,
) -> Result<Option<(ArchiveFragment, u32)>> {
    let fixed_end = offset
        .checked_add(RECORD_HEADER_BYTES as u64)
        .ok_or_else(|| corruption(offset, ArchiveCorruptionKind::TruncatedRecord))?;
    if offset == limit {
        return Ok(None);
    }
    if fixed_end > limit {
        return Err(corruption(offset, ArchiveCorruptionKind::TruncatedRecord));
    }
    let mut frame = [0u8; RECORD_HEADER_BYTES];
    if file.read(&mut frame[..1])? == 0 {
        // `limit` was frozen when opening the reader. An earlier EOF means
        // the file changed/truncated; it is not the end of the valid prefix.
        return Err(corruption(offset, ArchiveCorruptionKind::TruncatedRecord));
    }
    file.read_exact(&mut frame[1..]).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            corruption(offset, ArchiveCorruptionKind::TruncatedRecord)
        } else {
            error.into()
        }
    })?;
    let source_event_sequence = u64::from_le_bytes(frame[0..8].try_into().unwrap());
    let row_ordinal = u64::from_le_bytes(frame[8..16].try_into().unwrap());
    let fragment_offset = u64::from_le_bytes(frame[16..24].try_into().unwrap());
    let final_fragment = match frame[24] {
        0 => false,
        1 => true,
        _ => return Err(corruption(offset, ArchiveCorruptionKind::InvalidMarker)),
    };
    let length = u32::from_le_bytes(frame[25..29].try_into().unwrap()) as usize;
    if length > MAX_PAYLOAD_BYTES {
        return Err(corruption(offset, ArchiveCorruptionKind::OversizePayload));
    }
    let record_end = fixed_end
        .checked_add(length as u64)
        .ok_or_else(|| corruption(offset, ArchiveCorruptionKind::TruncatedRecord))?;
    if record_end > limit {
        return Err(corruption(offset, ArchiveCorruptionKind::TruncatedRecord));
    }
    if &frame[29..45] != header.generation.as_bytes() {
        return Err(corruption(offset, ArchiveCorruptionKind::Identity));
    }
    let expected = u32::from_le_bytes(
        frame[RECORD_PREFIX_BYTES..RECORD_HEADER_BYTES]
            .try_into()
            .unwrap(),
    );
    let mut payload = vec![0; length];
    file.read_exact(&mut payload).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            corruption(offset, ArchiveCorruptionKind::TruncatedRecord)
        } else {
            error.into()
        }
    })?;
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&frame[..RECORD_PREFIX_BYTES]);
    hasher.update(&payload);
    let checksum = hasher.finalize();
    if checksum != expected {
        return Err(corruption(offset, ArchiveCorruptionKind::Checksum));
    }
    Ok(Some((
        ArchiveFragment {
            source_event_sequence,
            row_ordinal,
            fragment_offset,
            final_fragment,
            payload,
        },
        checksum,
    )))
}

#[derive(Debug, Clone, Copy, Default)]
struct OrderState {
    source_sequence: Option<u64>,
    last_row: Option<u64>,
    open_row: Option<u64>,
    row_source_sequence: Option<u64>,
    next_fragment_offset: u64,
}

impl OrderState {
    fn accept(&mut self, fragment: &ArchiveFragment, offset: u64) -> Result<()> {
        if self
            .source_sequence
            .is_some_and(|previous| fragment.source_event_sequence < previous)
        {
            return Err(corruption(offset, ArchiveCorruptionKind::Ordering));
        }
        if let Some(row) = self.open_row {
            if fragment.row_ordinal != row
                || fragment.fragment_offset != self.next_fragment_offset
                || self.row_source_sequence != Some(fragment.source_event_sequence)
            {
                return Err(corruption(offset, ArchiveCorruptionKind::Ordering));
            }
        } else {
            if self
                .last_row
                .is_some_and(|previous| fragment.row_ordinal <= previous)
                || fragment.fragment_offset != 0
            {
                return Err(corruption(offset, ArchiveCorruptionKind::Ordering));
            }
            self.open_row = Some(fragment.row_ordinal);
            self.row_source_sequence = Some(fragment.source_event_sequence);
            self.next_fragment_offset = 0;
        }
        self.source_sequence = Some(fragment.source_event_sequence);
        self.next_fragment_offset = self
            .next_fragment_offset
            .checked_add(fragment.payload.len() as u64)
            .context("archive fragment offset exhausted")?;
        if fragment.final_fragment {
            self.last_row = Some(fragment.row_ordinal);
            self.open_row = None;
            self.row_source_sequence = None;
            self.next_fragment_offset = 0;
        }
        Ok(())
    }
}

/// Append-only writer. Appends are buffered by the filesystem; `commit` is the
/// event-level durability boundary and is valid only between complete rows.
pub struct ArchiveWriter {
    file: File,
    logical_position: u64,
    header: ArchiveHeader,
    order: OrderState,
    poisoned: bool,
    complete_since_commit: bool,
    current_row_start: Option<u64>,
    latest_complete: Option<ArchiveRowLocation>,
}

impl ArchiveWriter {
    pub fn create(path: impl AsRef<Path>, header: ArchiveHeader) -> Result<Self> {
        header.validate()?;
        let bytes = serde_json::to_vec(&header)?;
        ensure!(
            bytes.len() <= MAX_HEADER_BYTES,
            "archive header exceeds bound"
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
        let logical_position = (prefix.len() as u64)
            .checked_add(4)
            .context("archive header offset exhausted")?;
        file.sync_all()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        File::open(parent)?.sync_all()?;
        Ok(Self {
            file,
            logical_position,
            header,
            order: OrderState::default(),
            poisoned: false,
            complete_since_commit: false,
            current_row_start: None,
            latest_complete: None,
        })
    }

    pub fn header(&self) -> &ArchiveHeader {
        &self.header
    }

    pub fn append_fragment(
        &mut self,
        source_event_sequence: u64,
        row_ordinal: u64,
        fragment_offset: u64,
        final_fragment: bool,
        payload: &[u8],
    ) -> Result<()> {
        self.append_fragment_impl(
            source_event_sequence,
            row_ordinal,
            fragment_offset,
            final_fragment,
            payload,
            None,
        )
    }

    fn append_fragment_impl(
        &mut self,
        source_event_sequence: u64,
        row_ordinal: u64,
        fragment_offset: u64,
        final_fragment: bool,
        payload: &[u8],
        fault: Option<WriteFault>,
    ) -> Result<()> {
        ensure!(!self.poisoned, "archive writer poisoned by uncertain write");
        ensure!(
            payload.len() <= MAX_PAYLOAD_BYTES,
            "archive payload exceeds bound"
        );
        let fragment = ArchiveFragment {
            source_event_sequence,
            row_ordinal,
            fragment_offset,
            final_fragment,
            payload: payload.to_vec(),
        };
        let mut next_order = self.order;
        let record_start = self.logical_position;
        next_order.accept(&fragment, record_start)?;
        let mut frame = Vec::with_capacity(RECORD_HEADER_BYTES + payload.len());
        frame.extend_from_slice(&source_event_sequence.to_le_bytes());
        frame.extend_from_slice(&row_ordinal.to_le_bytes());
        frame.extend_from_slice(&fragment_offset.to_le_bytes());
        frame.push(u8::from(final_fragment));
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(self.header.generation.as_bytes());
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&frame);
        hasher.update(payload);
        let checksum = hasher.finalize();
        frame.extend_from_slice(&checksum.to_le_bytes());
        frame.extend_from_slice(payload);
        let end_offset = record_start
            .checked_add(frame.len() as u64)
            .context("archive offset exhausted")?;
        self.poisoned = true;
        match fault {
            #[cfg(test)]
            Some(WriteFault::AfterBytes(limit)) => {
                self.file.write_all(&frame[..limit.min(frame.len())])?;
                anyhow::bail!("injected partial archive write");
            }
            _ => self.file.write_all(&frame)?,
        }
        self.logical_position = end_offset;
        self.order = next_order;
        if fragment_offset == 0 {
            self.current_row_start = Some(record_start);
        }
        if final_fragment {
            self.complete_since_commit = true;
            self.latest_complete = Some(ArchiveRowLocation {
                start_offset: self.current_row_start.take().unwrap_or(record_start),
                end_offset,
                row_ordinal,
                source_event_sequence,
                last_fragment_checksum: checksum,
            });
        }
        self.poisoned = false;
        Ok(())
    }

    /// Current append position, useful for constructing durable index entries.
    pub fn position(&mut self) -> Result<u64> {
        ensure!(!self.poisoned, "archive writer poisoned by uncertain write");
        Ok(self.logical_position)
    }

    /// Most recently completed row. This remains provisional until the caller
    /// commits it and durably binds the location into its index/root.
    pub fn last_complete_row(&self) -> Option<ArchiveRowLocation> {
        self.latest_complete
    }

    pub fn commit(&mut self) -> Result<ArchiveReceipt> {
        self.commit_impl(false)
    }

    fn commit_impl(&mut self, fail_sync: bool) -> Result<ArchiveReceipt> {
        ensure!(!self.poisoned, "archive writer poisoned by uncertain write");
        ensure!(
            self.order.open_row.is_none(),
            "cannot commit inside an incomplete row"
        );
        ensure!(
            self.complete_since_commit,
            "no complete archive row to commit"
        );
        self.poisoned = true;
        #[cfg(test)]
        if fail_sync {
            anyhow::bail!("injected archive fsync failure");
        }
        #[cfg(not(test))]
        let _ = fail_sync;
        self.file.sync_all()?;
        let end_offset = self.logical_position;
        let location = self.latest_complete.unwrap();
        self.complete_since_commit = false;
        self.poisoned = false;
        Ok(ArchiveReceipt {
            session_id: self.header.session_id,
            generation: self.header.generation,
            row_ordinal: location.row_ordinal,
            source_event_sequence: location.source_event_sequence,
            end_offset,
            last_fragment_checksum: location.last_fragment_checksum,
        })
    }
}

#[derive(Clone, Copy)]
enum WriteFault {
    #[cfg(test)]
    AfterBytes(usize),
}

/// Streaming reader. It validates each complete row before exposing any of its
/// fragments, then re-reads that row one fragment at a time.
pub struct ArchiveReader {
    file: File,
    header: ArchiveHeader,
    offset: u64,
    order: OrderState,
    replay_end: Option<u64>,
    limit: u64,
    receipt: Option<ArchiveReceipt>,
    row_location: Option<ArchiveRowLocation>,
    receipt_verified: bool,
    poisoned: bool,
    report: ArchiveScan,
}

impl ArchiveReader {
    /// Open for diagnostics. Rows yielded here are CRC-valid and structurally
    /// complete, but this mode does not prove they crossed an fsync boundary.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(0))?;
        let header = read_header(&mut file)?;
        let offset = file.stream_position()?;
        let limit = file.metadata()?.len();
        Ok(Self {
            file,
            header,
            offset,
            order: OrderState::default(),
            replay_end: None,
            poisoned: false,
            limit,
            receipt: None,
            row_location: None,
            receipt_verified: false,
            report: ArchiveScan {
                complete_rows: 0,
                complete_fragments: 0,
                last_complete_row_offset: offset,
                corruption: None,
            },
        })
    }

    /// Open only through an fsync receipt. The offset is a hard read boundary;
    /// the final row is verified against the receipt before any fragment from
    /// that row is exposed.
    pub fn open_committed(path: impl AsRef<Path>, receipt: ArchiveReceipt) -> Result<Self> {
        let mut reader = Self::open(path)?;
        ensure!(
            receipt.session_id == reader.header.session_id
                && receipt.generation == reader.header.generation,
            "archive receipt identity mismatch"
        );
        ensure!(
            receipt.end_offset > reader.offset && receipt.end_offset <= reader.limit,
            "archive receipt offset outside file"
        );
        reader.limit = receipt.end_offset;
        reader.receipt = Some(receipt);
        Ok(reader)
    }

    /// Open exactly one indexed row without scanning its prefix. The location
    /// still requires an externally durable index/root binding for commit proof.
    pub fn open_row(
        path: impl AsRef<Path>,
        expected_header: &ArchiveHeader,
        location: ArchiveRowLocation,
    ) -> Result<Self> {
        expected_header.validate()?;
        let mut reader = Self::open(path)?;
        ensure!(reader.header == *expected_header, "archive header mismatch");
        let header_end = reader.offset;
        ensure!(
            location.start_offset >= header_end
                && location.end_offset > location.start_offset
                && location.end_offset <= reader.limit,
            "archive row location outside file"
        );
        reader.file.seek(SeekFrom::Start(location.start_offset))?;
        reader.offset = location.start_offset;
        reader.limit = location.end_offset;
        reader.row_location = Some(location);
        Ok(reader)
    }

    pub fn header(&self) -> &ArchiveHeader {
        &self.header
    }

    /// Yield fragments only after the whole containing row has validated.
    /// Durability is established only when this reader was opened with receipt.
    pub fn next_fragment(&mut self) -> Result<Option<ArchiveFragment>> {
        ensure!(
            !self.poisoned,
            "archive reader poisoned by prior corruption"
        );
        if let Some(end) = self.replay_end {
            let pos = self.file.stream_position()?;
            if pos >= end {
                self.replay_end = None;
                self.file.seek(SeekFrom::Start(self.offset))?;
                return self.next_fragment();
            }
            let (fragment, _) = self.read_at_current()?;
            if self.file.stream_position()? == end {
                self.replay_end = None;
                self.file.seek(SeekFrom::Start(self.offset))?;
            }
            return Ok(Some(fragment));
        }
        self.prepare_row()?;
        if self.replay_end.is_none() {
            return Ok(None);
        }
        self.next_fragment()
    }

    fn read_at_current(&mut self) -> Result<(ArchiveFragment, u32)> {
        let at = self.file.stream_position()?;
        let result = read_record(&mut self.file, &self.header, at, self.limit).and_then(|record| {
            record.ok_or_else(|| corruption(at, ArchiveCorruptionKind::TruncatedRecord))
        });
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn prepare_row(&mut self) -> Result<()> {
        let row_start = self.offset;
        self.file.seek(SeekFrom::Start(row_start))?;
        let mut next_order = self.order;
        let mut count = 0u64;
        loop {
            let at = self.file.stream_position()?;
            let record = match read_record(&mut self.file, &self.header, at, self.limit) {
                Ok(Some(record)) => record,
                Ok(None) if count == 0 => {
                    if self.receipt.is_some() && !self.receipt_verified {
                        self.poisoned = true;
                        return Err(corruption(at, ArchiveCorruptionKind::Receipt));
                    }
                    if self.row_location.is_some() && !self.receipt_verified {
                        self.poisoned = true;
                        return Err(corruption(at, ArchiveCorruptionKind::Receipt));
                    }
                    return Ok(());
                }
                Ok(None) => {
                    self.poisoned = true;
                    return Err(corruption(row_start, ArchiveCorruptionKind::IncompleteRow));
                }
                Err(error) => {
                    self.poisoned = true;
                    return Err(error);
                }
            };
            let (fragment, checksum) = record;
            if let Err(error) = next_order.accept(&fragment, at) {
                self.poisoned = true;
                return Err(error);
            }
            count += 1;
            if fragment.final_fragment {
                let end = self.file.stream_position()?;
                if let Some(location) = self.row_location {
                    if row_start != location.start_offset
                        || end != location.end_offset
                        || fragment.row_ordinal != location.row_ordinal
                        || fragment.source_event_sequence != location.source_event_sequence
                        || checksum != location.last_fragment_checksum
                    {
                        self.poisoned = true;
                        return Err(corruption(at, ArchiveCorruptionKind::Receipt));
                    }
                    self.receipt_verified = true;
                }
                if let Some(receipt) = self.receipt {
                    if end > receipt.end_offset {
                        self.poisoned = true;
                        return Err(corruption(at, ArchiveCorruptionKind::Receipt));
                    }
                    if end == receipt.end_offset {
                        if fragment.row_ordinal != receipt.row_ordinal
                            || fragment.source_event_sequence != receipt.source_event_sequence
                            || checksum != receipt.last_fragment_checksum
                        {
                            self.poisoned = true;
                            return Err(corruption(at, ArchiveCorruptionKind::Receipt));
                        }
                        self.receipt_verified = true;
                    }
                }
                self.order = next_order;
                self.offset = end;
                self.report.complete_rows += 1;
                self.report.complete_fragments += count;
                self.report.last_complete_row_offset = end;
                self.replay_end = Some(end);
                self.file.seek(SeekFrom::Start(row_start))?;
                return Ok(());
            }
        }
    }

    pub fn scan(&mut self) -> Result<ArchiveScan> {
        loop {
            match self.next_fragment() {
                Ok(Some(_)) => {}
                Ok(None) => return Ok(self.report.clone()),
                Err(error) => {
                    if let Some(corruption) = error.downcast_ref::<ArchiveCorruption>() {
                        let mut report = self.report.clone();
                        report.corruption = Some(corruption.clone());
                        return Ok(report);
                    }
                    return Err(error);
                }
            }
        }
    }
}

/// Diagnose the CRC-valid complete-row prefix. This does not establish that
/// any row was fsynced; use `ArchiveReader::open_committed` with a receipt.
pub fn scan_valid_prefix(path: impl AsRef<Path>) -> Result<ArchiveScan> {
    ArchiveReader::open(path)?.scan()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn header() -> ArchiveHeader {
        ArchiveHeader {
            session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            profile: "test-profile".into(),
        }
    }

    #[test]
    fn row_commit_and_streaming_roundtrip() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let h = header();
        let mut w = ArchiveWriter::create(&path, h.clone())?;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        w.append_fragment(1, 4, 0, false, b"ab")?;
        assert!(w.commit().is_err());
        w.append_fragment(1, 4, 2, true, b"cd")?;
        let receipt = w.commit()?;
        let mut r = ArchiveReader::open(&path)?;
        assert_eq!(r.header(), &h);
        assert_eq!(r.next_fragment()?.unwrap().payload, b"ab");
        let last = r.next_fragment()?.unwrap();
        assert!(last.final_fragment);
        assert_eq!(last.payload, b"cd");
        assert!(r.next_fragment()?.is_none());
        let scan = scan_valid_prefix(&path)?;
        assert_eq!((scan.complete_rows, scan.complete_fragments), (1, 2));
        assert_eq!(scan.last_complete_row_offset, receipt.end_offset);
        Ok(())
    }

    #[test]
    fn committed_reader_honors_receipt_boundary_and_verifies_final_row() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let mut writer = ArchiveWriter::create(&path, header())?;
        writer.append_fragment(1, 1, 0, true, b"committed")?;
        let receipt = writer.commit()?;
        writer.append_fragment(2, 2, 0, true, b"uncommitted")?;
        drop(writer);

        let mut reader = ArchiveReader::open_committed(&path, receipt)?;
        assert_eq!(reader.next_fragment()?.unwrap().payload, b"committed");
        assert!(reader.next_fragment()?.is_none());
        assert_eq!(scan_valid_prefix(&path)?.complete_rows, 2);

        let original = std::fs::read(&path)?;
        let mut malformed_tail = original.clone();
        let tail_record = receipt.end_offset as usize;
        malformed_tail[tail_record + 25..tail_record + 29].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, malformed_tail)?;
        let mut reader = ArchiveReader::open_committed(&path, receipt)?;
        assert_eq!(reader.next_fragment()?.unwrap().payload, b"committed");
        assert!(reader.next_fragment()?.is_none());
        std::fs::write(&path, &original)?;

        let mut bad_checksum = receipt;
        bad_checksum.last_fragment_checksum ^= 1;
        let mut reader = ArchiveReader::open_committed(&path, bad_checksum)?;
        assert!(reader.next_fragment().is_err());
        let mut bad_sequence = receipt;
        bad_sequence.source_event_sequence += 1;
        let mut reader = ArchiveReader::open_committed(&path, bad_sequence)?;
        assert!(reader.next_fragment().is_err());
        let mut bad_row = receipt;
        bad_row.row_ordinal += 1;
        let mut reader = ArchiveReader::open_committed(&path, bad_row)?;
        assert!(reader.next_fragment().is_err());
        let mut bad_boundary = receipt;
        bad_boundary.end_offset -= 1;
        let mut reader = ArchiveReader::open_committed(&path, bad_boundary)?;
        assert!(reader.next_fragment().is_err());
        let mut bad_identity = receipt;
        bad_identity.session_id = Uuid::new_v4();
        assert!(ArchiveReader::open_committed(&path, bad_identity).is_err());
        assert_eq!(std::fs::read(&path)?, original);
        Ok(())
    }

    #[test]
    fn indexed_row_reads_are_direct_bounded_and_validate_the_whole_row() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let h = header();
        let mut writer = ArchiveWriter::create(&path, h.clone())?;
        writer.append_fragment(1, 1, 0, true, b"prefix")?;
        let first = writer.last_complete_row().unwrap();
        writer.append_fragment(2, 2, 0, false, b"middle-")?;
        writer.append_fragment(2, 2, 7, true, b"row")?;
        let middle = writer.last_complete_row().unwrap();
        assert_eq!(middle.start_offset, first.end_offset);
        assert_eq!(middle.end_offset, writer.position()?);
        writer.commit()?;
        writer.append_fragment(3, 3, 0, true, b"later")?;
        drop(writer);

        // Damage an earlier row and a later tail. A direct range read must not
        // inspect either one, while validating all fragments of its target.
        let original = std::fs::read(&path)?;
        let mut damaged = original.clone();
        damaged[first.start_offset as usize + RECORD_HEADER_BYTES] ^= 0x40;
        damaged[middle.end_offset as usize + 25..middle.end_offset as usize + 29]
            .copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, &damaged)?;
        let mut reader = ArchiveReader::open_row(&path, &h, middle)?;
        assert_eq!(reader.next_fragment()?.unwrap().payload, b"middle-");
        let last = reader.next_fragment()?.unwrap();
        assert_eq!(last.payload, b"row");
        assert!(last.final_fragment);
        assert!(reader.next_fragment()?.is_none());

        let mut bad_header = h.clone();
        bad_header.profile.push_str("-other");
        assert!(ArchiveReader::open_row(&path, &bad_header, middle).is_err());
        let mut bad = middle;
        bad.row_ordinal += 1;
        assert!(
            ArchiveReader::open_row(&path, &h, bad)?
                .next_fragment()
                .is_err()
        );
        let mut bad = middle;
        bad.source_event_sequence += 1;
        assert!(
            ArchiveReader::open_row(&path, &h, bad)?
                .next_fragment()
                .is_err()
        );
        let mut bad = middle;
        bad.last_fragment_checksum ^= 1;
        assert!(
            ArchiveReader::open_row(&path, &h, bad)?
                .next_fragment()
                .is_err()
        );
        for bad in [
            ArchiveRowLocation {
                start_offset: 0,
                ..middle
            },
            ArchiveRowLocation {
                start_offset: middle.start_offset + 1,
                ..middle
            },
            ArchiveRowLocation {
                end_offset: middle.start_offset,
                ..middle
            },
            ArchiveRowLocation {
                end_offset: middle.end_offset + 1,
                ..middle
            },
        ] {
            if let Ok(mut reader) = ArchiveReader::open_row(&path, &h, bad) {
                assert!(reader.next_fragment().is_err());
            }
        }
        std::fs::write(&path, original)?;
        Ok(())
    }

    #[test]
    fn logical_positions_match_single_multifragment_and_empty_rows() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let h = header();
        let mut writer = ArchiveWriter::create(&path, h.clone())?;
        let header_end = std::fs::metadata(&path)?.len();
        assert_eq!(writer.position()?, header_end);

        writer.append_fragment(1, 1, 0, true, b"single")?;
        let single = writer.last_complete_row().unwrap();
        assert_eq!(single.start_offset, header_end);
        assert_eq!(single.end_offset, std::fs::metadata(&path)?.len());
        assert_eq!(writer.position()?, single.end_offset);

        writer.append_fragment(2, 2, 0, false, b"multi-")?;
        assert_eq!(writer.position()?, std::fs::metadata(&path)?.len());
        writer.append_fragment(2, 2, 6, true, b"fragment")?;
        let multi = writer.last_complete_row().unwrap();
        assert_eq!(multi.start_offset, single.end_offset);
        assert_eq!(multi.end_offset, std::fs::metadata(&path)?.len());
        assert_eq!(writer.position()?, multi.end_offset);

        writer.append_fragment(3, 3, 0, true, b"")?;
        let empty = writer.last_complete_row().unwrap();
        assert_eq!(empty.start_offset, multi.end_offset);
        assert_eq!(empty.end_offset, std::fs::metadata(&path)?.len());
        let receipt = writer.commit()?;
        assert_eq!(receipt.end_offset, empty.end_offset);

        let mut reader = ArchiveReader::open_row(&path, &h, single)?;
        assert_eq!(reader.next_fragment()?.unwrap().payload, b"single");
        let mut reader = ArchiveReader::open_row(&path, &h, multi)?;
        assert_eq!(reader.next_fragment()?.unwrap().payload, b"multi-");
        assert_eq!(reader.next_fragment()?.unwrap().payload, b"fragment");
        let mut reader = ArchiveReader::open_row(&path, &h, empty)?;
        let fragment = reader.next_fragment()?.unwrap();
        assert!(fragment.payload.is_empty());
        assert!(fragment.final_fragment);
        Ok(())
    }

    #[test]
    fn incomplete_row_is_not_exposed_and_prefix_is_preserved() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let mut w = ArchiveWriter::create(&path, header())?;
        w.append_fragment(1, 1, 0, true, b"complete")?;
        let receipt = w.commit()?;
        w.append_fragment(2, 2, 0, false, b"partial")?;
        drop(w);
        let mut r = ArchiveReader::open(&path)?;
        assert_eq!(r.next_fragment()?.unwrap().payload, b"complete");
        let e = r.next_fragment().unwrap_err();
        assert_eq!(
            e.downcast_ref::<ArchiveCorruption>().unwrap().kind,
            ArchiveCorruptionKind::IncompleteRow
        );
        let s = scan_valid_prefix(&path)?;
        assert_eq!(s.complete_rows, 1);
        assert_eq!(s.last_complete_row_offset, receipt.end_offset);
        assert_eq!(
            s.corruption.unwrap().kind,
            ArchiveCorruptionKind::IncompleteRow
        );
        Ok(())
    }

    #[test]
    fn create_excludes_other_writer_and_header_crc_is_checked() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let h = header();
        let w = ArchiveWriter::create(&path, h.clone())?;
        assert!(ArchiveWriter::create(&path, h).is_err());
        let contender = OpenOptions::new().write(true).open(&path)?;
        assert!(super::super::lock_writer(&contender).is_err());
        drop(w);
        let mut bytes = std::fs::read(&path)?;
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&path, bytes)?;
        assert!(ArchiveReader::open(path).is_err());
        let path = dir.path().join("record");
        let mut writer = ArchiveWriter::create(&path, header())?;
        writer.append_fragment(1, 1, 0, true, b"row")?;
        let receipt = writer.commit()?;
        drop(writer);
        let mut bytes = std::fs::read(&path)?;
        let record_start = receipt.end_offset as usize - 49 - 3;
        bytes[record_start + 45] ^= 1;
        std::fs::write(&path, bytes)?;
        assert_eq!(
            scan_valid_prefix(path)?.corruption.unwrap().kind,
            ArchiveCorruptionKind::Checksum
        );
        Ok(())
    }

    #[test]
    fn all_truncation_cuts_preserve_only_complete_prefix() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let mut w = ArchiveWriter::create(&path, header())?;
        w.append_fragment(1, 1, 0, true, b"row")?;
        let receipt = w.commit()?;
        let bytes = std::fs::read(&path)?;
        let header_end = 16 + u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        for cut in 0..bytes.len() {
            std::fs::write(&path, &bytes[..cut])?;
            if cut < header_end {
                assert!(ArchiveReader::open(&path).is_err());
            } else {
                assert_eq!(
                    scan_valid_prefix(&path)?.complete_rows,
                    u64::from(cut as u64 >= receipt.end_offset)
                );
            }
        }
        Ok(())
    }

    #[test]
    fn monotonic_order_and_payload_bound_are_enforced() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let mut w = ArchiveWriter::create(&path, header())?;
        assert!(
            w.append_fragment(1, 1, 0, true, &vec![0; MAX_PAYLOAD_BYTES + 1])
                .is_err()
        );
        w.append_fragment(3, 1, 0, false, b"a")?;
        assert!(w.append_fragment(4, 1, 1, true, b"b").is_err());
        w.append_fragment(3, 1, 1, true, b"b")?;
        assert!(w.append_fragment(2, 2, 0, true, b"bad").is_err());
        w.append_fragment(3, 2, 0, true, b"good")?;
        w.commit()?;
        Ok(())
    }

    #[test]
    fn uncertain_append_and_sync_poison_writer_until_reopen() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let mut w = ArchiveWriter::create(&path, header())?;
        assert!(
            w.append_fragment_impl(1, 1, 0, true, b"torn", Some(WriteFault::AfterBytes(9)))
                .is_err()
        );
        assert!(w.position().is_err());
        assert!(
            w.append_fragment(1, 1, 0, true, b"cannot continue")
                .is_err()
        );
        drop(w);
        let path = dir.path().join("second");
        let mut w = ArchiveWriter::create(&path, header())?;
        w.append_fragment(1, 1, 0, true, b"row")?;
        assert!(w.commit_impl(true).is_err());
        assert!(w.commit().is_err());
        Ok(())
    }

    #[test]
    fn truncation_after_open_is_corruption_and_poisoned_read_cannot_skip_it() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("archive");
        let mut writer = ArchiveWriter::create(&path, header())?;
        writer.append_fragment(1, 1, 0, false, b"first")?;
        writer.append_fragment(1, 1, 5, true, b"second")?;
        let receipt = writer.commit()?;
        let original = std::fs::read(&path)?;
        let header_end = 16 + u32::from_le_bytes(original[8..12].try_into().unwrap()) as usize;
        let mut reader = ArchiveReader::open_committed(&path, receipt)?;
        // Complete validation and the first fragment have already been read.
        assert_eq!(reader.next_fragment()?.unwrap().payload, b"first");
        writer
            .file
            .set_len((header_end + RECORD_HEADER_BYTES + 5) as u64)?;
        assert!(reader.next_fragment().is_err());
        assert!(reader.next_fragment().is_err());

        std::fs::write(&path, &original)?;
        let mut reader = ArchiveReader::open(&path)?;
        writer.file.set_len(header_end as u64)?;
        let report = reader.scan()?;
        assert_eq!(
            report.corruption.unwrap().kind,
            ArchiveCorruptionKind::TruncatedRecord
        );
        assert_eq!(report.complete_rows, 0);
        Ok(())
    }
}
