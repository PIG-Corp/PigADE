# Checkpoint-stage ACK stall — 0.0.1-dev

The30s READY-barrier smoke failed after31.730s on frozen host610efac4…:
`SOAK-0-4` was not visible within the unchanged5s ACK deadline. The new startup
barrier itself worked: all four initial frames contained only the9-byte READY
marker before START. Future harness runs now keep workload/alternate-screen
output behind that explicit barrier. This addresses a startup-oracle race; it
does not repair the later ACK stall.

[Bounded diagnostic](../../benchmarks/results/checkpoint-stall-20261017/ack-timeout.json)
shows stream0 in `checkpoint` for5.462s. The child recorded ACK write completion;
the PTY reader had read16 more bytes than it had enqueued. Input was admitted in
5.805ms. The last live frame was sequence410; final raw replay reaches411 and
contains the ACK. One host thread was in `D / wait_log_commit`, but its stream
identity is unproven. The stage spans export, scheduler-lock wait, filesystem
writes/sync and retention; no individual blocking syscall is identified.

All four frozen-host full/checkpoint-tail semantic and frame comparisons pass.
They contain zero cold records, so cold equality is vacuous. Producer ENDCRC is
unavailable after abort. Source journals were retained unchanged. Exact hashes,
final sequences and recovery results are in
[summary](../../benchmarks/results/checkpoint-stall-20261017/summary.json).
Concurrent builds, another soak and private input tests prevent isolated PERF
or24h claims.

The synchronous path holds a process-wide checkpoint mutex across terminal
export, file sync, rename, parent-directory sync and retention validation/prune.
That allows optional recovery-accelerator persistence to block live actors even
though raw journal and history are already durable. A bounded async persistence
change is being implemented and must still pass blocked-writer/real-PTY,
error/exit, complete recovery and fault tests. It must not defer raw journal or
history durability, drop output, raise timeouts, or relax recovery comparison.

Reproduce a short same-host run with the fixed startup barrier:

```sh
python3 scripts/soak-pty.py --binary /absolute/frozen/pigade-session-host \
  --directory /absolute/durable/state/root \
  --output /absolute/new/private/run --seconds 30 --verify
```

Retain failures rather than reusing their
output path. Original stage-instrumented7200s run remains independent and active
at the path in AGENT_HANDOFF.md; do not modify or duplicate it.
