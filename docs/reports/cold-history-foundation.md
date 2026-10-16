# Cold-history foundation — 0.0.1-dev

2026-10-17. This slice is not production archival or long-history acceptance.
ADR0005 defines bounded hot state and a separate immutable derived archive.

## Implemented boundary

The normal Grid can synchronously lend retiring rows to a runtime observer before
scroll reuse, width-shrink overflow, history-limit reduction, clear and reset.
The observer follows normal state across alternate-screen swaps. Reset reports
history and viewport once, then an epoch boundary; its internal clear is not a
second archival operation. Observers are absent on checkpoint import and skipped
by serialization. Row layout and its unsafe four-word swap remain unchanged.

Five core regressions check ordering, width-shrink retention, single reset epoch,
height growth and cursor-driven width growth. A composed test exceeds 10,000 hot
physical rows, exercises inactive-normal resize, alternate redraw, clear and RIS,
and compares complete serialized hot Term/parser checkpoints with and without an
observer. This proves the notification slice does not alter those tested hot
states; it does not prove a future persistent sink is correct.

Review initially suspected that height growth and the width-growth cursor branch
also lose old history. An executed probe disproved both suspicions: those paths
pull history into the viewport or remove empty bottom bookkeeping. Permanent
regressions now preserve those cases. No generic Storage shrink notification was
added, since it would archive blank or duplicate rows.

## Derived fragment file

`crates/storage/src/archive.rs` provides PIGARC01 files with a CRC-protected
session/generation/profile header and records bounded to 64 KiB payloads. A row
can span records; fragments have exact increasing byte offsets, one source event
sequence and a final marker. Rows and source sequences are monotonic. Appending
does not grant a durability receipt. Commit is allowed only between complete
rows, fsyncs, and returns the final generation-bound offset/ordinal/sequence and
last-fragment CRC. Uncertain writes or fsync poison the writer. Creation is
exclusive, mode0600, writer-locked and directory-fsynced.

The reader validates a whole physical row before exposing its fragments, using
bounded buffers and a second read rather than collecting the row in RAM.
`open_committed` freezes reads at a supplied receipt and verifies the final row
against it. Tail bytes cannot extend that committed view. Diagnostic open/scan
reports CRC-complete rows, **not proof of fsync durability**. File truncation
after opening is corruption, including during the fragment re-read; errors latch
instead of permitting a subsequent call to skip the damaged record. All reads
leave source bytes unchanged. Eight storage tests cover cuts, corruption,
receipt mismatch, unfinished rows, ordering, locks, bounds and write/fsync faults.

## Integration deliberately remains incomplete

The host does not attach a persistent observer yet. No archive is claimed in its
current checkpoint envelope. Typed cells/extras/link encoding and pooling,
palette provenance, soft-wrap continuation across hot/archive boundaries,
segment rotation, lazy index, durable index roots, replay idempotency and archive
fault recovery are next. A source journal remains authoritative and is retained.
The generic file's receipt must be durably bound to the host's event/checkpoint
root before it can authorize product history display.

No large-history, reboot, 24-hour soak, aggregate heap, input-to-present or
cross-platform result is inferred from this component work. In particular,
width-shrink still builds temporary reflow storage before discarding overflow,
and live combining sequences remain an unresolved resource gate.
