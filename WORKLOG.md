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

## 2026-10-16T20:36:31+00:00 — /root/archive_storage

Added bounded derived cold-history archive rows with fragment CRCs, generation/profile header, event-level fsync commits, valid-prefix streaming and poison-on-uncertain-write behavior

- Files: crates/storage/src/archive.rs
- Verification: cargo test -p pigade-storage archive: 6 passed; cargo clippy -p pigade-storage --all-targets -- -D warnings passed; rustfmt --edition 2024 --check crates/storage/src/archive.rs passed; git diff --check passed. cargo fmt -p pigade-storage --check reports only root-owned crates/storage/src/lib.rs module ordering.
- Risks / next: Root integrates durable journal-event ordering and checkpoint binding; archive remains derived only, with no SQLite/index/segmentation/pruning or terminal parsing. Root should format the concurrent lib.rs module registration before package-wide fmt check.

## 2026-10-16T20:38:50+00:00 — root

Started cold-history foundation after native milestone: adopted bounded-hot/archive semantic ADR and added borrowed normal-grid retirement hooks without changing Row layout or enabling production archival.

- Files: vendor/alacritty_terminal/src/grid/mod.rs, vendor/alacritty_terminal/src/grid/resize.rs, vendor/alacritty_terminal/src/grid/checkpoint.rs, vendor/alacritty_terminal/src/grid/history_tests.rs, vendor/alacritty_terminal/src/term/mod.rs, crates/terminal/src/lib.rs, crates/storage/src/lib.rs, docs/adr/0005-bounded-hot-state-and-cold-archive.md, vendor/alacritty_terminal/PIGADE_PATCH.md
- Verification: Targeted3core retirement tests passed: chronological scroll/history-limit, width-overflow+hot equivalence and reset singleepoch. Initial newtest compile failures (Row notiterable and Cell resetdiscriminant Color) corrected in fixtures; workspacecheck passes. Composed normal/alt/inactive-resize observer equality running; fullbaseline pending. Newarchive performer explicitly gpt-6-luna, owns archive.rs.
- Risks / next: Complete committed-watermark archive-reader guard, then integrate typed row encoding/segmentation/index/versionedcheckpoint roots; no production archive or long-history acceptance claimed.

## 2026-10-16T20:39:30+00:00 — /root/archive_storage

Added receipt-bounded archive reading that verifies identity, boundary, final row ordinal, source sequence and checksum before exposing that row; enforced same source event sequence across row fragments and removed rustfmt skip compressions

- Files: crates/storage/src/archive.rs
- Verification: cargo test -p pigade-storage archive: 7 passed; cargo clippy -p pigade-storage --all-targets -- -D warnings passed; cargo fmt -p pigade-storage and cargo fmt -p pigade-storage --check passed; git diff --check passed.
- Risks / next: Root must persist/bind only fsync ArchiveReceipt offsets for checkpoint readers. ArchiveReader::open and scan_valid_prefix are diagnostic CRC-prefix paths, not durability proof; archive remains derived and unindexed.

## 2026-10-16T20:47:45+00:00 — root

Integrated Luna archive primitive and borrowed normal-history hooks; repaired truncated-file reread handling and clarified complete-row versus durable receipt API names; retained resize false-positive reproducers as permanent tests.

- Files: crates/storage/src/archive.rs, crates/storage/src/lib.rs, crates/terminal/src/lib.rs, vendor/alacritty_terminal/src/grid, vendor/alacritty_terminal/src/term/mod.rs, vendor/alacritty_terminal/PIGADE_PATCH.md, docs/adr/0005-bounded-hot-state-and-cold-archive.md, docs/reports/cold-history-foundation.md, docs/AGENT_HANDOFF.md, docs/PROJECT_MAP.md, CHANGELOG.md
- Verification: Fresh workspace81/81 tests, strict workspace Clippy, fmt and release build passed. Core151/151 lib and45/45 reference tests passed; no-default-features check passed. Crate-local Clippy passes with only three explicitly documented unchanged upstream question_mark warnings. Five Grid retirement regressions and complete composed hot/parser byte equality pass. Git diff --check and version alignment pass.
- Risks / next: Archive remains derived/unindexed and unattached to host: typed cells/extras/links and palette provenance, continuation state, segments/lazy index, durable roots and archive+hot replay/fault integration next. Aggregate memory,4x1GiB/100kresizes,VM,24h,platform and fullMVP gates remain open. All new children used user-required gpt-6-luna; no active desktop automation.

## 2026-10-16T20:50:17+00:00 — archive_row_codec

Added bounded typed retired-row encoding and decoding with row-local hyperlink pooling

- Files: crates/terminal/src/history.rs
- Verification: cargo fmt --all --check; cargo test -p pigade-terminal history (4 passed); git diff --check
- Risks / next: Lead to review API and integrate host/archive caller; row codec does not provide persistence or transactional rollback itself.

## 2026-10-16T20:51:06+00:00 — /root/archive_range

Added bounded indexed archive row reads and writer row locations

- Files: crates/storage/src/archive.rs
- Verification: cargo fmt --all --check; cargo test -p pigade-storage archive --lib (9 passed); cargo clippy -p pigade-storage --all-targets -- -D warnings (passed)
- Risks / next: Lead to integrate archive::ArchiveRowLocation with durable append-only index/root; location itself does not prove fsync

## 2026-10-16T20:56:44+00:00 — archive_row_codec

Completed named-color coverage and widened per-cell zero-width wire count to u32

- Files: crates/terminal/src/history.rs
- Verification: cargo fmt --all; cargo test -p pigade-terminal history (7 passed); cargo clippy -p pigade-terminal --all-targets -- -D warnings; git diff --check
- Risks / next: Lead can review final row codec and complete archive integration.

## 2026-10-16T21:05:27+00:00 — root

Integrated typed cold rows with per-generation segments, fixed-slot committed index, PIGCP002 roots and normal-grid palette provenance. Recovery keeps immutable base plus private derived tail; full oracle compares all archive records, not only CRCs. Added real PTY cold-history kill/reopen and archive/index fault regressions. Implemented bounded shared checkpoint scheduler and unchanged-file semantic validation cache after normal-history throughput miss.

- Files: crates/session-host/src/history.rs, crates/session-host/src/history/tests.rs, crates/session-host/src/checkpoint.rs, crates/session-host/src/lib.rs, crates/session-host/tests/recovery.rs, crates/session-host/Cargo.toml, crates/terminal/src/lib.rs, vendor/alacritty_terminal/src/grid/mod.rs, vendor/alacritty_terminal/src/grid/row.rs, vendor/alacritty_terminal/src/term/mod.rs, scripts/stress-pty.py, benchmarks/results/native-cold-history-5s.json
- Verification: Workspace94 tests passed before scheduler cache; core151 passed. Real12500-line Python PTY + hostkill/reopen and every-record replay equality passed. Four history integration tests pass incllegacy raw reconstruction and raw/data-without-index recovery. StrictworkspaceClippy passed before scheduler/cache; cache checkpoint2tests and releasebuild pass, freshfullbaseline pending. First5s normal4stream diagnostic allbytes/allcoldrecords correct but producer14.63-15.24s, peak340471808bytes; concurrentrepositoryverification explicitly marked.
- Risks / next: Repeat normal-history measurement without verification overlap, verify cache invalidation/retention and fullbaseline, then finish lazy history API, fault boundaries and resource budgets. Stage0/MVP gates open; no active-desktop automation. All delegated performers/reviewers explicitlygpt-6-luna.

## 2026-10-16T21:05:38+00:00 — /root/retention_regression

Added checkpoint retention regression coverage for valid-file pruning, cache invalidation, identity mismatch and interrupted temporary files.

- Files: crates/session-host/tests/checkpoint_retention.rs, WORKLOG.md
- Verification: cargo fmt --all; cargo test -p pigade-session-host --test checkpoint_retention (4 passed); cargo clippy -p pigade-session-host --test checkpoint_retention -- -D warnings (passed)
- Risks / next: Lead integrates these tests with the checkpoint cache/pruning implementation and performs final verification.

## 2026-10-16T21:07:45+00:00 — /root/journal_read_metrics

Added actual journal read-byte accounting to JournalReader, including header, partial reads, and checkpoint tail reads

- Files: crates/storage/src/journal.rs
- Verification: cargo test -p pigade-storage journal (8 passed); cargo clippy -p pigade-storage --all-targets -- -D warnings (passed); cargo fmt --all --check (passed)
- Risks / next: Lead integrates API with host metrics; no wire format or writer behavior changes

## 2026-10-16T21:09:09+00:00 — archive_row_codec

Replaced PIGROW01 with PIGROW02 per-cell attribute and UTF-8 scalar deltas

- Files: crates/terminal/src/history.rs
- Verification: cargo fmt --all --check; cargo test -p pigade-terminal history (9 passed); cargo clippy -p pigade-terminal --all-targets -- -D warnings; git diff --check; 120 repeated ASCII cells encode to 135 bytes (PIGROW01 layout 2294 bytes)
- Risks / next: Host integration moves the archive profile to PIGROW02; incompatible old derived rows must be rejected and rebuilt from raw journal.

## 2026-10-16T21:11:03+00:00 — /root/archive_crash_tests

Added bounded real-child SIGKILL tests for four archive recovery cut points; recovery compares terminal/parser state and every cold record while checking source artifacts remain unchanged

- Files: crates/session-host/src/history/crash_tests.rs, WORKLOG.md
- Verification: cargo test -p pigade-session-host --lib history::crash_tests -- --nocapture (5 passed); cargo clippy -p pigade-session-host --lib -- -D warnings (passed); cargo fmt --all --check (passed); git diff --check (passed)
- Risks / next: These are current-OS process-kill tests and do not establish power-loss, VM reboot, or storage-cache-loss behavior; root owns module registration and final integration verification.

## 2026-10-16T21:12:54+00:00 — /root/memory_harness

Added a headless release-host memory diagnostic for up to 32 PTY sessions, defaulting to 12 sessions with 10,000 hot history rows each; captures host-only RSS/CPU by phase and verifies recovery after the measured hold.

- Files: scripts/measure-memory.py, WORKLOG.md
- Verification: python3 -m py_compile scripts/measure-memory.py; python3 scripts/measure-memory.py --help; git diff --check -- scripts/measure-memory.py (all passed; workload intentionally not run while root measures normal throughput)
- Risks / next: Root to review and run on a durable filesystem after throughput measurement; host-only 700 MiB result is necessary but does not prove total PigADE/GPU acceptance.

## 2026-10-16T21:22:24+00:00 — vm_reboot_prepare

Add pinned, signed local Arch QEMU SDK preparer

- Files: scripts/prepare-vm-sdk.py
- Verification: PYTHONPYCACHEPREFIX=/tmp/pigade-vm-sdk-pycache python3 -m py_compile scripts/prepare-vm-sdk.py (passed); python3 scripts/prepare-vm-sdk.py --help (passed); python3 scripts/prepare-vm-sdk.py --prepare (passed: 13 packages, 14057837 bytes, QEMU 11.1.2, mcopy 4.0.49, unresolved dependencies empty)
- Risks / next: Root owns VM launch/isolation and actual guest reboot test; SDK preparation is not reboot or AI conversation-resume proof.

