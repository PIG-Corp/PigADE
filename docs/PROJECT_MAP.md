# Project map — 0.0.1-dev

Read [AGENTS.md](../AGENTS.md), [SPEC](../SPEC.md), [DESIGN](../DESIGN.md) and the [handoff](AGENT_HANDOFF.md). New subagents must explicitly use gpt-6-luna. Read current git status and WORKLOG before editing.

| Location | Responsibility |
|---|---|
| Cargo.toml/Cargo.lock | Host/storage/terminal/layout workspace; locked dependencies; canonical first-party version |
| crates/terminal | Sole ANSI/core adapter, validated geometry, ordered timers, cell DTOs and full checkpoint envelope |
| crates/storage | CRC/sequence/generation journal with actual read-byte counters, fsync receipts, SQLite sole-writer schema3, launch locks, catalog CAS and migration backups |
| crates/storage/src/archive.rs | Bounded PIGARC01 fragment-file primitive, complete-row validation and fsync receipts used by host-owned cold history |
| crates/layout | Platform-neutral tiling transactions, geometry/focus projection and schema1 persistent100undo/redo |
| crates/session-host | Independent Linux PTY actors, bounded input/output/IPC, durable helper launch, checkpoint/replay, indexed cold-history archive, catalog/layout command routes |
| crates/session-host/src/history.rs | Generation-bound PIGROW02 archive segments and fixed-slot index; data fsync → index fsync → PIGCP002 root; recovery tail uses a private derived cache |
| crates/session-host/src/metrics.rs | Bounded numeric per-session pipeline counters; no terminal content or input bytes |
| crates/session-host/src/catalog.rs | Authoritative layout/projection validation and atomic persistence; explicit legacy projection import |
| crates/session-host/tests | Real PTY/reconnect/resize/kill/Stop/query-burst and durable placement scenarios |
| crates/app | Actual native GPUI application; cell replica only, pure keyboard encoder and device-rounded grid rendering; separate compatible lockfile |
| crates/app/bootstrap.sh | Immutable GPUI source+SHA verification and headless build; ignored .upstream source cache |
| crates/app/evidence | Historical smoke and current isolated render/reopen proof; never a platform-certification claim |
| vendor/vte |0.15.0 maintained UTF8/full-parser-state/OSC-bound patches, original licenses |
| vendor/alacritty_terminal |0.26.0 explicit Term/Grid export/import, allocation-aware pools, borrowed row export and runtime normal-history retirement notifications |
| tests/fixtures/terminal | Boundary/UTF8 reproducers; real captured upstream fixtures also live under vendor tests/ref |
| benchmarks/results | Hardware manifest, component timings, failed-before and current 5s/60s cold-history and 12-session host-memory evidence |
| scripts/isolated-render.py | Tests actual app inside verified private namespaces, software renderer and private display; no active-desktop access/input injection |
| scripts/prepare-platform-sdk.py | Signed local-only Weston/Lavapipe extraction; no system installation |
| docs/adr/research/reports/compatibility | Decisions, evidence, reviews, exact CLI support gaps and next recovery work; see `reports/cold-history-integration.md` for current archive scope and measurements |
| VERSION/CHANGELOG/WORKLOG | Version, coherent milestone history, every agent's own append-only engineering log |

Separate control applications were removed by user request. The initial Electron comparison remains only in Git history6b9ed5c and a historical research note. There is no Electron/npm runtime in PigADE.

The catalog and tiling contracts are implemented headlessly. Host cold history is integrated with a generation-bound index and checkpoint roots, but product paging/copy/search, quotas and long-history acceptance remain open. A 12-session, 10,000-hot-row-per-session diagnostic measured 447,610,880 bytes for the release host alone; the UI, GPU and provider processes are excluded. The normal-history 60-second 4×1 MiB/s workload retained exact recovery equality but took 77.4–78.1 seconds. Full product hierarchy/tiling UI, Git/worktree preparation, context/adapters and release gates remain incomplete. Do not mistake the four-session surface for the MVP. Stage0 remains open.

Assign explicit bounded ownership for parallel edits; preserve others' changes. Each modifying agent appends its own WORKLOG entry using scripts/log-change.py. Root integrates/tests/commits. Never commit runtime databases/journals, auth/history, build outputs or .upstream. Never automate the user's active desktop; read the incident/isolation reports first.
