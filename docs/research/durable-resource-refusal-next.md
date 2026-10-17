# Durable terminal resource refusal — proposed next slice

**Status: protocol chosen for implementation, admission not implemented.** The
storage crate now defines `EventKind::OutputRejectedResource = 5`, which stores
the exact output payload in one ordinary CRC-protected journal record. The
host profile gate, replay handling, resource estimator and shared reservation
ledger remain future work. No production event uses this kind yet; no memory
budget or heap bound is claimed here.

## Ordering and decision record

SPEC 7.4 requires one ordered stream from PTY events through
`Durable journal → authoritative terminal core → snapshots` and says queues
must backpressure rather than drop raw output (`SPEC.md:225–243`). It does not
require a durable raw-output write before a pure, read-only preflight or a
temporary resource reservation. The stronger order in the earlier draft of
`docs/research/terminal-admission-plan.md` was a self-imposed constraint, not a
SPEC requirement, and is superseded below.

The selected format uses one CRC-covered record per output. In the new journal
policy, existing `EventKind::Output` means accepted/applied output, as it does
today. `EventKind::OutputRejectedResource` means resource-refused output. Both
carry the exact unchanged raw payload; the record kind, sequence and payload
are CRC-covered by the current journal framing (`crates/storage/src/journal.rs`,
`EventKind` and `Journal::append_impl`, around lines 42–61 and 403–450). There
is no decision wrapper copy and no second decision fsync. The rejected kind
shares the existing `MAX_EVENT_BYTES` payload cap.

For either decision, the host first performs a strictly read-only
VTE/terminal census and obtains a shared-host reservation (or determines that
it cannot reserve). It then appends the exact bytes once and waits for the
record sync. Only after an accepted `Output` record is durable may it call
`history.begin`, mutate the authoritative terminal, commit history/checkpoint,
or publish a frame. If reservation refuses the event, the host writes one
`OutputRejectedResource` record with the exact bytes and bounded refusal
category supplied by the event kind itself, syncs it, then exposes a generic
resource-refusal diagnostic while retaining the prior
terminal/history/checkpoint prefix. If that journal append fails, the outcome
is a storage failure: the model still must not mutate or publish. Reserving
bounded journal write scratch is part of the resource work; refusal must not
depend on allocating an unbounded extra copy to preserve its bytes.

The preflight is not admission by itself and must not mutate parser/core
state, execute terminal effects, resize a PTY or consume a response sink. Its
reservation must remain held across append and model mutation, then reconcile
with measured retained allocations or release on failure. Counting callback
or input bytes alone is not a conservative allocation proof: COW mark vectors,
cursor/template sharing, CSI-only grid work, metadata, responses, history,
frames and checkpoint transients all matter. The existing resource censuses
are measurement inputs, not an aggregate heap or RSS upper bound.

This is smaller and safer than the previously proposed two-record
`Pending`/`Apply`/`Reject` protocol for the stated SPEC ordering. A complete
CRC-protected record already makes the raw event and its apply/refuse decision
durable together. The two-record option would add another sync and a crash
state without satisfying an additional SPEC requirement, so it is superseded
and not the selected protocol. It would be necessary only if a separate
requirement demanded raw durability before even a pure preflight/reservation.

## Replay, profile and checkpoint compatibility

The current host appends `EventKind::Output`, fsyncs it, then calls
`history.begin` and applies the bytes (`crates/session-host/src/lib.rs`,
`actor_loop`, around lines 1513–1538). On later failure the actor retains the
previous published frame/sequence and degrades (`lib.rs`, around lines
1618–1629). Recovery currently applies every valid `Output` (`lib.rs`,
`restore_model`, around lines 503–533), so it needs an explicit branch for
`OutputRejectedResource` before `history.begin` or parser invocation. That
branch must set an explicit refusal diagnostic, stop at the previous
model-applied prefix, and leave raw bytes untouched. As with the existing
invalid-resize-tail behavior, the journal record sequence may include the
rejected record while the view/history/checkpoint sequence remains at the last
applied event; no later events may follow the initial refusal until retry
semantics are designed. Initial policy is stop/degrade with no automatic
retry.

The journal header needs a new policy suffix, planned as
`;journal=resource-refusal-v1`. New host code must accept legacy headers with
their old event meanings, but reject kind 5 under those headers; old binaries
must reject the new header before interpreting its new event semantics. This
journal-policy suffix is separate from the terminal core profile embedded in
schema-2 checkpoint bytes, which can remain unchanged. New checkpoint wrapper
metadata must preserve the exact journal header profile. Existing legacy
journals/checkpoints remain byte-for-byte untouched and recover under legacy
semantics. Runtime launches remain on the legacy journal profile until the
whole-event estimator and reservation ledger are integrated and verified.

For accepted historical `Output` records, recovery must also reserve before
applying them, so reopening multiple sessions cannot bypass aggregate host
limits or replay an event into an OOM. If replay cannot reserve an accepted
event, it must stop at the last consistent prefix with an explicit recovery
resource error; it cannot relabel a previously accepted record as rejected.
Full replay and checkpoint-tail recovery must produce identical prefix,
diagnostic, history and unchanged source bytes for both applied and rejected
tails. Rejected output must never enter `history.begin`; no checkpoint may
include it.

## Resize is a separate transaction boundary

SPEC 7.3 distinguishes `ResizeRequested` from `ResizeApplied` and says the UI
waits for confirmation. The host enqueues resize after geometry validation
(`lib.rs`, `Host::handle`, around lines 786–809), while the actor currently
calls `master.resize` before journaling and applying terminal resize
(`actor_loop`, around lines 1517–1538). Acquire a resource reservation before
the kernel call. If reservation refuses, perform no PTY resize, append no
applied `Resize`, and leave model/frame/history/checkpoint sequence unchanged.
Report an explicit bounded operation result: today the IPC response confirms
queue admission only, and the actor has no per-operation resize reply channel.
Release reservations on queue rejection, kernel failure or completed apply.

A kernel resize failure must not be recorded as an applied resize. A separate
pre-existing gap remains if the kernel resize succeeds and the later journal
append fails: OS geometry can then differ from the retained model. Resource
admission must not claim to fix that ordering failure; define its outcome and
recovery behavior separately before changing resize ordering.

## Required implementation and tests

1. Add a new journal header profile suffix and a profile-aware replay gate.
   Legacy headers retain old `Output` semantics and reject kind 5; new headers
   permit kind 5. Keep terminal schema-2 core checkpoint bytes unchanged and
   preserve old wrapper/journal data.
2. Add full-replay and checkpoint-tail tests proving that `Output` applies,
   `OutputRejectedResource` stops before `history.begin`, and both paths retain
   exact previous hidden terminal state, frame, sequence and history root. The
   raw rejected payload and journal bytes must remain unchanged. Include a
   refusal diagnostic and prove no ANSI parser call occurs for rejected data.
3. Add resize tests proving reservation refusal precedes kernel resize and
   returns a failed operation without a `Resize` journal record or model
   change. Test kernel failure is not recorded as applied. Track the
   successful-kernel-resize/journal-failure gap separately.
4. Only then enable whole-event preflight/reservations. Test COW/template,
   title, CSI-only, query-response and resize-cache amplifiers; concurrent
   session reservations; reservation release; replay-time aggregate
   reservation; and refusal-prefix equivalence. Keep all limits measured and
   versioned as applicable. Do not crop/skip ANSI operations, auto-retry, or
   claim a memory bound from counters alone.