## 2026-10-16T21:40:56+00:00 — /root/vm_reboot_harness

Added an explicit prepare/run Alpine QEMU reboot harness with signed image verification, private staged runtime, synthetic four-session scenario, and recovery evidence capture.

- Files: scripts/vm-reboot.py, WORKLOG.md
- Verification: python3 -m py_compile scripts/vm-reboot.py; python3 scripts/vm-reboot.py --help; python3 scripts/vm-reboot.py --prepare --directory /home/afterburner/.local/state/pigade/vm-reboot --sdk-manifest /tmp/pigade-vm-sdk/evidence/manifest.json; embedded guest Python compile, cloud-config parse, mtools seed listing, and git diff --check passed. No QEMU VM was started.
- Risks / next: Lead reviews and runs --run on the verified durable filesystem; confirm the Limine cloud image boots with explicit SeaBIOS and assess real two-boot recovery output.

## 2026-10-16T21:43:43+00:00 — /root/cold_history_docs

Documented host-integrated cold history and current verification limits

- Files: README.md, CHANGELOG.md, docs/AGENT_HANDOFF.md, docs/PROJECT_MAP.md, docs/reports/cold-history-integration.md
- Verification: git diff --check and report-link validation pending
- Risks / next: Root integrates shared implementation, performs final verification and commits; long-history throughput, VM reboot, platform and MVP gates remain open

## 2026-10-16T21:43:54+00:00 — /root/cold_history_docs

Completed validation of the cold-history documentation update

- Files: README.md, CHANGELOG.md, docs/AGENT_HANDOFF.md, docs/PROJECT_MAP.md, docs/reports/cold-history-integration.md
- Verification: git diff --check -- README.md CHANGELOG.md docs/AGENT_HANDOFF.md docs/PROJECT_MAP.md docs/reports/cold-history-integration.md WORKLOG.md passed; relative Markdown links in all five edited documentation files exist; no tests run for this documentation-only change
- Risks / next: Root owns integrated verification and commit; 60-second rate target, 1-GiB history, VM reboot, 24-hour soak, platform and MVP gates remain open

## 2026-10-16T21:44:00+00:00 — /root/vm_reboot_harness

Corrected guest boot sequencing and shell command construction, added guest-owned CLI invocation evidence across recovery retries, and moved QMP to a short-lived private socket directory.

- Files: scripts/vm-reboot.py, WORKLOG.md
- Verification: python3 -m py_compile scripts/vm-reboot.py; --help; constructed all four embedded first-boot shell commands and passed /bin/sh -n; confirmed first_boot calls start_host and markers are written by launched CLI processes; verified QMP path under AF_UNIX limit; --prepare succeeded in /home/afterburner/.local/state/pigade/vm-reboot/run-20261016T214356Z-8e57d55c. No QEMU VM started.
- Risks / next: Lead may review and run --run; real guest boot and reboot remain unverified.

## 2026-10-16T21:44:33+00:00 — /root/cold_history_docs

Added actual raw journal-read counts to cold-history evidence and handoff

- Files: docs/reports/cold-history-integration.md, docs/AGENT_HANDOFF.md, docs/PROJECT_MAP.md
- Verification: git diff --check on owned docs and WORKLOG passed; relative link validation passed; 60s artifact confirms full replay 63,105,903–63,116,991 bytes/session and checkpoint tail 356 bytes/session
- Risks / next: Root integrates shared implementation and owns final verification/commit; 1-GiB workload, throughput target, VM reboot, soak, platform and product gates remain open

## 2026-10-16T21:47:05+00:00 — /root/long_history_corpus

Added a streaming production-model long-history corpus generator and durable-filesystem runner with frozen-binary hash, checkpoint cadence, boundary-state scenarios, and optional full recovery oracle.

- Files: crates/session-host/examples/history_corpus.rs, scripts/long-history.py
- Verification: rustfmt --edition 2024 crates/session-host/examples/history_corpus.rs; PYTHONPYCACHEPREFIX=/tmp/pigade-long-history-pycache python3 -m py_compile scripts/long-history.py; python3 scripts/long-history.py --help; cargo clippy --locked -p pigade-session-host --example history_corpus -- -D warnings; cargo build --release --locked -p pigade-session-host --example history_corpus; one-session 1 MiB/100-resize --verify smoke on btrfs passed: 3199 cold records, exact output/resize counts, full state/frame/cold-record equality, checkpoint-tail read 404/1058242 journal bytes; git diff --check passed.
- Risks / next: Root integrates and owns the 4-session 1 GiB/100000-resize corpus run and full SPEC budget assessment. Smoke is functionality evidence only; runner checks available space but configures no filesystem quota, logical 24 h is not elapsed soak, and full verification remains optional.

## 2026-10-16T21:47:41+00:00 — /root

Integrate typed cold history, indexed checkpoint roots, durable private replay tails, launch socket repair and measured host resource diagnostics

- Files: crates/session-host/src/lib.rs, crates/session-host/src/history.rs, crates/session-host/src/checkpoint.rs, crates/session-host/src/launch_gate.rs, crates/session-host/src/metrics.rs, crates/terminal/src/history.rs, crates/storage/src/archive.rs, crates/storage/src/journal.rs, scripts/stress-pty.py, scripts/measure-memory.py, scripts/vm-reboot.py, vendor/alacritty_terminal/PIGADE_PATCH.md, docs/adr/0006-generation-archive-index-and-checkpoint-root.md
- Verification: Workspace111 tests pass; fmt/strictworkspaceClippy/versioncheck pass; vendor151lib+45reference cases and no-default-features check pass. 12realPTY host-only peak447610880B with10000hotrows/session allrecoveryequal. Instrumented60s normal workload allbytes+hot+coldstateequal but rateFAILED86.05..86.36s: stagecounters identify journal+archivefsync dominant (about60s/session), preserved artifact. IsolatedVM first launch failed readonlyIDEseed; media=cdrom repair bootsLinuxroot, rebootnotyetverified.
- Risks / next: Commit coherent archive integration, then improve bounded output batching without durability loss; finish isolatedVM reboot,4x1GiB100kresizes,24hsoak and remaining stage0/platform/MVP gates.

## 2026-10-16T21:48:41+00:00 — /root/long_history_corpus

Follow-up review: freeze the corpus executable inside each run before hashing and execution, strengthen the free-space preflight, and narrow the logical-hours statement to manifest metadata without event timestamps.

- Files: crates/session-host/examples/history_corpus.rs, scripts/long-history.py
- Verification: rustfmt --edition 2024 --check crates/session-host/examples/history_corpus.rs; PYTHONPYCACHEPREFIX=/tmp/pigade-long-history-pycache python3 -m py_compile scripts/long-history.py; python3 scripts/long-history.py --help; freeze helper copied the current example into a private mode-0700 file and its recorded SHA-256 matched the copy; no corpus workload rerun.
- Risks / next: Root owns integration and the full corpus run. The earlier small-run recovery evidence applies to unchanged corpus event-generation/recovery logic; this follow-up changed the manifest label and runner binary/space safeguards only. The preflight estimate is not an enforced filesystem quota.

## 2026-10-16T21:50:33+00:00 — /root/real_time_soak

Added retained-state real-time four-PTY soak runner with durable JSONL sampling, generation-scoped IPC exercises and full output CRC verification

- Files: scripts/soak-pty.py, WORKLOG.md
- Verification: python3 -m py_compile scripts/soak-pty.py; python3 scripts/soak-pty.py --help; embedded producer compile; git diff --check -- scripts/soak-pty.py (passed); runtime soak intentionally not started per lead instruction
- Risks / next: Lead reviews and runs 10-second functionality smoke after main performance benchmark; no 24-hour soak executed, so REC-08 remains open

## 2026-10-16T21:51:36+00:00 — /root/conditional_poll

Added bounded conditional host-view polling metadata and focused helper tests; selects four sessions by stable UUID order and only clones views whose generation, sequence, state, PID, or storage error differs from the client watermark.

- Files: crates/session-host/src/poll.rs, WORKLOG.md
- Verification: rustfmt --edition 2024 crates/session-host/src/poll.rs and git diff --check -- crates/session-host/src/poll.rs passed; Rust compilation/tests not run because lead requested the current 60s performance/resource window remain undisturbed.
- Risks / next: Root registers and integrates the new poll module and IPC command, then owns compilation, focused tests, and integrated verification.

## 2026-10-16T21:52:26+00:00 — /root/conditional_poll

Corrected polling identity to use exact session-generation pairs, allowing multiple retained generations for one session; removed PollResult PartialEq, which View does not implement, and added a historical-generation regression test.

- Files: crates/session-host/src/poll.rs, WORKLOG.md
- Verification: rustfmt --edition 2024 crates/session-host/src/poll.rs and git diff --check -- crates/session-host/src/poll.rs passed; compilation/tests intentionally left to root during integration.
- Risks / next: Root integrates and owns compilation, tests, and final verification.

## 2026-10-16T21:52:36+00:00 — /root/real_time_soak

Hardened real-time PTY soak verification, bounded evidence, producer handshakes and frozen executable durability

- Files: scripts/soak-pty.py, WORKLOG.md
- Verification: python3 -m py_compile scripts/soak-pty.py; python3 scripts/soak-pty.py --help; embedded producer compile; git diff --check -- scripts/soak-pty.py WORKLOG.md (passed); runtime workload not started per lead direction
- Risks / next: Lead to run only after requested VM run and confirm runtime-smoke window; 24-hour soak remains unexecuted and REC-08 open

## 2026-10-16T21:56:19+00:00 — /root/real_time_soak

Fixed stale producer_started runtime NameError and verified four-session 10-second soak with full recovery oracle

- Files: scripts/soak-pty.py, WORKLOG.md
- Verification: Initial root-authorized run failed before measurement with NameError: producer_started; its artifact was retained at /home/afterburner/.local/state/pigade/soak/soak-20261016T215447Z-d3ff5eca. Removed stale event reference and stale summary key; python3 -m py_compile scripts/soak-pty.py; 10-second run with --sample-seconds 2 --verify completed in 10.036s; all four journal CRCs and full recovery (checkpoint-tail, complete model/frame/cold history) passed; each session acknowledged input and processed 3 resizes; 5 samples/session recorded checkpoint age and sequence lag; output artifacts retained at /home/afterburner/.local/state/pigade/soak/soak-20261016T215546Z-596983b0; git diff --check passed
- Risks / next: Root reviews result; no 24-hour soak was run and REC-08 remains open

## 2026-10-16T22:03:19+00:00 — /root/cold_history_docs

Updated cold-history and VM recovery documentation with current 250us collector status and verified synthetic cold reboot

