# Changelog

## 0.0.1-dev — in development

- Revisioned prompt library with CAS editing, idempotent deletion, frozen draft insertion and explicit clipboard copy. No terminal input or automatic task submission; schema12 migration keeps a durable backup.

- Именованные профили запуска агентов: библиотека, редактор, сохранение настроек из review, проверка ревизии при подготовке и неизменяемый источник профиля в снимке запуска. Схема11 мигрирует библиотеку с durable backup; CRUD не меняет живые CLI.

- Ручное продолжение точного диалога OpenCode1.19.3: проверка сохранённой папки через публичный локальный API перед review/start, отдельный bounded worker, новое поколение при сохранении прежней истории и раскладки. Контекст PigADE и последнее задание повторно не отправляются; неизвестный исход запуска остаётся заблокированным.

- Native read-only workspace Git snapshot: branch/worktree, staged/unstaged/untracked/conflicted groups, explicit async refresh, bounded incomplete state and generation/process-preserving UI.

- Добавлено отдельное подтверждаемое действие остановки CLI для выбранной панели и поколения из списка сессий. Stop фиксирует session/generation, сохраняет размещение и историю, не повторяется после неизвестного ответа и подтверждается только точным состоянием `exited`. Потерянный ответ сверяется запросами только на чтение; `history`/`degraded` остаются неподтверждёнными. На Unix SIGHUP отправляется только верхнему CLI-процессу, дочерние процессы могут сохраниться. Закрытие диалога после отправки скрывает наблюдение, но не отзывает запрос.

- Refine pointer dragging to retain the original relative grab point, including header/edge grabs; extend the private native fixture with stationary-pointer keyboard focus and exact Dwindle sibling insertion assertions.

- Project/workspace/tab management menus, revision-checked rename, workspace
  archive/restore without stopping CLI, and archived checkout identity checks.
- Saved geometry layouts with native thumbnails, keyboard navigation, CAS
  save/delete and single-step undoable application to existing stable panes.
  Schema9 adds optional per-slot launch settings and durable template-run
  records; schema10 adds durable per-pane aliases. Upgrades from v8/v9 are
  backed up before migration. Four new-tab presets are available;
  custom launch configuration remains optional. A run first freezes and reviews
  the workspace, process count, working directory and each slot's context. It
  creates no tab or process until explicitly advanced, then starts at most one
  CLI per user action. Applying a saved layout to an existing tab remains an
  undoable geometry operation and does not launch processes. Geometry-only
  templates remain valid; older hosts without launch-schema support report the
  feature unavailable and are not restarted automatically.
- Pane aliases use CAS revisions and are bounded to128 Unicode characters /512
  UTF-8 bytes; empty names reset to the CLI title and control characters or
  surrounding whitespace are rejected. Aliases do not alter OSC titles, launch
  context, generations, process state or placement undo. The palette searches
  placed panes by alias/provider/frozen role/state and project/workspace/tab,
  with64-result pages and explicit next/first actions. Mouse and arrow/Enter
  activation atomically focuses the pane and selects its project/workspace/tab,
  preserving generation pins without launching CLI. Older hosts without the
  pane-label capability leave this feature unavailable and are not restarted.

- Terminal listener retains only live response FIFO and the final ordered title
  update. Ignored UI/clipboard/bell and replay response events no longer build a
  transient queue; response bytes and checkpoint continuation are preserved.
- Added nonmutating parser resource preflight for grid and listener callbacks,
  with actual-model continuation checks and source-bound release diagnostics.
  Removed discarded shared combining-vector copies during wide-cell clearing;
  terminal schema2 and sibling state remain unchanged. Global memory admission
  remains in progress.

