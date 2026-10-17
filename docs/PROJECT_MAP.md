# Project map — 0.0.1-dev

Read [AGENTS.md](../AGENTS.md), [SPEC](../SPEC.md), [DESIGN](../DESIGN.md) and the [handoff](AGENT_HANDOFF.md). New subagents must explicitly use gpt-6-luna. Read current git status and WORKLOG before editing.

| Location | Responsibility |
|---|---|
| Cargo.toml/Cargo.lock | Host/storage/terminal/layout workspace; locked dependencies; canonical first-party version |
| crates/terminal | Sole ANSI/core adapter, validated geometry, ordered timers, cell DTOs and full checkpoint envelope |
| crates/terminal/src/lib.rs listener | Retains all live response events FIFO and one last title update; ignores historical/external effects already excluded by the runtime profile |
| crates/terminal/examples/allocation_census.rs | Actual-model capacity-census diagnostics and bounded traversal cost; excludes process RSS and transient allocations |
| crates/terminal/examples/resource_preflight.rs, wide_clear_allocations.rs | Actual-parser nonmutating callback costs and scoped allocator-request comparison for shared wide-cell clearing; source-bound samples in benchmarks/results/resource-preflight-20261017 |
| crates/storage | CRC/sequence/generation journal with actual read-byte counters, fsync receipts, SQLite sole-writer schema10, launch locks, catalog CAS, optional template launch settings, durable template runs and CAS pane aliases |
| crates/storage/src/archive.rs | Bounded PIGARC01 fragment-file primitive, complete-row validation and fsync receipts used by host-owned cold history |
| crates/layout | Platform-neutral tiling transactions, identity-free schema1 geometry templates, geometry/focus projection and persistent100undo/redo |
| crates/session-host | Independent Linux PTY actors, bounded input/output/IPC, durable helper launch, checkpoint/replay, indexed cold-history archive, catalog/layout/template-run routes and exact-generation asynchronous Stop |
| crates/session-host/src/template_run.rs | Durable explicit template runs: freeze reviewed plans without launching, advance one CLI per command, reconcile existing plan/launch records without retry, and atomically commit final placement with completion |
| crates/app/src/template_flow.rs, template_ui.rs, template_commands.rs | Four optional launch presets, per-slot configuration and context/count/cwd review; user-driven prepare/advance/history/cancel flow |
| crates/session-host/src/panel_directory.rs | Bounded64-entry search pages over placed panes, matching alias, provider, frozen role/state and project/workspace/tab metadata without returning terminal frames |
| crates/app/src/navigation.rs, navigation_ui.rs | Dialog/query/editor-revision-scoped panel search receipts, alias editing, mouse/keyboard result ordering and atomic pane activation requests |
| crates/app/src/session_control.rs, session_control_ui.rs | Separate confirmed Stop for one frozen session/generation; one Stop submission, read-only reconciliation, exact-`exited` completion, and retained placement/history |
| crates/session-host/src/history.rs | Generation-bound PIGROW02 archive segments and fixed-slot index; data fsync → index fsync → PIGCP002 root; recovery tail uses a private derived cache |
| crates/session-host/src/history_view.rs | Immutable exact-watermark cold+hot normal history snapshots, bounded worker/LRU/TTL, Unicode text paging and incremental literal search |
| crates/app/src/catalog_flow.rs | Frozen-revision metadata rename/archive batches; archive never stops processes or edits placement |
| crates/app/src/pointer_layout.rs | Pure Dwindle tear-off/drop projection with relative grab retention and bounded latest-intent hover focus persistence helpers; integration in main.rs |
| crates/app/src/history_view.rs | Snapshot identity, bounded page navigation and plain-text soft-wrap copy helpers; native readonly field UI is in product/main |
| crates/terminal/src/history_text.rs | Read-only UTF8-safe text projection from typed cold rows and the normal grid; no ANSI parser |
| crates/session-host/src/history_read.rs | Bounded asynchronous canonical-record reads; exact generation/sequence, no ANSI parsing or host-command-thread cold I/O |
| crates/session-host/src/replay_cache.rs | Leased, marked private replay caches and bounded fail-closed orphan collection; sources are preserved |
| crates/session-host/tests/query_backpressure.rs | Real PTY cyclic-wait regression, exact50k query FIFO and bounded-response degradation/raw-tail recovery |
| crates/session-host/src/metrics.rs | Bounded cumulative counters plus independent fixed-size actor/reader stage probe in liveSnapshot/List; no terminal content or input bytes |
| crates/session-host/src/poll.rs | Conditional bounded polling: exact panel generation or authoritative durable head, one model per logical session, metadata-only replies when current |
| crates/session-host/src/generations.rs | Indexed launch-order pages, explicit historical pins and follow-head selection; metadata-only catalog CAS, no spawn |
| crates/session-host/src/recovery_diagnostics.rs | Bounded unavailable-artifact notices with escaped display basenames; no fabricated sessions or source changes |
| crates/session-host/src/request_decode.rs | Streaming Input-array preflight before internally tagged Serde buffering; bounded raw IPC frame remains a separate limit |
| crates/session-host/src/catalog.rs | Authoritative layout/projection validation and atomic persistence; explicit legacy projection import |
| crates/session-host/tests | Real PTY/reconnect/resize/kill/Stop/query-burst and durable placement scenarios |
| crates/storage/src/context.rs | Immutable scoped instructions/role/memory revisions with CAS, SHA-256/time/author and validated ordered effective snapshots; updates to running CLI remain separate |
| crates/storage/src/memory.rs | Lossless #/## source sections outside fenced code, bounded review metadata and explicit revision/hash-bound 32KiB UTF-8 selection |
| crates/app/src/memory_choice.rs | Number-entry selection and dirty-preview gating using host section identities; UI never reparses Markdown |
| crates/storage/src/conversation.rs | Immutable user conversation bindings, per-session CAS, current provider/ID uniqueness and schema6 migration with durable backup |
| crates/storage/src/launch.rs, catalog.rs | Schema7 launch-order ordinals allocated with intent/lock, indexed exclusive-cursor pages, durable pre-v7 backups and per-pane generation pins in WindowState |
| crates/storage/src/prepared_launch.rs | Create-only reviewed launch payloads; durable prepared → attempting → unconfirmed/unknown phase CAS |
| crates/session-host/src/prepared.rs | Review/start/session-context and explicit PrepareResume routes; frozen binding/context/cwd, unchanged exited-head checks and one durable attempt; resumed generations preserve layout |
| crates/session-host/src/adapters.rs | Bounded version detection, versioned Claude/Codex startup arguments and exact-ID manual Codex0.162.1 resume argv; other resume versions remain unsupported |
| crates/session-host/src/projects.rs, panel_launch.rs | Explicit new-folder Git initialization, repository identity, durable workspace/worktree preparation and prevalidated tab-scoped CLI launch |
| crates/session-host/tests/project_setup.rs | Real IPC create/reopen, existing-target preservation and bounded provider-version fixture |
| crates/app/src/chrome.rs, assets/icons | Shared neutral/lavender chrome palette and embedded original16-icon SVG family; terminal ANSI palette is separate |
| docs/design | Imagegen workspace/welcome reference images and exact prompts; design artifacts, not test screenshots |
| crates/app/src/product.rs | Always-visible open/create project controls, task/worktree sheet, detected CLI chooser, compact toolbar/pane menu, context review and editor |
| crates/app/src/setup_flow.rs | Exact-request project → first shared workspace/tab setup; stale replies do not advance setup or start a CLI |
| crates/app/src/text_edit.rs | Unicode grapheme/UTF-16 editing, grouped composition, bounded100-entry/2MiB undo, plain-string wire serialization |
| crates/app/src/edit_field.rs | Native GPUI wrapped text fields, cached shaping, caret/selection/IME geometry, horizontal and vertical reveal |
| crates/app/src/startup.rs | Private local host discovery/startup, version-derived paths, host-lock-mediated stale-endpoint recovery; explicit sockets never auto-start |
| crates/app/src/viewport.rs | Pure device-aligned canvas translation/reveal, scrollbars and thumb dragging; per-tab offsets persist independently of layout undo |
| crates/app/src/tiling.rs | Pure split-boundary resize helpers used by keyboard/mouse previews |
| crates/app | Actual native GPUI application; cell replica only, pure keyboard encoder and device-rounded grid rendering; separate compatible lockfile |
| crates/app/src/protocol.rs, main.rs | Client watermark merge and revision-gated UI state replacement; unchanged poll metadata avoids cloning frames/cells |
| crates/app/src/command_worker.rs | Explicit token-bound known-rejection acknowledgement, unknown lock, actual dispatch closure, unsent counts and resize cache; gates are exact session/generation targets |
| crates/app/src/cell_style.rs | Pure bounded device-pixel underline/strike recipes, wide/hidden/spacer policy and decorated-space foreground admission |
| crates/app/src/block_glyph.rs | Device-pixel solid Unicode block recipes; shades/ordinary text keep font rendering |
| crates/app/src/mouse_delivery.rs, main.rs admission | Bounded receipt reservations, optimistic mouse rollback and motion headroom; local saturation never latches input or retries bytes |
| crates/app/src/cursor.rs | Pure typed cursor geometry, wide-cell normalization, blink/reduced-motion and block glyph contrast; native paint/lifecycle in main.rs |
| crates/app/src/palette.rs | Bounded host-cell color resolver; typed sorted dynamic overrides, truecolor and unchanged default theme; no ANSI parser |
| crates/app/src/input.rs | Pure keyboard/atomic explicit paste encoding and local UTF16 composition; no ANSI parser or implicit shared-prompt delivery |
| crates/app/src/controls.rs | Exact held-modifier panel bindings, EN/RU normalization and bounded atomic UI-local controls.json; default Ctrl, optional Alt/Super/Ctrl+Alt/prefix-only |
| crates/app/src/mouse.rs, selection.rs | Pure mode-aware mouse encoding, bounded wheel accumulation and frame-bound Unicode cell selection; native event/clipboard integration remains in main.rs |
| docs/reports/ime-composition.md, docs/research/linux-ime-reset.md | Local input/overlay evidence and the unverified native Linux reset boundary |
| crates/app/src/font.rs, assets/fonts | Embedded unique-family OFL derivatives, exact manifests, disabled liga/calt and fixed JP CJK fallback policy; unsupported platform fallback is unpinned |
| crates/app/bootstrap.sh, patches/gpui-x11-ime, patches/xim-preedit | Pinned GPUI/XIM archives, exact focus/style and synchronous preedit-reply patches; ignored .upstream sources; test-xim runs actual transport unit tests |
| scripts/apply-gpui-patches.py, test_gpui_xim_focus.py | Bounded locked patch pre/postimage guard and exact embedded XIM lifecycle tests |
| scripts/prepare-x11-ime-sdk.py, isolated-ime-x11.py, x11-ime-input.c | Signed test-only Xvfb/Fcitx/Pinyin SDK, verified private namespaces/display/window ownership and fixed native Pinyin/EN/RU/CLI byte proofs; no active desktop access |
| crates/app/evidence/conditional-poll-20261017 | Fresh isolated software-rendered UI reopen evidence; not GPU or platform certification |
| vendor/vte |0.15.0 maintained UTF8/full-parser-state/OSC-bound patches, original licenses |
| vendor/alacritty_terminal |0.26.0 explicit Term/Grid export/import, allocation-aware pools, borrowed row export and runtime normal-history retirement notifications |
| vendor/alacritty_terminal/src/term/checkpoint/borrowed.rs | Validated streaming serializer with exact schema2 pool/ring order; borrowed payloads; owned DTO remains compatibility oracle |
| docs/research/terminal-admission-plan.md | Remaining whole-event/global allocation work and resource-refusal contracts; not an implemented budget |
| docs/research/terminal-payload-copy-bound.md | Combining/link/title logical-copy formulas, current COW amplification and missing capacity/aggregate contracts; does not authorize admission |
| tests/fixtures/terminal | Boundary/UTF8 reproducers; real captured upstream fixtures also live under vendor tests/ref |
| benchmarks/results | Hardware manifest, component timings, failed-before and current 5s/60s cold-history, host-memory, idle-resource and long-history smoke evidence |
| scripts/isolated-render.py | Tests actual app inside verified private namespaces, software renderer and private display; no active-desktop access/input injection |
| scripts/prepare-terminal-fonts.py | Offline deterministic alias derivation with pinned fontTools; verifies source hashes, glyph/metrics/cmap preservation and byte-identical regeneration |
| scripts/prepare-platform-sdk.py | Signed local-only Weston/Lavapipe extraction; no system installation |
| scripts/long-history.py, crates/session-host/examples/history_corpus.rs | Frozen-binary, durable-filesystem production-model long-history generator/runner; versioned SHA-bound sidecars map ordered events to logical timestamps; these are not elapsed soak |
| scripts/recover-corpus.py | Owned host SIGKILL/reopen measurement with exact IPC peer, source hashes, frames and checkpoint-tail counters; no UI-present claim |
| scripts/codex-conversation.py, scripts/test_codex_conversation.py | Bounded native Codex exec/exact-ID resume continuity diagnostic; direct existing auth, no credential/history copy, redacted evidence; not product auto-resume |
| scripts/provider-pty.py, scripts/test_provider_pty.py | Frozen-host offline provider startup/resize checks inside disposable isolated homes; redacted outcomes in reports/provider-pty-20261017.md and benchmarks/results/provider-pty-20261017 |
| scripts/soak-pty.py | Real-time synthetic PTY soak, bounded QueueFull/ACK diagnostics, numeric producer milestones and identity-revalidated wait-channel/pidfd cleanup; failed runs and one-hour instrumented reproducer do not pass24h |
| scripts/prepare-vm-sdk.py, scripts/vm-reboot.py, scripts/test_vm_reboot.py | Signed local QEMU SDK and isolated two-boot recovery; optional completed-corpus staging, source inventories and exact checkpoint-tail reference; provider resume and power-loss proof are out of scope |
| docs/adr/research/reports/compatibility | Decisions, evidence, reviews, exact CLI support gaps and next recovery work; see `reports/cold-history-integration.md` and `reports/vm-reboot.md` for current recovery scope and measurements |
| VERSION/CHANGELOG/WORKLOG | Version, coherent milestone history, every agent's own append-only engineering log |