- Files: README.md, CHANGELOG.md, docs/AGENT_HANDOFF.md, docs/PROJECT_MAP.md, docs/reports/cold-history-integration.md, docs/reports/vm-reboot.md
- Verification: git diff --check on owned docs and WORKLOG passed; relative Markdown links resolved; Python manifest assertions passed for vm-reboot-cold-history.json (two distinct boot IDs, four sessions with 2,467 cold records and complete hot/frame/cold equality, 356-byte checkpoint tails, unchanged invocation markers); no tests run by this documentation agent
- Risks / next: Current 250us output collector still needs fresh 5s/60s throughput artifacts; final app all-target Clippy and isolated render/reopen remain pending; 1-GiB corpus, 24h soak, provider/layout acceptance and MVP gates remain open

## 2026-10-16T22:04:34+00:00 — /root/cold_history_docs

Updated handoff, project map, changelog, README and cold-history report with completed workspace/app verification and isolated UI render/reopen evidence

- Files: README.md, CHANGELOG.md, docs/AGENT_HANDOFF.md, docs/PROJECT_MAP.md, docs/reports/cold-history-integration.md
- Verification: git diff --check passed; conditional-poll evidence result.json reports render_and_reopen_pass; relative Markdown links checked; no code tests run by documentation agent
- Risks / next: Await fresh 250us 5s/60s benchmark result; 1GiB corpus, 24h soak, provider/layout and platform gates remain open

## 2026-10-16T22:08:00+00:00 — /root/live_memory_review

Bounded width-shrink reflow with rolling retained rows and Unicode/reference regression

- Files: vendor/alacritty_terminal/src/grid/resize.rs, vendor/alacritty_terminal/src/grid/row.rs, vendor/alacritty_terminal/src/grid/history_tests.rs
- Verification: Not run: root requested no compile/tests while 60-second benchmark is active; diff inspection only
- Risks / next: Root review, then run focused vendor regression and verify formatting after benchmark window; combining-mark live growth/global admission remain open

## 2026-10-16T22:11:09+00:00 — /root/archive_logical_offsets

Track archive append offsets in memory to avoid per-fragment seek syscalls while preserving poisoning and commit boundaries

- Files: crates/storage/src/archive.rs, WORKLOG.md
- Verification: cargo test -p pigade-storage archive::tests (10 passed); cargo clippy -p pigade-storage --all-targets -- -D warnings (passed)
- Risks / next: No known risks; lead owns integration and final workspace verification

## 2026-10-16T22:15:38+00:00 — /root/long_history_corpus

Follow-up review: colocated all generated session artifacts under the single corpus recovery root and added a generation/profile-bound, versioned JSONL logical event timeline with a per-event SHA-256.

- Files: crates/session-host/examples/history_corpus.rs, scripts/long-history.py
- Verification: rustfmt; strict targeted example Clippy; locked release example build; Python compile; git diff --check; one agreed 4-session 1 MiB/100-resize --verify smoke on btrfs passed. All four journals, checkpoints, and history roots were colocated. Every timeline had 215 monotonic sequence/kind entries from 0 to 86400000000000 ns, and SyncTimeout was exactly 200000000 ns after sync begin; stored SHA-256 matched. Per session: requested 1048576 output bytes, 100 resizes, 3199 cold records, and complete state/frame/all-record equality; checkpoint-tail read 404/1058242 journal bytes.
- Risks / next: Root owns the 4-session 1 GiB/100000-resize corpus and SPEC budget/recovery assessment. This 1 MiB smoke is functionality evidence only; logical timestamps are synthetic and no real 24 h soak or filesystem quota is established.

## 2026-10-16T22:20:56+00:00 — /root/long_history_corpus

Added four distinct final checkpoint boundary recipes, separate ending byte/event metrics, and per-sequence monotonic logical timelines with exact 200 ms synchronized-output timeout pairs

- Files: crates/session-host/examples/history_corpus.rs, scripts/long-history.py
- Verification: cargo fmt --all --check; cargo clippy --locked -p pigade-session-host --example history_corpus -- -D warnings; python3 -m py_compile scripts/long-history.py; python3 scripts/long-history.py --directory /home/afterburner/Projects/PigADE/benchmarks/results/long-history-smoke-parent --sessions 4 --output-bytes 1048576 --resize-events 100 --logical-hours 24 --verify (passed: 4/4 checkpoint-tail; complete state, frame, and cold-history equality; timeline SHA-256 and endpoints matched; each sync timeout exactly 200000000 ns after its begin)
- Risks / next: Root owns large 4-session corpus and performance/SPEC assessment; smoke is functionality evidence only, logical schedule is synthetic, no 24-hour elapsed soak or filesystem quota

## 2026-10-16T22:23:35+00:00 — /root/cold_history_docs

Reconciled current cold-history performance, idle and long-history smoke evidence across handoff and project docs

- Files: README.md, CHANGELOG.md, docs/AGENT_HANDOFF.md, docs/PROJECT_MAP.md, docs/reports/cold-history-integration.md
- Verification: Parsed new benchmark and corpus JSON artifacts; verified producer/body/exit/lateness ranges, full recovery flags and four-session smoke parameters; relative Markdown links and git diff --check passed; no builds/tests run by documentation agent
- Risks / next: Lead to complete fresh full workspace rerun; 1GiB/100000-resize corpus, real 24h soak, input latency, global combining heap bound, provider/layout/platform and MVP gates remain open

## 2026-10-16T22:25:04+00:00 — /root/gpui_accessibility_upgrade

Recorded decision to upgrade the existing GPUI application to an upstream revision with Linux accessibility integration

- Files: docs/adr/0007-gpui-accessibility-platform-upgrade.md, WORKLOG.md
- Verification: git diff --check -- docs/adr/0007-gpui-accessibility-platform-upgrade.md passed; no application build or platform test run because this initial scope is documentation only and app edits require lead go-ahead
- Risks / next: Await lead review/go before changing app manifests, bootstrap, lockfile or constructor; AT-SPI/Orca runtime acceptance remains open and requires isolated platform environment

## 2026-10-16T22:25:12+00:00 — /root/long_history_corpus

Extended the recovery oracle to prove full replay and checkpoint-tail state/frame/history equality against the originating live Terminal and History

- Files: crates/session-host/src/lib.rs, crates/session-host/src/history/tests.rs, crates/session-host/examples/history_corpus.rs
- Verification: cargo fmt --all --check; cargo test --locked -p pigade-session-host cold_records_and_hot_parser_match_full_replay_checkpoint_and_live -- --nocapture (1 passed); cargo clippy --locked -p pigade-session-host --all-targets -- -D warnings; python3 scripts/long-history.py --directory /home/afterburner/.local/state/pigade/long-history-smoke --sessions 4 --output-bytes 1048576 --resize-events 100 --logical-hours 24 --verify (passed: all 4 checkpoint-tail; full/fast/live state, frame, and cold-history equality true)
- Risks / next: Root owns larger SPEC corpus and integration/commit; this small smoke verifies functionality only, not long-duration performance.

## 2026-10-16T22:25:52+00:00 — /root/gpui_accessibility_upgrade

Corrected GPUI archive SHA-256 from cached archive verification and added pinned upstream source/license references

- Files: docs/adr/0007-gpui-accessibility-platform-upgrade.md, WORKLOG.md
- Verification: sha256sum /home/afterburner/.cache/pigade/gpui/322cd509dcccaee1f6bc44725d270598ae8171ad.tar.gz returned 27e6a724515cb6e6fa8a456098d075bfa2a50b4f6145491105555e56de2d256d; git diff --check passed; no app build or platform test run
- Risks / next: App migration remains held until lead authorizes after baseline commit; recheck resolved component license graph during integration

## 2026-10-16T22:26:43+00:00 — /root

Integrated conditional cell polling, bounded durable output/index batching, archive logical offsets and width-reflow scratch; verified cold-history reboot and started full corpus/real-time soak

- Files: crates/app/src/main.rs, crates/app/src/protocol.rs, crates/session-host/src/lib.rs, crates/session-host/src/poll.rs, crates/session-host/src/history.rs, crates/session-host/src/history/tests.rs, crates/session-host/tests/recovery.rs, crates/storage/src/archive.rs, vendor/alacritty_terminal/src/grid/resize.rs, vendor/alacritty_terminal/src/grid/row.rs, vendor/alacritty_terminal/src/grid/history_tests.rs, scripts/vm-reboot.py, scripts/stress-pty.py, scripts/long-history.py, scripts/soak-pty.py, docs/AGENT_HANDOFF.md, docs/reports/cold-history-integration.md, benchmarks/results
- Verification: Fresh workspace fmt, strict all-target Clippy, 120 tests and release bins/examples pass. Vendor 152 library +45 reference tests and no-default-features check pass with documented patched VTE config (initial command omitted patch and failed; corrected). App17 tests/strictClippy/build and private software-render UI kill/reopen passed earlier in this integration. Actual two-boot VM four cold histories matched. Normal-history 60s producer bodies59.9845s all539088coldrecords exact; up to7.5s schedulelateness, exitdeadlinefail. TUI60s116ACK p95=42.91ms p99=59.07ms CRC/recoverypass; notUIpresent. Headlessidle8PTY60s0CPUticks. Full4x1GiB100kresizes and24hsoak now running, notpassed. Moved full smoke runtime outsideGit, retained evidence only. LinkerSIGBUS investigated as/tmpEDQUOT; task-owned cache movedhome, normalbuildpassed.
- Risks / next: Continue actual corpus/soak, same-app GPUI accessibility upgrade perADR0007; global live-extra admission, product history UI/quota, platform/CLI/MVP gates remain open. Never automate active desktop.

## 2026-10-16T22:31:51+00:00 — /root/replay_cache_cleanup

Added locked, profile-bound private replay-cache ownership and bounded fail-closed orphan collection with same-parent no-replace quarantine; tests cover live lease, SIGKILL orphan, invalid/foreign markers, symlinks and unsafe parent

- Files: crates/session-host/src/replay_cache.rs, WORKLOG.md
- Verification: rustfmt --edition 2024 crates/session-host/src/replay_cache.rs; cargo test --locked -p pigade-session-host replay_cache::tests -- --nocapture --test-threads=1 (5 passed; compile emitted expected dead_code warnings before History/Host integration)
- Risks / next: Root integrates ReplayCache into History and invokes cleanup under host lock; root owns integrated verification. Cleanup is bounded by 256 parent entries and 512 cache files per pass and does not establish any quota guarantee.

## 2026-10-16T22:33:13+00:00 — /root/gpui_accessibility_upgrade

Upgraded the existing GPUI application to the pinned AccessKit-capable upstream revision and migrated platform API call sites

- Files: crates/app/Cargo.toml, crates/app/Cargo.lock, crates/app/bootstrap.sh, crates/app/README.md, crates/app/src/main.rs, WORKLOG.md
- Verification: GPUI archive SHA-256 verified; bootstrap fmt -- --check passed; bootstrap build --locked passed; bootstrap test --locked passed (17 tests); strict bootstrap clippy --locked --all-targets -- -D warnings passed; python3 scripts/check-version.py and git diff --check passed; no window or desktop input used
- Risks / next: AT-SPI/Orca runtime inspection on isolated Wayland and X11 remains unverified; broader transitive license audit remains incomplete; lead owns integrated verification and commit

