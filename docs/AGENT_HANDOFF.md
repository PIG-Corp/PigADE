# Handoff — 0.0.1-dev

## Current status

Implement the Linux MVP against SPEC/DESIGN. The project remains a runnable
foundation under stage-0 evaluation; **MVP and stack/platform gates are open**.
The user removed separate control stands; maintain the Rust+GPUI application.
All new subagents must explicitly use `gpt-6-luna`, a model-overridable role,
`fork_turns=none`, and a self-contained bounded task. Do not reactivate old-model
children.

Root is integrating the current shared work. Read `git status --short` and the
latest WORKLOG before editing: implementation, benchmark evidence, and VM harness
files may be uncommitted. Older agent/path descriptions are historical. Root
owns integrated verification and the commit.

## Implemented contracts

Host owns the only alacritty/VTE model. Output, resize, and significant timer
events receive sequence/generation identity and are CRC-journaled and fsynced
before model publication. Bounded queues, a separate input writer, private IPC,
durable launch intent/helper/OS identity checks, and unknown-outcome locks are
implemented. Stop has a blocking wake path while an actor is otherwise idle.
Adjacent output can be coalesced up to 64 KiB, with 16 KiB per PTY read and
queue capacity eight. A newer 64 KiB index-batch run met producer body rate in
all four streams, but host exit missed 60 s and producer schedule lateness was
7.1–7.5 s; it is not steady-rate or latency acceptance. The prior 250 μs
variant also remains a rate miss. Conditional polling selects four stable `(session,generation)`
pairs from at most 32 and returns metadata alone when the client's
`(session,generation,sequence,state,pid,error)` watermark is unchanged. Changed
views include a frame; the GPUI client validates selection and revision before
replacing state or cloning cells. Input is not journaled and recovery does not
repeat PTY writes or launch tasks.

Complete schema-2 terminal checkpoints cover both screens, hot history, cursors,
configuration, modes, colors and continuation-sensitive parser state. Checkpoint
publication is temporary-file fsync, rename, then directory fsync. Checkpoint
work is serialized across actors; a bounded cache reuses semantic validation
only for unchanged files with matching identity/profile and still validates the
external cold-history root. This reduces duplicate hot-grid imports; it is not a
global heap-budget proof. Quiet dirty sessions also checkpoint. Bounded numeric
per-session counters report journal, model, archive, checkpoint and frame costs;
they contain no terminal contents or input bytes.

Cold normal history is attached to the host. PIGROW02 typed rows, row-local
hyperlink pooling, retirement palette snapshots and continuation metadata are
stored in generation-bound segments and a fixed 64-byte-slot index. The
durability order is raw event fsync → model update/provisional rows → archive
data fsync → index fsync → checkpoint/frame publication. PIGCP002 binds the
committed index root. Recovery validates that bounded dependency and builds its
tail in a private derived cache; source archive and journal remain immutable.
Legacy hot-only checkpoints trigger full raw replay to reconstruct cold history.
The complete recovery oracle compares hot Term/parser state and every cold
record, not only the digest. See [cold-history integration](reports/cold-history-integration.md)
and [ADR0006](adr/0006-generation-archive-index-and-checkpoint-root.md).

SQLite schema-3 catalog, migration backups, layout CAS, valid legacy-v2 import,
and bounded wire-size checks are integrated. The headless layout model supports
split/move/swap/transfer/resize/equalize/rotate/focus and persistent undo/redo.
The GPUI app remains a small cell-replica prototype; the full hierarchy and
tiling UI, Git/worktree preparation and context/adapters are pending.

## Fresh evidence and limits

- The fresh root run passed 120 workspace tests, including real-PTY
  integration tests and conditional-poll coverage. Fresh vendor verification passed 152 Alacritty library tests and 45
  reference cases. Workspace fmt, strict Clippy and release build passed. The
  app's 17 tests, all-target strict Clippy
  and build passed (with two known upstream GPUI float-future warnings). A fresh
  isolated software-rendered UI run rendered and reopened four views at 69×15
  with the same PIDs/generations; root visually checked the screenshot for text,
  focus and clipping. This does not establish GPU or platform acceptance.
- `native-cold-history-5s-instrumented.json`: all four 5 MiB producers met the
  short 5-second rate target in 4.984–4.988 seconds; journal/output, full
  checkpoint-tail state and all 35,728 cold records matched. Host-exit-by-5s
  observation did not pass. This short diagnostic is not the 60-second SPEC
  workload.
- `native-cold-history-5s-64k-batch.json`: the prior no-wait 64 KiB batching
  diagnostic also met the 5-second producer rate with exact output, hot/parser
  and cold-record equality. Its short duration does not replace the 60-second
  target or measure the current 250 μs collector.
- `native-cold-history-60s-64k-batch.json`: the earlier no-wait variant preserved
  valid raw data and exact full-replay/checkpoint-tail hot and cold state,
  including 539,088 cold records per session. With no-wait 64 KiB coalescing,
  producers took 66.41–67.09 seconds, improving on the earlier 77.4–78.1-second
  run but still **missing** the 60-second target. Instrumented aggregate journal
  fsync cost was about 11.6–12.5 s/session and archive commit 25.8–27.5 s/session.
  Actual journal-read counters report 63.1 MB/session for full replay and 356
  bytes/session for checkpoint-tail in the earlier corpus. No UI-present/input
  latency is measured; this is not the SPEC 1 GiB long-history test.
- `native-cold-history-60s-250us-batch.json`: the 250 μs collector preserved
  complete recovery equality but missed producer rate at 62.275–62.517 s.