Separate control applications were removed by user request. The initial Electron comparison remains only in Git history6b9ed5c and a historical research note. There is no Electron/npm runtime in PigADE.

The catalog and tiling contracts are implemented headlessly. Product text history paging/search is integrated; full terminal scrolling, quotas and whole-event/global memory admission remain open. The full four-session1GiB/100k-resize corpus passed live/full/checkpoint-tail terminal and all54,360,410 cold-record comparisons. Thirty host SIGKILL/reopens passed exact frames/canonical tails and immutable source hashes (host-only p95=1367.28ms, p99=1731.69ms; not UI presentation). A real full-corpus two-boot VM run passed historical-frame/canonical-tail recovery and inventory equality; it does not run the complete cold model oracle inside the guest or certify provider/layout resume. See reports/long-history-full-20261017.md and benchmarks/results/vm-reboot-full-20261017/.

Actual24h soak remains failed/open. The latest3079.132s attempt hit sustained resize QueueFull; post-failure complete replay equality passes all four journals. The same-frozen-host one-hour reproducer failed after1013.16s with input ACK timeout; bounded diagnostics are retained at the path in AGENT_HANDOFF.md and root-cause investigation is active. A20-minute instrumented run passed, then a7200s requested diagnostic failed after548.82s: child ACK write completed but live publication lagged; final raw journal/recovery includes the ACK. A new stage-instrumented7200s diagnostic is running on frozen610efac4; initial probes are available, final result pending (handoff). Do not mistake these diagnostics for24h acceptance. The60s producer-body rate passes but host-exit deadline/schedule lateness and total UI/GPU/provider memory gates remain open. Visible-cell selection, explicit bounded paste and mouse wire input are integrated/tested, while native gestures/clipboard, full hierarchy/tiling UI, Git/worktrees, revisioned context/adapters and release gates are incomplete. Stage0 remains open.

