# Cold-history integration — 0.0.1-dev

**Date:** 2026-10-17. **Status:** host implementation integrated; Stage 0 and
long-history acceptance remain open.

## What is implemented

The host observes normal-grid retirement events and persists cold rows as
derived, generation-scoped history. The row-local PIGROW02 codec stores typed
cell attributes, UTF-8 scalar deltas, extra characters, occupancy, soft-wrap
state and row-local hyperlink references. Retirement palette snapshots retain
dynamic color provenance, including changes made while alternate screen is
active. Each physical row carries logical-line/continuation metadata and source
event sequence.

PIGARC01 segment files contain bounded row fragments. The host rotates segments
at an 8 MiB target between rows; a single row can exceed that target up to the
codec limit, so 8 MiB is not a hard maximum. A generation/profile-bound index
uses fixed 64-byte CRC-protected slots. A checkpoint root records the relative
archive directory, first ordinal, committed count and last index CRC. PIGCP002
binds that root atomically with the complete hot terminal/parser checkpoint.
The host can seek to an indexed row range without enumerating or scanning all
archive segments at startup.

For each source event the required order is:

```text
raw journal fsync
  → apply event to the sole host model and write bounded provisional rows
  → archive data fsync
  → index fsync
  → publish checkpoint/frame
```

An archive append address does not authorize display or recovery. Only the
committed index prefix bound by the checkpoint root does. On recovery, the host
preserves the immutable source archive prefix and writes replayed tail rows to a
separate private derived cache. The cache cannot become a new source checkpoint;
the source checkpoint and raw tail remain the restart lineage. Legacy hot-only
checkpoints cause full raw-event replay so cold rows can be reconstructed. If
reconstruction is unavailable, the hot view may be retained with an explicit
history-unavailable warning. Raw journals are retained as authoritative source
data.

The verification oracle compares complete hot Term/parser checkpoint bytes
and every canonical cold record between full replay and checkpoint+tail. It
does not accept matching frame screenshots or a cold-history digest as a
substitute. Fault tests cover archive/index publication cuts, raw reconstruction,
private tail recovery, checkpoint retention/cache invalidation and current-OS
child-process kills. Those process-kill tests do not prove power-loss or VM
reboot behavior.

`JournalReader` counts bytes actually read, including headers, partial reads and
checkpoint tails. In the 60-second artifact, full replay read 63,105,903 to
63,116,991 journal bytes per session; the checkpoint-tail path read 356 bytes
per session. This demonstrates the observed current test's bounded tail read.
It does not certify recovery on the SPEC's 1 GiB corpus or its time/RSS budgets.

Checkpoint export/import/pruning work is serialized across host actors to avoid
concurrent duplicate hot-grid allocations. A bounded semantic-validation cache
reuses work only for unchanged checkpoint files with the same session,
generation and profile; it still checks the bounded external cold-root
dependency. The host also checkpoints dirty state while idle, has a Stop wake
path while blocked waiting for events, and publishes bounded numeric pipeline
counters for journal fsync, model apply, archive commit, checkpoint and frame
costs. These are scheduling/resource improvements, not proof of total heap,
queue occupancy, or latency-to-present budgets.

## Verification evidence

The latest fresh root workspace run recorded by the integrating agent passed
111 tests: 13 layout, 28 host library, 4 checkpoint-retention, 11 recovery, 1
slow-input, 34 storage and 20 terminal tests. Fresh strict workspace Clippy and
format checks also passed in that run. This report records that integrated run;
the documentation pass did not rerun it. The latest recorded standalone vendor
run (151 Alacritty library tests and 45 reference cases) and GPUI app run (13
tests) are prior evidence and were not fresh in this integration. None establishes
platform acceptance.

### Short 5-second diagnostic

[`native-cold-history-5s-instrumented.json`](../../benchmarks/results/native-cold-history-5s-instrumented.json)
records four 5 MiB normal-history streams. Each producer wrote its body in
4.984–4.988 seconds, meeting that diagnostic's 5-second producer-rate target.
All producer/journal bytes were valid, and full replay matched checkpoint+tail
hot state plus all 35,728 cold records per session. Host exit was not observed
inside the 5-second deadline. This short instrumented run is not the SPEC's
60-second 4×1 MiB/s workload, and it measures no input latency or presentation.