## 2026-10-16T22:35:57+00:00 — /root/gpui_accessibility_upgrade

Inventoried locked Linux GPUI app dependency license metadata and documented GPL/LGPL expression review

- Files: docs/research/gpui-322cd509-linux-licenses.json, docs/research/gpui-322cd509-linux-licenses.md, WORKLOG.md
- Verification: cargo metadata --locked --format-version 1 --filter-platform x86_64-unknown-linux-gnu captured outside repo; independent traversal matched all 481 reachable packages; JSON assertions and python3 -m json.tool passed; git diff --check passed; no build or GUI test run in this licensing slice
- Risks / next: One optional GPL expression (Apache-2.0 OR GPL-2.0-only), no LGPL; one MPL-2.0 package requires separate review; product license remains unknown; no automatic dependency removal recommended

## 2026-10-16T22:38:43+00:00 — /root/command_admission

Added structured pre-admission queue-full responses for resize, input, and IPC job saturation; stress harness exposes typed rejections and soak retries only resize queue-full within a ten-second deadline

- Files: crates/session-host/src/lib.rs, crates/session-host/src/ipc_server.rs, scripts/stress-pty.py, scripts/soak-pty.py
- Verification: cargo test -p pigade-session-host ipc_server::tests -- --nocapture (4 passed); cargo clippy --workspace --all-targets -- -D warnings (passed); cargo build --release -p pigade-session-host --bin pigade-session-host (passed); cargo fmt --all -- --check; python3 scripts/check-version.py; python3 -m py_compile scripts/stress-pty.py scripts/soak-pty.py; queue-full retry helper smoke retried explicit queue_full once and propagated ambiguous TimeoutError without retry; 7-second four-session soak --verify completed, full recovery and CRC passed, 2 resizes/session, no retry required; git diff --check passed
- Risks / next: No 24-hour soak rerun; prior failed run remains preserved outside Git. Lead reviews/integrates typed admission contract; longer soak remains open.

## 2026-10-16T22:39:24+00:00 — /root/command_admission

Persisted bounded per-sample resize queue-full retry count and wait summaries in the existing fsynced soak progress stream

- Files: scripts/soak-pty.py
- Verification: PYTHONPYCACHEPREFIX=/tmp/pigade-admission-pycache python3 -m py_compile scripts/stress-pty.py scripts/soak-pty.py; second 7-second four-session --verify soak completed with full recovery and CRC; each fsynced progress sample contained four bounded retry-count/wait summaries; git diff --check passed
- Risks / next: Lead reviews/integrates; no retry was needed in the soak, so only the helper simulation exercised the queue-full retry path.

## 2026-10-16T22:46:03+00:00 — /root/font_profile

Prepared a reproducible JetBrains Mono primary-font candidate and documented the CJK fallback width blocker

- Files: docs/research/terminal-font-profile.md, crates/app/assets/fonts/jetbrains-mono-v2.304
- Verification: python3 -m json.tool on asset-manifest.json; inline fontTools 4.63.0 checks passed for each bundled face sample coverage and M/box advances; Noto CJK JP metrics reproduced mismatch; SHA-256 and byte counts matched every manifest entry; trailing-whitespace check passed; no app build, tests, GUI, or platform claims
- Risks / next: Lead integrates or declines bundled primary files and chooses a CJK policy only after exact cell-advance/render fixtures; CORE_PROFILE remains font=unfixed-prototype

## 2026-10-16T22:47:57+00:00 — /root/font_profile

Corrected CJK span rationale from lead review; recorded current per-cell paint behavior and added an optional Noto JP Regular fallback candidate

- Files: docs/research/terminal-font-profile.md, crates/app/assets/fonts/jetbrains-mono-v2.304, crates/app/assets/fonts/noto-sans-mono-cjk-jp-v2.004
- Verification: Read paint_panel: host-indexed origin, whole-cluster GPUI shaping, TextAlign::Left, pane viewport mask only, no per-cell clip/centering. Verified official Noto Sans Mono CJK JP v2.004 release ZIP size/SHA and included OFL. python3 -m json.tool and per-file manifest size/SHA checks passed; fontTools 4.63.0 verified bundled JBM script samples and Noto JP CJK samples/advances; trailing whitespace passed. No app build, GUI, platform, or pixel tests.
- Risks / next: Lead chooses explicit CJK region policy and baseline/placement/span-clipping behavior; JP forms remain opt-in only. No exact natural-advance equality requirement; CORE_PROFILE remains unchanged.

## 2026-10-16T22:50:28+00:00 — /root/gpui_accessibility_upgrade

Added opt-in isolated AT-SPI tree probe with bounded async-tree polling and recorded native-bridge evidence/report

- Files: scripts/isolated-render.py, crates/app/evidence/accessibility-atspi-20261017, docs/reports/accessibility-atspi-20261017.md, WORKLOG.md
- Verification: python3 -m py_compile scripts/isolated-render.py; --help and three JSON parse checks passed; git diff --check passed; opt-in isolated run at /home/afterburner/.local/state/pigade/isolated-accessibility/atspi-20261017-pass-final passed render/reopen and AT-SPI assertions (6 nodes, 47 D-Bus calls, 2 snapshots, 0.366s tree wait), private namespaces/device-free preflight verified
- Risks / next: Bridge exposure verified only in private headless Wayland harness; no Orca, input/actions, WCAG assessment, X11, or full accessibility acceptance

## 2026-10-16T22:50:58+00:00 — /root/gpui_accessibility_upgrade

Reran bounded AT-SPI probe after annotating the bridge Frame role and refreshed curated evidence

- Files: scripts/isolated-render.py, crates/app/evidence/accessibility-atspi-20261017, docs/reports/accessibility-atspi-20261017.md, WORKLOG.md
- Verification: python3 -m py_compile scripts/isolated-render.py; latest opt-in isolated run at /home/afterburner/.local/state/pigade/isolated-accessibility/atspi-20261017-pass-final2 passed render/reopen and accessibility assertions; 6 nodes, 47 calls, 2 snapshots, 0.354s tree wait; git diff --check passed
- Risks / next: Only the private headless Wayland native bridge tree was verified; screen-reader operation and wider accessibility acceptance remain unverified

## 2026-10-16T22:51:24+00:00 — /root/history_record_chunks

Added source-preserving bounded canonical history record chunks with exact sequence guarding and prefix/private-tail coverage

- Files: crates/session-host/src/history.rs, crates/session-host/src/history/tests.rs, WORKLOG.md
- Verification: cargo fmt --all --check; cargo test --locked -p pigade-session-host history::tests -- --nocapture (5 passed); cargo clippy --locked --workspace --all-targets -- -D warnings; git diff --check
- Risks / next: Root integrates the host IPC command; returned chunks are capped at 64 KiB, while record lookup still materializes one codec-bounded row up to 16 MiB.

## 2026-10-16T22:53:37+00:00 — /root/command_admission

Classified app IPC command failures as explicitly not admitted only for recognized queue_full responses; surfaced distinct Russian status wording while preserving sticky errors and no-retry behavior

- Files: crates/app/src/protocol.rs, crates/app/src/main.rs
- Verification: crates/app/bootstrap.sh fmt -- --check passed; crates/app/bootstrap.sh test --locked passed (19 tests); crates/app/bootstrap.sh clippy --locked --all-targets -- -D warnings passed; crates/app/bootstrap.sh build --locked passed; python3 scripts/check-version.py and git diff --check passed; no desktop automation used
- Risks / next: Lead reviews/integrates alongside concurrent app accessibility edits; request failures remain sticky and no UI retry path was added.

## 2026-10-16T22:55:12+00:00 — /root/terminal_memory_budget

Added cloneable VTE processor census primitives and REP aggregation callback; added terminal resource census for input, zero-width, title, hyperlink, and title-stack callback counts

- Files: vendor/vte/src/ansi.rs, vendor/vte/PIGADE-PATCH.md, vendor/alacritty_terminal/src/term/resources.rs, vendor/alacritty_terminal/src/term/mod.rs, vendor/alacritty_terminal/PIGADE_PATCH.md
- Verification: cargo test --manifest-path vendor/vte/Cargo.toml --features ansi --lib (55 passed); cargo test --manifest-path vendor/alacritty_terminal/Cargo.toml --lib --config patch.crates-io.vte.path=... (156 passed before overflow helper); focused terminal census tests (5 passed); cargo check --manifest-path vendor/alacritty_terminal/Cargo.toml --no-default-features --config patch.crates-io.vte.path=... (passed); VTE clippy strict passed; Alacritty clippy passed with 3 documented upstream question_mark warnings; rustfmt --check resources.rs passed; ANSI rustfmt check reports only pre-existing compact checkpoint helper formatting; git diff --check passed
- Risks / next: Root can integrate resource census into whole-event pre-admission; census duplicates bounded pending parser state (up to 2 MiB sync and 64 KiB OSC) and reports counts only, not heap usage or an admission policy

## 2026-10-16T23:02:25+00:00 — /root/vm_large_corpus_plan

Added optional isolated corpus-backed two-boot VM mode with completion/hash/reference preflight, read-only private staging, serial-verified writable guest data disk, and exact frame plus canonical checkpoint-tail comparison; preserved synthetic command path

- Files: scripts/vm-reboot.py, scripts/test_vm_reboot.py
- Verification: PYTHONPYCACHEPREFIX=/tmp/pigade-vm-reboot-pycache python3 -m py_compile scripts/vm-reboot.py scripts/test_vm_reboot.py; python3 -m unittest discover -s scripts -p test_vm_reboot.py -v (11 passed); git diff --check -- scripts/vm-reboot.py scripts/test_vm_reboot.py; no QEMU prepare/boot or corpus copy performed
- Risks / next: Parent reviews changes and reference API integration; run only with explicit parent go after fresh completed corpus and same-binary recover-corpus result exist

## 2026-10-16T23:03:21+00:00 — /root/vm_large_corpus_plan

Corrected corpus guest IPC to connect through the dedicated short runtime endpoint required by serve --runtime

- Files: scripts/vm-reboot.py, scripts/test_vm_reboot.py
- Verification: PYTHONPYCACHEPREFIX=/tmp/pigade-vm-reboot-pycache python3 -m py_compile scripts/vm-reboot.py scripts/test_vm_reboot.py; python3 -m unittest discover -s scripts -p test_vm_reboot.py -v (12 passed); git diff --check -- scripts/vm-reboot.py scripts/test_vm_reboot.py; no VM or corpus copy performed
- Risks / next: Parent reviews final diff and then authorizes any VM preparation/run separately

## 2026-10-16T23:05:41+00:00 — /root/terminal_memory_budget

Corrected incidental rustfmt recursion from the prior census slice by restoring formatting-only changes in unassigned vendored cell and search modules

- Files: vendor/alacritty_terminal/src/term/cell.rs, vendor/alacritty_terminal/src/term/search.rs, WORKLOG.md
- Verification: Inspected both diffs and confirmed they contained only rustfmt line wrapping/comma-style changes; git restore returned both paths to HEAD and follow-up git diff is empty
- Risks / next: Read-only review of dynamic terminal resource-accounting design completed separately; no admission code changed

