# Handoff — 0.0.1-dev

## Current task and boundaries

Implement the Linux MVP against the complete SPEC.md and DESIGN.md. The user
explicitly requires continuing until the end-to-end MVP and release blockers
pass. **Stage0 and the complete MVP are still open.** A tested foundation is
not the finished product. Separate control applications were removed by user
request; develop the Rust+GPUI application itself.

Read AGENTS.md, README.md, SPEC.md, DESIGN.md, PROJECT_MAP.md and current Git
status before edits. Every modifying agent appends its own WORKLOG via
`scripts/log-change.py`. Root integrates/tests/commits. New native children must
explicitly use `model=gpt-6-luna`, `fork_turns=none` and bounded ownership. Do not
reactivate older children with an unknown/different model. Preserve parallel edits.

Never inject input, manipulate focus/workspaces, launch automated windows on
the active desktop or change monitor/zoom/configuration. Use verified private
namespaces/compositor or VM. Read [the incident report](reports/desktop-incident.md).
No provider credential/history copying, global config changes, blind prompt
injection, `--last`, automatic repeated task or silent new conversation.

## Implemented foundation

- Host owns the sole Alacritty/VTE parser/model. UI receives validated cells and
  generation/sequence watermarks, never independently parses ANSI. Ordered
  output/resize/timer records are CRC-journaled/fsynced before model publication.
  Output batches are bounded64KiB with a250μs collector, reader queue8×16KiB;
  input has a separate bounded writer. Pending terminal query replies use a
  coalesced16MiB payload FIFO and at most eight16KiB admission attempts before
  servicing output; a full writer no longer deadlocks the actor against child
  output. Operational response-cap degradation retains the live published view
  while recovery can apply the raw-durable tail; see reports/query-response-backpressure.md.
  Input itself is not journaled.
- Durable intent/helper launch protocol, OS boot/PID/start identity, operation
  IDs, generation locks and unknown-outcome reconciliation prevent blind retry.
  QueueFull is marked pre-admission; unknown input/launch outcomes never retry.
  Stop wakes idle actors. Private state/runtime namespaces remain locked;
  short runtime endpoints support long durable paths.
- Full schema2 checkpoints include both grids/history/cursors/modes/palette,
  parser UTF8/CSI/OSC/REP/synchronized state. Borrowed export preserves canonical
  bytes; import validates profile/title/envelope before owned allocation and
  shares a two-grid cell budget. Frame equality alone is insufficient.
- Cold normal history uses PIGROW02 generation-bound segments and fixed64B
  indexes. Durability order: raw event → model/provisional retirement → archive
  data → index → checkpoint/frame. PIGCP002 binds the history root. Fast recovery
  uses bounded dependency validation and a private leased derived cache;
  originals remain immutable. Complete oracles compare every cold record.
- Typed-invalid durable tails retain the last valid prefix with an explicit
  sequence error. Unavailable journals produce bounded nonfatal artifact notices
  without fake generations. Healthy panel input survives unrelated notices.
- Two asynchronous history workers, queue4 and64KiB response chunks expose exact
  canonical ranges without blocking the host command thread. Product history
  paging/search/scrolling and storage quotas/compression remain unfinished.
- SQLite catalog schema3, migration backups/CAS and the pure layout model support
  stable pane IDs, split/move/swap/transfer/resize/equalize/rotate/focus and100
  persistent undo/redo. Full hierarchy and tiling UI are still pending.
- GPUI commit322cd509dcccaee1f6bc44725d270598ae8171ad is pinned/hashed; Linux
  Wayland+X11 builds use a separate compatible Cargo workspace. Bootstrap now
  applies the exact `pigade-x11-ime-focus-v1` X11 lifecycle profile with guarded
  pre/postimages; see ADR0010. No Electron or
  imported Zed GPL terminal integration. Bundled OFL font aliases, fixed JP CJK
  fallback and cell-span clipping are documented in ADR0008. Unsupported glyph
  fallback is not deterministic across platforms.
