# PigADE — agent change history

Append-only engineering history. Every contributing agent records its own work with `scripts/log-change.py`; see AGENTS.md. Tests, measurements and gaps are recorded as observed, without implying release acceptance. Current development version: 0.0.1-dev.

## 2026-10-16T19:23:39+00:00 — root

Initialized Git, required per-agent shared worklog and project/handoff docs; fixed development version at 0.0.1-dev. Backfill: scaffolded native terminal/host, UTF-8 downstream fix, explicit state-export experiment and tests.

- Files: AGENTS.md, VERSION, CHANGELOG.md, WORKLOG.md, docs/PROJECT_MAP.md, docs/AGENT_HANDOFF.md, docs/VERSIONING.md, scripts/log-change.py, Cargo.toml, crates/terminal, crates/session-host, vendor/vte
- Verification: Before version sync: terminal 9 tests pass; workspace clippy/check passed before checkpoint integration. Full baseline pending latest changes.
- Risks / next: Finish checkpoint persistence and responsiveness review fixes; reverify before committing code. Stage 0 and MVP remain open.