- Initial SPEC.md and DESIGN.md product contracts preserved.
- Git repository, mandatory shared agent history, project map, handoff and version policy introduced.
- Native terminal/PTY/storage foundation, complete-state checkpoint experiments and GPUI cell-replica application under verification. Separate Electron control application removed by user request.
- VTE UTF-8 fix, bounded checkpoint decoding, launch recovery and saturated-input regressions added; stack/MVP gates remain open.
- Pure tiling with stable UUIDs, atomic cross-workspace transfers and persistent100undo/redo; schema3 catalog CAS and backed-up legacy migrations.
- Allocation-aware checkpoint export/decode, all-event/idle checkpoint cadence, bounded output batching and chunked query replies.
- Verified private software-Wayland rendering/reopen and measured4-stream throughput; complete MVP/performance gates remain open.
- Host-integrated cold normal history: PIGROW02 rows, generation segments, fixed-slot index and PIGCP002 committed roots; complete hot/parser and every-record recovery equality. The index-batch run meets the 60-second producer body rate but misses host-exit deadline and has no input-latency samples; see the cold-history integration report.
- Bounded conditional polling avoids repeating unchanged terminal frames. The 64 KiB index buffer preserves data fsync before index fsync and uses logical archive offsets to avoid per-row seeks. Synthetic isolated VM reboot verifies four sessions with 2,467 cold rows each; provider resume/layout acceptance and the 24h soak remain open.
- Fresh root verification passed 128 workspace tests, 162 vendor library tests plus 45 reference cases, 57 VTE tests and the native app's 21 tests/all-target strict Clippy/build. Isolated software-rendered UI reopen preserved four views and their PIDs/generations. No GPU or platform acceptance is claimed.
- The live-model recovery oracle now compares complete state, frame and every cold record against both replay paths; a mismatched-history regression fails explicitly.
- The latest frozen corpus smoke verified four 1 MiB sessions with 100 resizes, common-root recovery, SHA-256 and logical 24-hour metadata. It is not the 1 GiB corpus or a real-time soak.
- Separate locked runtime endpoints support long durable state paths. Leased replay caches have bounded background orphan maintenance that preserves unknown/source files.
- Canonical cold-history records can be read in bounded chunks by two asynchronous workers; a blocked archive read does not block commands to another live PTY. The product history viewer is still pending.
- Queue-full replies distinguish proven rejection before admission from unknown outcomes. The soak retries only explicitly rejected resize commands; the UI does not retry input or launches. The failed 104-second soak is retained and the new actual 24-hour attempt remains in progress.
- The same app builds on pinned GPUI platform/AccessKit. A private Wayland AT-SPI probe sees named application, heading, terminal and status nodes; screen-reader text/history operation remains unverified. The pinned Linux Rust-dependency license inventory covers 481 reachable packages.
- Bounded VTE callback and Term allocation censuses cover REP, partial parser state, both grids/cached rows, shared extras, links and metadata; release diagnostics measure traversal cost without running it per output event. Global memory admission is still pending.
- Bundled terminal font aliases avoid ambiguous system-family selection; an offline reproducible generator preserves outlines/metrics/cmap and includes OFL attribution. The painter clips clusters to host-assigned spans. JP CJK forms are explicit; uncovered platform fallback and cross-platform parity remain unverified.
- Invalid typed journal tails retain the last valid generation/model/history with a sequence-specific error instead of dropping the session. Full/fast recovery and hidden-state continuation regressions cover this path; raw sources are unchanged.
- Unreadable/incompatible journal artifacts produce bounded nonfatal recovery warnings without fake sessions or blocking healthy terminals. Poll validation is atomic; notice-only updates preserve cached cells. Unix filename controls and extreme omission counts are covered.
- Resource census now distinguishes shared allocation size from reference-weighted combining capacity; a checkpoint-import/VTE continuation regression demonstrates copy-on-write fanout. Whole-event/global admission is still open.
- Checkpoint export borrows the validated grid and payloads while preserving exact schema2 bytes; import checks the borrowed envelope before owned decoding and shares a cell budget across both grids. Model-process RSS measurements and their scope are recorded in ADR0009.
- The second real-time soak failed after64minutes. A producer bug discarding the next PING after a coalesced RELEASE is reproduced and repaired; failed evidence is retained and the24h gate remains open.
- Parser sync storage starts empty and grows on demand. Spare grid rows use a width-sensitive payload target; logical rows and checkpoint compatibility are preserved. Numeric censuses expose grid/cache shapes, logical payloads and parser buffers.
- IPC input arrays are checked before Serde's internally tagged buffering. A response that cannot be encoded returns an explicit unknown-outcome error before any frame header is written; dispatched commands are never retried.
- Frame hyperlink interning retains shallow handles instead of a second owned copy of every ID/URI, preserving content deduplication and wire indices.
- Native local IME composition now has UTF-16 range/selection handling, a clipped cursor overlay and lifecycle cancellation. Same-app private 100%/200% captures verify the overlay stays out of host history; actual Linux IME/reset ordering remains unverified.
- Isolated offline PTY runs exercise exact Codex/OpenCode binaries through startup, two resizes and a one-second running observation. Claude reaches both resizes but exits with an API-connectivity failure category; task/context/resume certification remains open.
- Native mouse reports follow acknowledged modes and stable generation capture. Visible selection handles wide/combining text and soft wraps; explicit copy/paste uses local controls and bounded bracketed input. Raw PTY tests verify exact mouse/paste bytes and stale-generation rejection; platform gesture/clipboard certification remains open.
- Full four-session1GiB/100k-resize corpus matches the live model, full replay, checkpoint+tail and all54.36M cold records. Thirty host kill/reopens and a real full-data two-boot VM preserve exact historical frames/canonical tails and sources; UI-present/provider-resume and complete MVP gates remain open.
- A third24h soak attempt failed after3079s on sustained resize QueueFull. Post-failure hot/frame/cold recovery equality passes; bounded rejection diagnostics and pidfd cleanup prepare the retained same-binary reproducer. The24h gate remains open.
- Installed Codex0.162.1 completed a real two-process exec/exact-ID resume continuity check with retained synthetic context. Redacted hashes and a bounded reproduction harness are retained; product/interactive automatic resume remains disabled.
- Dynamic terminal palette/foreground/background overrides now reach the native painter through bounded typed metadata. Set/reset/checkpoint tests and a source-bound private pixel capture verify colors without a second ANSI parser.
- User-selectedgpt-6-luna model for all new subagents.
- Mandatory desktop isolation after a live input automation incident; unsafe smoke tooling removed.

