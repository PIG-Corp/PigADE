# PigADE — agent change history

Append-only engineering history. Every contributing agent records its own work with `scripts/log-change.py`; see AGENTS.md. Tests, measurements and gaps are recorded as observed, without implying release acceptance. Current development version: 0.0.1-dev.

## 2026-10-16T19:23:39+00:00 — root

Initialized Git, required per-agent shared worklog and project/handoff docs; fixed development version at 0.0.1-dev. Backfill: scaffolded native terminal/host, UTF-8 downstream fix, explicit state-export experiment and tests.

- Files: AGENTS.md, VERSION, CHANGELOG.md, WORKLOG.md, docs/PROJECT_MAP.md, docs/AGENT_HANDOFF.md, docs/VERSIONING.md, scripts/log-change.py, Cargo.toml, crates/terminal, crates/session-host, vendor/vte
- Verification: Before version sync: terminal 9 tests pass; workspace clippy/check passed before checkpoint integration. Full baseline pending latest changes.
- Risks / next: Finish checkpoint persistence and responsiveness review fixes; reverify before committing code. Stage 0 and MVP remain open.

## 2026-10-16T19:24:17+00:00 — electron_control

Backfill: implemented independent Electron/headless-xterm control prototype and read-only native host review; stock serialize checkpoint rejected by seven continuation fixtures.

- Files: prototypes/electron-control, docs/reports/host-review.md
- Verification: Executed npm run check and 16 diagnostic tests successfully; actual four-panel Electron software-rendered Wayland smoke passed close/reopen identity equality and dark/light screenshots. Native read-only review dynamically reproduced malformed resize panic using legacy-profile binary and blocked input/resize; source boundary inspected, no current checkpoint acceptance claimed.
- Risks / next: Native input writer fix assigned in input.rs and slow_input.rs. Root fixes geometry and integrates. Production long-history/VM/soak/GPU/provider/IME acceptance remains open.

## 2026-10-16T19:24:18+00:00 — checkpoint_research

Backfill upstream terminal recovery/license research, minimal VTE UTF-8 loss reproducer, proposed open recovery ADR, and clean GPUI source build/smoke evidence.

- Files: docs/research/terminal-core.md, docs/adr/0001-terminal-recovery-gate.md, tests/fixtures/terminal/utf8-regression.rs, tests/fixtures/terminal/utf8-regression.json, docs/research/gpui-source-check
- Verification: Read SPEC/DESIGN fully and later local AGENTS/map/handoff/version/worklog. Isolated cargo run with unmodified alacritty_terminal0.26/vte0.15 confirmed Grid serde cursor loss and parser-reset UTF8/REP/CSI/sync divergence. Root boundary test reproduced chunk3 cell8 mismatch; direct VTE repro é 界 chunks1,5 outputs é界. Clean GPUI official hello_world initial cargo build failed xattr0.2.3/libc0.2.190 E0425; upstream-lock retry passed debug build in3m08s, native Wayland window mapped during40s run ending intentionally by timeout124. Screenshot showed other workspace; visual/input/accessibility not verified. JSON fixture and markdown fences checked.
- Risks / next: Lead owns reviewed VTE/core patches and integration. GPUI needs proper visible-surface/input/accessibility harness and release measurements; complete checkpoint/cold history, VM reboot and24h soak remain open. No stack approval. Temporary GPUI Cargo home isolated after releasing initial global cache lock.

## 2026-10-16T19:26:07+00:00 — storage

Backfill: implemented durable bounded journal and SQLite launch state machine, real PTY/recovery tests, and experimental explicit Alacritty Term/Grid checkpoint API with deterministic anonymous hyperlink IDs.

- Files: crates/storage, crates/session-host/tests/recovery.rs, vendor/alacritty_terminal
- Verification: cargo test -p pigade-storage:10 passed; storage clippy all-targets -D warnings passed; host recovery process tests:4 passed serialized and default parallel, test clippy passed; /home btrfs release microbenchmark1000x16KiB fsync/event p50=0.883413ms,p95=1.364661ms,p99=1.567758ms; vendored Alacritty137 unit tests passed plus no-serde check; strict vendor clippy found3 unchanged upstream question_mark lints, precise force-warn rerun passed with those3 warnings. Additional135 upstream-capture checkpoint/tail comparisons running.
- Risks / next: Stage0 experimental only. Root integrating composed parser/Term checkpoint publication. No proof of hardware power-loss, VM reboot,24h soak,4x1GiB/cold-history,renderer/platform/CLI certification. New composed upstream-capture regression pending; no raw journal compaction authorized.