## 2026-10-16T23:07:24+00:00 — /root/vm_large_corpus_plan

Fixed corpus fixture staging to copy only explicit files, allowed only recognized Host runtime artifacts while excluding them from stage, corrected guest path references, and made startup readiness probe the selected runtime endpoint

- Files: scripts/vm-reboot.py, scripts/test_vm_reboot.py
- Verification: PYTHONPYCACHEPREFIX=/tmp/pigade-vm-reboot-pycache python3 -m py_compile scripts/vm-reboot.py scripts/test_vm_reboot.py; python3 -m unittest discover -s scripts -p test_vm_reboot.py -v (16 passed); git diff --check -- scripts/vm-reboot.py scripts/test_vm_reboot.py; no VM launch or corpus copy
- Risks / next: Ready for parent review and explicit root small-corpus smoke authorization

## 2026-10-16T23:09:33+00:00 — /root/font_render_profile

Pinned the app-level JetBrains Mono terminal render profile, disabled liga/calt, and clipped glyph clusters to host cell spans

- Files: crates/app/src/font.rs, crates/app/src/main.rs, crates/app/assets/fonts/jetbrains-mono-v2.304/asset-manifest.json, crates/app/assets/fonts/render-profile.json, docs/research/terminal-font-profile.md
- Verification: crates/app/bootstrap.sh test --locked (21 passed); crates/app/bootstrap.sh clippy --locked --all-targets -- -D warnings (passed); crates/app/bootstrap.sh build --locked (passed); crates/app/bootstrap.sh fmt -- --check (passed); JSON validation and git diff --check passed; no GUI or desktop automation
- Risks / next: Profile remains separate from CORE_PROFILE; root should run isolated renderer adjacency fixtures for regular/bold/italic, combining clusters and wide glyphs at requested sizes/scales. CJK policy and platform fallback identity remain unpinned/unaccepted.

## 2026-10-16T23:09:41+00:00 — /root/vm_large_corpus_plan

Pinned the official Arch qemu-img 11.1.2-1 package in the private VM SDK, recorded the extracted binary and version, and documented package verification/provenance

- Files: scripts/prepare-vm-sdk.py, docs/research/vm-sdk-qemu-img.md
- Verification: PYTHONPYCACHEPREFIX=/tmp/pigade-vm-sdk-pycache python3 -m py_compile scripts/prepare-vm-sdk.py; python3 scripts/prepare-vm-sdk.py --prepare --directory /tmp/pigade-vm-sdk-qemu-img-20261017 (ready; 14 packages; no unresolved ldd); manifest assertion confirmed qemu-img version 11.1.2, pinned SHA-256 and David Runge primary signer; python3 scripts/check-version.py; git diff --check -- scripts/prepare-vm-sdk.py docs/research/vm-sdk-qemu-img.md; no package installation or VM launch
- Risks / next: Use /tmp/pigade-vm-sdk-qemu-img-20261017/evidence/manifest.json for the separately authorized corpus VM preflight

## 2026-10-16T23:11:22+00:00 — /root/terminal_memory_budget

Added bounded persistent terminal allocation census over screens, cached rows, cursor templates, extras, hyperlinks, titles, and config storage

- Files: vendor/alacritty_terminal/src/term/resources.rs, vendor/alacritty_terminal/src/term/cell.rs, vendor/alacritty_terminal/src/grid/mod.rs, vendor/alacritty_terminal/src/grid/row.rs, vendor/alacritty_terminal/src/grid/storage.rs, vendor/alacritty_terminal/PIGADE_PATCH.md, WORKLOG.md
- Verification: cargo test --manifest-path vendor/alacritty_terminal/Cargo.toml --config patch.crates-io.vte.path=vendor/vte term::resources --lib (9 passed); full vendor library tests (161 passed); reference tests (45 passed); no-default-features cargo check passed; Clippy passed with the 3 documented upstream question_mark warnings forced to warning; git diff --check passed
- Risks / next: Root integrates any caller scheduling or policy; census is bounded by caller-supplied visits and unique Arc identities, is read-only, and is not heap/RSS accounting or output-loop admission

## 2026-10-16T23:11:29+00:00 — /root/font_render_profile

Applied root decision to pin Noto Sans Mono CJK JP Regular as explicit default JP-regional fallback and updated the separate app render profile; CORE_PROFILE remains unchanged

- Files: crates/app/src/font.rs, crates/app/src/main.rs, crates/app/assets/fonts/jetbrains-mono-v2.304/asset-manifest.json, crates/app/assets/fonts/noto-sans-mono-cjk-jp-v2.004/asset-manifest.json, crates/app/assets/fonts/render-profile.json, docs/research/terminal-font-profile.md
- Verification: crates/app/bootstrap.sh test --locked (21 passed); crates/app/bootstrap.sh clippy --locked --all-targets -- -D warnings (passed); crates/app/bootstrap.sh build --locked (passed); crates/app/bootstrap.sh fmt -- --check (passed); font manifest/profile JSON and asset byte-count/SHA-256 checks passed; git diff --check passed; no GUI automation
- Risks / next: App profile now fixes JP regional CJK forms for covered glyphs; fallback style parity, uncovered glyph platform fallback and glyph ink/baseline remain unverified. Root should run isolated-render with A漢B and xあ中한y adjacency output; semantic CORE_PROFILE/corpus migration remains a separate future gate.

## 2026-10-16T23:12:47+00:00 — /root/terminal_memory_budget

Extended terminal allocation census to account for tab-stop, keyboard-mode and damage-line Vec capacities and clarified its scoped-estimate limits

- Files: vendor/alacritty_terminal/src/term/resources.rs, vendor/alacritty_terminal/PIGADE_PATCH.md, WORKLOG.md
- Verification: cargo test --manifest-path vendor/alacritty_terminal/Cargo.toml --config patch.crates-io.vte.path=vendor/vte term::resources --lib (10 passed); no-default-features cargo check passed; Clippy passed with three documented upstream question_mark warnings forced to warnings; scoped git diff --check passed
- Risks / next: Root can proceed with runtime policy integration; complete census remains a scoped payload-capacity estimate and is not an upper bound on total memory

## 2026-10-16T23:18:25+00:00 — /root/font_render_profile

Use reproducible unique OFL-compliant aliases for bundled terminal font faces and narrow identity claims

- Files: crates/app/src/font.rs, crates/app/src/main.rs, crates/app/assets/fonts/derived-terminal-v1, scripts/prepare-terminal-fonts.py, crates/app/assets/fonts/render-profile.json, docs/research/terminal-font-profile.md
- Verification: ./bootstrap.sh test --locked (21 passed), ./bootstrap.sh clippy --locked --all-targets -- -D warnings (passed), ./bootstrap.sh build --locked (passed), ./bootstrap.sh fmt -- --check (passed), scripts/prepare-terminal-fonts.py --verify (byte-identical fresh regeneration with outline/metric/cmap preservation), JSON checks and git diff --check passed
- Risks / next: Root runs isolated renderer fixture; unsupported glyph platform fallback, visual span/baseline acceptance, and macOS/Windows behavior remain unverified.

## 2026-10-16T23:18:27+00:00 — /root

Integrated short locked runtime sockets, source-preserving asynchronous history reads, typed admission failures, leased cache maintenance and real corpus-backed two-boot VM diagnostics; measured bounded model-census costs

- Files: crates/session-host/src/history_read.rs, crates/session-host/src/replay_cache.rs, crates/session-host/src/history.rs, crates/session-host/src/lib.rs, crates/session-host/tests/recovery.rs, crates/session-host/tests/slow_input.rs, crates/session-host/src/main.rs, crates/terminal/src/lib.rs, crates/terminal/examples/allocation_census.rs, scripts/recover-corpus.py, scripts/vm-reboot.py, scripts/test_vm_reboot.py, scripts/test_soak_admission.py, scripts/check.sh, scripts/soak-pty.py, scripts/isolated-render.py, README.md, CHANGELOG.md, docs/PROJECT_MAP.md, docs/AGENT_HANDOFF.md, docs/reports/runtime-and-replay-cache.md, docs/reports/vm-reboot.md, docs/reports/terminal-memory-census.md, benchmarks/results
- Verification: bash scripts/check.sh passed (128 Rust tests, 23 Python tests, strict all-target Clippy, fmt, release bins/examples, version/whitespace); patched Alacritty 162 lib +45 reference +no-default passed; VTE57 +no-default passed earlier in same slice; one-cycle exact canonical-tail recovery passed319.16ms, small corpus VM real2boots passed exactframes/all6tailrecords and sourcehashes; actual-model census5fixtures30samples with unchangedcheckpoints; independent Luna review reran42host+targetedPTYtests; prepared SDK/tool/identity failures retained/repaired and regression-tested
- Risks / next: Root finishes uniquefontalias integration and isolatedvisual check before commit; full4x1GiB/100kresize corpus and actual24hsoak remainrunning/frozen, notpassed; globaladmission/UIhistory/platform/provider/fullMVP gates remain open

## 2026-10-16T23:21:55+00:00 — /root/font_render_profile

Remove stale DSIG from derived JP font and generate accurate routed redistribution notice

- Files: scripts/prepare-terminal-fonts.py, crates/app/assets/fonts/derived-terminal-v1, crates/app/assets/fonts/render-profile.json, docs/research/terminal-font-profile.md
- Verification: python3 scripts/prepare-terminal-fonts.py --verify passed with byte-identical regeneration; manifest/source/output notice JSON checks passed; confirmed source DSIG has zero signature records and derived OTF has no DSIG; ./bootstrap.sh test --locked (21 passed), clippy --locked --all-targets -- -D warnings, build --locked, fmt -- --check, and git diff --check passed
- Risks / next: Root reruns isolated renderer against the rebuilt app payload; cross-platform font selection and unsupported glyph fallback remain unverified.

## 2026-10-16T23:26:56+00:00 — /root

Verified uniquely aliased embedded font payloads and native AT-SPI/reopen in isolated100%/200% software outputs; invalidated stale-target visual evidence and added preflight regression

- Files: crates/app/src/main.rs, crates/app/src/font.rs, crates/app/assets/fonts, scripts/isolated-render.py, scripts/test_isolated_render.py, crates/app/evidence/font-profile-20261017, crates/app/evidence/font-profile-stale-build-20261017, docs/reports/font-render-profile.md, docs/adr/0008-bundled-terminal-font-identities.md, crates/app/README.md, docs/PROJECT_MAP.md, docs/AGENT_HANDOFF.md, CHANGELOG.md
- Verification: Root app21tests/alltargetstrictClippy/fmt pass; childlockedbuildpass and selectedbinary275e47a1 has all5exactfontpayloads; root freshgenerator--verify byte-identical; Python25tests pass inclstalebinary/manifest; isolated actualapp scale1(1280x800,69x13) andscale2(2560x1600,73x14) render/AT-SPI/reopen pass samePID/generation andmanualimageinspection; previouswrongcache8b248 invalidated/preserved ratherthan claimed; firstscaleharness5120capturefailure retained/corrected
- Risks / next: Commit coherent integration afterfinalwhitespace/version checks; stillnoIME/fractional/nativeGPU/crossplatform/OrcaText/700MiBglobaladmission/fullMVPacceptance; continuefoundationgates and frozenlongruns

