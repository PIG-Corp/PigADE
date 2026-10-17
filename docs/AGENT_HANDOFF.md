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

- The fresh root run passed 130 workspace tests, including real-PTY
  integration tests and conditional-poll coverage. Fresh vendor verification passed 162 Alacritty library tests and 45
  reference cases. Workspace fmt, strict Clippy and release build passed. The
  app's 21 tests, all-target strict Clippy
  and build passed. A fresh
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
2. Build product history paging/copy/search on the asynchronous canonical
   record API; add quota reporting. Leased replay-cache cleanup is integrated.
   Keep raw journals as source data.
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

ADR0007 records the integrated same-app GPUI upgrade and the isolated native
AT-SPI bridge proof. The previous source pin lacks AccessKit. Complete
screen-reader/platform acceptance remains pending.

## New integration slice in progress

The same app now builds against GPUI `322cd509dcccaee1f6bc44725d270598ae8171ad`.
The isolated read-only AT-SPI probe sees the PigADE root, heading, selected
terminal and status nodes; see `reports/accessibility-atspi-20261017.md`. The
terminal has no Text/Action interface yet, so this does not satisfy accessible
history, Orca or ACC-01. A pinned Linux Rust-dependency license inventory is in
`research/gpui-322cd509-linux-licenses.{md,json}`; native/system library and
cross-platform distribution obligations are outside that inventory.

The first actual 24h soak failed after 104.13 s on a full resize queue. That
failed artifact is preserved as `benchmarks/results/soak-initial-104s-queue-rejection.json`.
The host now tags only proven pre-admission queue rejection. The soak retries
only such resize rejections with a bounded deadline; input/launch/unknown
outcomes are not retried. A new actual 24h attempt is running at
`/home/afterburner/.local/state/pigade/soak/soak-20261016T224310Z-0f87d77e`
(tool session 86044). At 1,981.94 s it recorded 391 ACKs and 645 resizes per
PTY, with explicit bounded resize admission retries and host RSS 281,661,440
bytes. This is a progress sample, not completion or a total memory claim.
The full corpus remains running at
`/home/afterburner/.local/state/pigade/long-history/long-history-run-32c6f5c4-645d-4845-a029-8f61da9a7cef`
(tool session 65474). It has advanced to the second journal (about 896 MB at
the latest inspection); the first journal reached the requested 1 GiB plus
framing and its oracle completed. All four manifest results and successful
runner completion remain required before acceptance.

Long durable paths now support `serve STATE --runtime PRIVATE_SHORT_DIRECTORY`.
Both namespaces stay exclusively locked. Marked, leased private replay caches
have bounded background orphan maintenance; see `reports/runtime-and-replay-cache.md`.
Unknown/unmarked/source objects remain preserved. Root added a regression for
unknown source-like files even inside a marked orphan. Chunked canonical record
reads are integrated with two workers/four queued tasks, exact watermarks and
no host-command-thread cold I/O. A real-PTY FIFO fault test proves another
session can accept input while a cold read is blocked. This is not a user-facing
history viewer. An independent Luna review found no actionable bug and reran
42 host tests plus the targeted cold-history/runtime integration tests.

`recovery-canonical-tail-smoke.json` records exact canonical tail reads and
unchanged sources for the small corpus. `vm-reboot-corpus-small.json` proves
that same four-session fixture recovers across two real guest boots with exact
frames and all six tail records. The full 1 GiB/recovery percentile gate is
still open. `scripts/vm-reboot.py` requires a completed corpus and same-binary
reference; small fixtures need the explicit diagnostic override. Its new
private data disk is serial-checked before formatting. The signed local SDK
with qemu-img is `/tmp/pigade-vm-sdk-qemu-img-20261017/evidence/manifest.json`;
the older SDK lacks qemu-img. See `reports/vm-reboot.md` for commands and failed
attempts. Python verification now runs 25 tests, including stale-font-build,
unknown-outcome
retry safety and pidfd-scoped cleanup after post-spawn failure.

The app font profile is integrated with bundled JetBrains Mono and Noto
CJK JP, liga/calt disabled, host-cell span clipping and unique internal OFL
family aliases. Root review proved upstream family names can resolve a system
copy despite add_fonts success; alias derivation/collision checks address that
ambiguity. Root caught a stale `/tmp` build in the first visual attempts and
marked those artifacts invalid for font proof. Fresh runs verify all five exact
embedded payloads before window creation, then pass render/AT-SPI/reopen at 100%
and 200% private output scales using app SHA275e47a1…; root inspected both
screenshots. See `reports/font-render-profile.md` and ADR0008. Do not claim
deterministic unsupported-codepoint fallback, fractional-scale or native
platform parity. CORE_PROFILE remains unchanged for the frozen corpus.