## 2026-10-16T19:27:37+00:00 — storage

Validated composed experimental parser+Term checkpoints against all45 pinned upstream recordings at135 cutpoints, including continuation beyond each capture; completed vendor API handoff.

- Files: vendor/alacritty_terminal/PIGADE_PATCH.md, vendor/alacritty_terminal/src/term/checkpoint.rs
- Verification: New upstream_reference_corpus_composed_checkpoint_and_tail passed135 checkpoint/tail/continuous comparisons in43.61s debug; cargo test -p pigade-terminal9/9 passed after patch. Initial137 vendor unit tests and no-serde check passed. Vendor precise baseline clippy exception documented.
- Risks / next: Root owns persistent checkpoint integration. Moving to assigned bounded IPC frontend file only; all production/reboot/soak/cold-history gates remain open.

## 2026-10-16T19:28:11+00:00 — electron_control

Added bounded independent PTY input writer and real slow-reader regression; root integrates actor/direct-input and priority Stop wiring.

- Files: crates/session-host/src/input.rs, crates/session-host/tests/slow_input.rs
- Verification: cargo test -p pigade-session-host --test slow_input: 1 passed (unread raw slave, queue saturation, same-PID resize and Stop). cargo test -p pigade-session-host input::tests --lib: 2 passed (bounded FIFO admission and async error latch). Owned rustfmt --check and git diff --check passed. Package clippy found existing concurrent checkpoint-path collapsible_if in lib.rs:210; root notified; no lint violation reported in owned files.
- Risks / next: Root owns fresh full-suite/clippy after integration fix. Worker uses one blocking thread per session, eight pending16KiB chunks plus one in flight; no shared-fd nonblocking flag changes. Final clone drop ends idle worker; killing final slave holder unblocks a pending OS write. Drop stale Session input clones when generation exits.

## 2026-10-16T19:29:24+00:00 — storage

Implemented bounded authenticated IPC framing frontend so an incomplete client no longer occupies the single Host/SQLite writer loop.

- Files: crates/session-host/src/ipc_server.rs
- Verification: cargo test -p pigade-session-host --lib ipc_server:3/3 passed; cargo clippy -p pigade-session-host --lib -- -D warnings passed. All-targets clippy was blocked by concurrent tests/slow_input.rs115 collapsible_if and reported to root. Fresh recovery process suite after root integration is running.
- Risks / next: Root owns serve integration and slow-client process test. Frontend bounds16 workers,16 jobs,1 response per worker,2s I/O idle timeout and15s reply timeout; command outcome on reply loss is explicit unknown, never automatically retried. A trickling peer can occupy one bounded worker beyond2s because timeout is socket inactivity, not total frame deadline.

## 2026-10-16T19:29:56+00:00 — storage

Fresh native process recovery suite remains green after bounded IPC/root checkpoint integration.

- Files: crates/session-host/src/ipc_server.rs, crates/session-host/tests/recovery.rs
- Verification: cargo test -p pigade-session-host --test recovery:4/4 passed in10.44s after current integration; IPC unit tests3/3 and lib clippy -D warnings passed. Existing source-scoped git diff --check passed.
- Risks / next: Root owns full workspace validation and process slow-client regression; vendor generated target and Cargo.lock must remain uncommitted. Stage0 platform,large-history,VM reboot and24h soak gates remain unverified.

## 2026-10-16T19:43:35+00:00 — root

Integrated complete parser/Term checkpoint experiment, durable publication/tail recovery and independent safety review fixes; enforced version/worklog pre-commit hooks.

- Files: crates/terminal, crates/session-host, crates/storage/src/journal.rs, vendor/vte, Cargo.toml, .githooks, scripts, docs/adr, README.md, AGENTS.md
- Verification: Fresh workspace:32 tests passed (7 host unit,4 recovery,1 saturatedinput,10 storage,10 terminal), including100 reconnects and1000 resizes; release binaries/examples built. VTE malformed UTF8/Params regressions2 pass. Repeat after final pooled-core integration.
- Risks / next: State.2 pooled checkpoint final review, fair measurements, native GPUI canvas spike; stage0 gate remains open (coldhistory,VM,24h,platform,AIresume).

