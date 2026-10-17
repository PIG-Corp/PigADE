# Project map — 0.0.1-dev

Read [AGENTS.md](../AGENTS.md), [SPEC](../SPEC.md), [DESIGN](../DESIGN.md) and the [handoff](AGENT_HANDOFF.md). New subagents must explicitly use gpt-6-luna. Read current git status and WORKLOG before editing.

| Location | Responsibility |
|---|---|
| Cargo.toml/Cargo.lock | Host/storage/terminal/layout workspace; locked dependencies; canonical first-party version |
| crates/terminal | Sole ANSI/core adapter, validated geometry, ordered timers, cell DTOs and full checkpoint envelope |
| crates/terminal/examples/allocation_census.rs | Actual-model capacity-census diagnostics and bounded traversal cost; excludes process RSS and transient allocations |
| crates/storage | CRC/sequence/generation journal with actual read-byte counters, fsync receipts, SQLite sole-writer schema3, launch locks, catalog CAS and migration backups |
| crates/storage/src/archive.rs | Bounded PIGARC01 fragment-file primitive, complete-row validation and fsync receipts used by host-owned cold history |
| crates/layout | Platform-neutral tiling transactions, geometry/focus projection and schema1 persistent100undo/redo |
| crates/session-host | Independent Linux PTY actors, bounded input/output/IPC, durable helper launch, checkpoint/replay, indexed cold-history archive, catalog/layout command routes |
| crates/session-host/src/history.rs | Generation-bound PIGROW02 archive segments and fixed-slot index; data fsync → index fsync → PIGCP002 root; recovery tail uses a private derived cache |
| crates/session-host/src/history_read.rs | Bounded asynchronous canonical-record reads; exact generation/sequence, no ANSI parsing or host-command-thread cold I/O |
| crates/session-host/src/replay_cache.rs | Leased, marked private replay caches and bounded fail-closed orphan collection; sources are preserved |
| crates/session-host/src/metrics.rs | Bounded numeric per-session pipeline counters; no terminal content or input bytes |
| crates/session-host/src/poll.rs | Conditional bounded polling: stable `(session,generation)` selection and metadata-only replies when the client watermark is current |
| crates/session-host/src/recovery_diagnostics.rs | Bounded unavailable-artifact notices with escaped display basenames; no fabricated sessions or source changes |
| crates/session-host/src/request_decode.rs | Streaming Input-array preflight before internally tagged Serde buffering; bounded raw IPC frame remains a separate limit |
| crates/session-host/src/catalog.rs | Authoritative layout/projection validation and atomic persistence; explicit legacy projection import |
| crates/session-host/tests | Real PTY/reconnect/resize/kill/Stop/query-burst and durable placement scenarios |
| crates/app | Actual native GPUI application; cell replica only, pure keyboard encoder and device-rounded grid rendering; separate compatible lockfile |
| crates/app/src/protocol.rs, main.rs | Client watermark merge and revision-gated UI state replacement; unchanged poll metadata avoids cloning frames/cells |
| crates/app/src/palette.rs | Bounded host-cell color resolver; typed sorted dynamic overrides, truecolor and unchanged default theme; no ANSI parser |
| crates/app/src/input.rs | Pure keyboard/atomic explicit paste encoding and local UTF16 composition; no ANSI parser or implicit shared-prompt delivery |
| crates/app/src/mouse.rs, selection.rs | Pure mode-aware mouse encoding, bounded wheel accumulation and frame-bound Unicode cell selection; native event/clipboard integration remains in main.rs |
| docs/reports/ime-composition.md, docs/research/linux-ime-reset.md | Local input/overlay evidence and the unverified native Linux reset boundary |
| crates/app/src/font.rs, assets/fonts | Embedded unique-family OFL derivatives, exact manifests, disabled liga/calt and fixed JP CJK fallback policy; unsupported platform fallback is unpinned |
| crates/app/bootstrap.sh | Immutable GPUI source+SHA verification and headless build; ignored .upstream source cache |
| crates/app/evidence/conditional-poll-20261017 | Fresh isolated software-rendered UI reopen evidence; not GPU or platform certification |
| vendor/vte |0.15.0 maintained UTF8/full-parser-state/OSC-bound patches, original licenses |
| vendor/alacritty_terminal |0.26.0 explicit Term/Grid export/import, allocation-aware pools, borrowed row export and runtime normal-history retirement notifications |
| vendor/alacritty_terminal/src/term/checkpoint/borrowed.rs | Validated streaming serializer with exact schema2 pool/ring order; borrowed payloads; owned DTO remains compatibility oracle |
| docs/research/terminal-admission-plan.md | Remaining whole-event/global allocation work and resource-refusal contracts; not an implemented budget |
| tests/fixtures/terminal | Boundary/UTF8 reproducers; real captured upstream fixtures also live under vendor tests/ref |
| benchmarks/results | Hardware manifest, component timings, failed-before and current 5s/60s cold-history, host-memory, idle-resource and long-history smoke evidence |
| scripts/isolated-render.py | Tests actual app inside verified private namespaces, software renderer and private display; no active-desktop access/input injection |
| scripts/prepare-terminal-fonts.py | Offline deterministic alias derivation with pinned fontTools; verifies source hashes, glyph/metrics/cmap preservation and byte-identical regeneration |
| scripts/prepare-platform-sdk.py | Signed local-only Weston/Lavapipe extraction; no system installation |
| scripts/long-history.py, crates/session-host/examples/history_corpus.rs | Frozen-binary, durable-filesystem production-model long-history generator/runner; versioned SHA-bound sidecars map ordered events to logical timestamps; these are not elapsed soak |
| scripts/recover-corpus.py | Owned host SIGKILL/reopen measurement with exact IPC peer, source hashes, frames and checkpoint-tail counters; no UI-present claim |
| scripts/codex-conversation.py, scripts/test_codex_conversation.py | Bounded native Codex exec/exact-ID resume continuity diagnostic; direct existing auth, no credential/history copy, redacted evidence; not product auto-resume |
| scripts/provider-pty.py, scripts/test_provider_pty.py | Frozen-host offline provider startup/resize checks inside disposable isolated homes; redacted outcomes in reports/provider-pty-20261017.md and benchmarks/results/provider-pty-20261017 |
| scripts/soak-pty.py | Real-time synthetic PTY soak, bounded QueueFull diagnostics and identity-revalidated pidfd cleanup; failed runs and one-hour instrumented reproducer do not pass24h |
| scripts/prepare-vm-sdk.py, scripts/vm-reboot.py, scripts/test_vm_reboot.py | Signed local QEMU SDK and isolated two-boot recovery; optional completed-corpus staging, source inventories and exact checkpoint-tail reference; provider resume and power-loss proof are out of scope |
| docs/adr/research/reports/compatibility | Decisions, evidence, reviews, exact CLI support gaps and next recovery work; see `reports/cold-history-integration.md` and `reports/vm-reboot.md` for current recovery scope and measurements |
| VERSION/CHANGELOG/WORKLOG | Version, coherent milestone history, every agent's own append-only engineering log |

