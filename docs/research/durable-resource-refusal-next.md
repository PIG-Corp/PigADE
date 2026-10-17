# Durable terminal resource refusal — proposed next slice

**Status: proposed, not implemented.** This note captures the boundary needed
before adding whole-event terminal memory admission. It does not define a
budget, claim an aggregate heap bound, or authorize a profile change by itself.

## Current output ordering and replay gap

The session actor receives an output action, maps it to `EventKind::Output`,
and synchronously appends/fsyncs it before calling `history.begin` or mutating
the terminal (`crates/session-host/src/lib.rs`, `actor_loop`, around lines
1513–1538). It then commits derived history, publishes a checkpoint and frame.
If an error occurs after the append, the actor keeps the previous published
frame and sequence, marks the view degraded, and stops processing (`lib.rs`,
around lines 1618–1629). This is a useful live failure posture, but it does
not define replay refusal.

`restore_model` validates event payloads before history mutation, then applies
every valid `Output` unconditionally (`lib.rs`, `validate_replay_payload` and
`restore_model`, around lines 393–405 and 503–533). Therefore a durable output
that failed admission in the live process is replayed as accepted. A hostile
output may grow the model again during recovery. Existing rejected-tail
handling retains a prefix only for an event that can be rejected before
application, such as invalid resize JSON; `Output` has no equivalent
applicability check. The separate query-response byte check currently occurs
after output has already been parsed and mutated (`lib.rs`, around lines
1540–1555), so its rejection also cannot be reused as a pre-mutation memory
gate.

Appending a `ResourceRejected` record after the ordinary `Output` is not
sufficient. Replay has already encountered a syntactically valid output and
would need to buffer/look behind one event to undo its application when it
later sees the marker. More seriously, a crash after the output fsync and
before the rejection marker leaves no durable evidence that the output was
refused; restart would apply it. A no-op sentinel without an event reference
or sequence rule is ambiguous and could make other valid events disappear.

## Proposed journal decision boundary

The smallest crash-safe *shape* is a versioned pending-output record plus a
durable decision record. The pending record stores the exact raw bytes. A
following bounded decision identifies that pending record and says either
`Apply` or `Reject` with a bounded, typed refusal reason. The actor writes and
syncs the decision before it mutates the model or commits derived history.
`Apply` means replay may deterministically apply the pending bytes. `Reject`
means recovery stops at the model prefix before those bytes, exposes an
explicit resource-refusal diagnostic, and retains both raw records unchanged.

If recovery reaches an undecided pending record at EOF, it must fail closed at
the prior model prefix and report an unresolved admission decision. It must
not guess that the bytes were accepted and retry a potentially dangerous
allocation. Such an output remains available in the raw journal for later
operator/client retry policy; automatic retries are out of scope. This makes
the crash window explicit instead of silently treating an unfinished event
as successful.

This requires separating journal-record sequence/offset from the last
model-applied sequence used by the frame, history root, and checkpoint. A
checkpoint may cover only a fully decided prefix. Recovery starting from a
checkpoint must resume at its exact journal offset, process pending/decision
pairs identically to full replay, and never publish a checkpoint that includes
an undecided or rejected model event. The pending bytes and decision payloads
need existing event-size bounds; the recovery lookahead must be bounded to one
pending output, not an event queue.

This is a recovery protocol change, not an innocuous new enum value. The
journal event decoder currently knows only `Output`, `Resize`, `SyncTimeout`,
and `Exit` (`crates/storage/src/journal.rs`, `EventKind` around lines 42–60).
The terminal profile is exact-gated by `restore_model` (`lib.rs` around lines
419–424); checkpoint decode also requires the current profile/schema
(`crates/terminal/src/lib.rs`, `CORE_PROFILE` and checkpoint validation around
lines 68 and 317–333). A new event meaning must use an explicitly versioned
profile/policy. The new reader should retain support for existing
`CORE_PROFILE` journals and schema-2 checkpoints with their old event
semantics, while only the new profile requires pending/decision records. If
the old checkpoint profile cannot be loaded by the new terminal decoder, fall
back to full replay of the old journal rather than rejecting or rewriting its
source. Older binaries must reject the new profile before attempting to decode
its new event types. Do not rewrite existing journals or checkpoint bytes.

