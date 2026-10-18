# Project map — 0.0.1-dev

Read [AGENTS.md](../AGENTS.md), [SPEC](../SPEC.md), [DESIGN](../DESIGN.md) and the [handoff](AGENT_HANDOFF.md). New subagents must explicitly use gpt-6-luna. Read current git status and WORKLOG before editing.

| Location | Responsibility |
|---|---|
| Cargo.toml/Cargo.lock | Host/storage/terminal/layout workspace; locked dependencies; canonical first-party version |
| crates/terminal | Sole ANSI/core adapter, validated geometry, ordered timers, cell DTOs and full checkpoint envelope |
| crates/terminal/src/lib.rs listener | Retains all live response events FIFO and one last title update; ignores historical/external effects already excluded by the runtime profile |
| crates/terminal/examples/allocation_census.rs | Actual-model capacity-census diagnostics and bounded traversal cost; excludes process RSS and transient allocations |
| crates/terminal/examples/resource_preflight.rs, wide_clear_allocations.rs | Actual-parser nonmutating callback costs and scoped allocator-request comparison for shared wide-cell clearing; source-bound samples in benchmarks/results/resource-preflight-20261017 |
| crates/storage | CRC/sequence/generation journal with actual read-byte counters, fsync receipts, SQLite sole-writer schema14, launch locks, catalog CAS, optional template launch settings/engine options, durable template runs and CAS pane aliases |
| crates/storage/src/prompt.rs | Schema12 bounded revisioned prompts; CAS writes, UUID-preserving tombstones and idempotent exact-revision deletes, migration backup |
| crates/app/src/tab_close.rs | Frozen tab/layout/selection confirmation, exact request receipts, explicit unknown-result inspection and closed viewport cleanup |
| crates/layout/src/lib.rs, session-host/src/catalog.rs, storage/src/catalog.rs | Metadata tab removal rebases all placement snapshots, validates active-run dependencies and commits selection/layout/delete atomically; no CLI stop or schema bump |
| crates/app/src/prompt_flow.rs, prompt_commands.rs, prompt_ui.rs | Reusable prompt editor/library; exact draft selection/undo restoration, no terminal input, explicit clipboard copy |
| crates/storage/src/agent_profile.rs, catalog.rs, launch.rs | Bounded128-entry named launch-profile library with optional validated model/reasoning settings; revision CAS writes/deletes; schema13 keeps a durable v12 backup, legacy options default empty and old-host capability gates nonempty values |
| crates/storage/src/agent_profile_roster.rs | Schema14 independent roster CAS for profile order/pins; exact current membership, migration backup, atomic create/delete maintenance and no launch/placement side effects |
| crates/app/src/recipe_profiles.rs | Frozen pinned-first searchable profile chooser, unchanged-slot/capability guarded copy into template settings; no launch or live profile link |
| crates/app/src/profile_access_ui.rs | Validated pinned-profile rail and launcher, selected ready-tab gate and readonly preparation modal; existing profile launch protocol remains authoritative |
| crates/app/src/activity.rs, activity_commands.rs, activity_ui.rs | Bounded background-output/exit/error summaries, painter-bound read watermarks, socket-scoped coalesced persistence, cached badges and activity navigation; ADR0020 |
| crates/app/src/profile_roster.rs | Pure pinned-first projection, partition moves, validated mutation payload and stable selected-profile identity; absent capability preserves old-host order |
| crates/app/src/profile_flow.rs, profile_commands.rs, profile_ui.rs | Native profile editor/library with optional model/effort, exact dialog/request receipts, retained post-save edits, explicit review and unchanged live sessions |
| crates/storage/src/custom_command.rs; session-host/src/prepared.rs | Bounded exact executable/argv settings, canonical identity freeze and generic one-shot launch with no probe/context/resume; additive custom_cli_schema capability |
| crates/app/src/custom_cli.rs, custom_launch_ui.rs | Bounded shell-free argument lexer/formatter and readonly custom-command confirmation; reusable profile drafts keep literal argv across edits |
| crates/storage/src/archive.rs | Bounded PIGARC01 fragment-file primitive, complete-row validation and fsync receipts used by host-owned cold history |
| crates/layout | Platform-neutral tiling transactions, identity-free schema1 geometry templates, geometry/focus projection and persistent100undo/redo |
| crates/session-host | Independent Linux PTY actors, bounded input/output/IPC, durable helper launch, checkpoint/replay, indexed cold-history archive, catalog/layout/template-run routes and exact-generation asynchronous Stop |
| crates/session-host/src/workspace_git.rs, workspace_git_index.rs, workspace_git_commit.rs, workspace_git_commit_hooks.rs | Bounded Git snapshots/diffs, ticket-bound Stage/Unstage, and guarded native commit with original hooks/signing, exact ref publication proofs and durable nonretrying receipts |
| crates/app/src/workspace_git.rs, git_status_ui.rs | Dialog/workspace-bound snapshot and diff UI, ticket-bound Ctrl+S Stage/Unstage, Ctrl+Enter frozen commit review/message, persistent outcomes, Ctrl+R inspect/refresh and explicit Ctrl+Shift+R acknowledgement |
| crates/session-host/src/template_run.rs | Durable explicit template runs: freeze reviewed plans without launching, advance one CLI per command, reconcile existing plan/launch records without retry, and atomically commit final placement with completion |
| crates/app/src/template_flow.rs, template_ui.rs, template_commands.rs | Four optional launch presets, per-slot model/effort configuration and context/count/cwd review; user-driven prepare/advance/history/cancel flow |
| crates/session-host/src/panel_directory.rs | Bounded64-entry search pages over placed panes, matching alias, provider, frozen role/state and project/workspace/tab metadata without returning terminal frames |
| crates/app/src/navigation.rs, navigation_ui.rs | Dialog/query/editor-revision-scoped panel search receipts, alias editing, mouse/keyboard result ordering and atomic pane activation requests |
| crates/app/src/session_control.rs, session_control_ui.rs | Separate confirmed Stop for one frozen session/generation; one Stop submission, read-only reconciliation, exact-`exited` completion, and retained placement/history |
| crates/session-host/src/history.rs | Generation-bound PIGROW02 archive segments and fixed-slot index; data fsync → index fsync → PIGCP002 root; recovery tail uses a private derived cache |
| crates/session-host/src/history_view.rs | Immutable exact-watermark cold+hot normal history snapshots, bounded worker/LRU/TTL, Unicode text paging and incremental literal search |
| crates/app/src/catalog_flow.rs | Frozen-revision metadata rename/archive batches; archive never stops processes or edits placement |
| crates/app/src/transfer_flow.rs | Frozen source validation and bounded searchable Ready project/workspace/tab destination pages; shared exact empty/populated transfer construction, preserving no-follow selection |
| crates/app/src/pointer_layout.rs | Pure immediate modifier grab / 10px header threshold, nearest gutter target, relative grab retention, exact Move-tree preview and bounded latest-intent hover focus helpers; main.rs keeps per-session geometry slots, temporarily polls the focus-mode remainder, and provides clipped cross-tab drop previews with atomic TransferAndActivate |
| crates/app/src/history_view.rs | Snapshot identity, bounded page navigation and plain-text soft-wrap copy helpers; native readonly field UI is in product/main |
| crates/terminal/src/history_text.rs | Read-only UTF8-safe text projection from typed cold rows and the normal grid; no ANSI parser |
| crates/session-host/src/history_read.rs | Bounded asynchronous canonical-record reads; exact generation/sequence, no ANSI parsing or host-command-thread cold I/O |
| crates/session-host/src/replay_cache.rs | Leased, marked private replay caches and bounded fail-closed orphan collection; sources are preserved |
| crates/session-host/tests/query_backpressure.rs | Real PTY cyclic-wait regression, exact50k query FIFO and bounded-response degradation/raw-tail recovery |
| crates/session-host/src/metrics.rs | Bounded cumulative counters plus independent fixed-size actor/reader stage probe in liveSnapshot/List; no terminal content or input bytes |
| crates/session-host/src/poll.rs | Conditional bounded polling: exact panel generation or authoritative durable head, unchanged-cell reuse and optional metadata-only background activity; activity failure does not withhold frames |
| crates/session-host/src/generations.rs | Indexed launch-order pages, explicit historical pins and follow-head selection; metadata-only catalog CAS, no spawn |
| crates/session-host/src/recovery_diagnostics.rs | Bounded unavailable-artifact notices with escaped display basenames; no fabricated sessions or source changes |
| crates/session-host/src/request_decode.rs | Streaming Input-array preflight before internally tagged Serde buffering; bounded raw IPC frame remains a separate limit |
| crates/session-host/src/catalog.rs | Authoritative layout/projection validation and atomic persistence, including same-workspace pane TransferAndActivate with layout/selection CAS; explicit legacy projection import |
| crates/session-host/tests | Real PTY/reconnect/resize/kill/Stop/query-burst and durable placement scenarios |
| crates/storage/src/context.rs | Immutable scoped instructions/role/memory revisions with CAS, SHA-256/time/author and validated ordered effective snapshots; updates to running CLI remain separate |
| crates/storage/src/memory.rs | Lossless #/## source sections outside fenced code, bounded review metadata and explicit revision/hash-bound 32KiB UTF-8 selection |
| crates/app/src/memory_choice.rs | Number-entry selection and dirty-preview gating using host section identities; UI never reparses Markdown |
| crates/storage/src/conversation.rs | Immutable user conversation bindings, per-session CAS, current provider/ID uniqueness and schema6 migration with durable backup |
| crates/storage/src/launch.rs, catalog.rs | Schema7 launch-order ordinals allocated with intent/lock, indexed exclusive-cursor pages, durable pre-v7 backups and per-pane generation pins in WindowState |
| crates/storage/src/prepared_launch.rs | Create-only reviewed launch payloads; durable prepared → attempting → unconfirmed/unknown phase CAS |
| crates/session-host/src/prepared.rs | Review/start/session-context, exact-revision PrepareProfileLaunch and explicit PrepareResume routes; frozen engine options/argv/profile source with binding/context/cwd, unchanged exited-head checks and one durable attempt; resumed generations preserve layout and native resume leaves original model options unapplied |
| crates/session-host/src/resume_probe.rs, opencode_resume.rs | Bounded asynchronous exact-ID OpenCode metadata preflight through authenticated pure loopback server; frozen binding/head/executable/directory identity, no provider transcript parsing or IPC-supplied permit |
| crates/session-host/src/adapters.rs | Bounded version detection, versioned Claude/Codex startup arguments and exact-ID manual Codex0.162.1/OpenCode1.19.3 resume argv; unknown versions remain generic |
| crates/session-host/src/projects.rs, panel_launch.rs | Explicit new-folder Git initialization, repository identity, durable workspace/worktree preparation and prevalidated tab-scoped CLI launch |
| crates/session-host/tests/project_setup.rs | Real IPC create/reopen, existing-target preservation and bounded provider-version fixture |
| crates/app/src/chrome.rs, assets/icons | Shared dark/light chrome tokens and embedded original16-icon SVG family; terminal ANSI palette is separate |
| crates/app/src/appearance.rs, appearance_commands.rs, appearance_ui.rs | Schema1 local CAS preferences for chrome theme, dark/light/follow-app terminal palette, reduced motion and10–24 px font size; legacy files keep dark terminal/14px, no-op choices do not write |
| docs/design | Imagegen workspace/welcome reference images and exact prompts; design artifacts, not test screenshots |
| crates/app/src/product.rs | Appearance/header actions, always-visible open/create project controls, task/worktree sheet, detected CLI chooser, compact toolbar/pane menu, context review and editor |
| crates/app/src/setup_flow.rs | Exact-request project → first shared workspace/tab setup; stale replies do not advance setup or start a CLI |
| crates/app/src/text_edit.rs | Unicode grapheme/UTF-16 editing, grouped composition, bounded100-entry/2MiB undo, plain-string wire serialization |
| crates/app/src/edit_field.rs | Native GPUI wrapped text fields, cached shaping, caret/selection/IME geometry, horizontal and vertical reveal |
| crates/app/src/startup.rs | Private local host discovery/startup, version-derived paths, host-lock-mediated stale-endpoint recovery; explicit sockets never auto-start |
| crates/app/src/viewport.rs | Pure device-aligned canvas translation/reveal, scrollbars and thumb dragging; per-tab offsets persist independently of layout undo |
| crates/app/src/tiling.rs | Pure split-boundary resize helpers used by keyboard/mouse previews |
| crates/app | Actual native GPUI application; cell replica only, pure keyboard encoder and device-rounded grid rendering; separate compatible lockfile |
| crates/app/src/protocol.rs, main.rs | Client watermark merge and revision-gated UI state replacement; terminal font metrics drive grid geometry and ordinary PTY resize without process relaunch |
| crates/app/src/command_worker.rs | Explicit token-bound known-rejection acknowledgement, unknown lock, actual dispatch closure, unsent counts and resize cache; gates are exact session/generation targets |
| crates/app/src/palette.rs | Pure renderer default palettes; explicit host RGB/OSC overrides and xterm16–255 retain precedence, history freezes only actual per-row overrides |
| crates/app/src/cell_style.rs | Pure bounded device-pixel underline/strike recipes, wide/hidden/spacer policy and decorated-space foreground admission |
| crates/app/src/block_glyph.rs | Device-pixel solid Unicode block recipes; shades/ordinary text keep font rendering |
| crates/app/src/mouse_delivery.rs, main.rs admission | Bounded receipt reservations, optimistic mouse rollback and motion headroom; local saturation never latches input or retries bytes |
| crates/app/src/cursor.rs | Pure typed cursor geometry, wide-cell normalization, blink/reduced-motion and block glyph contrast; native paint/lifecycle in main.rs |
| crates/app/src/palette.rs | Bounded host-cell color resolver; typed sorted dynamic overrides, truecolor and unchanged default theme; no ANSI parser |
| crates/app/src/terminal_insert.rs, terminal_insert_commands.rs | Bounded POSIX file-path quoting and generation/mode-bound explicit paste review; native file-drop targeting and clipboard files, no FS read/upload/automatic Enter; existing input worker remains sole send route |
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