Separate control applications were removed by user request. The initial Electron comparison remains only in Git history6b9ed5c and a historical research note. There is no Electron/npm runtime in PigADE.

The catalog and tiling contracts are implemented headlessly. Product history paging/search/scrolling, quotas and whole-event/global memory admission remain open. The full four-session1GiB/100k-resize corpus passed live/full/checkpoint-tail terminal and all54,360,410 cold-record comparisons. Thirty host SIGKILL/reopens passed exact frames/canonical tails and immutable source hashes (host-only p95=1367.28ms, p99=1731.69ms; not UI presentation). A real full-corpus two-boot VM run passed historical-frame/canonical-tail recovery and inventory equality; it does not run the complete cold model oracle inside the guest or certify provider/layout resume. See reports/long-history-full-20261017.md and benchmarks/results/vm-reboot-full-20261017/.

Actual24h soak remains failed/open. The latest3079.132s attempt hit sustained resize QueueFull; post-failure complete replay equality passes all four journals. A same-frozen-host one-hour reproducer with bounded rejection diagnostics is running at the path in AGENT_HANDOFF.md. Do not mistake it for24h acceptance. The60s producer-body rate passes but host-exit deadline/schedule lateness and total UI/GPU/provider memory gates remain open. Visible-cell selection, explicit bounded paste and mouse wire input are integrated/tested, while native gestures/clipboard, full hierarchy/tiling UI, Git/worktrees, revisioned context/adapters and release gates are incomplete. Stage0 remains open.

Assign explicit bounded ownership for parallel edits; preserve others' changes. Each modifying agent appends its own WORKLOG entry using scripts/log-change.py. Root integrates/tests/commits. Never commit runtime databases/journals, auth/history, build outputs or .upstream. Never automate the user's active desktop; read the incident/isolation reports first.

The app now uses GPUI commit `322cd509dcccaee1f6bc44725d270598ae8171ad`
through `gpui_platform`; ADR0007 and the target-filtered Linux license inventory
record the upgrade. Separate short runtime sockets and marked/leased replay-cache
maintenance are described in `reports/runtime-and-replay-cache.md`. Storage quota,
compression and global model heap admission remain independent unfinished work.
