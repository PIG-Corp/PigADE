# Soak resize-admission saturation

## Finding

The 24-hour soak run `soak-20261017T000738Z-6244149d` failed after 3079.132 seconds with `RuntimeError: resize admission retry deadline exceeded`. This is a real host admission-pressure failure, not an unknown IPC outcome: the runner retries only an explicit `queue_full` rejection, and the frozen host implementation at commit `1c6c05a4c276257a8c8f562c45524a1d1c459c38` maps a full per-generation `sync_channel` on `Action::Resize` to that structured pre-admission result.

All four generations had exactly 996 accepted resize journal records. The runner visits streams in order, so the equal totals and 10-second terminal failure place the first unresolved request at stream 0's next resize, after its 996th accepted resize. The journals end with output events and contain no Exit event; they were retained as source evidence but are not copied into this report. This identifies the saturated queue and caller, but does not establish why the actor stopped draining that queue for longer than the retry deadline.

The last periodic sample, at 3003.79 seconds, showed four running generations, host RSS 235,917,312 bytes, fresh checkpoints aged 0.66–3.69 seconds, and sequence lag of 13–68. The last progress records observe ACK token 604 for each synthetic stream. There is no sample after that point, so the retained data cannot distinguish actor work, filesystem latency, or another stall during the final 75 seconds. Concurrent VM or disk I/O is a hypothesis only; it is not measured evidence.

The curated evidence files in [`benchmarks/results/soak-resize-saturation-20261017/`](../../benchmarks/results/soak-resize-saturation-20261017/) contain the sanitized run summary, final periodic sample, and final progress metadata. They omit PIDs, private paths, PTY bytes, terminal frames, and raw journals.

## Bounded diagnostics for the next run

The soak runner now writes `resize-admission-diagnostics.jsonl` only after explicit resize `queue_full` responses. It records the monotonic elapsed time, stream/session/generation, target geometry, retry count, best-effort host RSS/CPU, and a bounded snapshot containing only state, sequence, storage error, and numeric session metrics. It does not serialize frame or terminal text. Snapshot requests have a 20 ms timeout; that diagnostic time is subtracted from the retry backoff so the configured 25 ms retry interval and 10-second deadline remain intact. Unknown or transport outcomes are still never retried.

Sampling is limited to the first rejection and at most once per second per generation. The JSONL stream has a hard 1024-record cap with an omitted-record counter. The final manifest retains the last diagnostic in memory, including if the file cap has been reached, and records a best-effort fsync error without hiding the soak result.

Tests cover throttling, the global record cap, frame-content exclusion, and unchanged retry cadence, deadline, and unknown-outcome behavior. No reproduction run was started as part of this change; the next run should use the existing frozen four-stream workload and inspect these samples to locate the actor stall.

## Post-failure durability check

Root ran `verify-recovery` sequentially on all four retained journals using
the same frozen67ed712c host. Complete terminal/parser state, frame and every
cold record match between full replay and checkpoint+tail in all four
generations (12,726 /12,708 /12,670 /12,641 cold records). The
[redacted proof](../../benchmarks/results/soak-resize-saturation-20261017/post-failure-recovery.json)
records individual timings; this is a post-kill replay comparison, not a
successful soak, live-model oracle or producer-output CRC completion. No claim
is made about PTY bytes that had not reached durable storage before cleanup.

A one-hour instrumented reproducer was then started with the same frozen
executable and synthetic workload at private
`~/.local/state/pigade/soak/resize-repro-20261017`. It is not a24h acceptance run.
No timeout was relaxed and no unknown operation is retried.