## 2026-10-16T23:28:03+00:00 — /root

Preserved upstream font license and authors bytes during staged whitespace verification

- Files: .gitattributes, WORKLOG.md
- Verification: Staged diff check identified original trailing spaces in four upstream/reproduced legal files; exact-path attributes retain their bytes and disable only end-of-line whitespace warnings for those files. Other whitespace checks and the pre-commit hook remain enabled.
- Risks / next: Recheck staged diff and commit the verified integration; do not normalize source license hashes.

## 2026-10-16T23:31:04+00:00 — /root/recovery_slice_review

Cover CRC-valid semantic journal-tail rejection across full replay, checkpoint-tail recovery, and live host listing while preserving valid prefix and source bytes

- Files: crates/session-host/tests/recovery.rs, WORKLOG.md
- Verification: rustfmt --edition 2024 --check crates/session-host/tests/recovery.rs; cargo test -p pigade-session-host --test recovery semantically_invalid_crc_valid_tail_preserves_and_serves_last_good_prefix -- --nocapture (passed, four invalid-tail payload cases)
- Risks / next: Host implementation now validates semantic payloads before mutation; broader host/recovery suite and final integration verification remain with root.

## 2026-10-16T23:36:14+00:00 — /root

Kept valid terminal generations visible when a CRC-valid journal tail contains an invalid typed event

- Files: crates/session-host/src/lib.rs, crates/session-host/src/recovery_tests.rs, crates/session-host/tests/recovery.rs, docs/reports/runtime-and-replay-cache.md, docs/AGENT_HANDOFF.md, CHANGELOG.md
- Verification: Red invalid-resize reproduction failed before fix; payload validation now precedes history.begin/model changes. Fresh workspace checks passed 130 tests, strict all-target Clippy, formatting and release bins/examples. Integration covers four bad tails, full/fast frames, source preservation and host reads. Root strengthened fixture to exceed10000hotrows and assert nonempty cold archive; targeted rerun passed. Separate exact checkpoint and partial UTF-8 continuation test passed for both recovery paths. Clean recovery oracle rejects degraded results.
- Risks / next: Continue with visible diagnostics for journals whose header/profile cannot be recovered at all; global memory admission and complete MVP gates remain open.

## 2026-10-16T23:38:32+00:00 — /root/recovery_slice_review

Verify bounded persistent diagnostics for invalid journal artifacts while preserving healthy historical and live PTY sessions

- Files: crates/session-host/tests/recovery.rs, WORKLOG.md
- Verification: rustfmt --edition 2024 --check crates/session-host/tests/recovery.rs; targeted unavailable-journals recovery test passed; semantic-tail regression passed; git diff --check -- crates/session-host/tests/recovery.rs
- Risks / next: Root owns diagnostic implementation, broader integration checks, and commit.

## 2026-10-16T23:39:43+00:00 — /root/terminal_memory_budget

Added reference-weighted CellExtra census counters and checkpoint-import parser-driven COW regression

- Files: vendor/alacritty_terminal/src/term/resources.rs, vendor/alacritty_terminal/PIGADE_PATCH.md, WORKLOG.md
- Verification: cargo test --manifest-path vendor/alacritty_terminal/Cargo.toml --config patch.crates-io.vte.path=vendor/vte term::resources --lib (11 passed); no-default-features cargo check passed; Clippy passed with three documented upstream question_mark warnings forced to warnings; scoped diff-check passed
- Risks / next: Counters expose COW fanout risk for root admission design; they are diagnostic and do not change unique payload estimates or introduce runtime policy

## 2026-10-16T23:40:09+00:00 — /root/font_render_profile

Parse bounded recovery diagnostics atomically and surface nonfatal recovery/history warnings in the app

- Files: crates/app/src/protocol.rs, crates/app/src/main.rs
- Verification: ./bootstrap.sh test --locked (28 passed), ./bootstrap.sh clippy --locked --all-targets -- -D warnings passed, ./bootstrap.sh fmt -- --check passed, ./bootstrap.sh build --locked passed; default binary /home/afterburner/Projects/PigADE/crates/app/target/debug/pigade-app sha256 f56f802635a1bb975d07b3b568a0253c8251e0fb2ebe2c3d5b0590e7a21e49d0; git diff --check passed
- Risks / next: Root reruns isolated render/accessibility check against this app binary; recovery warning layout is compact and visible but requires visual review.

## 2026-10-16T23:45:11+00:00 — /root/font_render_profile

Add opt-in isolated malformed-journal recovery warning fixture with PTY and AT-SPI coverage

- Files: scripts/isolated-render.py, scripts/test_isolated_render.py
- Verification: python3 -m unittest scripts/test_isolated_render.py -v: 3 passed; python3 -m py_compile scripts/isolated-render.py scripts/test_isolated_render.py: passed; git diff --check -- scripts/isolated-render.py scripts/test_isolated_render.py: passed; isolated scale-1 --accessibility --recovery-warning run: render_and_reopen_pass with four protocol input echoes, list/poll recovery match and unchanged source hash, AT-SPI Role::Alert exported as Notification(101); app SHA 99567b416716a9012ed86875293e3a2209779f4f6e972b3f95db93775bd3679d, host SHA fface4d93ec786546c182c7fced9f8ee1199ac2529773c6e6785706f69a75c4a
- Risks / next: Root to review screenshot and integrate; limits: private Weston/Pixman scale 1, AT-SPI bridge-tree only, no screen reader/keyboard/real GPU/platform acceptance. First failed role-id assumption is retained as evidence; successful mapping is documented in result.json.

## 2026-10-16T23:48:00+00:00 — /root/font_render_profile

Strengthen isolated recovery fixture with child-generated ACK tokens

- Files: scripts/isolated-render.py, scripts/test_isolated_render.py, crates/app/evidence/recovery-warning-20261017/final-child-ack/run.json, crates/app/evidence/recovery-warning-20261017/final-child-ack/result.json, crates/app/evidence/recovery-warning-20261017/final-child-ack/preflight.json, crates/app/evidence/recovery-warning-20261017/final-child-ack/screenshots/wayland-screenshot-2026-10-16_23-47-44.png
- Verification: Earlier proof observed tty-echoed command text only, not child-shell processing. Corrected fixture submits __PIGADE_RECOVERY_PROBE_N__ and waits for distinct __PIGADE_RECOVERY_CHILD_ACK_N__ generated by bash. python3 -m unittest scripts/test_isolated_render.py -v: 4 passed including executing the shell-loop test; python3 -m py_compile scripts/isolated-render.py scripts/test_isolated_render.py: passed; git diff --check scoped files: passed; final external isolated scale-1 run: render_and_reopen_pass, list/poll diagnostics matched, malformed source hash unchanged, four PTYs running and child ACKs observed, AT-SPI warning reason present (AccessKit Alert maps to Notification 101). App SHA 26027250ef4f1eb2de28ec60db217b1d15462f85e30a47124bf83ef4dc20dc47; host SHA fface4d93ec786546c182c7fced9f8ee1199ac2529773c6e6785706f69a75c4a.
- Risks / next: Root to review curated final-child-ack screenshot/result. Earlier runtime artifacts remain outside repository. Scope is private Weston/Pixman scale 1; no screen reader, keyboard/IME, real GPU, or cross-platform accessibility acceptance.

## 2026-10-16T23:48:48+00:00 — /root/font_render_profile

Settle recovery fixture capture so child ACK output is visible in the rendered panes

- Files: scripts/isolated-render.py, crates/app/evidence/recovery-warning-20261017/final-child-ack-present/run.json, crates/app/evidence/recovery-warning-20261017/final-child-ack-present/result.json, crates/app/evidence/recovery-warning-20261017/final-child-ack-present/preflight.json, crates/app/evidence/recovery-warning-20261017/final-child-ack-present/screenshots/wayland-screenshot-2026-10-16_23-48-36.png
- Verification: Prior child ACK proof passed at the host but its immediate capture showed stale UI sequence 2 without the ACKs. Added recovery-fixture-only 0.5s settle (not a latency measurement). Fresh same-binary scale-1 isolated run passed; screenshot visibly contains all four probe and child-ACK lines, warning, and PTY seq 3. result.json state views seq 4 each. Diagnostics and malformed source hashes match; AT-SPI warning passed. App SHA 26027250ef4f1eb2de28ec60db217b1d15462f85e30a47124bf83ef4dc20dc47; host SHA fface4d93ec786546c182c7fced9f8ee1199ac2529773c6e6785706f69a75c4a. Python suite 4 passed; py_compile and scoped diff-check passed.
- Risks / next: Root to visually inspect final-child-ack-present screenshot and sequence presentation. Same private scale-1 scope and accessibility limits remain.

## 2026-10-16T23:49:11+00:00 — /root

Expose bounded unavailable-artifact recovery notices without blocking healthy terminals; verify native warning and live replicas, strengthen shared-mark accounting evidence

- Files: crates/session-host/src/recovery_diagnostics.rs, crates/session-host/src/lib.rs, crates/session-host/src/history_read.rs, crates/session-host/src/ipc_server.rs, crates/session-host/tests/recovery.rs, crates/app/src/main.rs, crates/app/src/protocol.rs, scripts/isolated-render.py, scripts/test_isolated_render.py, crates/app/evidence/recovery-warning-20261017, crates/terminal/examples/allocation_census.rs, vendor/alacritty_terminal/src/term/resources.rs, vendor/alacritty_terminal/PIGADE_PATCH.md, benchmarks/results/terminal-allocation-census-references.json, README.md, CHANGELOG.md, docs/AGENT_HANDOFF.md, docs/PROJECT_MAP.md, docs/reports/runtime-and-replay-cache.md, docs/reports/terminal-memory-census.md
- Verification: Fresh root workspace133tests +Python27tests, strictClippy/fmt/releasebins/examples passed; app29tests/clippy/build passed with localrejected-input gate restored; Alacritty163lib+45ref+no-default passed. Root verified finalisolated app26027250/hostfface4d9 screenshot: namedwarning and four distinctchildACKs/seqadvance, samegeneration/PIDreopen; nativeAlert maps Notification101. Initialroleassumption/echo-only/immediatecapture limitations retained. Fresh30sample census5fixtures preserves checkpoint bytes and prioruniqueestimates.
- Risks / next: Commit reviewed slice; continue whole-event/global memory admission and remainingstage0 gates. Fullcorpus firsttwooracles done, thirdgenerating; actual24hsoak running, neither completed. No fullMVP/platform/700MiB claim.

## 2026-10-16T23:51:04+00:00 — terminal_memory_budget

Extended pinned-VTE resource census with title and hyperlink callback/payload maxima and Term query callback counts; added continuation, OSC, title-push, and query-storm fixtures.