- Typed dynamic palette overrides now reach the cell painter and terminal padding; reset returns to the unchanged default UI palette. Invalid palette updates are bounded/atomic. See reports/dynamic-palette.md. Default-theme terminal query responses and font-exact decoration positions are still open. Cell underline variants/custom color/strikeout and decorated spaces now render from host attributes; fixed-scale1/2 captures are in reports/terminal-decorations.md. Typed cursor shapes/visibility/blink now reach the painter; four source-bound steady software captures verify block/underline/beam/hollow/hidden pixels. Actual blink timing remains open; see reports/terminal-cursor.md.
- Native local IME has UTF16 range/selection handling and a clipped overlay;
  one real X11 Fcitx-Pinyin commit passes in debug/release with no premature or
  duplicate PTY input. Clean upstream failed IC activation after render/focus.
  Native Linux session-generation reset ordering remains unverified. Do not reintroduce the removed
  cancelled-until-KeyDown latch: IMEs may consume physical keys themselves.
- Mode-aware mouse input, stable generation drag capture, visible cell selection
  and explicit clipboard paste are integrated. Shift selects locally. Prefix
  Ctrl+Shift+Space then1–4/A/C/V selects pane/all/copy/paste; footer copy/paste
  buttons exist. Selection invalidates on a new frame sequence. Paste is one
  bounded16KiB command including brackets; unsafe embedded controls are refused.
  Stale clipboard completion produces a nonblocking notice. Explicit prefix+Esc
  reconciles a locally rejected release; shared unknown errors remain blocked.
  See [mouse](reports/mouse-input.md) and [selection/paste](reports/selection-and-paste.md).

## Latest verification

- Root workspace fmt, strict all-target Clippy and180 tests passed (including
  the27-test input target:25 shared pure codec tests and2 real raw-PTY tests).
  Release workspace bins/examples build passed. Native app90 tests, strict
  all-target Clippy, fmt and debug/release builds passed. Python126 tests and9 explicit pinned XIM state tests passed.
  Fresh checks must still follow future changes.
- Private same-app software-Wayland render/reopen preserves four live CLI
  identities. Root inspected selection/Unicode/child ACKs/footer controls in
  `crates/app/evidence/selection-20261017`. This does not certify native gestures,
  clipboard roundtrip, real GPU, X11, IME or cross-platform behavior. AccessKit
  private AT-SPI proof exposes named application/heading/terminal/status/alert;
  terminal Text/Action and Orca acceptance remain open.
- The full4×1GiB /100,000-resize corpus completed with live/full/checkpoint-tail
  hot/parser/frame and all54,360,410 cold-record equality. Duration8,020.847s;
  logical24h sidecars are not elapsed24h. Thirty SIGKILL/reopen cycles on this
  corpus preserve all four frames, six canonical tail records and source hashes.
  Host-only four-frame p95=1,367.28ms, p99=1,731.69ms; raw tail reads405–441B.
  These are host response timings under concurrent work, not UI presentation or
  isolated PERF acceptance. See [full corpus report](reports/long-history-full-20261017.md).
- Full-data VM rerun completed `recovery_verified` with distinct boot IDs,
  four matching historical frames/history watermarks and unchanged source
  inventory. Checkpoint-tail reads405–441B. Guest hashes full files for staging/
  inventory integrity; it does not run the full cold-history model oracle. The preceding run failed
  because the serial collector parsed an incomplete line; the LF-gated fix has
  regression coverage. Do not infer a completed VM proof before final exit.
- Codex0.162.1 and OpenCode1.18.22 pass bounded offline startup, two resizes and
  a one-second running observation. Claude2.1.259 reaches both resizes but exits
  with an API-connectivity failure category in the deliberately network-isolated
  disposable home. No AI task/context/native-resume certification follows from
  these runs. See [CLI matrix](compatibility/CLI.md) and [report](reports/provider-pty-20261017.md).
- The60s four-stream producer-body rate passes at59.9845s with exact CRC/hot/cold
  recovery. Host exit misses60s and schedule lateness reaches7.1–7.5s; no
  input-to-present samples. Twelve hot sessions measure447,610,880B host RSS;
  eight idle sessions measure0 CPU ticks/60s and319,963,136B host RSS. UI/GPU/
  providers/harness are excluded. These do not establish total resource gates.

## Active runs and failed soak

No actual24h test has passed. Retain all failures:

1.104.13s resize QueueFull before the runner learned bounded pre-admission retry.
2.3,849.40s ACK timeout; the producer could discard coalesced RELEASE+next PING.
   The command-queue bug and EOF busyspin are fixed with a real PTY smoke.