Earlier 5-second files preserve the optimization history. The initial
[`native-cold-history-5s.json`](../../benchmarks/results/native-cold-history-5s.json)
passed exact history/recovery checks but took 14.63–15.24 seconds per 5 MiB
producer body. The row-codec run
[`native-cold-history-5s-row2.json`](../../benchmarks/results/native-cold-history-5s-row2.json)
took 12.29–13.19 seconds. The model-work run
[`native-cold-history-5s-row2-model-work.json`](../../benchmarks/results/native-cold-history-5s-row2-model-work.json)
took 5.05–5.44 seconds. The post-checkpoint-cache run
[`native-cold-history-5s-after-checkpoint-cache.json`](../../benchmarks/results/native-cold-history-5s-after-checkpoint-cache.json)
also missed the rate target. These failed artifacts remain useful and must not
be overwritten or described as passes.

### Current 60-second workload: correctness passes, rate target fails

[`native-cold-history-60s.json`](../../benchmarks/results/native-cold-history-60s.json)
contains the full normal-history workload: four concurrent producers, 60 MiB
each, target 60 seconds per stream. Each body completed in 77.43–78.06 seconds.
Therefore the 4×1 MiB/s producer-rate gate failed, and host exit was not seen by
the target deadline. The output and journal checks passed. For each session,
full replay and checkpoint+tail had equal hot/parser state and all 539,088 cold
records. Actual journal-read counters show full replay read 63,105,903 to
63,116,991 bytes per session, while checkpoint+tail read 356 bytes. Sampled
peak RSS was 218,689,536 bytes for the release host. This
run excludes UI/GPU/provider processes and measures no input-to-present latency.
Its 60 MiB corpus does not exercise the SPEC's 4×1 GiB/100,000-resize gate.

### 12-session host memory diagnostic

[`native-memory-12-sessions.json`](../../benchmarks/results/native-memory-12-sessions.json)
records 12 headless sessions, 10,000 hot rows each, four visible-session
geometry, and a 10-second hold. Peak sampled release-host RSS was 447,610,880
bytes, below the diagnostic's 700 MiB host-only threshold. All session recovery
checks matched. The 447.6 MB result includes the host's native threads but
excludes the Python harness/producers, UI, GPU, provider processes and helper
processes; it is necessary host evidence, not total PigADE memory acceptance.
[`native-memory-12-sessions-initial-long-socket-failure.json`](../../benchmarks/results/native-memory-12-sessions-initial-long-socket-failure.json)
preserves the earlier launch failure: the UUID-length helper handshake socket
exceeded `SUN_LEN` although `host.sock` fit. The host now uses the shorter private
`g.sock` handshake path, with peer/nonce and ownership checks intact. A real-PTY
regression covers long state-directory paths.

## Reproduction

Use a durable filesystem for the state directory; the harness rejects tmpfs and
ramfs. Build the release host first. Write new output paths so the historical
evidence above remains immutable:

```sh
python3 scripts/stress-pty.py \
  --directory /absolute/durable/path/pigade-cold-history-60s \
  --output benchmarks/results/native-cold-history-recheck.json \
  --seconds 60 --normal-history

python3 scripts/measure-memory.py \
  --directory /absolute/durable/path/pigade-memory-recheck \
  --output benchmarks/results/native-memory-recheck.json
```

The scripts use deterministic Python PTY producers, not AI CLI tasks. They do
not open UI windows or inject desktop input. The cold-history harness compares
full replay and checkpoint+tail and validates every cold record; its normal
history mode intentionally does not measure input ACK latency.

## Remaining gates

- Improve the normal-history 60-second producer throughput without weakening
  raw journal/archive/index/checkpoint durability or exact replay equality.
- Complete user-facing paginated history, copy/search, quota reporting and
  cleanup of private replay-cache orphans.
- Establish total live/reflow/export/import heap admission, not only host RSS.
- Run the SPEC's four 1 GiB streams with 100,000 resizes per session, bounded
  checkpoint-tail bytes read, complete record equality, and the applicable
  fault matrix.
- Complete the isolated VM reboot test. The pinned signed Arch QEMU SDK and
  reboot harness are prepared, and an initial headless guest reached its Linux
  root mount. No successful reboot or AI conversation-resume result is available.
- Run a separate 24-hour real-time soak and the remaining isolated UI/platform,
  renderer/GPU, IME/clipboard/accessibility and real provider-version checks.

Stage 0 remains open. A green short diagnostic, a host-only memory pass, or
prepared VM tooling does not close these gates.