- Files: vendor/alacritty_terminal/src/term/resources.rs, vendor/alacritty_terminal/PIGADE_PATCH.md, WORKLOG.md
- Verification: Passed targeted patched-vte resource tests (15/15), alacritty_terminal no-default-features check, and Clippy with --cap-lints warn (only three pre-existing tty/unix.rs question_mark warnings; strict -D warnings remains blocked by those upstream warnings). rustfmt and git diff --check passed.
- Risks / next: Census reports callback counts/payload sizes only; host admission integration and any response-byte bound remain separate work.

## 2026-10-16T23:54:49+00:00 — recovery_slice_review

Share a bounded initialized-cell decode budget across both checkpoint grids while keeping the standalone grid limit and valid generic Term maximum pair intact; add a small hostile second-grid regression.

- Files: vendor/alacritty_terminal/src/grid/checkpoint.rs, vendor/alacritty_terminal/src/term/checkpoint.rs
- Verification: cargo fmt --all --check; cargo test --manifest-path vendor/alacritty_terminal/Cargo.toml --config patch.crates-io.vte.path=vendor/vte checkpoint --lib (16 passed); cargo test -p pigade-terminal checkpoint --lib (4 passed); git diff --check on owned files
- Risks / next: Root owns integration and full workspace verification; aggregate budget permits 16,777,216 normal-grid cells plus the maximum 512x4096 alternate viewport (2,097,152), preserving every checkpoint accepted by existing per-grid and terminal validation bounds.

## 2026-10-16T23:58:34+00:00 — terminal_memory_budget

Fixed soak producer command loss: preserve commands queued after RELEASE and added a real PTY regression for coalesced RELEASE+next PING.

- Files: scripts/soak-pty.py, scripts/test_soak_admission.py, WORKLOG.md
- Verification: All five soak admission/producer PTY tests passed; py_compile and git diff --check passed. Reproduced exact committed producer on a bounded PTY: after one ACK, coalesced RELEASE+next PING was dropped (no second ACK, producer kept running); fixed producer PTY test receives second ACK. Frozen run metadata and checkpoint-tail replay were inspected read-only; no host degradation observed in final sample.
- Risks / next: The archived soak does not record whether the exact coalesced read occurred at SOAK-0-755, though the frozen producer has a deterministic command-loss race matching the missing ACK. Review the runner fix; do not start another full soak until root approves validation timing.

## 2026-10-17T00:01:59+00:00 — /root/font_render_profile

Added a borrowed semantic checkpoint serializer with byte-identical canonical row and pool traversal; source remains unmodified and each grid retains its configured history limit.

- Files: vendor/alacritty_terminal/src/term/checkpoint/borrowed.rs, vendor/alacritty_terminal/src/term/cell.rs, vendor/alacritty_terminal/src/grid/mod.rs
- Verification: cargo test --offline --manifest-path vendor/alacritty_terminal/Cargo.toml --config patch.crates-io.vte.path=/home/afterburner/Projects/PigADE/vendor/vte: full vendor suite 172 unit + 45 ref + 1 doctest passed before the final grown-row fixture; same targeted borrowed suite afterward: 5 passed including all 45 corpus dirs at 3 cuts. cargo check --no-default-features passed. RUSTFLAGS=--cap-lints=warn cargo clippy --all-targets passed with 3 pre-existing tty/unix question_mark warnings; strict clippy fails only on those warnings.
- Risks / next: Root owns checkpoint module reexport and cross-project integration; no whole-process or heap-cap claim. Keep new getters serde-gated and preserve canonical row/cursor traversal.

## 2026-10-17T00:07:04+00:00 — terminal_memory_budget

Added EOF exit handling to the soak producer ACK wait and a PTY hangup regression; ran requested bounded verified host smoke.

- Files: scripts/soak-pty.py, scripts/test_soak_admission.py, WORKLOG.md
- Verification: All six scripts/test_soak_admission.py tests passed, including coalesced RELEASE+PING and PTY EOF during ACK wait; py_compile and diff-check passed. 10.015s four-session soak-pty --verify completed on frozen release binary SHA-256 67ed712c0e0f228aa3ee3e15ed2b86a35b364b75af803c66587bfd1bac8ffb58. All four producer CRC/framing checks and checkpoint-tail recovery checks passed (complete_state_equal/frame_equal/cold_history_equal true); each stream acknowledged one ping and exited cleanly. 24h completion/integrity flag remains false.
- Risks / next: The bounded smoke validates the runner and current host only; it does not replace the requested 24-hour soak. No full soak was launched.

## 2026-10-17T00:10:54+00:00 — /root

Stream validated borrowed checkpoints with exact schema2 bytes, bound envelope/two-grid import, expand callback census, repair soak command queue and restart actual24h verification

- Files: crates/terminal/src/lib.rs, vendor/alacritty_terminal/src/term/checkpoint.rs, vendor/alacritty_terminal/src/term/checkpoint/borrowed.rs, vendor/alacritty_terminal/src/term/cell.rs, vendor/alacritty_terminal/src/grid/mod.rs, vendor/alacritty_terminal/src/grid/checkpoint.rs, vendor/alacritty_terminal/src/term/resources.rs, vendor/alacritty_terminal/PIGADE_PATCH.md, scripts/soak-pty.py, scripts/test_soak_admission.py, benchmarks/results, docs/adr/0009-borrowed-checkpoint-export.md, docs/research/terminal-admission-plan.md, docs/AGENT_HANDOFF.md, docs/PROJECT_MAP.md, docs/reports/terminal-memory-census.md, CHANGELOG.md
- Verification: Fresh workspace135 tests/fmt/strictClippy/releasebins/examples pass; rootPython29pass; vendor173lib+45ref+no-default pass; vendoralltargetclippy passes with exactlythreeoldtty question_mark forcedwarnings (plainstrictfails, retained). Headerregression failswithoutguard andpassesfixed; fullenvelope matchesownedpathateveryfixturebyte; borrowed135refcuts/poolcursor/ring/grownrow/sourcevalidation paritypass. Tenalternatingfreshprocessesbefore/after showRSSp95 90484/82168KiB, identicalfixtures; no productbudgetclaim. Fixedproducer10.015s real4PTYsmoke passesCRC/fullfaststate+frame+coldoracle; failed3849.40ssoakretained.
- Risks / next: Commit integration; actualthird24hsoak nowrunning tool44117 at retainedsoak-20261017T000738Z-6244149d, frozenhost67ed712c; fullcorpus tool65474 thirdoracle running. Continue whole-event/global accounting and remainingstage0/platform/CLI/productgates, noMVPcompleteclaim.

## 2026-10-17T00:17:58+00:00 — terminal_memory_budget

Extended persistent terminal allocation census with per-grid geometry/storage shape and logical mark, hyperlink, and title lengths while leaving its existing capacity estimate and admission behavior unchanged.

- Files: vendor/alacritty_terminal/src/term/resources.rs, vendor/alacritty_terminal/src/grid/mod.rs, vendor/alacritty_terminal/PIGADE_PATCH.md
- Verification: cargo test --manifest-path vendor/alacritty_terminal/Cargo.toml --config patch.crates-io.vte.path=/home/afterburner/Projects/PigADE/vendor/vte term::resources::tests --lib: 16 passed; same patched vendor cargo test --lib: 174 passed; patched cargo check --no-default-features passed; git diff --check passed.
- Risks / next: Root owns policy integration. Per-grid row cell capacity and short-row gap are partial if census.incomplete; complete census remains an ownership payload report, not a heap/RSS bound.

## 2026-10-17T00:18:46+00:00 — /root/font_render_profile

Added numeric VTE parser/processor buffer usage and lazy bounded sync-buffer growth

- Files: vendor/vte/src/lib.rs, vendor/vte/src/ansi.rs, vendor/vte/PIGADE-PATCH.md
- Verification: cargo test --offline --manifest-path vendor/vte/Cargo.toml --features ansi,serde: 60 passed; cargo test --offline --manifest-path vendor/vte/Cargo.toml --no-default-features --features ansi: 59 unit + 1 doctest passed; cargo clippy --offline --manifest-path vendor/vte/Cargo.toml --features ansi,serde --all-targets -- -D warnings passed; patched alacritty_terminal term::resources: 16 passed; git diff --check passed.
- Risks / next: Processor::buffer_usage reports existing Vec capacity and inline size, not allocator overhead/RSS; generic Timeout-owned heap is excluded. No direct VTE Postcard byte oracle is added; root owns the cross-crate byte/continuation test. OOM remains the existing infallible allocator behavior.

## 2026-10-17T00:31:12+00:00 — terminal_memory_budget

Bounded initialized spare-row cache growth and shrink cleanup to a width-sensitive 256 KiB cell-payload target, retaining the existing row ring and checkpoint row representation.

- Files: vendor/alacritty_terminal/src/grid/storage.rs, vendor/alacritty_terminal/PIGADE_PATCH.md
- Verification: Pre-change target regression failed with 999 inactive rows vs 10-row target; after change targeted storage test passed, proving 9 spare rows plus unchanged active checkpoint rows. Patched vendor storage tests: 18 passed; full patched vendor library suite: 176 passed including owned/borrowed checkpoint corpus; patched no-default-features check passed; diff check passed.
- Risks / next: Only spare cached row cell payload is targeted. Required active rows, already-retained cache rows, per-row headers, Vec outer capacity, allocator overhead and total scrollback remain outside this local policy; root owns integration and aggregate admission.

## 2026-10-17T00:31:25+00:00 — recovery_slice_review

Preflight IPC request input arrays before Serde internally tagged content buffering

- Files: crates/session-host/src/lib.rs, crates/session-host/src/ipc_server.rs, crates/session-host/src/input.rs, WORKLOG.md
- Verification: cargo test -p pigade-session-host (52 unit, 4 checkpoint retention, 15 recovery integration, 1 slow-input passed); cargo clippy -p pigade-session-host --all-targets -- -D warnings passed; cargo fmt --all -- --check passed
- Risks / next: Root to review and integrate; IPC preflight scans bounded raw request without Content allocation, rejects oversized Input arrays and preserves normal Request decoding.

## 2026-10-17T00:33:11+00:00 — recovery_slice_review

Extract IPC raw input preflight into dedicated request_decode module

- Files: crates/session-host/src/request_decode.rs, crates/session-host/src/lib.rs, WORKLOG.md
- Verification: cargo test -p pigade-session-host request_decode::tests --lib (6 passed); cargo clippy -p pigade-session-host --all-targets -- -D warnings passed; cargo fmt --all -- --check passed; git diff --check passed
- Risks / next: Root to review/integrate. Preflight bounds only Input byte arrays; other payload decoding remains under IPC_LIMIT and normal Serde behavior.

## 2026-10-17T00:33:20+00:00 — terminal_memory_budget

Refined the spare-row cache slice by removing incidental rustfmt churn, clarifying the local target is not a global cached-byte cap, and comparing compact cache scroll continuation against an explicit legacy 1,000-row Grid oracle.

- Files: vendor/alacritty_terminal/src/grid/storage.rs, vendor/alacritty_terminal/PIGADE_PATCH.md
- Verification: Patched vendor grid::storage::tests: 19 passed, including 48-step compact-vs-legacy canonical checkpoint row comparisons; git diff --check passed.
- Risks / next: The payload target applies to newly appended rows and shrink cleanup uses current ring row width; retained heterogeneous cache rows can exceed it. Root owns final policy integration.

