# Project map — 0.0.1-dev

Read [AGENTS.md](../AGENTS.md) first. Product/UX contracts are [SPEC.md](../SPEC.md) and [DESIGN.md](../DESIGN.md); do not weaken them to match current implementation.

| Location | Responsibility / current status |
|---|---|
| Cargo.toml / Cargo.lock | Rust workspace, exact resolved dependency graph; first-party version from VERSION |
| crates/terminal | Sole host-side alacritty/VTE adapter, validated geometry, cell DTO, explicit timer events, experimental complete checkpoint envelope and continuation tests |
| crates/storage | CRC/sequence/generation-framed append-only journal, fsync receipts, corruption prefix scanner, SQLite WAL/FULL launch state machine and locks |
| crates/session-host | Independent Linux PTY actors, bounded queues, private versioned Unix IPC, startup helper/identity gate, historical replay; not a production service yet |
| crates/session-host/tests/recovery.rs | Actual PTY/process tests: input/resize, 100 reconnects, 1,000 resizes, host kill, helper gate |
| vendor/vte | Pinned 0.15.0 with documented UTF-8 fix, explicit parser/processor state experiment and OSC cap; retain licenses |
| vendor/alacritty_terminal | Pinned 0.26.0 with explicit complete Term/Grid checkpoint experiment; retain Apache license |
| prototypes/electron-control | Mandatory comparative stack; independent Node/xterm host and four cell-replica panels. Tests include known serialize failures, real PTY, UI reopen and screenshots. Not production fallback approval |
| tests/fixtures/terminal | Captures/reproducers and machine-readable terminal examples |
| benchmarks/results | Actual hardware manifest and scoped component measurements, never inferred UI budgets |
| docs/adr / docs/research | Evidence-backed decisions/proposals, pinned upstream source/licensing, open recovery gate |
| docs/reports | Verification/review findings and exact gaps |
| docs/compatibility | Installed CLI version/help evidence; provider context/resume remains uncertified |
| scripts | Reproducible tooling, mandatory shared-history append helper |
| VERSION / CHANGELOG.md / WORKLOG.md | Product version / milestone history / per-agent detailed engineering history |

No product-level projects/workspaces/tabs/tiling/context UI is implemented yet; broad work waits for SPEC stage-0 gate. A renderer screenshot is not a native terminal or recovery certification.

## Interaction and ownership

Read the handoff and current `git status`; never assume a clean checkout or reset it. Assign bounded paths for parallel work. Current contributors own independent areas; root integrates. Append your own change entry before handing off, including failed checks. Keep runtime journals/credentials outside Git and the product repository. State paths are explicit in prototype launch commands. Use test-only temporary directories and identity-checked cleanup for subprocesses; never kill unrelated CLI processes.
