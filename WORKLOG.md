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
