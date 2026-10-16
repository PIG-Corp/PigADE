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
Adjacent output can be coalesced without crossing control events. Input is not
journaled and recovery does not repeat PTY writes or launch tasks.

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

- The latest fresh root workspace run passed **111 tests**: 13 layout, 28 host
  library, 4 checkpoint-retention, 11 recovery, 1 slow-input, 34 storage and 20
  terminal tests. Root also reports fresh strict workspace Clippy and formatting
  checks passed. These verify the current integrated Rust workspace only.
- The recorded vendor run (151 Alacritty library tests plus 45 reference cases)
  and the app run (13 tests) are prior evidence, not rerun by this integration
  pass. Their records do not establish platform acceptance.
- `native-cold-history-5s-instrumented.json`: all four 5 MiB producers met the
  short 5-second rate target in 4.984–4.988 seconds; journal/output, full
  checkpoint-tail state and all 35,728 cold records matched. Host-exit-by-5s
  observation did not pass. This short diagnostic is not the 60-second SPEC
  workload.
- `native-cold-history-60s.json`: all four 60 MiB streams preserved valid raw
  data and exact full-replay/checkpoint-tail hot and cold state, including
  539,088 cold records per session. Producers took 77.4–78.1 seconds, so the
  4×1 MiB/s target **failed**. Peak sampled host RSS was 218,689,536 bytes for
  that run. The actual raw journal-read counter reports 63.1 MB per session for
  full replay and 356 bytes for checkpoint-tail in this corpus. It measured no
  UI-present/input latency and is not the SPEC 1 GiB long-history test.
- `native-memory-12-sessions.json`: 12 headless sessions with 10,000 hot rows
  each reached 447,610,880 bytes peak sampled RSS for the release host and
  passed the host-only 700 MiB threshold; recovery equality passed. This excludes
  UI, GPU, Python harness/producers, provider processes and helpers, so it cannot
  establish the total PigADE memory budget. The earlier long-socket startup
  failure remains in its own JSON artifact as failed evidence.
- The signed QEMU SDK and reboot harness are prepared. Root started a headless
  isolated guest, which reached its Linux root mount; no successful reboot result
  exists yet. Preparation/initial boot is not recovery or AI conversation proof.
- Historical app isolation/render evidence is limited to the recorded software
  Wayland scenario. No active-desktop input automation is allowed. No Windows,
  macOS, X11, IME, clipboard, fractional-scale, accessibility, GPU or provider
  dialogue/resume acceptance is claimed.

Do not present a short diagnostic, host-only metric, prior test record, or
prepared harness as completion of a SPEC gate. Preserve failed measurements.

## Next work

1. Rerun and improve the normal-history throughput workload without concurrent
   verification; retain both the current 60-second miss and all earlier failed
   runs. Do not weaken durability or correctness to reach producer rate.
2. Implement and verify paginated history/copy/search, quota reporting and
   cleanup for private replay-cache orphans. Keep raw journals as source data.
3. Measure total live/reflow/export/import allocations and set a global
   admission/heap budget; the current host-only 12-session result is necessary
   evidence, not product acceptance.
4. Run the SPEC corpus of four 1 GiB histories with 100,000 resizes each,
   bounded checkpoint-tail reads and complete-history equality. Verify crash
   and VM reboot paths after an actual isolated guest boot; none is complete.
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