## 2026-10-16T19:43:40+00:00 — recovery_review

Read-only checkpoint review: reproduced six correctness/resource/fallback defects, reported root causes, and independently reverified exact repairs in state.2; recorded reproducible probes and remaining acceptance gaps.

- Files: docs/reports/checkpoint-review.md
- Verification: Fresh terminal10/10; hostcheckpoint2/2; VTEcheckpoint2/2; Alacrittycheckpoint8/8 with explicitlocalVTEpatch; terminal+hostlib clippy-Dwarnings pass; diffcheckpass. Probes: OSC8snapshot137827052->87259bytes, frame61871281->234461bytes; malformed5mrows decodepeak171336->13892KiB; UTF8/Params invalid imports nowreject; oldercp restoredseq1. Initial vendorcommand withoutpatch failedE0599, correctedcommand passed.
- Risks / next: Root owns integration and fullworkspacechecks/commits. No global <=700MiB decoder/import proof, coldhistory4x1GiB/100kresizes, VMreboot,24hsoak, provider/platform/renderer acceptance. No stack/MVPapproval.

## 2026-10-16T19:45:33+00:00 — storage

Replaced experimental checkpoint wire with schema2 shared extra/hyperlink pools and allocation-aware seeded field decoding; fixed repeated-URI snapshot amplification and oversized row-vector admission without Unicode truncation.

- Files: vendor/alacritty_terminal/src/grid/checkpoint.rs, vendor/alacritty_terminal/src/term/checkpoint.rs, vendor/alacritty_terminal/src/term/checkpoint/bounded.rs, vendor/alacritty_terminal/src/term/checkpoint/pools.rs, vendor/alacritty_terminal/src/term/cell.rs, vendor/alacritty_terminal/Cargo.toml, vendor/alacritty_terminal/PIGADE_PATCH.md
- Verification: Vendored144 unit tests passed including45capture corpus at135 cuts; latest explicit-ID content interning regression passed;10 composed terminal tests passed.37,001-byte fixture produced75,149-byte checkpoint at40x12 with canonical re-export/continuation;4097 combining marks roundtrip.5m rowhint rejected before firstelement; malicious Postcard header rejected withoutbody. No-serde check passed. Vendor clippy passed with precisely3 documented upstream question_mark warnings forced visible.
- Risks / next: Root owns final integration/profile bump/export validate and bounded128MiB encoder. Shared pergrid/collection bounds do not prove aggregate process heapbudget or bound live combining accumulation. Coldhistory,globaldecodedallocationbudget,VM reboot,24h soak and product gates remain open; retain rawfallback. Source API TermCheckpointV1 remains but embedded schema/core is2/state.2; oldexperimentalbytes intentionally incompatible.

## 2026-10-16T19:50:21+00:00 — electron_control

Drafted Linux-only real PTY stress harness; stopped all stress/test work immediately for root display incident response. No display settings or monitor modes changed by this lane.

- Files: scripts/stress-pty.py
- Verification: Python syntax check passed. Short2s private btrfs smoke failed ACK timeout (possible producer GO/PING startup race); no60s result or performance acceptance produced. RAII cleanup completed; read-only process inventory confirms no stress host/CLI/Electron smoke processes remain. Last owned Python CLI PIDs261641,261648,261655,261662; host PID was not recorded by draft.
- Risks / next: Do not resume testing during display incident. Root owns incident. On authorized continuation, fix GO/PING startup handling, record host PID, recheck2s harness then run60s; output native-pty-stress.json still pending.

## 2026-10-16T19:50:39+00:00 — native_canvas

Implemented bounded isolated GPUI protocol-2 four-pane cell replica spike 0.0.1-dev, immutable upstream bootstrap, actual Wayland UI kill/reopen smoke and documented failed keyboard harness; stopped desktop automation on lead emergency.

