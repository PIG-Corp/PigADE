//! Host-owned derived cold history. Checkpoints bind an fsynced index prefix;
//! recovery writes only a separate temporary tail, never the source archive.
use anyhow::{Context, Result, ensure};
use pigade_storage::{
    JournalHeader,
    archive::{ArchiveHeader, ArchiveReader, ArchiveRowLocation, ArchiveWriter, MAX_PAYLOAD_BYTES},
};
use pigade_terminal::{
    CellFlags, HistoryCell, HistoryObserver, HistoryRetirement, HistoryRow, PaletteRgb,
    history::{MAX_ROW_BYTES, encode_row},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

const INDEX_MAGIC: &[u8; 8] = b"PIGIDX01";
const INDEX_HEADER: u64 = 64;
const INDEX_SLOT: u64 = 64;
const SEGMENT_TARGET: u64 = 8 * 1024 * 1024;
const METADATA_LIMIT: usize = 32 * 1024;
const RECORD_LIMIT: usize = MAX_ROW_BYTES + METADATA_LIMIT + 4;
const PALETTE_SIZE: usize = 269;
pub const ARCHIVE_FORMAT: &str = "row2,index1,retirement1";
#[cfg(test)]
mod crash_tests;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub next_entry: u64,
    pub retired_rows: u64,
    pub epoch: u64,
    pub logical_line: u64,
    pub part: u64,
    pub previous_wrap: bool,
    pub palette: Vec<Option<[u8; 3]>>,
    pub digest: u32,
    pub applied_sequence: u64,
}
impl Default for State {
    fn default() -> Self {
        Self {
            next_entry: 0,
            retired_rows: 0,
            epoch: 0,
            logical_line: 0,
            part: 0,
            previous_wrap: false,
            palette: vec![None; PALETTE_SIZE],
            digest: 0,
            applied_sequence: 0,
        }
    }
}
impl State {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.palette.len() == PALETTE_SIZE,
            "invalid history palette"
        );
        ensure!(
            self.retired_rows <= self.next_entry,
            "invalid history counters"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreRoot {
    pub directory: String,
    pub first_ordinal: u64,
    pub entries: u64,
    pub last_index_crc: u32,
}
impl StoreRoot {
    fn end(&self) -> Result<u64> {
        self.first_ordinal
            .checked_add(self.entries)
            .context("history ordinal overflow")
    }
    fn path(&self, parent: &Path) -> Result<PathBuf> {
        let path = Path::new(&self.directory);
        ensure!(
            matches!(
                (path.components().next(), path.components().nth(1)),
                (Some(Component::Normal(_)), None)
            ),
            "history directory must be one relative component"
        );
        Ok(parent.join(path))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema: u32,
    pub root: StoreRoot,
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub records: u64,
    pub retired_rows: u64,
    pub epoch: u64,
    pub digest: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Row,
    Boundary,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordMetadata {
    pub schema: u32,
    pub kind: Kind,
    pub epoch: u64,
    pub logical_line: u64,
    pub part: u64,
    pub reason: String,
    pub wrap: bool,
    /// Sparse dynamic palette at retirement; indexed cell colors stay typed.
    pub palette: Vec<(u16, [u8; 3])>,
}

fn header(source: &JournalHeader) -> ArchiveHeader {
    ArchiveHeader {
        session_id: source.session_id,
        generation: source.generation,
        profile: format!("{};cold={ARCHIVE_FORMAT}", source.profile),
    }
}
fn index_header(header: &ArchiveHeader) -> [u8; 64] {
    let mut bytes = [0; 64];
    bytes[..8].copy_from_slice(INDEX_MAGIC);
    bytes[8..24].copy_from_slice(header.session_id.as_bytes());
    bytes[24..40].copy_from_slice(header.generation.as_bytes());
    bytes[40..44].copy_from_slice(&crc32fast::hash(header.profile.as_bytes()).to_le_bytes());
    let crc = crc32fast::hash(&bytes[..60]);
    bytes[60..].copy_from_slice(&crc.to_le_bytes());
    bytes
}
#[derive(Clone, Copy)]
struct IndexEntry {
    segment: u64,
    location: ArchiveRowLocation,
}
impl IndexEntry {
    fn bytes(self) -> [u8; 64] {
        let mut bytes = [0; 64];
        for (slot, value) in [
            self.location.row_ordinal,
            self.location.source_event_sequence,
            self.segment,
            self.location.start_offset,
            self.location.end_offset,
        ]
        .into_iter()
        .enumerate()
        {
            bytes[slot * 8..slot * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[40..44].copy_from_slice(&self.location.last_fragment_checksum.to_le_bytes());
        let crc = crc32fast::hash(&bytes[..60]);
        bytes[60..].copy_from_slice(&crc.to_le_bytes());
        bytes
    }
    fn decode(bytes: [u8; 64], ordinal: u64) -> Result<Self> {
        ensure!(bytes[44..60] == [0; 16], "unknown history index fields");
        ensure!(
            crc32fast::hash(&bytes[..60]) == u32::from_le_bytes(bytes[60..].try_into()?),
            "history index CRC mismatch"
        );
        let value = |index: usize| {
            u64::from_le_bytes(
                bytes[index * 8..index * 8 + 8]
                    .try_into()
                    .expect("fixed slice"),
            )
        };
        ensure!(value(0) == ordinal, "history index ordinal mismatch");
        Ok(Self {
            segment: value(2),
            location: ArchiveRowLocation {
                row_ordinal: value(0),
                source_event_sequence: value(1),
                start_offset: value(3),
                end_offset: value(4),
                last_fragment_checksum: u32::from_le_bytes(bytes[40..44].try_into()?),
            },
        })
    }
}
fn segment_path(root: &Path, segment: u64) -> PathBuf {
    root.join(format!("{segment:020}.archive"))
}
fn open_index(parent: &Path, root: &StoreRoot, h: &ArchiveHeader) -> Result<File> {
    let path = root.path(parent)?;
    ensure!(
        !fs::symlink_metadata(&path)?.file_type().is_symlink(),
        "symlink history directory"
    );
    let mut file = File::open(path.join("rows.index"))?;
    let mut bytes = [0; 64];
    file.read_exact(&mut bytes)?;
    ensure!(
        bytes == index_header(h),
        "history index identity/profile mismatch"
    );
    let length = root
        .entries
        .checked_mul(INDEX_SLOT)
        .and_then(|n| n.checked_add(INDEX_HEADER))
        .context("history index length overflow")?;
    ensure!(
        file.metadata()?.len() >= length,
        "history index shorter than committed root"
    );
    if root.entries > 0 {
        file.seek(SeekFrom::Start(length - INDEX_SLOT))?;
        file.read_exact(&mut bytes)?;
        let _ = IndexEntry::decode(bytes, root.end()? - 1)?;
        ensure!(
            u32::from_le_bytes(bytes[60..].try_into()?) == root.last_index_crc,
            "history index root mismatch"
        );
    } else {
        ensure!(
            root.last_index_crc == 0,
            "nonempty CRC for empty history index"
        );
    }
    Ok(file)
}

struct Store {
    parent: PathBuf,
    reference: StoreRoot,
    header: ArchiveHeader,
    index: File,
    writer: Option<ArchiveWriter>,
    segment: u64,
    dirty: bool,
    #[cfg(test)]
    fail_before_index_sync: bool,
}
impl Store {
    fn create(
        parent: &Path,
        directory: String,
        h: ArchiveHeader,
        first: u64,
        exists: bool,
    ) -> Result<Self> {
        let path = parent.join(&directory);
        if !exists {
            fs::DirBuilder::new().mode(0o700).create(&path)?;
        }
        let mut index = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path.join("rows.index"))?;
        index.write_all(&index_header(&h))?;
        index.sync_all()?;
        File::open(&path)?.sync_all()?;
        File::open(parent)?.sync_all()?;
        Ok(Self {
            parent: parent.into(),
            reference: StoreRoot {
                directory,
                first_ordinal: first,
                entries: 0,
                last_index_crc: 0,
            },
            header: h,
            index,
            writer: None,
            segment: 0,
            dirty: false,
            #[cfg(test)]
            fail_before_index_sync: false,
        })
    }
    fn append(
        &mut self,
        sequence: u64,
        ordinal: u64,
        metadata: &RecordMetadata,
        row: Option<&HistoryRow<HistoryCell>>,
        previous_digest: u32,
    ) -> Result<u32> {
        ensure!(
            ordinal == self.reference.end()?,
            "non-contiguous history append"
        );
        if let Some(writer) = &mut self.writer
            && writer.position()? >= SEGMENT_TARGET
        {
            // Previous writes are fsynced before closing a segment, even if
            // rotation occurs in the middle of one large source event.
            if self.dirty {
                writer.commit()?;
            }
            self.writer = None;
            self.segment = self
                .segment
                .checked_add(1)
                .context("history segment overflow")?;
        }
        if self.writer.is_none() {
            self.writer = Some(ArchiveWriter::create(
                segment_path(&self.reference.path(&self.parent)?, self.segment),
                self.header.clone(),
            )?);
        }
        let writer = self.writer.as_mut().expect("created writer");
        let metadata = serde_json::to_vec(metadata)?;
        ensure!(
            metadata.len() <= METADATA_LIMIT,
            "history record metadata exceeds budget"
        );
        let mut fragments = FragmentWriter::new(writer, sequence, ordinal, previous_digest);
        fragments.write_all(&(metadata.len() as u32).to_le_bytes())?;
        fragments.write_all(&metadata)?;
        if let Some(row) = row {
            encode_row(row, &mut fragments)?;
        }
        let digest = fragments.finish()?;
        let entry = IndexEntry {
            segment: self.segment,
            location: writer
                .last_complete_row()
                .context("archive row incomplete")?,
        };
        let bytes = entry.bytes();
        self.index.write_all(&bytes)?;
        self.reference.entries = self
            .reference
            .entries
            .checked_add(1)
            .context("history entry overflow")?;
        self.reference.last_index_crc = u32::from_le_bytes(bytes[60..].try_into()?);
        self.dirty = true;
        Ok(digest)
    }
    fn commit(&mut self) -> Result<()> {
        if self.dirty {
            self.writer
                .as_mut()
                .context("missing history writer")?
                .commit()?;
            #[cfg(test)]
            if self.fail_before_index_sync {
                anyhow::bail!("injected failure after archive data fsync, before index fsync");
            }
            self.index.sync_all()?;
            self.dirty = false;
        }
        Ok(())
    }
}

struct FragmentWriter<'a> {
    writer: &'a mut ArchiveWriter,
    sequence: u64,
    ordinal: u64,
    offset: u64,
    buffer: Vec<u8>,
    hasher: crc32fast::Hasher,
    total: usize,
}
impl<'a> FragmentWriter<'a> {
    fn new(writer: &'a mut ArchiveWriter, sequence: u64, ordinal: u64, digest: u32) -> Self {
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&digest.to_le_bytes());
        Self {
            writer,
            sequence,
            ordinal,
            offset: 0,
            buffer: Vec::with_capacity(MAX_PAYLOAD_BYTES),
            hasher,
            total: 0,
        }
    }
    fn finish(self) -> Result<u32> {
        self.writer.append_fragment(
            self.sequence,
            self.ordinal,
            self.offset,
            true,
            &self.buffer,
        )?;
        Ok(self.hasher.finalize())
    }
}
impl Write for FragmentWriter<'_> {
    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<usize> {
        let length = bytes.len();
        ensure_io(
            self.total
                .checked_add(length)
                .is_some_and(|n| n <= RECORD_LIMIT),
            "history record byte budget exceeded",
        )?;
        self.total += length;
        self.hasher.update(bytes);
        while !bytes.is_empty() {
            let n = bytes.len().min(MAX_PAYLOAD_BYTES - self.buffer.len());
            self.buffer.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.buffer.len() == MAX_PAYLOAD_BYTES {
                self.writer
                    .append_fragment(
                        self.sequence,
                        self.ordinal,
                        self.offset,
                        false,
                        &self.buffer,
                    )
                    .map_err(std::io::Error::other)?;
                self.offset += self.buffer.len() as u64;
                self.buffer.clear();
            }
        }
        Ok(length)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn ensure_io(valid: bool, message: &str) -> std::io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(std::io::Error::other(message))
    }
}

struct Inner {
    store: Store,
    base: Option<StoreRoot>,
    state: State,
    pending_sequence: Option<u64>,
    error: Option<String>,
    _temporary: Option<tempfile::TempDir>,
}
pub struct History {
    parent: PathBuf,
    header: ArchiveHeader,
    inner: Mutex<Inner>,
}
impl std::fmt::Debug for History {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostHistory")
    }
}
impl History {
    pub fn live(parent: &Path, source: &JournalHeader) -> Result<Arc<Self>> {
        let h = header(source);
        let store = Store::create(
            parent,
            format!("{}.history", source.generation),
            h.clone(),
            0,
            false,
        )?;
        Ok(Arc::new(Self {
            parent: parent.into(),
            header: h,
            inner: Mutex::new(Inner {
                store,
                base: None,
                state: State::default(),
                pending_sequence: None,
                error: None,
                _temporary: None,
            }),
        }))
    }
    pub fn replay(
        parent: &Path,
        source: &JournalHeader,
        saved: Option<Checkpoint>,
    ) -> Result<Arc<Self>> {
        let h = header(source);
        let (base, state) = if let Some(saved) = saved {
            validate_checkpoint(parent, source, &saved)?;
            (Some(saved.root), saved.state)
        } else {
            (None, State::default())
        };
        let temporary = tempfile::Builder::new()
            .prefix(".history-replay-")
            .tempdir_in(parent)?;
        let directory = temporary
            .path()
            .file_name()
            .context("replay directory name")?
            .to_str()
            .context("replay name UTF8")?
            .to_owned();
        let store = Store::create(parent, directory, h.clone(), state.next_entry, true)?;
        Ok(Arc::new(Self {
            parent: parent.into(),
            header: h,
            inner: Mutex::new(Inner {
                store,
                base,
                state,
                pending_sequence: None,
                error: None,
                _temporary: Some(temporary),
            }),
        }))
    }
    pub fn begin(&self, sequence: u64) -> Result<()> {
        let mut i = self.inner.lock().expect("history lock");
        if let Some(error) = &i.error {
            anyhow::bail!("history failed: {error}");
        }
        ensure!(
            i.pending_sequence.is_none() && sequence > i.state.applied_sequence,
            "history source sequence out of order"
        );
        i.pending_sequence = Some(sequence);
        Ok(())
    }
    pub fn commit(&self) -> Result<()> {
        self.end_event(true)
    }
    /// Recovery publishes only its final frame. Intermediate source events keep
    /// their order/identities but may share the final derived-cache fsync.
    pub(crate) fn replay_event(&self) -> Result<()> {
        self.end_event(false)
    }
    fn end_event(&self, durable: bool) -> Result<()> {
        let mut i = self.inner.lock().expect("history lock");
        if let Some(error) = &i.error {
            anyhow::bail!("history failed: {error}");
        }
        ensure!(
            durable || i._temporary.is_some(),
            "live history requires an event fsync"
        );
        let sequence = i.pending_sequence.context("history event not started")?;
        if durable && let Err(error) = i.store.commit() {
            i.error = Some(format!("{error:#}"));
            return Err(error);
        }
        i.pending_sequence = None;
        i.state.applied_sequence = sequence;
        Ok(())
    }
    pub(crate) fn finish_replay(&self) -> Result<()> {
        let mut i = self.inner.lock().expect("history lock");
        ensure!(
            i._temporary.is_some() && i.pending_sequence.is_none() && i.error.is_none(),
            "invalid replay publication"
        );
        if let Err(error) = i.store.commit() {
            i.error = Some(format!("{error:#}"));
            return Err(error);
        }
        Ok(())
    }
    pub fn checkpoint(&self) -> Result<Checkpoint> {
        let i = self.inner.lock().expect("history lock");
        ensure!(
            i.error.is_none() && i.pending_sequence.is_none() && !i.store.dirty,
            "uncommitted history checkpoint"
        );
        ensure!(
            i.base.is_none() && i._temporary.is_none(),
            "replay cache cannot become a source checkpoint"
        );
        Ok(Checkpoint {
            schema: 1,
            root: i.store.reference.clone(),
            state: i.state.clone(),
        })
    }
    pub fn state(&self) -> State {
        self.inner.lock().expect("history lock").state.clone()
    }
    pub fn summary(&self) -> Summary {
        let s = self.state();
        Summary {
            records: s.next_entry,
            retired_rows: s.retired_rows,
            epoch: s.epoch,
            digest: s.digest,
        }
    }
    /// Read one physical record using one fixed index slot and the addressed
    /// segment range. Older prefix data is never scanned during startup/read.
    pub fn record(&self, ordinal: u64) -> Result<Vec<u8>> {
        let i = self.inner.lock().expect("history lock");
        ensure!(
            i.error.is_none() && i.pending_sequence.is_none() && !i.store.dirty,
            "history is not committed"
        );
        let tail = &i.store.reference;
        let reference = if ordinal >= tail.first_ordinal {
            tail
        } else {
            i.base
                .as_ref()
                .context("history ordinal before available range")?
        };
        read_record(&self.parent, reference, &self.header, ordinal)
    }
    fn notification(&self, row: Option<&HistoryRow<HistoryCell>>, reason: HistoryRetirement) {
        let mut i = self.inner.lock().expect("history lock");
        if i.error.is_some() {
            return;
        }
        let result = (|| -> Result<()> {
            let sequence = i
                .pending_sequence
                .context("history notification outside durable event")?;
            let s = &i.state;
            let wrap = row
                .and_then(|row| row.last())
                .is_some_and(|cell| cell.flags.contains(CellFlags::WRAPLINE));
            let metadata = RecordMetadata {
                schema: 1,
                kind: if row.is_some() {
                    Kind::Row
                } else {
                    Kind::Boundary
                },
                epoch: s.epoch,
                logical_line: s.logical_line,
                part: s.part,
                reason: match reason {
                    HistoryRetirement::Scroll => "scroll",
                    HistoryRetirement::WidthReflow => "width_reflow",
                    HistoryRetirement::HistoryLimit => "history_limit",
                    HistoryRetirement::Clear => "clear",
                    HistoryRetirement::Reset => "reset",
                }
                .into(),
                wrap,
                palette: s
                    .palette
                    .iter()
                    .enumerate()
                    .filter_map(|(index, rgb)| rgb.map(|rgb| (index as u16, rgb)))
                    .collect(),
            };
            let ordinal = s.next_entry;
            let digest = s.digest;
            let digest = i.store.append(sequence, ordinal, &metadata, row, digest)?;
            let s = &mut i.state;
            s.next_entry = s
                .next_entry
                .checked_add(1)
                .context("history ordinal overflow")?;
            s.digest = digest;
            if row.is_some() {
                s.retired_rows = s
                    .retired_rows
                    .checked_add(1)
                    .context("history row count overflow")?;
                if wrap {
                    s.part = s
                        .part
                        .checked_add(1)
                        .context("history continuation overflow")?;
                } else {
                    s.logical_line = s
                        .logical_line
                        .checked_add(1)
                        .context("history line overflow")?;
                    s.part = 0;
                }
                s.previous_wrap = wrap;
            } else {
                s.epoch = s.epoch.checked_add(1).context("history epoch overflow")?;
                s.logical_line = s
                    .logical_line
                    .checked_add(1)
                    .context("history line overflow")?;
                s.part = 0;
                s.previous_wrap = false;
            }
            Ok(())
        })();
        if let Err(error) = result {
            i.error = Some(format!("{error:#}"));
        }
    }
}
impl HistoryObserver<HistoryCell> for History {
    fn row(&self, row: &HistoryRow<HistoryCell>, columns: usize, reason: HistoryRetirement) {
        if row.len() != columns {
            self.inner.lock().expect("history lock").error =
                Some("retired row geometry mismatch".into());
            return;
        }
        self.notification(Some(row), reason);
    }
    fn boundary(&self, reason: HistoryRetirement) {
        self.notification(None, reason);
    }
    fn palette(&self, index: usize, color: Option<PaletteRgb>) {
        let mut i = self.inner.lock().expect("history lock");
        if index >= PALETTE_SIZE {
            i.error = Some("invalid history palette index".into());
            return;
        }
        let color = color.map(|rgb| [rgb.r, rgb.g, rgb.b]);
        if i.pending_sequence.is_none()
            && i.state.applied_sequence > 0
            && i.state.palette[index] != color
        {
            i.error = Some("hot/checkpoint history palette mismatch".into());
            return;
        }
        i.state.palette[index] = color;
    }
}