Assign explicit bounded ownership for parallel edits; preserve others' changes. Each modifying agent appends its own WORKLOG entry using scripts/log-change.py. Root integrates/tests/commits. Never commit runtime databases/journals, auth/history, build outputs or .upstream. Never automate the user's active desktop; read the incident/isolation reports first.

The app now uses GPUI commit `322cd509dcccaee1f6bc44725d270598ae8171ad`
through `gpui_platform`; ADR0007 and the target-filtered Linux license inventory
record the upgrade. Separate short runtime sockets and marked/leased replay-cache
maintenance are described in `reports/runtime-and-replay-cache.md`. Storage quota,
compression and global model heap admission remain independent unfinished work.

The pinned GPUI X11 focus correction now passes one native Pinyin commit in the actual debug/release app; a settled clean-upstream baseline lacked an active IC. See [ADR0010](adr/0010-gpui-x11-ime-focus-lifecycle.md) and [source-bound report](reports/x11-ime-20261017.md). The callback-only X11 update now has one visually verified inline preedit and protected-hover sequence; native target/generation barriers, complete caret placement and the full platform gate remain open.

The response-priority actor wait cycle is reproduced and repaired with bounded FIFO replies while draining PTY output; [report](reports/query-response-backpressure.md). This repair does not establish a cause for the separate soak ACK timeout.