- Files: prototypes/native-canvas
- Verification: Owned debug build, fmt, four unit tests and strict all-target clippy passed; unmodified upstream GPUI retains two future float warnings. Actual four-Bash Wayland UI kill/reopen preserved session/generation/PIDs; own surface screenshots inspected. Keyboard wtype proof failed at 0ms and100ms/key (only initial chars); no acceptance claim. Read-only ps after emergency: no lingering owned UI/host/wtype/harness.
- Risks / next: Root owns desktop resolution investigation; no monitor/config/output/mode/scale commands issued by this lane. Native keyboard/input stall, font/device rounding, selection/clipboard, IME/accessibility, X11, performance and full stage0 gates remain open. No more desktop automation until root resolves report.

## 2026-10-16T19:53:34+00:00 — root

Contained live-desktop input incident: stopped all agents/tests, restored observed Hyprland cursor zoom1.3 to1.0, disabled unsafe native smoke and added mandatory isolation rule. Resuming local/headless work only after user instruction.

- Files: AGENTS.md, prototypes/native-canvas/smoke.py, prototypes/native-canvas/README.md, docs/reports/desktop-incident.md
- Verification: Runtime zoom readback1.0; monitor modes match pre-test manifest; configerrors empty; scoped /proc check found no owned wtype/native/host/benchmark processes. No GUI tests rerun.
- Risks / next: Implement verified isolated UI harness before further input tests. Continue Rust/GPUI core direction with cross-platform parity as required gate.

## 2026-10-16T20:02:53+00:00 — layout_model

Implemented platform-neutral tiling transactions with stable UUIDs, four-way docking, atomic cross-tab transfer, pure geometry/focus projection and validated schema-1 persistent 100-step undo/redo.

- Files: crates/layout/Cargo.toml, crates/layout/src/lib.rs, crates/layout/tests/invariants.rs, crates/layout/README.md
- Verification: cargo test -p pigade-layout:12 tests passed, including8,000 deterministic eight-pane transactions and history reopen; cargo clippy -p pigade-layout --all-targets -- -D warnings passed; cargo fmt -p pigade-layout --check passed; cargo build -p pigade-layout --release passed; git diff --check -- crates/layout passed. Headless only.
- Risks / next: Root owns host durability/catalog and UI integration. Model has no PTY effects; UI must clamp drag fractions against child minimums. Host must bound serialized bytes before decode. Platform/visual/MVP gates remain open.

## 2026-10-16T20:04:27+00:00 — native_canvas

Integrated pure mode-aware keyboard encoder into promoted native app; preserved platform text/IME commit path, sticky input errors, and shared device-rounded active-pane geometry without desktop automation.

- Files: crates/app/src/input.rs, crates/app/src/main.rs
- Verification: Standalone rustc input tests6/6 passed. Headless crates/app bootstrap test --locked11/11 passed; strict all-target clippy -D warnings passed; debug build --locked passed; rustfmt owned files and scoped diff-check passed. GPUI upstream retains two known future float warnings; no windows, focus, screenshots, keyboard injection or compositor commands run.
- Risks / next: Root owns main.rs integration with layout/catalog. Preserve PaneGeometry shared paint/PTY calculations. Runtime input/IME acceptance remains pending verified isolated compositor/VM; prior wtype failure is not attributed to internal input by these tests. Full UTF16 composition range editing/focus ownership, app-keypad/Kitty keyboard extensions, renderer glyph/pixel checks, performance and platform gates remain open.

## 2026-10-16T20:08:09+00:00 — isolated_platform_plan

Researched pinned Weston headless actual-app isolation, private socket/namespace preflight and screenshot authorization without launching UI or changing system state.

- Files: docs/research/isolated-platform.md
- Verification: Read AGENTS and incident; command/package inventory and bwrap help; official Weston15.0.1 archive SHA256 verified and pinned source/API inspected. No compositor, window, input or namespace executed; no runtime platform pass claimed.
- Risks / next: Root owns execution. Weston/screenshooter and software Vulkan ICD absent; complete namespace preflight before actual-app capture. Generic Weston input driver remains unverified.

## 2026-10-16T20:08:30+00:00 — storage

Added the product catalog to the sole SQLite writer: UUID-owned projects and named roots, workspaces with visible preparation failures, tabs, persisted selection/window, and CAS global layout history; schema v3 safely upgrades v1/v2 only after a consistent durable SQLite backup.