pub fn validate_checkpoint(
    parent: &Path,
    source: &JournalHeader,
    saved: &Checkpoint,
) -> Result<()> {
    ensure!(saved.schema == 1, "unsupported cold history checkpoint");
    saved.state.validate()?;
    ensure!(
        saved.root.first_ordinal == 0 && saved.root.end()? == saved.state.next_entry,
        "history checkpoint root/count mismatch"
    );
    ensure!(
        saved.root.directory == format!("{}.history", source.generation),
        "history checkpoint directory mismatch"
    );
    let mut file = open_index(parent, &saved.root, &header(source))?;
    if saved.root.entries > 0 {
        file.seek(SeekFrom::Start(
            INDEX_HEADER + (saved.root.entries - 1) * INDEX_SLOT,
        ))?;
        let mut bytes = [0; 64];
        file.read_exact(&mut bytes)?;
        let entry = IndexEntry::decode(bytes, saved.root.entries - 1)?;
        ensure!(
            entry.location.source_event_sequence <= saved.state.applied_sequence,
            "history root ahead of checkpoint event"
        );
        // Validate the bounded root dependency, never the whole cold prefix.
        let mut reader = ArchiveReader::open_row(
            segment_path(&saved.root.path(parent)?, entry.segment),
            &header(source),
            entry.location,
        )?;
        ensure!(
            reader.next_fragment()?.is_some(),
            "history root references an empty row range"
        );
    }
    Ok(())
}