The catalog and tiling contracts are implemented headlessly. Product text history paging/search and an immutable colored normal-history pane viewport are integrated; horizontal history panning/cold reflow, quotas and whole-event/global memory admission remain open. The full four-session1GiB/100k-resize corpus passed live/full/checkpoint-tail terminal and all54,360,410 cold-record comparisons. Thirty host SIGKILL/reopens passed exact frames/canonical tails and immutable source hashes (host-only p95=1367.28ms, p99=1731.69ms; not UI presentation). A real full-corpus two-boot VM run passed historical-frame/canonical-tail recovery and inventory equality; it does not run the complete cold model oracle inside the guest or certify provider/layout resume. See reports/long-history-full-20261017.md and benchmarks/results/vm-reboot-full-20261017/.

Actual24h soak remains failed/open. The latest3079.132s attempt hit sustained resize QueueFull; post-failure complete replay equality passes all four journals. The same-frozen-host one-hour reproducer failed after1013.16s with input ACK timeout; bounded diagnostics are retained at the path in AGENT_HANDOFF.md and root-cause investigation is active. A20-minute instrumented run passed, then a7200s requested diagnostic failed after548.82s: child ACK write completed but live publication lagged; final raw journal/recovery includes the ACK. A new stage-instrumented7200s diagnostic is running on frozen610efac4; initial probes are available, final result pending (handoff). Do not mistake these diagnostics for24h acceptance. The60s producer-body rate passes but host-exit deadline/schedule lateness and total UI/GPU/provider memory gates remain open. Visible-cell selection, explicit bounded paste and mouse wire input are integrated/tested, while release gates remain open. Git/worktrees and other feature areas have partial implementations; none of these slices completes Stage0 or the product MVP.

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