- Files: crates/storage/src/catalog.rs, crates/storage/src/launch.rs, crates/storage/src/lib.rs
- Verification: cargo test -p pigade-storage: 20 passed, including catalog transactions, CAS rollback, reopen persistence, v1 committed-WAL backup, v2 catalog and unknown-lock preservation with/without early layout, backup refusal and migration rollback; cargo clippy -p pigade-storage --all-targets -- -D warnings passed; cargo fmt -p pigade-storage --check passed; scoped git diff --check passed.
- Risks / next: Root integrates Git identity validation, LayoutHistory schema validation and host/UI commands. Storage stores bounded valid JSON, not pane-tree semantics; no delete API or Windows build claim. Existing launch protocol preserved. No desktop actions or long-history workload were run.

## 2026-10-16T20:09:34+00:00 — layout_model

Integrated validated durable catalog placement helpers: atomic cross-workspace transfer and history CAS, synchronized tab projections, raw placement write rejection, metadata-tab history rebasing and pre-commit JSON reader/precision preflight.

- Files: crates/session-host/src/catalog.rs, crates/layout/src/lib.rs, crates/layout/tests/invariants.rs, crates/layout/README.md
- Verification: Fresh cargo test -p pigade-layout:13 passed (supersedes prior12); cargo test -p pigade-session-host --lib catalog:6 passed, including50undo/redo across DB reopen, CAS rollback, corruption preservation, deep-tree request rejection before commit and40fractional resizes. The new fractional regression first reproduced default serde_json1ULP drift; root enabled existingfloat_roundtrip feature and fresh regression passes. Strict clippy for layout+host all-targets, owned fmt, releasebuild and scoped diffcheck all passed. No GUI actions.
- Risks / next: Root owns full integration baseline/routes/commits. History stays bounded100transactions/2MiB; serde recursion limit remains enabled and overly deep valid-model candidates are explicitly rejected before durable write. Full-history preflight cost is not a measured UX performance claim; CLI lifecycle/platform/MVP gates remain open.

## 2026-10-16T20:12:44+00:00 — isolated_platform_plan

Prepared local signed Weston15.0.1-3/screenshooter and Mesa26.2.1 Lavapipe SDK with reproducible bounded fetch/signature verification script; no display execution.

- Files: scripts/prepare-platform-sdk.py, docs/research/isolated-platform.md
- Verification: Both official Arch package signatures and pinned SHA256 passed; five required ELF ldd closures have no unresolved libraries after private LD path; script execution, py_compile and scoped diff-check passed. Approximately3.96MB downloaded/27.1MB SDK. No compositor, namespace, Vulkan init, app or input launched.
- Risks / next: Root must verify namespace/socket isolation, use manifest private environment and only then run actual-app render/capture. SDK depends on existing host ABI; no runtime acceptance yet.

## 2026-10-16T20:17:50+00:00 — native_canvas

Fixed native grid signed-descent height and two-phase cell rendering after root isolated screenshot exposed clipped Cyrillic/CJK/descenders; terminal line paint now shares exact device-rounded PTY cell height.

- Files: crates/app/src/main.rs
- Verification: Viewed only existing isolated screenshot file. Headless app test --locked13/13 passed, including signed positive/negative descent baseline containment at100/125/150/200% math and production paint-order regression for wide-spacer/next-row overwrite. Strict all-target app clippy-Dwarnings, debug build, owned rustfmt and diff-check passed. Known upstream GPUI float warnings remain. No GUI/compositor/window/input automation executed.
- Risks / next: Root owns next scripts/isolated-render.py run and actual visual comparison. Glyph fallback/ZWJ/cluster widths, dynamic colors/decorations, underline/cursor shapes, fractional-scale pixel output, accessibility and full Unicode/platform certification remain open; pure tests are not visual acceptance.

## 2026-10-16T20:17:54+00:00 — storage

Removed the checkpoint export intermediate clone of complete grid storage and inactive resize cache. Canonical active rows are now borrowed in zero-normalized ring order and mapped directly to unchanged schema-2 DTOs; Row layout and unsafe swap remain untouched.

