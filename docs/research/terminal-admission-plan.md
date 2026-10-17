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
4. Reserve before `history.begin`, model mutation and any PTY resize. For output
   already received from the PTY, retain raw durability before admission. A
   resize additionally has an OS side effect: audit its failure/outcome contract
   before changing the existing successful-kernel-resize → journal order. A
   failed kernel operation must not become an apparently applied replay event.
   Account for grid/cache/reflow backing, combining
   COW, metadata, event/response queues, parser clones, the prior/new frame and
   checkpoint/import/export workspaces. Release or replace reservations as
   retained allocations shrink; a lifetime-output counter is unsuitable.
5. Integrate a shared host ledger, durable rejection identity and retry policy.
   A refusal retains the already durable raw event, previous frame/sequence and
   history root. Replay must return that same valid prefix with an explicit
   rejected-event reason. It must not crop or skip individual ANSI operations.

Two limits need distinct treatment. Fixed semantic acceptance limits must be
versioned in generation policy/profile so replay applies the same contract.
Contention-dependent admission is an operational refusal, dependent on the
other live sessions; it needs an explicit retained-suffix/retry transition.
Neither may silently change core behavior under the old profile.

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