## 2026-10-17T00:36:00+00:00 — /root/font_render_profile

Added plan-only-by-default isolated provider TUI PTY diagnostic with bounded launch, query, resize, and cleanup checks

- Files: scripts/provider-pty.py, scripts/test_provider_pty.py, docs/research/provider-pty-plan.md
- Verification: python3 scripts/test_provider_pty.py: 9 passed; python3 -m py_compile scripts/provider-pty.py scripts/test_provider_pty.py passed; plan-only runs for codex, claude, opencode reported no process spawn; generated Bubblewrap namespace ran harmless /usr/bin/true with exit 0; git diff --check passed. No provider CLI was launched.
- Risks / next: Root should review before any live provider run. The harness proves the DSR response using its fixed companion PTY child; provider-specific query receipt, authentication, task execution, resume, GUI presentation and cross-platform behavior remain untested. Resize requires new provider PTY output bytes plus frame geometry, but does not prove semantic repaint correctness.

## 2026-10-17T00:37:02+00:00 — /root

Bound parser/cache growth and IPC input buffering; preserve explicit unknown outcomes and canonical frame/checkpoint behavior

- Files: crates/session-host/src/input.rs, crates/session-host/src/ipc_server.rs, crates/session-host/src/lib.rs, crates/session-host/src/request_decode.rs, crates/terminal/src/lib.rs, crates/terminal/examples/allocation_census.rs, vendor/vte/src/ansi.rs, vendor/vte/src/lib.rs, vendor/vte/PIGADE-PATCH.md, vendor/alacritty_terminal/src/grid/storage.rs, vendor/alacritty_terminal/src/grid/mod.rs, vendor/alacritty_terminal/src/term/resources.rs, vendor/alacritty_terminal/PIGADE_PATCH.md, benchmarks/results/terminal-allocation-census-shapes.json, benchmarks/results/terminal-allocation-census-cache-target.json, docs/research/terminal-admission-plan.md, docs/reports/terminal-memory-census.md, docs/PROJECT_MAP.md, docs/AGENT_HANDOFF.md, CHANGELOG.md
- Verification: Root workspace146tests/fmt/strictClippy/release pass; Python29 before subsequent UI/provider harness edits. VTE60stdserde +59no-defaultANSI +doctest/strictClippy pass. Vendor177lib +45ref +doctest/no-default pass; new checked-division Clippy failure repaired, focused19storage and strict all-target Clippy with exactly3oldtty forcedwarnings pass. Encoding regression red EOF then green explicitunknown; independent Luna review caught internallytaggedContent preallocation, streaming preflight repaired and6regressions pass. Both source-bound census snapshots retained; cache alignment can slightly increase fullhot payload.
- Risks / next: Commit coherent backend slice; app IME/localoverlay and provider harness remain separate uncommitted work. Whole-event/global memory budget and remaining stage0 gates open; frozen corpus finaloracle and third actual24h soak stillrunning, no acceptance claim.

## 2026-10-17T00:39:05+00:00 — font_render_profile

Harden provider PTY diagnostic lifecycle and preserve redacted failed-run evidence

- Files: scripts/provider-pty.py, scripts/test_provider_pty.py, docs/research/provider-pty-plan.md
- Verification: python3 scripts/test_provider_pty.py (15 passed); python3 -m py_compile scripts/provider-pty.py scripts/test_provider_pty.py; python3 scripts/provider-pty.py --provider codex/claude/opencode (all plan-only, spawned_processes=false); git diff --check for owned files
- Risks / next: No provider CLI was launched. Root review before any isolated live provider startup; failure runtime is private and retained on error.

## 2026-10-17T00:40:46+00:00 — font_render_profile

Fix DSR exit race and separate short IPC runtime path from durable state

- Files: scripts/provider-pty.py, scripts/test_provider_pty.py, docs/research/provider-pty-plan.md
- Verification: python3 scripts/test_provider_pty.py (18 passed); python3 -m py_compile scripts/provider-pty.py scripts/test_provider_pty.py; provider plans for codex/claude/opencode (all plan-only); git diff --check for owned files
- Risks / next: No provider CLI launched. Root can run the reviewed isolated live startup/resizes with private socket runtime and retained failure state.

## 2026-10-17T00:45:27+00:00 — recovery_slice_review

Document pinned GPUI Linux IME reset boundary; found no provable Wayland stale-commit barrier, so no upstream patch was prepared

- Files: docs/research/linux-ime-reset.md
- Verification: Read-only source inspection of pinned GPUI Wayland/X11 event, reset, and text-input notification paths; no tests run (documentation-only evidence note)
- Risks / next: Root should integrate the best-effort/reset limitation explicitly; pursue a stronger native reset only with real compositor and IME evidence

## 2026-10-17T00:45:31+00:00 — font_render_profile

Add exact standalone provider ELF override and redacted host failure classification

- Files: scripts/provider-pty.py, scripts/test_provider_pty.py, docs/research/provider-pty-plan.md
- Verification: python3 scripts/test_provider_pty.py (20 passed); python3 -m py_compile scripts/provider-pty.py scripts/test_provider_pty.py; python3 scripts/provider-pty.py --help; git diff --check for owned files; inspected retained-run manifests and replay metadata without emitting terminal text or provider logs
- Risks / next: No provider CLI launched. Root should review exact Claude ELF override and the new safe OpenCode/host failure fields before rerunning providers.

## 2026-10-17T00:51:27+00:00 — font_render_profile

Record provider PTY harness source SHA-256 in live and failure results

- Files: scripts/provider-pty.py, scripts/test_provider_pty.py
- Verification: python3 scripts/test_provider_pty.py (20 passed); python3 -m py_compile scripts/provider-pty.py scripts/test_provider_pty.py; codex plan-only check; git diff --check for owned files
- Risks / next: No provider launched. Root may run the Claude exact ELF override and OpenCode once, with results bound to this harness hash.

## 2026-10-17T00:51:37+00:00 — /root

Implement local UTF16 composition document and clipped native overlay with input lifecycle cancellation; preserve actual native IME limits

- Files: crates/app/src/input.rs, crates/app/src/main.rs, scripts/isolated-render.py, scripts/test_isolated_render.py, crates/app/evidence/ime-composition-20261017, docs/research/ime-composition-plan.md, docs/research/linux-ime-reset.md, docs/reports/ime-composition.md, docs/PROJECT_MAP.md, docs/AGENT_HANDOFF.md, CHANGELOG.md
- Verification: Freshapp33tests/fmt/strictalltargetClippy/debugbuild pass. Python50tests pass (providerharness concurrent). Finalprivate100/200percent render/reopen passes appa3559318; rootvisuallyinspectedbothscreenshots, scale1fourdistinctchildACKs/warning/localJapaneseemoji preedit; textabsentfromhostframes/journals andsamePIDs/generations. IndependentLunareviewfoundmissingnativeinvalidation/modeclear, repaired. PinnedLinuxAPIreviewprovesnotificationnoop; noformalresetclaim. Speculativelatchremoved aftersource-evidencedIMEusabilitydefect, freshIMEwithoutKeyDown regressionpasses.
- Risks / next: Commit localIME foundation; true IBus/Fcitx/Wayland/X11 ordering/candidate/clipboard gates remainopen. Fullcorpus completed and30hostreopens pass; bigVMreboot tool69343 running. ReleaseGPUI build tool38685 running. Provider script independentuncommitted; next native mouse protocol/UI integration.

## 2026-10-17T00:52:51+00:00 — terminal_memory_budget

Add pure mouse event encoder for acknowledged terminal modes

- Files: crates/app/src/mouse.rs
- Verification: rustfmt --edition 2021 crates/app/src/mouse.rs; rustc --edition=2021 --test crates/app/src/mouse.rs -o /tmp/pigade-mouse-tests && /tmp/pigade-mouse-tests (8 passed); git diff --check -- crates/app/src/mouse.rs
- Risks / next: Root to register module and connect post-IME mouse events; standalone encoder does not prove GPUI integration.

## 2026-10-17T00:57:18+00:00 — terminal_memory_budget

Extend mouse encoder with cancellation releases, pressed-state query, event-signature deduplication, and bounded wheel accumulator

- Files: crates/app/src/mouse.rs
- Verification: rustfmt --edition 2021 crates/app/src/mouse.rs; rustc --edition=2021 --test crates/app/src/mouse.rs -o /tmp/pigade-mouse-tests && /tmp/pigade-mouse-tests (11 passed); git diff --check -- crates/app/src/mouse.rs
- Risks / next: Root may integrate transactionally by cloning encoder and wheel accumulator, then retaining candidate state only after UI queue admission; reset wheel remainder at session boundaries.

## 2026-10-17T00:57:47+00:00 — font_render_profile

Record curated offline provider PTY outcomes and bounded Claude connectivity finding

- Files: docs/research/provider-pty-plan.md, docs/reports/provider-pty-20261017.md, benchmarks/results/provider-pty-20261017/codex.json, benchmarks/results/provider-pty-20261017/claude.json, benchmarks/results/provider-pty-20261017/opencode.json, benchmarks/results/provider-pty-20261017/SHA256SUMS
- Verification: Parsed final root result envelopes and retained Claude failure/replay metadata; generated redacted JSON allowlist artifacts; checked each artifact hash against SHA256SUMS; checked no frame text/title/stderr/argv/path fields; git diff --check
- Risks / next: No provider rerun and no isolation change. Stage-0 provider evidence is limited to these binaries, environment, startup/resizes/one-second running state and DSR probe; no auth/task/context/resume/semantic rendering claims.

## 2026-10-17T01:00:26+00:00 — terminal_memory_budget

Add real PTY coverage for mode-acknowledged mouse reports and resized coordinates

- Files: crates/session-host/tests/mouse_input.rs
- Verification: cargo test -p pigade-session-host --test mouse_input (12 passed, including six wire ACKs from raw PTY child); git diff --check -- crates/session-host/tests/mouse_input.rs
- Risks / next: Root owns UI wiring; this test proves generated SGR press/drag/release/wheel bytes traverse generation-scoped host input after mode acknowledgement and are observed by the PTY child.

## 2026-10-17T01:00:53+00:00 — /root

Integrate redacted offline provider PTY evidence and precise CLI matrix

- Files: scripts/provider-pty.py, scripts/test_provider_pty.py, docs/research/provider-pty-plan.md, docs/reports/provider-pty-20261017.md, docs/compatibility/CLI.md, benchmarks/results/provider-pty-20261017, docs/PROJECT_MAP.md, docs/AGENT_HANDOFF.md, CHANGELOG.md
- Verification: Fresh scripts unittest discover: 53 passed. Root executed frozen-host Codex/OpenCode offline startup+2resizes+1s checks; Claude running check failed after resizes, retained and classified. Curated hashes reviewed; no task/context/resume certification.
- Risks / next: Finish mouse/selection PTY integration; native provider conversations and exact resume remain open.
