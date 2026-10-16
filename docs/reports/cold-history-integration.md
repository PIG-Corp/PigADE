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

The fresh root run passed 120 workspace tests, including real-PTY
integration tests and conditional-poll coverage.
Fresh vendor verification passed 152 Alacritty library tests and 45 reference
cases. Workspace fmt, strict Clippy and release build passed. The native
application passed 17 tests, all-target strict Clippy and build (with two known upstream GPUI float-future
warnings). The isolated conditional-poll UI run rendered and reopened four
views at 69×15 with stable PIDs/generations; the root visually checked the
screenshot. Evidence is in
[`conditional-poll-20261017`](../../crates/app/evidence/conditional-poll-20261017/).
The GPUI relink SIGBUS was traced to `EDQUOT` under `/tmp`; the build passed
after moving its cache. These checks do not establish GPU or platform
acceptance.

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

### Earlier 60-second workload: correctness passes, rate target fails

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

### Previous no-wait 64 KiB batching: correctness passes, rate target fails

This measured implementation coalesced up to 64 KiB of already-available output
while each PTY read remained capped at 16 KiB. Queue capacity remained eight.
This prior version added no collection window. The short artifact
[`native-cold-history-5s-64k-batch.json`](../../benchmarks/results/native-cold-history-5s-64k-batch.json)
meets the 5-second producer rate (4.9845 seconds per 5 MiB body) with exact
journal, hot/parser and all 35,728 cold-record equality.

The latest 60-second artifact
[`native-cold-history-60s-64k-batch.json`](../../benchmarks/results/native-cold-history-60s-64k-batch.json)
retains exact data and full-replay/checkpoint-tail equality, including all
539,088 cold records per session. Its four 60 MiB producer bodies complete in
66.41–67.09 seconds, so the 60-second producer-rate target remains missed,
although this improves the earlier 77.43–78.06-second run. Per-stream cumulative
metrics report about 11.56–12.45 seconds in journal fsync and 25.79–27.49 seconds
in archive commit. This normal-history mode has no input-latency claim; it also
does not measure UI presentation. No durability step was removed to obtain the
improvement.

### 250 μs collection-window run

[`native-cold-history-60s-250us-batch.json`](../../benchmarks/results/native-cold-history-60s-250us-batch.json)
records a 250 μs collection-window variant with exact recovery, but producer
body times were 62.275–62.517 seconds and missed the 60-second rate target.

### 64 KiB index-batch run: producer rate passes; host deadline and latency do not

The newer [`native-cold-history-60s-index-batch.json`](../../benchmarks/results/native-cold-history-60s-index-batch.json)
uses a bounded 64 KiB index buffer, preserves archive-data fsync before
index-fsync, and uses logical archive offsets to avoid per-row `lseek`. All four
60 MiB producer bodies completed in 59.9845–59.9846 seconds. Journal CRCs,
complete hot/parser/frame state and all 539,088 cold records per session
matched full replay. The producer body-rate predicate passed in all streams.
Host exit was observed at 60.118–60.400 seconds, so the host-exit deadline
failed; producer maximum schedule lateness was 7.1–7.5 seconds. No input latency
was sampled. Host peak sampled RSS was 218,050,560 bytes, excluding Python
producers, UI, GPU and provider processes. This is not steady-rate, latency,
total-memory or SPEC long-history acceptance.

The separate [`native-idle-8-sessions.json`](../../benchmarks/results/native-idle-8-sessions.json)
host-only diagnostic sampled 0.0% steady CPU and 319,963,136-byte peak RSS with
complete recovery equality for all eight sessions. It excludes UI/GPU/provider
and producer processes and is not a total product idle-resource result.

### Alternate-screen throughput and input check

[`native-tui-60s-index-batch.json`](../../benchmarks/results/native-tui-60s-index-batch.json)
records four alternate-screen PTYs producing 60 MiB each in 59.9845–59.9859 s.
Producer/journal CRCs and complete recovery comparison passed. All 116 input
ACKs were observed: p50 27.04 ms, p95 42.91 ms, p99 59.07 ms. These are
host-snapshot observations, not UI input-to-present measurements. Peak sampled
host RSS was 30,949,376 bytes. Exit observation at 60.015–60.025 s missed the
strict 60 s exit deadline; that deadline includes trailing output, exit and
polling after the final scheduled body chunk. This alternate-screen workload
does not replace normal-history or product performance acceptance.

### Small corpus and soak smoke runs

The latest frozen corpus run
([`run 29039a7a`](../../benchmarks/results/long-history-smoke-parent/long-history-run-29039a7a-5470-4ef8-928c-bad3cd4bb721/runner.json))
passed four 1 MiB sessions with 100 resizes each, a common-root comparison,
3,199 cold rows per session and complete hot/frame/cold equality. It covered
partial final UTF-8/CSI and alternate-screen, OSC8 and synchronized-output
checkpoints, plus streamed SHA-256 and a per-event logical 24-hour timeline.
Logical time is metadata, not elapsed time. The 1 GiB corpus and real-time
24-hour soak remain open. See [`long-history.py`](../../scripts/long-history.py)
and [`history_corpus.rs`](../../crates/session-host/examples/history_corpus.rs).

The corrected `scripts/soak-pty.py` then passed a 10-second four-session
functionality run in 10.036 seconds: each session acknowledged input, processed
three resizes, retained valid journal CRCs, and passed the full recovery oracle.
An earlier attempt failed before measurement with a `NameError`; that artifact
was retained. The successful short run is not a 24-hour real-time soak.

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

- Evaluate the index-batch run against steady-rate, host-exit and latency
  requirements without weakening durability or exact replay equality; retain
  the 250 μs rate miss and earlier failed results.
- Complete user-facing paginated history, copy/search, quota reporting and
  cleanup of private replay-cache orphans.
- Establish total live/reflow/export/import heap admission, not only host RSS.
- Run the SPEC's four 1 GiB streams with 100,000 resizes per session, bounded
  checkpoint-tail bytes read, complete record equality, and the applicable
  fault matrix.
- The synthetic cold-history portion of an isolated two-boot VM run now passes
  for four sessions × 2,467 rows; see [VM reboot evidence](vm-reboot.md). Provider
  conversation resume, product layout restoration and power-loss behavior are
  still untested, so SPEC REC-03 remains open.
- Run a separate 24-hour real-time soak and the remaining isolated UI/platform,
  renderer/GPU, IME/clipboard/accessibility and real provider-version checks.

Stage 0 remains open. A green short diagnostic, a host-only memory pass, or a
partial synthetic VM recovery does not close these gates.

### Live-model oracle extension

The current corpus verifier also compares the original generating live model
with both recovery paths: complete checkpoint bytes, frame, history continuation
state, generation and every canonical cold record. The fresh
[`long-history-live-smoke`](../../benchmarks/results/long-history-live-smoke/)
run passed all three comparisons for four 1 MiB sessions and 100 resizes each.
A regression rejects mismatched same-generation live history. Earlier artifacts
without `live_*` fields only certify full replay versus checkpoint-tail.