- `native-cold-history-60s-index-batch.json`: a subsequent indexed batch run
  wrote each 60 MiB body in 59.9845–59.9846 s and preserved exact journal CRC,
  complete hot/parser/frame state and all 539,088 cold records per session.
  Producer body rate passed in all four streams; host exit took 60.118–60.400 s,
  so the host-exit deadline failed. Maximum schedule lateness was 7.1–7.5 s and
  no input-latency samples were collected. This does not establish steady-rate
  or latency acceptance. Host peak sampled RSS was 218,050,560 bytes, excluding
  UI/GPU/provider and Python producer processes.
- The bounded-width reflow deque fix retains rows and cursor state under the
  tested resize/reflow scenarios. The global combining-character heap remains
  unbounded and is still an open memory-budget issue.
- `native-memory-12-sessions.json`: 12 headless sessions with 10,000 hot rows
  each reached 447,610,880 bytes peak sampled RSS for the release host and
  passed the host-only 700 MiB threshold; recovery equality passed. This excludes
  UI, GPU, Python harness/producers, provider processes and helpers, so it cannot
  establish the total PigADE memory budget. The earlier long-socket startup
  failure remains in its own JSON artifact as failed evidence; the host now uses
  the shorter private `g.sock` handshake path, covered by a long-state-path
  real-PTY regression.
- [`vm-reboot-cold-history.json`](../benchmarks/results/vm-reboot-cold-history.json)
  verifies a real isolated two-boot synthetic guest run: four sessions × 2,467
  cold records, complete hot/parser/frame/cold equality, distinct boot IDs, and
  356-byte checkpoint-tail reads taking 65.8–68.95 ms per session. Input to
  `launch_outcome_unknown` generations was refused, retry PID remained null, and
  scenario invocation markers did not change. This does not test provider AI
  resume, product layout restoration, power loss or REC-03 acceptance. Failed
  seed/boot attempts and the older hot-only VM result remain preserved as JSON.
- The latest frozen corpus builder/runner passed four 1 MiB sessions with 100
  resizes each, common-root verification, partial final UTF-8/CSI plus alternate
  screen/OSC8/synchronized-output checkpoints, streamed SHA-256 and per-event
  logical 24-hour metadata. Logical time is not elapsed time. The SPEC 1 GiB
  corpus and real 24-hour soak remain open. The corrected four-session soak runner
  passed a 10.036-second functionality smoke with CRC/recovery, input ACKs and
  three resizes per session. The eight-idle-session host-only diagnostic recorded 0.0%
  steady CPU and 319,963,136-byte peak sampled RSS, excluding UI/GPU/provider
  and producer processes.
- [`conditional-poll-20261017`](../crates/app/evidence/conditional-poll-20261017/)
  records the fresh isolated render/reopen run. The GPUI build completed after
  moving its 14 GiB cache away from quota-limited `/tmp`; `ld.lld`'s SIGBUS was
  traced to `EDQUOT` during output mmap, not an application runtime failure. No
  active-desktop input automation was used. No Windows, macOS, X11, IME,
  clipboard, fractional-scale, accessibility, GPU or provider dialogue/resume
  acceptance is claimed.

Do not present a short diagnostic, host-only metric, prior test record, or
prepared harness as completion of a SPEC gate. Preserve failed measurements.

## Next work

1. Evaluate the index-batch run against steady-rate, host-exit and latency
   requirements; retain the 250 μs rate miss and earlier failed runs. Do not
   weaken durability or correctness to meet producer rate.
2. Implement and verify paginated history/copy/search, quota reporting and
   cleanup for private replay-cache orphans. Keep raw journals as source data.
3. Measure total live/reflow/export/import allocations and set a global
   admission/heap budget; the current host-only 12-session result is necessary
   evidence, not product acceptance.
4. Run the SPEC corpus of four 1 GiB histories with 100,000 resizes each,
   bounded checkpoint-tail reads and complete-history equality. The cold-history
   portion of the synthetic VM reboot is now exercised; provider/layout/REC-03
   acceptance and the remaining fault matrix are open.
5. Run a separate 24-hour real-time soak. Then complete isolated platform,
   input/IME/clipboard/accessibility, renderer, GPU and real CLI version/dialogue
   checks under the desktop incident guard.
6. Finish the product hierarchy/tiling UI, safe Git/worktree flow, context and
   provider adapters, exact-resume capabilities, packaging and the end-to-end
   MVP scenario. No blind prompt replay, `--last`, silent new conversation or
   duplicate task launch.

Stage 0 remains open. Preserve originals and report unavailable/degraded history
explicitly; do not call the history product surface complete while it has no
user-facing paging/copy/search workflow.

## Active retained validation runs — 2026-10-17

Root started the full 4×1 GiB / 100,000-resize corpus with `--verify`, including
original live-model comparisons, and a separate actual 86,400-second PTY soak.
Both use frozen binaries and private durable state below
`/home/afterburner/.local/state/pigade/{long-history,soak}`. Inspect the newest
runner/manifest and streamed progress before starting duplicates. A running
manifest is not a completed test. The soak and corpus overlap and therefore do
not establish isolated performance budgets. No runtime state belongs in Git.

The current TUI stress result has 116 ACK observations (p95 42.91 ms, p99
59.07 ms), producer rate and CRC/recovery equality passes; it does not measure
UI presentation. Full model comparison against the generating live instance is
now opt-in via `verify_recovery_against_live`; earlier JSON without `live_*`
fields does not include that third oracle.

ADR0007 selects a same-app GPUI source upgrade to enable native Linux
accessibility; implementation/build/runtime acceptance is still pending. The
previous source pin lacks AccessKit. No platform acceptance follows from source
presence or a dependency upgrade.
