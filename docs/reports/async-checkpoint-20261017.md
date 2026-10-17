# Bounded background checkpoint persistence — 0.0.1-dev

Checkpoint disk publication no longer holds live actors. The actor still
synchronously makes journal and history durable before publishing any new
cells. It exports one immutable semantic checkpoint at that exact receipt;
one process-wide permit and one worker bound capture/persistence work.
Busy admission keeps work due without queuing snapshots or dropping output.
Exit waits for pending completion and forces a final checkpoint using the same
permit. [ADR0011](../adr/0011-bounded-background-checkpoint-persistence.md)
records error, cadence, resource and metric semantics.

Fresh root verification passes191 workspace tests, strict all-target Clippy,
format and release binaries/examples. The real-PTY fault regression deliberately
pauses persistence at the target generation, then proves a second output event
is journaled, history-committed and visible while the worker remains paused.
After release/Exit it compares full recovery; truncating Exit also preserves the
exact live terminal/history prefix via checkpoint+tail without changing source
bytes during recovery. A separate test observes worker failure with no new PTY
event; busy retry remains nonzero even with saturated dirty counters. Existing
retention/damaged/torn-checkpoint tests pass. Completion error text is bounded
to1024 UTF-8 bytes; failure cleanup releases only test-owned resources.

Frozen binary SHA
`4e4a0fcb6f77d082bbebe7470311114d4a470db5aa846816f7fdd31a78593e2a`
passed a30.043s four-PTY smoke with9 resizes and5 ACK cycles per stream, all4
producer/framing CRCs and full/checkpoint-tail semantic/frame comparisons.
There are zero cold records, so that comparison is vacuous in this smoke.
[Summary](../../benchmarks/results/async-checkpoint-20261017/smoke-summary.json)
and [exact host source hashes](../../benchmarks/results/async-checkpoint-20261017/host-source.json)
are retained. The earlier failed synchronous run remains separately retained.
A20-minute diagnostic on the new frozen binary is active; its result is pending.

This proves the blocked-worker ordering/progress contract and a short real run,
not a unique syscall diagnosis or universal latency improvement. The old failed
run also had concurrent builds. Export still uses actor CPU; journal/history
fsync can still block; pending actors poll at10ms, whose transient CPU cost is
not certified. Full long-history, checkpoint-age,24h, whole-event/global heap,
physical UI/GPU and release gates remain open. `checkpoint_ns` now measures
actor scheduling/export/forced-sync work, not background persistence duration;
`checkpoints` counts observed successful durable completions.