[ADR0014](adr/0014-diff-bound-git-index-actions.md) records per-file index action durability and refusal boundaries. Git dialog control focus and pagination policy live in `crates/app/src/workspace_git.rs`; `git_status_ui.rs` renders the same control/page state.

[ADR0015](adr/0015-guarded-native-git-commit.md) records native commit publication guards, original hook delegation, signing, host-restart identity and Unknown receipts. Real IPC coverage is in `crates/session-host/tests/project_setup.rs`; existing isolated catalog fixture includes commit review and confirmation after its Stage/Unstage checks.

Native close protection: `crates/app/src/close_flow.rs` owns review/flush/deadline states; `close_ui.rs` binds WM close, modal input and ACK-driven pending resources; `dirty_documents.rs` inventories active/suspended authored drafts without including their text. The existing isolated context fixture tests WM_DELETE_WINDOW cancel/discard with an independent host. See [ADR0017](adr/0017-native-window-close-review.md).

`terminal/src/history_grid.rs` projects typed normal/cold rows to bounded styled cells; `session-host/src/history_view.rs` shares frozen grid/text snapshots and palettes. `app/src/scrollback.rs` owns one historical viewport's coalesced intent/request/lease state, and `scrollback_ui.rs` caches display cells, marks readonly state and handles safe return to live. Native history verification extends the existing isolated history fixture. [ADR0018](adr/0018-in-pane-cell-history.md) defines bounds and remaining width/persistence limitations.
