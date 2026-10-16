# Storage contract (Linux prototype)

`Journal` holds an exclusive advisory writer lock. A generation has one immutable
header (session/generation UUID, initial geometry, core/Unicode/font profile).
Each append records a strictly increasing sequence, generation UUID, type, payload
length and CRC32 over both metadata and payload. Events are at most 1 MiB; headers
are at most 64 KiB. Reader allocation is bounded to one event. CRC detects damage;
it is not authentication against a malicious actor.

`append` returns a `Receipt` only after `File::sync_all`. Publish terminal changes
only after this receipt. Every I/O failure poisons that writer; do not retry on the
same handle. A complete unacknowledged event can survive an uncertain fsync; reopen
and replay to determine the valid persisted prefix rather than reuse its sequence.
These tests establish syscall ordering and process-crash recovery, not hardware
power-loss guarantees.

`scan_valid_prefix` reports the valid byte/sequence watermark and an explicit
corruption kind. A malformed header is an error because no initial profile can be
trusted. `JournalReader` replays valid events until corruption and then refuses to
continue. Recovery leaves original bytes intact. `Journal::open` refuses a torn
or corrupt tail: it never silently truncates or overwrites history. Archival,
segment continuation, checkpoints and compaction are not implemented here.

`MetadataStore` holds the sole writer lock and uses SQLite WAL with synchronous
FULL. `begin_launch` commits an intent and the session generation lock in one
transaction. Its `created` flag is false on every identical operation retry;
callers must never create another helper from such a result. Helper OS identity
is persisted before `permit_exec`, which is committed before the host authorizes
actual exec. Unknown outcomes keep the generation lock. Only explicit verified
reconciliation releases an unknown lock. An absent PID in an in-memory registry
is not reconciliation evidence. The host validates the OS identity/nonce and
implements the helper handshake; this crate supplies persistence, not the OS gate.

Files are created with mode 0600. The host must create its dedicated state
directory with mode 0700 and must not place these files in a shared directory.
Schema versions newer than the writer are rejected before changing journal mode.
No product license is chosen by this crate.

```sh
cargo test -p pigade-storage
cargo clippy -p pigade-storage --all-targets -- -D warnings
cargo run --release -p pigade-storage --example journal_bench -- /private/state/new-benchmark.journal 1000
```

The benchmark is a storage microbenchmark, not a product performance result. Use
a new path and capture hardware/filesystem/power profile separately. Test probes
include every partial-frame write boundary, uncertain fsync, checksums, generation
and sequence corruption, launch ordering, idempotency, persistent unknown locks,
and real SIGKILL of journal and SQLite writers. VM reboot and 24-hour soak remain
integration validation outside this crate.