This section describes development work; it is not a release announcement.

- Typed host cursor state now drives block/underline/beam/hidden rendering, inactive hollow outlines and reduced-motion-aware blinking. Four private same-app steady captures verify pixels and process-preserving reopen; actual blink timing and other platform gates remain open.

- Soak ACK failures now retain bounded producer-side milestones, input admission/snapshot timings and identity-checked process wait metadata. The prior17-minute failure has exact frozen-host replay verification; sustained operation remains an open gate.

- Terminal underline variants, custom underline colors and strikeout render from host cell metadata, including blank/wide cells. Bounded pure plans and source-bound software captures at scale1/2 cover the integration; font-exact metrics and physical platform checks remain open.

- GPUI X11 now creates/focuses valid XIM contexts in order and filters stale IC callbacks. Exact downstream patch hashes, nine lifecycle tests and private actual-app Pinyin debug/release commits are retained; session-generation reset and full platform acceptance remain open. Signed test-only X11/IME tooling never drives the active desktop.

- Fixed a reproduced PTY output/query-reply wait cycle. Coalesced bounded FIFO replies no longer prevent actor output draining; real PTY tests verify50,000 exact replies and explicit response-cap degradation with durable raw-tail recovery. This is independent of the unresolved soak ACK incident.

- Fixed lost Alt-printable input and verified an actual release-app EN/RU/Ctrl/Alt/arrow sequence against raw PTY bytes, followed by a native Pinyin regression. Failed baselines, exact source/binary hashes and private captures are retained.
- Known host queue rejection now has token-bound explicit acknowledgement; unknown outcomes stay locked and later unsubmitted requests are counted. Rejected resize caches recover without resending input. Per-session UI failure isolation remains open.

- Live host snapshots now expose a separate bounded numeric actor-stage/PTY-reader probe even when frame publication stalls; contention/poison/old-host absence stay explicit. Soak ACK/resize diagnostics retain a validated whitelist, without raw input/output or deadline changes.

- Exact session/generation command rejection gates isolate other sessions; bounded FIFO mouse receipts reconcile speculative buttons and require explicit, token-checked release cleanup. Unknown outcomes stay locked; native saturation acceptance remains open.

- Worker input and UI state/receipt updates now wake on events through bounded coalescing; fixed50-key private X11 latency fixtures retain baseline and changed samples. Formal latency/GPU acceptance remains open.

- Synthetic soak now waits for all four visible READY markers before START; a short run reproduced an ACK timeout at checkpoint stage with complete final recovery retained. Async checkpoint persistence remains under verification.

- One bounded checkpoint worker persists immutable exact-watermark snapshots; live actors continue durable output while persistence is paused. Exit retains forced checkpoint-tail recovery. Fresh191-test workspace and30s CRC/recovery smoke pass; long soak remains pending.

- One private actual-app Wayland-v3/Fcitx Pinyin fixture passes with exact raw PTY bytes and reviewed preedit/candidate captures. Signed SDK preparation, kernel/peer namespace attestation and a persistent fixed test keyboard are reproducible; target-generation and full platform gates remain open.

- Frozen async-checkpoint host completed1200 actual seconds with four producer CRCs,238 ACKs/396 resizes per stream and exact full/checkpoint-tail recovery; slow resize/journal cases retained. A new actual24h run is active; cold rows in this short run are zero and the24h/resource gates remain open.

- Dedicated bounded host change waits wake cell polling independently of input. Tokens close publication races and survive host restart through fresh epochs; failed service falls back without changing committed launch results. Real host/PTy and app UDS regressions pass. Retained private latency runs still miss formal budgets; finite numeric traces prepare the next redraw fix.

- Redraws now track visible preedit/ACK changes instead of every accepted input receipt.127 app tests and both private native Pinyin regressions pass; one50-key software trace reduces p95 from97.208to66.165ms, still outside formal budgets. Full samples and initial failed run remain retained.

- Added versioned durable raw resource-refusal records and exact-prefix replay, with sealed journal writers and refusal-safe checkpoint receipts.217 workspace tests plus extended process-level suffix checks pass; production launch policy stays legacy until whole-event memory admission is implemented.
