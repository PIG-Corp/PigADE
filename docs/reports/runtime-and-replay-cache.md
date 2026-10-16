# Runtime endpoints and replay cache maintenance — 0.0.1-dev

The long-history recovery harness exposed a real path-length failure: a retained
corpus directory could hold valid durable files but could not bind `host.sock`
within Unix `sun_path`. The failed run is preserved in
[`recovery-smoke-long-socket-failure.json`](../../benchmarks/results/recovery-smoke-long-socket-failure.json).

`serve STATE_DIRECTORY --runtime PRIVATE_DIRECTORY` now separates endpoint paths
from durable data. The runtime directory is dedicated, owned and mode 0700;
sockets are 0600. A no-follow, regular, single-link 0600 `endpoint.lock` stays
exclusively locked throughout service. The state lock remains independent.
Neither a different state using the same runtime nor the same state using a
new runtime may replace the live owner's endpoint. Launch-helper gates use that
same short runtime directory; journals, SQLite, checkpoints and archives remain
under the durable state path. Existing `serve STATE_DIRECTORY` keeps its former
state-local endpoints.

A real-PTY regression launches from a state path longer than 108 bytes, verifies
input, refuses both duplicate-owner combinations without changing the socket
inode/PID, then kills/reopens the host and restores the historical generation.
The three-cycle small-corpus smoke passed with identical frames, history roots,
sequences and source-file hashes. Its host-only p50/p95/p99 were
1144.62/1200.63/1200.63 ms under concurrent corpus work; this is functionality
proof, not the 1 GiB workload or an isolated performance result. See
[`recovery-smoke-runtime.json`](../../benchmarks/results/recovery-smoke-runtime.json).

New private replay caches hold an exclusive lease for their full lifetime. A
bounded CRC-protected owner marker binds schema, session/generation, exact core
and archive profiles, and directory device/inode. The collector ignores old
unmarked caches, invalid/foreign markers, symlinks, unsafe owners/modes and live
leases. Valid orphans move through a same-parent no-replace quarantine; only
flat owned regular contents are reclaimed, with the marker removed last.
Interrupted collection therefore leaves a recognizable private cache. Sources
never occupy this namespace and remain untouched.

Collection runs once in a background maintenance thread after recovery and IPC
binding. First frames do not wait for orphan deletion. A pass scans at most 256
parent entries and processes at most 512 files per candidate; preserved or
incomplete candidates can remain for a later pass. The lease also protects
independent recovery verifiers that do not hold the host's state lock. Tests
include a real child SIGKILL, live-lease retention, malformed/foreign marker and
source/symlink preservation. This maintenance mechanism is not a storage quota,
compression policy, source-history deletion permission or full disk-budget proof.

`scripts/recover-corpus.py` freezes the host executable and performs owned
SIGKILL/reopen cycles over a completed four-session corpus. It checks IPC peer
PID/UID, historical generation/sequence, identical frames and archive summaries,
checkpoint-tail byte counts and source-file SHA-256 inventories. Inventories
are hashed outside the timed window. It reports host-only startup and explicit
OS-cache limitations, and does not claim UI presentation or REC-08 acceptance.

## Bounded canonical history reads

The additive protocol-2 `history_read` command accepts a `query` with exact
`session`, `generation`, `expected_sequence`, `ordinal`, `offset` and
`max_bytes` (1–65,536). The reply contains `history_chunk` and no terminal frames.
It binds bytes to the core/archive profile, model watermark, record's source
event sequence and committed record count. Recovered histories also expose the
immutable checkpoint-prefix length. `next_offset` is absent at the end of the
canonical record. Stale, uncommitted and out-of-range reads fail explicitly.

Two workers and a four-task queue keep cold storage reads off the host's
command/SQLite thread. The history mutex is released after capturing committed
roots, before disk I/O. An Arc keeps the private replay tail leased until the
read completes. Queue saturation is explicitly rejected before admission.
One codec-bounded record is still materialized internally (up to 16 MiB plus
bounded metadata); the wire chunk limit is not a global heap bound or a
streaming row decoder.

The real-PTY test reassembles first/middle/last records before and after host
SIGKILL and compares exact canonical bytes. A fault-injected blocked index read
does not prevent a second live PTY accepting input and producing its ACK; the
original index is retained and restored by the test. This is an internal API
foundation. The user-facing history viewer, selection/copy and search remain
unfinished.

The latest one-cycle small-corpus recovery diagnostic also reconstructs every
private cold-tail record through this API and compares identities, offsets and
lengths. It completed in 319.16 ms for four host frames, with unchanged source
hashes. This single concurrent-workload sample is not a percentile or release
performance claim; the 30-cycle 1 GiB experiment remains required.

## Invalid typed journal tails

A CRC-valid frame can still carry a resize payload outside the terminal
profile, malformed JSON or a nonempty timer/exit payload. Previously the
history transaction began before resize decoding; its error escaped recovery
and Host::open omitted the entire otherwise recoverable generation.

Recovery now validates these payloads before history.begin or model mutation.
It retains the last good frame/sequence and committed history prefix, reports
the rejected event sequence and leaves the raw tail untouched. Earlier
checkpoint warnings are retained too. Degraded recovery cannot pass the clean
recovery oracle.

The regression covers four CRC-valid malformed tails through full replay,
checkpoint-tail and actual host listing. Its prefix exceeds 10,000 hot rows,
asserts nonempty cold history and reads every retained canonical record through
the service. Root added a separate complete-checkpoint/continuation test with
hidden normal screen, alternate screen, modes and partial UTF-8: both recovery
paths preserve the exact pre-tail model and continue it identically. This does
not yet cover resource rejection or every archive/I/O failure; whole-event
memory admission remains open.
