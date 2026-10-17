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

## Nonzero cold-root continuation follow-up

A new focused fixture retires actual rows past the10,000-row hot limit, freezes
its committed terminal/history/receipt, pauses persistence and appends more cold
rows plus Exit. It validates the old committed root after that growth and checks
persisted sequence/offset/semantic bytes/history against the captured snapshot.
The complete oracle compares both recovery paths with the still-live terminal
and every cold record; cold record count is nonzero and increases after capture.
Raw journal bytes are identical before/after recovery. Root checkpoint-focused
12 tests and strict all-target Clippy pass. This complements the smoke's
zero-cold scope; it is not the full4GiB long-history performance gate.

Review also caught a segmented error-formatting corner case: an already-full
prefix plus a truncation marker could exceed1024 bytes by up to3 bytes. The
writer now trims the prior prefix on a UTF-8 boundary before adding that marker;
ASCII and multibyte segmented-write regressions pass. Active frozen4e4a0fcb…
diagnostics predate this tiny error-format fix; their source binding remains
unchanged and must not be relabeled as the later executable.

## Completed20-minute diagnostic and next elapsed run

Frozen4e4a0fcb… completed **1200.043s** on Btrfs. All four producers exited
cleanly with matching output CRC,238 ACKs and396 applied resizes per stream;
all four full replay/checkpoint-tail complete-state and frame comparisons pass.
Every comparison reports zero cold records, so cold-record equality here is
vacuous; the separate nonzero cold-growth regression remains necessary.

Retained [manifest](../../benchmarks/results/async-checkpoint-20261017/1200s-manifest.json),
[samples](../../benchmarks/results/async-checkpoint-20261017/1200s-samples.jsonl)
and [resize admission diagnostics](../../benchmarks/results/async-checkpoint-20261017/1200s-resize-admission-diagnostics.jsonl)
include the slower cases: stream0 needed126 explicit pre-admission resize
retries/3.15s total, and its last sampled stall was journal_append age670ms.
No timeout or retry rule was relaxed. Maximum sampled checkpoint age was5.037s,
and maximum sampled sequence lag93. Final recovery read356 raw-tail bytes per
generation, taking53.86–65.89ms versus605.43–615.90ms full replay. Four individual
recoveries are not a p95 distribution or UI-present measurement.

Host sampled RSS rose from20,324,352 to206,229,504 bytes while hot history filled.
That is not a demonstrated steady-state/leak bound, nor total product memory.
The run overlapped builds, the old stage-instrumented host and private software
UI tests, so it provides integrity/liveness diagnosis, not isolated PERF proof.

A new actual86400s run uses that **same frozen host**, whose original source
binding is retained in host-source.json (commit4d75513). Output directory:
`~/.local/state/pigade/soak/async-checkpoint-24h-20261017`, tool82653. It remains
pending until real elapsed completion, producer CRC and recovery checks finish.
Later notification/code edits do not change that active process. The earlier
7200s diagnostic tool55262 continues independently on frozen610efac4…. No24h
or full REC08 pass is claimed by this20-minute result.