- Files: vendor/alacritty_terminal/src/grid/storage.rs, vendor/alacritty_terminal/src/grid/checkpoint.rs, vendor/alacritty_terminal/PIGADE_PATCH.md
- Verification: Vendor lib146/146 passed, including45capture composed checkpoint corpus; borrowed rows matched complete serialized clone/truncate oracle across64ring combinations plus real cache grow/reuse/shrink/rotations/empty states; non-Clone cell borrow test passed; pigade-terminal10/10 passed; vendor no-default-features check passed; Clippy passed with only three documented unchanged upstream question_mark force-warnings; scoped diff-check passed. First new test had incorrect shrink/cache-threshold expectation, corrected and full suite rerun green. Additional upstream ref target44/45: hyperlinks expects old anonymous suffix _alacritty while existing experiment emits _pigade; documented without changing fixture or IDs.
- Risks / next: Root owns integrated baseline and memory measurements. This removes one clone but does not prove aggregate heap/cold-history budgets or approve stage0; wire/core profile unchanged. No desktop or GUI actions.

## 2026-10-16T20:19:28+00:00 — root

Removed separate Electron control application per user; promoted actual native app with reproducible pinned GPUI build; integrated durable catalog/layout transactions and complete-state recovery verifier; repaired resize/timer/idle checkpoint cadence and EOF Stop responsiveness; established verified private render test infrastructure.

- Files: Cargo.toml, crates/app, crates/session-host, README.md, SPEC.md, DESIGN.md, docs, scripts/isolated-render.py, scripts/stress-pty.py, vendor/vte/PIGADE-PATCH.md, .gitignore, CHANGELOG.md
- Verification: Fresh actual PTY recovery tests6/6 before added quiet-cadence regression; catalog/layout/storage suites verified by owning agents. App test11/11 before renderer fix then owning agent13/13. Actual private bwrap/Weston/Lavapipe render+UI kill/reopen passed without host desktop access; first screenshot exposed clipping and repair is being rechecked. Stress3s4streams integrity+full-state checkpoint-tail equality passed after fixing test producer shared O_NONBLOCK; full60s and finalbaseline pending.
- Risks / next: Root completing latest regression/fullbaseline, isolated visual recheck,60s workload and coherent integration commit. Cold archive/global heap proof, long-history/VM/24h/real AI CLI and productUI remain open; no MVP/stack acceptance.

## 2026-10-16T20:23:09+00:00 — /root/foundation_review

Read-only review reproduced v2 host catalog migration and encoded catalog response blockers

- Files: docs/reports/foundation-review.md
- Verification: Fresh host build; 33 host/storage unit tests and 8 real PTY integration tests passed; scoped clippy and Python compilation passed; two owned headless probes reproduced both HIGH findings; git diff --check passed
- Risks / next: Root coordinates explicit validated legacy history migration and precommit wire-size contract; rerun focused regressions after fixes. GUI/platform and long-history acceptance remain open.

## 2026-10-16T20:23:11+00:00 — layout_model

Fixed schema2-to3 catalog layout recovery with explicit validated legacy import: preserve all tab JSON, UUIDs, weights and focus, create no invented undo, publish a visible migration notice, and leave malformed legacy projections untouched.

- Files: crates/session-host/src/catalog.rs
- Verification: cargo test -p pigade-session-host --lib catalog:8 passed (supersedes earlier6), including actual schema2 DB without layout_revisions upgraded by MetadataStore to3 with retained2 backup, exact placement import and unchanged launch registry; malformed schema2 projection/reopen preserved logical DB unchanged with actionable error. cargo clippy -p pigade-session-host --all-targets -- -D warnings, scoped rustfmt, release build and diffcheck passed. Headless only.
- Risks / next: Root owns Host::open migration-helper integration and visible catalog_notice response plus full baseline. Deep/oversize or unknown legacy layouts intentionally remain blocked for explicit repair with original backup; no CLI operations or desktop automation performed.

## 2026-10-16T20:24:19+00:00 — storage

Fixed catalog IPC amplification before durable commit: retained the4MiBSQL source cap and added7MiBencoded JSON preflight using a fixed-size allocation-free counting writer. The shared reader rejects oversized legacy catalog state explicitly without truncation; valid newlines, Unicode and control escapes remain supported below budget.