Actual fixed X11 EN/RU/CLI keyboard now passes after a reproduced Alt-printable loss repair; the same release passes Pinyin again. See [keyboard evidence](reports/x11-keyboard-20261017.md). Full native target-generation barriers remain open; [source review](research/ime-generation-barrier-next.md) demonstrates why reused XIM IC IDs are insufficient.

`crates/app/src/command_worker.rs` owns per-target known/unknown command gates
and resize cache invalidation. `mouse_delivery.rs` owns the bounded speculative
mouse state/host-admission receipt ledger and explicit release-cleanup plans.

`crates/app/src/wake_signal.rs` supplies one coalesced pending bit and one
cancellable waiter; IPC state/receipt notifications use it without new parser
state. Private fixed-key latency samples and their explicit limits live in
[the input latency report](reports/input-latency-x11-20261017.md).

`session-host/checkpoint.rs` now owns a single-flight snapshot/persistence permit
and one background writer, shared with synchronous final publication. Actor
`CheckpointProgress` services bounded completion/busy retries; raw journal and
history commits remain synchronous. ADR0011 and the async-checkpoint report
describe metric and recovery limits.

`scripts/prepare-wayland-ime-sdk.py`, `isolated-ime-wayland.py` and the fixed
`wayland-ime-input` C helper supply signed test-only Sway/Fcitx payloads and a
verified private Wayland-v3 fixture. Actual one-fixture success, failed attempts,
source hashes and visual captures are recorded in
[the Wayland IME report](reports/wayland-ime-20261017.md).

`session-host/src/changes.rs` supplies coalesced incarnation/revision tokens;
WaitForChange bypasses serialized command dispatch within bounded peer-auth IPC.
The app command and Poll threads are independent; optional `publication_trace.rs`
records finite numeric milestones. See ADR0012 and
[notification measurements](reports/host-notification-20261017.md).

`session-host/src/journal_policy.rs` separates exact legacy/resource-refusal journal
policies from unchanged terminal checkpoint schema2. `resource_refusal_tests.rs`
proves complete-prefix and every-cold-record behavior; storage kind5 seals writes
and typed receipts protect checkpoint watermarks. This is reader/storage support,
not live memory admission; see ADR0013 and the resource-refusal report.