VTE callback and bounded Term capacity censuses are integrated without changing
accepted terminal semantics. The release actual-model diagnostic measures
1.2M-cell traversal p95 3.486 ms and resized-model p95 6.537 ms under concurrent
work. It is absent from the output/frame loop; see `reports/terminal-memory-census.md`.
This is not an allocator/RSS bound. Whole-event admission and a shared global
budget, including checkpoint/frame/reflow/import/export transients, remain open.

Root fixed typed-invalid CRC-valid journal tails: validate before history.begin
and mutation, retain the last good sequence/frame/history, name the rejected
sequence and preserve raw bytes. Tests cover full replay, actual checkpoint-tail,
host list/read, nonempty cold history and exact hidden-state continuation.
Clean recovery verification still refuses any storage warning. See the runtime
report. Completely unreadable headers/profiles now remain visible as bounded
artifact diagnostics, without fabricated sessions. The app merges them atomically,
shows a nonfatal warning and leaves healthy panel input available. Legal Unix
control/backslash filenames are escaped; omitted-count formatting saturates.
Fresh root checks pass 133 workspace tests, 27 Python tests, strict Clippy/fmt
and release bins/examples. App29 tests/strict Clippy/build pass. Alacritty163
library +45 reference tests and no-default check pass. Isolated warning native
AT-SPI and four distinct child-ACK checks pass with app SHA26027250… and host
SHAfface4d9…. The bridge maps Role::Alert to Notification101, not Alert2.
Root caught/restored the prior local-input-error gate, so rejected input still
requires acknowledgement; artifact notices remain nonfatal. The warning is
legible in the screenshot. Root inspected `final-child-ack-present`: all four
panes display distinct child ACKs and advance to seq3. The fixture waits 0.5s
before capture, so this is live-replica correctness evidence, not latency data.

CellExtra census now exposes references, reference-weighted combining capacity
and the largest extra. A valid imported shared-mark witness shows that two VTE
combining callbacks can create two large COW allocations; this does not assert
ordinary ANSI reachability. Fresh release diagnostic results are in
`benchmarks/results/terminal-allocation-census-references.json`. Admission cannot
use incoming mark bytes alone; global/event/transient budgeting remains open.

Latest retained-run inspection: the first two full corpus journals have reached
1 GiB plus framing and their oracles completed; session three has reached its
requested byte count and is verifying. The second real-time soak **FAILED at
3,849.40 seconds** with `input ACK timeout: SOAK-0-755`; tool86044 has exited.
See `benchmarks/results/soak-second-3849s-ack-timeout.json`. Earlier running
samples above are historical. Do not start another24h attempt before the runner
command-queue defect and its bounded PTY smoke are verified. Coalesced RELEASE
and next PING were reproducibly discarded by the old producer; this matches the
failure but input is intentionally not journaled, so the exact archived cause
cannot be established as unique. The frozen corpus remains running/tool65474.

Next resource work is sequenced in `research/terminal-admission-plan.md`.
Borrowed-header import validation, aggregate two-grid decode budget and the
query/title/link callback census are integrated. The wrapper now streams
borrowed Term state into the same bounded writer, preserving exact schema2
bytes. Root135 workspace tests and29 Python tests pass, plus vendor173 library
tests,45 reference cases and no-default check. Workspace strict Clippy/fmt and
release bins/examples pass. Vendor all-target Clippy passes with only the
three existing tty/unix question_mark lints forced to warning; do not describe
that exception as an entirely clean strict vendor run. App29 tests remain the
unchanged verified baseline. ADR0009 records source validation, canonical pool
order, all45×3 capture comparisons and full-envelope boundary oracles.

Ten alternating fresh diagnostic processes per variant reduce model-process
maximum-RSS p95 from90,484 to82,168 KiB. This includes fixture creation/reflow,
census and export, not isolated checkpoint workspace or product RSS. Sources,
frozen binaries and all samples are in the checkpoint-workspace JSON artifacts.
Initial exploratory baseline remains marked as such.

The producer queue and EOF fixes passed six focused tests and a real10.015s
four-PTY smoke with journal CRC and full/checkpoint-tail hot/parser/frame/cold
equality. Curated result: `benchmarks/results/soak-command-queue-fixed-10s.json`.
A third actual24h attempt is now running at
`/home/afterburner/.local/state/pigade/soak/soak-20261017T000738Z-6244149d`, tool44117,
frozen hostSHA67ed712c…/PID1004612 with boot/start identity in its manifest. At
120.07s it has23 ACKs and39 resizes perPTY and no admission retries. This does
not pass24h; do not launch a duplicate. Previous tool86044 is a retained failure.