- Files: crates/storage/src/catalog.rs
- Verification: cargo test -p pigade-storage:23/23passed; targeted sequential escaping regression measured7,082,718accepted JSON bytes versus7,476,137rejected bytes at19large control tasks, proving rollback of inserted row/project revision/selection and retention of unknown launch lock across reopen. Legacy22large-task fixture rejects reads before and after reopen with all23rows and launch lock retained. Unicode/newline roundtrip test passed. Scoped all-targetClippy-Dwarnings, fmtcheck and diffcheck passed.
- Risks / next: Root owns bounded host Response/notice envelope and integrated IPC recovery regression. No schema/API change or pagination; existing oversized catalogs remain preserved and return explicit errors pending a deliberate repair workflow. No host/UI or other storage files edited.

## 2026-10-16T20:25:48+00:00 — /root/foundation_review

Independently verified both catalog blocker fixes and reviewed bounded output coalescing

- Files: docs/reports/foundation-review.md
- Verification: Fresh host build and scoped clippy passed;40host/storage unit tests+8real PTY tests passed; dynamic nonempty v2 catalog migration preserves placement/backup and returns notice;22escaping-growth attempts now reject19-22 beforecommit and preserve readable18rowcatalog; diffcheck passed
- Risks / next: Reviewed fix slice approved with zero unresolved critical/high findings; lead continues60s/fullstate acceptance and remaining long-history/platform gates. No GUI executed.

## 2026-10-16T20:26:42+00:00 — root

Persisted user requirement that all new subagents use gpt-6-luna. Closed reviewed legacy-layout migration and encoded catalog budget defects; bounded adjacent-output coalescing preserves ordering and restores four-stream throughput. Added chunked synchronized query replies and real PTY regression.

- Files: AGENTS.md, crates/session-host, crates/storage/src/catalog.rs, benchmarks/results, scripts/isolated-render.py, vendor/alacritty_terminal/tests/ref/hyperlinks/grid.json, vendor/alacritty_terminal/PIGADE_PATCH.md
- Verification: Independent reviewer re-tested nonempty v2 downgrade/reopen + notice/backup and oversized-control-task rollback; approved slice. Coalesce2unit + real20kquery PTY tests passed.60s4x1MiB/s nowproducer-rate passes, allbyteCRC andcompletecheckpoint-tail/fullreplaymodels equal; ACKp95=67.648ms is snapshot observation, not present; sampledhostRSS30,334,976bytes. Fullfinalbaseline in progress.
- Risks / next: Commit foundation after freshbaseline. Preserve failed-before-coalesce evidence and honest latency/present/platform/long-history gaps. Allfuturechildren mustexplicitlygpt-6-luna, nofollowupoldmodel.

## 2026-10-16T20:29:07+00:00 — root

Finalized native foundation documentation, review repairs, performance evidence and isolated render artifacts; all new children pinned to user-selected gpt-6-luna.

- Files: docs/PROJECT_MAP.md, docs/AGENT_HANDOFF.md, docs/reports/verification-0.0.1-dev.md, README.md, CHANGELOG.md, crates/app/evidence, crates/app/src/main.rs, scripts/isolated-render.py
- Verification: Fresh rootworkspace72/72 tests, strictclippy, fmt and releasebuild passed. App13 tests + strictclippy/build passed; last-window-close hook compiles with strictclippy. VTE56 and Alacritty146lib+45ref passed. Actual isolated finalrender/reopen passed; latest query-burst realPTY passes. Independent review approves repairedmigration/wire/coalescing slice.60s4streamscorrectandratepasses; ACKp95=67.648ms notpresent, nofullperfgateclaim.
- Risks / next: Stage0remainsopen: implementcoldarchiveandaggregateheapbudget, then longhistory/VM/24h/platform/provider/productgates. RootpreparescoherentGitcommitnow; do notclaimMVP.

## 2026-10-16T20:29:19+00:00 — root

Marked raw terminal capture fixtures binary to preserve CR/ANSI/trailing-space bytes exactly in Git; first staging whitespace check correctly exposed captures as textual before attributes.

- Files: .gitattributes
- Verification: No fixture bytes changed; binary attributes prevent newline normalization and textual whitespace rewrites. Cached whitespace check rerunning with diagnostics captured.
- Risks / next: Commit reviewed native foundation; continue strict whitespace checking first-party sources and textual docs.