## Reservation and deterministic preflight

The decision protocol does not supply a resource estimator. A conservative
reservation still needs to cover persistent model growth, COW copies,
frame/checkpoint transients, parser and response workspaces, and aggregate
host contention. The pinned-VTE clone census and allocation census are
inputs, not proofs of an upper bound. In particular, counting output bytes or
combining callbacks alone misses CSI-only grid work, cursor/template sharing,
and COW amplification from shared mark vectors.

A deterministic preflight can reduce avoidable refusals only if it reserves
against a defensible worst case before mutation. The output’s raw durability
must still precede terminal mutation. Computing a census before the append
does not itself admit the event; a reservation could be acquired first, the
raw pending bytes synced, then an `Apply` decision synced before mutation. If
the reservation cannot be acquired, the raw pending event still needs a
durable `Reject` decision without mutating the model. A post-append decision
marker with no pending/decision protocol leaves the crash ambiguity described
above. Any estimate not covering persistent COW and commit/frame/checkpoint
workspaces must be described as partial admission guidance, not a memory
limit.

## Resize is a separate transaction boundary

The host enqueues resize actions after validating geometry (`lib.rs`,
`Host::handle`, around lines 786–809). The actor currently calls
`master.resize` before journal append, then resizes the terminal model
(`actor_loop`, around lines 1517–1538). A resource refusal must be decided
before `master.resize`; it must emit no `Resize` event and leave the published
model/history/checkpoint sequence untouched. Do not record a failed request as
an applied resize, because replay would call `terminal.resize` for it.

Today the resize IPC result only confirms queue admission and returns a
snapshot; the actor has no per-operation result channel to report a later
resource or kernel failure. A scoped resize slice therefore needs an
explicit bounded outcome (for example, a per-action reply sender completed
after reservation and kernel/model result) or an operation status the client
can query. A refused resize is an operation result, not a terminal event, so
it need not advance the model sequence. Release any reservation on command
queue rejection, kernel resize failure, or completed application.

There is also a pre-existing durability gap distinct from resource refusal:
if `master.resize` succeeds but journal append fails, the OS PTY geometry may
have changed while the last published model remains at the old geometry.
The actor degrades and stops, while replay sees only the previous durable
prefix. Admission must not claim to repair this. Changing resize ordering
requires its own contract for the kernel side effect and durable record; a
failed kernel resize must never be journaled as an applied event.

## Smallest reviewable implementation/test sequence

1. Specify the new profile/event protocol and separate applied-model
   watermarks from journal record offsets. Keep old-profile replay behavior
   byte-for-byte compatible.
2. Add storage/replay tests without enabling admission: paired pending/apply
   reproduces ordinary terminal state; pending/reject and EOF-pending retain
   the exact previous frame, hidden checkpoint state, history root and
   continuation; full replay and checkpoint-tail replay agree; source journal
   bytes are unchanged. Test malformed/out-of-order decision references as a
   rejected suffix before `history.begin`.
3. Add a resize outcome path with tests proving refusal occurs before the
   PTY resize call, appends no `Resize`, preserves frame/sequence/history, and
   reports refusal; separately test kernel failure is not recorded as
   applied. Keep the existing successful-resize append-failure limitation
   explicit until handled.
4. Only then integrate the shared host reservation ledger and a measured,
   defensible reservation rule. Test deterministic replay and concurrent
   session reservations, plus raw-tail retention on refusal. No lifetime
   counter, skipped ANSI operations, or arbitrary fixed threshold substitutes
   for the resource model.
