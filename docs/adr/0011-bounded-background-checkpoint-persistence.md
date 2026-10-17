# ADR0011: one bounded background checkpoint writer

Status: accepted for the bounded host implementation slice, 0.0.1-dev, 2026-10-17.

## Evidence and decision

The [short diagnostic](../reports/checkpoint-stall-20261017.md) observed a live
actor in checkpoint stage for5.462s while a child-generated ACK waited in the
PTY reader. This does not identify a particular syscall. Source inspection
establishes that synchronous checkpoint publication holds a global mutex across
semantic export, writes/fsyncs, rename, directory sync and retention work.
Checkpoint persistence is a recovery accelerator; raw journal and cold history
are already durable at its capture watermark.

Keep journal append/fsync, model apply and history commit in the ordered actor.
After those commits, capture exact semantic terminal bytes and the immutable
history root/state with the same journal receipt. Move only checkpoint file
persistence and retention to one process-wide worker. There is no second ANSI
parser and no asynchronous raw-journal admission.

A global permit spans capture through persistence and byte-buffer destruction.
It is acquired before terminal export. A busy permit skips that derived
checkpoint attempt and keeps dirty counters due; raw output is never discarded.
A capacity-one queue and completion channel bound retained work. One worker and
one permit prevent out-of-order checkpoint publication for a generation.
The existing128MiB encoded checkpoint cap is preserved; this is not a proof of
total model/export/COW/process heap admission.

Synchronous publication shares the same permit and exact envelope/CRC/temp
file sync -> rename -> parent sync/retention implementation. Exit waits for an
owned pending result, then forces a final synchronous snapshot. This preserves
deterministic immediate checkpoint-tail recovery after a clean exit. Forced
exit can still wait on slow storage; raw recovery remains authoritative after
an unclean stop. No live-process resurrection or launch retry is introduced.

Actors poll bounded completion state even when no PTY output arrives. While a
job is pending or a due capture was deferred, a10ms receive timeout services
completion/retry without a zero-duration idle spin. Existing significant
terminal timer events stay ordered and journaled. Worker failure degrades the
associated live generation on observation, preserving its already durable and
published prefix; it does not roll back output. A dropped receiver cannot free
the worker-owned permit early.

## Tradeoffs and validation boundary

Terminal semantic serialization still runs on the actor and can be expensive.
Journal/history fsync can still block on the underlying filesystem. This change
does not guarantee the5s synthetic ACK deadline, physical input/present budgets,
24h soak, or all memory/recovery gates. Phase-specific timing and isolated
hardware measurements remain useful if another stall appears.

`metrics.checkpoints` counts observed successful durable completions, not jobs
merely enqueued. `checkpoint_ns` measures actor scheduling/export or forced
synchronous work; background disk time is not silently reported as zero-cost
persistence. No report may compare that field to the previous inline total as
an equivalent end-to-end cost.

Verification requirements for this implementation: immutable exact-watermark
snapshot + hidden-state/history recovery; busy admission with no extra export;
blocked writer with continued real-PTY publication; bounded error/disconnect
and permit cleanup; idle completion failure; final Exit/recovery; existing
retention/damaged/torn-file tests, workspace Clippy/tests/release and a fresh
same-app diagnostic. The complete MVP and stage0 gates remain open regardless
of this slice's result.

Fresh root evidence:191 workspace tests pass, including paused-writer real-PTY
progress, exact checkpoint-tail recovery after a truncated Exit, idle failure
observation and nonzero busy retry; strict all-target Clippy, fmt and release
build pass. Frozen4e4a0fcb… completes30.043s with all4 producer CRC and complete
recovery comparisons passing (zero cold records). A1200s diagnostic is active,
not yet passed. See [report](../reports/async-checkpoint-20261017.md).
