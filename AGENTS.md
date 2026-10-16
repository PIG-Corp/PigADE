# PigADE — operating contract for all agents

<!-- AUTONOMY DIRECTIVE — DO NOT REMOVE -->
Execute authorized local work through implementation and verification. Do not ask permission for ordinary reversible edit/test steps. Use native subagents for independent bounded tasks when useful. Preserve other agents' edits and assign file ownership before parallel writes.
<!-- END AUTONOMY DIRECTIVE -->

## User-selected subagent model

The user requires **gpt-6-luna** for subagents. Explicitly select `model="gpt-6-luna"` when spawning, with `fork_turns="none"` and a self-contained bounded task. Use a model-overridable role; fixed-model specialist roles cannot satisfy this preference. Do not reactivate an older child on a different model. The lead remains responsible for integration and verification.

## Desktop isolation — mandatory incident guard

Do not run keyboard/mouse injection (wtype, ydotool, xdotool), change compositor focus/workspaces, or open automated UI test windows on the user's active desktop. Product implementation authorization does not authorize these actions. UI automation requires a verified isolated compositor/display or a VM; an environment label alone is not isolation proof. Never alter monitor modes, desktop zoom or user configuration as part of product tests. The 2026-10-17 live-input incident enabled desktop zoom and triggered application shortcuts; the legacy native smoke script is disabled. Read docs/reports/desktop-incident.md before any platform testing.

## Start here

1. Read this file, README.md, SPEC.md and DESIGN.md completely before changing product behavior.
2. Read docs/PROJECT_MAP.md, docs/AGENT_HANDOFF.md, WORKLOG.md and the latest relevant ADR/report. Inspect `git status --short` and the applicable code before edits.
3. Current version is **0.0.1-dev** (dev 0.0.1). Read VERSION and docs/VERSIONING.md; keep first-party manifests aligned. Third-party vendor versions retain upstream identities plus documented patch profiles.
4. Respect SPEC stages and gates. Stage 0 is in progress. A green diagnostic suite with expected negative checkpoint cases does not approve a stack. Do not build broad product chrome to bypass an unresolved terminal/recovery gate.

## Mandatory shared history

**Every agent that changes code, tests, configuration or documentation MUST append its own entry to WORKLOG.md before finishing or handing off.** Record agent ID, concrete change, owned files, actual verification result (or explicit gap), risks and next step. Backfill work performed earlier in the current run. Do not claim another agent's verification as your own.

Use `python3 scripts/log-change.py --agent <id> --summary '<change>' --files <paths...> --validation '<actual commands/results>' --next '<risks/next step>'`. The helper locks the shared file so concurrent entries cannot overwrite each other. Do not rewrite, truncate or reorder history; corrections are new entries. The lead agent integrates and owns final verification and commits. The repository pre-commit hook requires a newly appended WORKLOG entry and aligned first-party versions; do not bypass it with --no-verify.

Before ending a milestone, the lead updates docs/AGENT_HANDOFF.md and docs/PROJECT_MAP.md when needed, adds a meaningful CHANGELOG.md entry, checks `git diff --check`, and commits a coherent tested slice. Never commit credentials, runtime state, node_modules, target outputs, or diagnostic data from provider authentication/history directories. Do not amend/rewrite commits or undo another agent's changes without a concrete task requirement.

## Contracts

- Host owns the only authoritative parser/model. UI receives cells; it never independently reparses ANSI.
- Output/resize/timers are ordered, journaled and durable before model publication. No raw output loss for frame rate.
- Session/generation/operation IDs and verified OS identity prevent duplicate launch. Unknown launch outcome stays locked. No `--last`, silent new conversation or repeated task on resume.
- Input is not journaled. Replay has no PTY writes, clipboard, URL, bell or provider effects.
- Cell frames are not full checkpoints. Checkpoint completeness needs hidden state and continuation-sensitive equality, not screenshots.
- Do not overwrite user AGENTS.md/CLAUDE.md/global configs, copy credentials or import Zed GPL terminal code. Product license is undecided.
- Public claims follow measured evidence. Keep Linux/platform/CLI/recovery limitations explicit.

## Verification

Rust baseline: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --release --workspace --bins --examples`. Also verify modified vendored crates with their targeted tests and documented patch provenance. The native application has a separate pinned GPUI dependency workspace under crates/app; use its build commands. Separate control stands were removed by user request. Benchmarks must use the recorded actual hardware and durable filesystem; `/tmp` here is tmpfs and cannot prove NVMe fsync performance. No fabricated VM reboot, 24h soak, GPU, IME or accessibility results.

Subagents report scope expansion/shared-file conflicts upward; they do not recursively coordinate other lanes. Keep updates concise. Stop only at verified completion, explicit cancellation, or an objective blocker with reproducible evidence and a concrete next step.
