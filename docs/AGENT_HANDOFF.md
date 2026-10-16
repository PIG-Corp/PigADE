# Current handoff — 0.0.1-dev

## Target and status

Implement Linux PigADE exactly to SPEC/DESIGN. Initial repository contained only README/SPEC/DESIGN. Git initialized during implementation on user request. **Stage 0 is in progress; the MVP is not complete and the stack is not approved.** Read WORKLOG for recent work and fresh tests, not an assumed final state.

## Active work

- Root: terminal adapter, versioned parser/processor export, native host integration, bounded durable pipeline, verification and integration commits.
- storage subagent: storage/process tests completed; now explicit Term/Grid export in vendor/alacritty_terminal only.
- checkpoint_research: source/license and recovery research; clean pinned GPUI hello-world build validation in an isolated temporary Cargo home. Record actual build/platform blockers.
- electron_control: comparative prototype completed; read-only native host review, report in docs/reports/host-review.md.

Each agent MUST append its own WORKLOG entry via scripts/log-change.py before finishing. Root owns this handoff/map; request updates upward if work changes.

## Findings / known pending fixes

Unmodified VTE 0.15.0 loses ASCII after a split UTF-8 scalar; local narrow fix passes the boundary fixture. Grid serde skips cursors; unmodified core and xterm serialize do not provide full checkpoints. Local explicit state-export experiments now pass initial byte-boundary continuation tests, but do not approve long-history recovery or the stack.

Root must finish persistent checkpoint publication/tail recovery, fix blocking PTY input writes so Stop/Resize remain serviceable, validate malformed replay geometry and bound degraded-state behavior. Read the reviewer report for current evidence. Runtime state/old profiles must never be opened with a silently different parser.

## Latest evidence at handoff creation (must refresh)

- Rust terminal: 9 tests passing, including all-byte-boundary experimental checkpoint continuation.
- Storage: 10 tests passing, including real SIGKILL durability and launch unknown locks.
- Native host: 4 process integration tests passing before latest checkpoint integration; rerun workspace checks after integration.
- Electron: 16 diagnostic tests, actual four-panel software-rendered dark/light UI close/reopen. Seven checkpoint comparisons intentionally show serialize failures; full replay matches observable states. This is a rejected stock checkpoint path, not a passed production gate.

## Required before release

Follow SPEC stages in order. Complete clean native GPUI build/platform/accessibility spike, real two-version provider matrix, durable launch boundary fault coverage, bounded/cold-history recovery with 4×1GiB+100k resizes, VM reboot and 24h real-time soak; then product structure/tiling/context/chrome and full MVP scenario. Current machine's `/tmp` is tmpfs; only measured durable filesystem runs can inform fsync budgets. Do not claim any of these unexecuted checks.