3.3,079.132s resize admission deadline after repeated explicit QueueFull. This is
   sustained host pressure, not unknown IPC. Four journals each contain996
   accepted resizes; last sample3003.79s lacks the final actor stall. All four
   journals pass post-failure full/checkpoint-tail hot/frame/cold equality with
   the same frozen67ed712c host. It is not clean producer CRC or a live oracle.

[Soak saturation report](reports/soak-resize-saturation.md) and redacted evidence
retain the findings. The runner now samples bounded/throttled QueueFull numeric
metrics and uses identity-revalidated pidfd child cleanup. Deadline10s and
unknown-outcome rules are unchanged. Concurrent VM I/O is only a hypothesis.

**Failed one-hour reproducer:** tool50088 exited1 after1013.162s with
`input ACK timeout: SOAK-0-196`; no24h/recovery pass. Bounded rejection metrics
and the exact frozen binary remain in
`~/.local/state/pigade/soak/resize-repro-20261017`, same frozen67ed712c host from
`soak-20261017T000738Z-6244149d/state/host-release`, with new instrumentation.
It is not a24h acceptance run. Raw journals are CRC-valid and the missing token is absent from durable output. All four complete hot/parser/frame recovery comparisons passed afterward (cold counts0 in this prefix). New bounded producer milestones and identity-checked wait-channel diagnostics are covered by15 tests; see reports/soak-ack-timeout-20261017.md.

**Completed1200s ACK reproducer:** tool9206 exited0 after1200.068s,
`~/.local/state/pigade/soak/resize-ack-repro-20261017`, same frozen67ed712c binary.
All four CRC/full-recovery checks passed,238 ACK cycles per stream,2856 producer
records with no sequence gaps. This neither explains prior failures nor passes24h.

**Failed7200s diagnostic:** tool10513 exited1 after548.820s,
`~/.local/state/pigade/soak/resize-ack-long-20261017`, same frozen67ed712c binary.
Token104 was received and its ACK write completed in the child. The timeout
snapshot remained at10509; the final durable event10510 contains the ACK and
frozen-host recovery displays it. All four complete hot/frame recovery checks
pass (zero cold records); clean producer CRC is unavailable after abort. The
specific post-write/pre-publication stage is unresolved. No soak is currently
running; do not restart one until the independent actor/reader stage probe is
reviewed. No24h or isolated PERF claim.

**Completed VM run:** tool50099 exited successfully,
`~/.local/state/pigade/vm-reboot-full-20261017/run-20261017T005802Z-34a0410e`.
Full corpus builder tool65474, reopen tool52970 and soak tool44117 have exited;
do not poll/restart them as active jobs. Private source data never belongs in Git.

## Next work, in priority order

1. Diagnose/fix sustained resize admission saturation with the instrumented same
   binary reproducer; retain failures and repeat the necessary recovery checks.
2. Full-data VM evidence is committed/passed at its documented scope. Repeat actual24h soak only after resolving the producer/host failure above.
3. Whole-event/global heap admission remains open. Lazy VTE sync storage,
   width-sensitive spare-row payload targets and shallow frame link interning
   remove specific copies; they are not a memory bound. Follow
   [the allocation plan](research/terminal-admission-plan.md), including COW
   combining/title/REP amplification, frames/checkpoint/reflow/import/export
   transients and durable whole-event refusal. Never crop ANSI operations.
4. Complete isolated native input/IME/clipboard/X11/fractional scale/real GPU and
   accessibility checks, actual CLI conversations and exact native resume.
   The reviewed Codex conversation harness passed two real native exec turns
   with exact-ID resume and retained synthetic context. See
   reports/codex-conversation-20261017.md. Product/interactive TUI binding and
   automatic resume are still pending; no credential/history copying occurred.
5. Build stable history viewing/search/copy, then full project/workspace/tab/
   tiling UI, Git/worktrees, revisioned context/roles/memory and versioned adapters.
6. Finish packaging, platform build matrix, performance/fault matrix and the
   end-to-end MVP. Do not mark the goal complete while release blockers remain.

Detailed historical observations remain in WORKLOG.md, linked reports, benchmark
artifacts and Git history. This handoff records current state rather than stale
running snapshots.