fn read_record(
    parent: &Path,
    root: &StoreRoot,
    h: &ArchiveHeader,
    ordinal: u64,
) -> Result<Vec<u8>> {
    ensure!(
        ordinal >= root.first_ordinal && ordinal < root.end()?,
        "history ordinal outside committed root"
    );
    let mut file = open_index(parent, root, h)?;
    file.seek(SeekFrom::Start(
        INDEX_HEADER + (ordinal - root.first_ordinal) * INDEX_SLOT,
    ))?;
    let mut bytes = [0; 64];
    file.read_exact(&mut bytes)?;
    let entry = IndexEntry::decode(bytes, ordinal)?;
    ensure!(
        entry
            .location
            .end_offset
            .saturating_sub(entry.location.start_offset)
            <= (RECORD_LIMIT + (RECORD_LIMIT / MAX_PAYLOAD_BYTES + 2) * 49) as u64,
        "history record disk budget exceeded"
    );
    let mut reader = ArchiveReader::open_row(
        segment_path(&root.path(parent)?, entry.segment),
        h,
        entry.location,
    )?;
    let mut encoded = Vec::new();
    while let Some(fragment) = reader.next_fragment()? {
        ensure!(
            encoded.len() + fragment.payload.len() <= RECORD_LIMIT,
            "history record decode byte budget exceeded"
        );
        encoded.extend_from_slice(&fragment.payload);
    }
    Ok(encoded)
}
pub fn decode_record(
    bytes: &[u8],
) -> Result<(RecordMetadata, Option<pigade_terminal::history::DecodedRow>)> {
    ensure!(
        bytes.len() >= 4 && bytes.len() <= RECORD_LIMIT,
        "history record length invalid"
    );
    let length = u32::from_le_bytes(bytes[..4].try_into()?) as usize;
    ensure!(
        length <= METADATA_LIMIT && length <= bytes.len() - 4,
        "history metadata length invalid"
    );
    let metadata: RecordMetadata = serde_json::from_slice(&bytes[4..4 + length])?;
    ensure!(
        metadata.schema == 1 && metadata.palette.len() <= PALETTE_SIZE,
        "invalid history metadata"
    );
    ensure!(
        matches!(
            metadata.reason.as_str(),
            "scroll" | "width_reflow" | "history_limit" | "clear" | "reset"
        ),
        "invalid history retirement reason"
    );
    ensure!(
        metadata.kind != Kind::Boundary || matches!(metadata.reason.as_str(), "clear" | "reset"),
        "invalid history epoch boundary"
    );
    let mut previous = None;
    for (index, _) in &metadata.palette {
        ensure!(
            (*index as usize) < PALETTE_SIZE && previous.is_none_or(|last| last < *index),
            "invalid history palette order"
        );
        previous = Some(*index);
    }
    let body = &bytes[4 + length..];
    let row = match metadata.kind {
        Kind::Row => Some(pigade_terminal::history::decode_row(body)?),
        Kind::Boundary => {
            ensure!(body.is_empty(), "boundary has unexpected cells");
            None
        }
    };
    if let Some(row) = &row {
        ensure!(
            row.cells
                .last()
                .is_some_and(|cell| cell.flags.contains(CellFlags::WRAPLINE))
                == metadata.wrap,
            "history soft-wrap metadata mismatch"
        );
    } else {
        ensure!(!metadata.wrap, "epoch boundary cannot soft-wrap");
    }
    Ok((metadata, row))
}
