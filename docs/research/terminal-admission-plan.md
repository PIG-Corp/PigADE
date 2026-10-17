# Terminal allocation work — 0.0.1-dev

The resource work remains part of stage 0. Current bounded queues, encoded
checkpoint limits and payload censuses do not establish the 700 MiB product
budget. The host can still grow combining extras, title stacks and wide hot
grids beyond a safe aggregate limit. Do not publish a completed memory gate.

The next implementation follows actual allocation ownership:

1. Reject incompatible checkpoint headers and oversized envelope titles before
   decoding owned grids/pools/parser buffers. Share the cell decode budget
   across normal and alternate grids without excluding existing valid generic
   checkpoints. Retain raw sources and existing checkpoint fallback.
2. Serialize borrowed model state rather than constructing a second owned grid,
   combining/title/link payloads and then an encoded buffer. Preserve exact
   schema2 bytes, canonical pool identity/sharing and all continuation tests.
   Identity tables and output buffers still require explicit workspace limits.
3. Use the pinned VTE clone to count resource-relevant callbacks, including
   REP, partial OSC and synchronized-buffer flushes. Query callback counts do
   not alone bound response bytes: Term modes, cursor, known colors and closure
   payloads matter. Census must never execute external terminal effects.
4. Run read-only preflight and acquire a shared reservation before
   `history.begin`, model mutation, or any PTY resize. SPEC 7.4 requires the
   durable journal before terminal-core mutation/publication; it does not
   require raw-output fsync before pure preflight or reservation. For either
   output decision, write the exact bytes once in a CRC-protected journal
   record and sync before model mutation: `Output` means accepted, while the
   versioned `OutputRejectedResource` event retains refused raw bytes without
   applying them. A resize reservation must precede its OS side effect. Audit
   the existing successful-kernel-resize → journal order separately; a failed
   kernel operation must not become an apparently applied replay event.
   Account for grid/cache/reflow backing, combining COW, metadata,
   event/response queues, parser clones, the prior/new frame and
   checkpoint/import/export workspaces. Include bounded journal-write scratch
   for rejected payloads. Release or reconcile reservations as retained
   allocations shrink; a lifetime-output counter is unsuitable.
5. Integrate a shared host ledger and profile-gated durable rejection
   semantics. A refusal retains exact raw bytes but leaves the previous
   model/frame/sequence/history root unchanged. Replay stops before applying
   the rejected event and reports its reason. Historical accepted events also
   need replay-time reservations under aggregate host limits. Start with
   stop/degrade on refusal and no automatic retry; define any future retry as
   an explicit retained-event transition. Never crop or skip individual ANSI
   operations.

Two limits need distinct treatment. Fixed semantic acceptance limits must be
versioned in generation policy/profile so replay applies the same contract.
Contention-dependent admission is an operational refusal, dependent on the
other live sessions; its durable decision must be retained with the raw event.
Do not silently change event interpretation under an old journal profile. The
storage primitive `OutputRejectedResource`, exact journal policy suffix/profile
gate, refused-prefix replay and checkpoint/writer guards are implemented in
ADR0013. The admission ledger and estimator are not integrated; live launches
remain on the legacy profile until the complete estimator/reservation path is
implemented and tested. Neither these census values nor the selected
event format establish a global memory bound.

The imported shared-extra regression demonstrates why input-byte limits are
insufficient: four cells share 4,096 combining marks; two VTE combining inputs
cause two large COW copies. Reference-weighted capacity intentionally overcounts
current shared storage. Repeated template sharing can create further copies
inside one event. Title pushes similarly multiply a retained title. CSI-only
scroll/erase/query output can allocate despite zero printable characters.

Small fixtures must cover those amplifiers, whole-event refusal before mutation,
concurrent reservations, exact raw-prefix recovery, resize caches and near-limit
checkpoint work. Fresh release measurements must include UI, host/helpers and
GPU separately. Payload accounting remains a scoped envelope until allocator
overhead, process baseline and every retained/transient owner are covered and
the SPEC workloads actually pass.

The next frame-copy repair preserves content deduplication for independently
allocated equal hyperlinks. First lock that wire behavior with a regression;
then retain shallow Hyperlink handles as interner keys, replacing the duplicate
owned `(String,String)` keys. Keep the frame's single owned wire pool and its
indices unchanged. This removes specific payload copies without claiming that
frame construction or IPC DTO ownership is globally budgeted.
