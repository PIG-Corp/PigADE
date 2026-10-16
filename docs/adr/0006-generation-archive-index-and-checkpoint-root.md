# ADR0006 — Generation archive index and checkpoint roots

Date: 2026-10-17. Status: implementation under verification, not stage-0 acceptance.

ADR0005's bounded normal history now has a host sink. One generation actor owns
its append-only derived archive and index. This does not add a SQLite writer:
the existing metadata database still has its sole host writer. The archive index
uses fixed 64-byte CRC-protected slots and a generation/profile-bound header.
A root contains one directory name, first ordinal, committed entry count and last
index CRC. Locating a physical record reads its slot and addressed segment range;
startup does not enumerate archive segments or scan their contents.

Segments rotate between records at an 8 MiB target. One physical row may exceed
that target: the codec limits it to 16 MiB, plus bounded metadata and framing, so
the maximum segment is approximately target plus one record. It is not an 8 MiB
hard segment limit. A logical line can span arbitrarily many separately indexed
physical records without being assembled into a single buffer.

For source event N: raw journal fsync → sole model update and bounded provisional
row writes → archive data fsync → index fsync → checkpoint/frame publication.
An append address is not a durability receipt. Index roots become authoritative
only after these barriers and, for recovery, binding into the CRC-protected atomic
PIGCP002 checkpoint. Encoding or storage failure latches degradation and stops
further publication; bounded queues retain pending output and backpressure the PTY.
Source raw journals are retained.

PIGROW02 stores typed cell data with per-cell attribute deltas, canonical UTF-8
scalars, explicit flags/extra characters and row-local hyperlink pooling. It
preserves occupancy and soft-wrap flags; no ANSI parser is added outside the
host. Sparse dynamic-palette snapshots record the palette at retirement, including
changes while alternate screen is active. Indexed colors remain typed instead of
being converted to a guessed RGB palette. Archive display can use the recorded
palette; it must distinguish this historical provenance from later live changes.
The envelope also binds deterministic ordinals, epoch, logical continuation,
palette and source-event sequence. CRC summaries are diagnostics; correctness
tests compare every canonical encoded record and complete hot/parser bytes.

The new codec has an explicit archive profile (`row2,index1,retirement1`). Prior
derived formats are rejected without rewriting originals and rebuilt from raw
events. A valid legacy hot-only checkpoint cannot certify cold history: a valid
raw journal triggers full reconstruction. If raw corruption prevents that, the
legacy hot checkpoint can still be shown with an explicit unavailable-history
warning; no complete-history claim follows.

Recovery keeps the immutable checkpoint prefix and builds its bounded source tail
in a separate private temporary archive. At most two store references are held.
Recovered caches cannot be published as source checkpoints; restarts always use
the original source checkpoint and raw tail, so cache lineage does not grow.
Normal shutdown removes private caches. Orphans after abrupt process death still
need quota-aware cleanup; originals must never be confused with those caches.

Replay closes each ordered source event without publishing a frame, then fsyncs
the derived tail once before exposing the recovered view. This batching applies
only to private replay caches whose source journal is already durable. Live
history retains its data-then-index barrier for every source event. A replay
cache with pending writes cannot serve indexed records or become a checkpoint.

Checkpoint export/import/pruning is serialized across actors to limit concurrent
hot-grid duplication. A bounded cache remembers semantic validation for unchanged
checkpoint files, keyed by device/inode/size/mtime/ctime plus session, generation
and profile. External cold-root dependencies are still checked. Changed or unknown
files get full validation; damaged originals remain preserved. This removes
repeated imports of three identical hot grids during every publication. It does
not establish the global heap budget or remove the need for measured recovery.

The model-work checkpoint trigger measures parsing and retirement encoding;
journal/archive fsync time is measured separately and is not charged as replay
work. Including storage waits caused unnecessary checkpoint exports under load.
Clean actors block for work; dirty actors wake for the five-second checkpoint
deadline, and synchronized output keeps its ordered timeout. A nonsemantic wake
message keeps priority Stop serviceable without periodic idle polling.

Launch-helper handshakes use the fixed private `g.sock` path, shorter than the
main host socket. A UUID-length suffix exceeded the Unix socket path limit for
otherwise valid state directories. Launches are serialized by the existing host
writer and nonce/peer-identity checks remain unchanged. Socket cleanup checks
ownership/type and, on drop, device/inode; unexpected paths remain intact.

Open work includes paginated product history/copy/search, durable storage quota
signals, closed-segment compression, cache-orphan collection, aggregate admission
for live extras/reflow, the full failure matrix, VM reboot and all long-history,
platform, real-time soak and MVP gates. No criterion is relaxed by this ADR.
