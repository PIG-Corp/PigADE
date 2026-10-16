# Foundation review — 2026-10-17

Read-only production review by `/root/foundation_review`, during concurrent implementation. This report records the inspected pre-fix state; later root/performer fixes require fresh verification. No production source or tests were written. The only review-owned edits are this report and its WORKLOG entry.

## Scope and verdict

Reviewed six primary files: `crates/session-host/src/lib.rs`, `crates/session-host/src/catalog.rs`, `crates/session-host/src/checkpoint.rs`, `crates/storage/src/catalog.rs`, `crates/storage/src/launch.rs`, and `scripts/isolated-render.py`. Also inspected the terminal timer, journal watermark, layout history, IPC framing, and relevant integration-test contracts. Checked SPEC §§8, 10–13, AGENTS.md and the desktop incident report.

**REQUEST CHANGES: two HIGH findings; no CRITICAL finding established.** These are concrete availability/recovery defects, not evidence that durable terminal bytes were lost or a duplicate process was spawned.

## HIGH — schema migration leaves a valid v2 catalog unreadable by the host

Locations: `crates/storage/src/catalog.rs:231` (`migrate_layout_document`, inserting null); `crates/session-host/src/catalog.rs:157` (`load_layout`, rejecting null with existing tabs).

The v2→v3 migration durably upgrades the schema and preserves existing tab projections, but initializes the authoritative global layout document to `null`. The host rejects that combination. Every Catalog read fails, and CatalogApply/LayoutEdit also call the rejecting reader before any repair can occur. A backup exists, but the normal API cannot access or update the preserved catalog.

Confirmed with a real freshly built host and valid projection, not the arbitrary JSON used by the storage-only migration fixture:

1. Start host in a fresh owned temporary directory. Use CatalogApply to create one valid project, workspace and empty tab. The host stores the exact projection `{"root":null,"focused":null,"focus_mode":false}`.
2. Stop this owned host. Through SQLite, execute `DROP TABLE layout_revisions; PRAGMA user_version=2;` and commit. This recreates the genuine v2 boundary without modifying the tab payload.
3. Restart host. Catalog returns `catalog tabs exist without authoritative layout history`.

Minimal regression shape: seed valid host metadata; close the store; drop the global table and set schema2; reopen; exercise the host's `read_metadata`, not only `MetadataStore::catalog()`.

Required repair: an explicit migration from validated v2 tab projections to the versioned initial history, preserving IDs/focus and the durable pre-migration backup. Validate every projection and roundtrip before committing. Unknown or malformed documents must remain preserved with an actionable error; do not silently reset them or accept arbitrary JSON. Root is already coordinating this repair.

## HIGH — committed catalog can exceed the IPC wire limit and become inaccessible

Locations: `crates/storage/src/catalog.rs:552` (raw database-byte accounting), `crates/storage/src/catalog.rs:280` (commit before response encoding), `crates/session-host/src/lib.rs:110` (8MiB encoded limit), `crates/session-host/src/ipc_server.rs:150` (serialization failure closes the connection).

The 4MiB catalog guard counts raw text/layout bytes, whereas the 8MiB IPC guard counts the complete JSON response. Escaping and structural metadata amplify the response. A valid CatalogApply can commit a state that no Catalog response can encode. The initiating client receives EOF after the mutation committed; subsequent Catalog reads receive EOF as well.

Confirmed with a real freshly built host in a separate owned temporary directory:

1. Create one valid project.
2. In separate CatalogApply calls, add Ready workspaces with unique UUIDs and valid paths/owner, each task equal to `"Task" + "\u0001" * 65532`. This is nonempty, contains no NUL, and is within the 65,536-byte task limit. Supply empty projects/tabs arrays in each batch.
3. The 22nd call closes without a response. SQLite confirms all 22 workspaces committed, totaling only **1,441,792 raw task bytes**, below the catalog bound.
4. A later Catalog request also closes without a response.

Required repair: enforce the actual bounded encoded response contract before committing, or introduce a deliberately bounded paged protocol. Preserve explicit errors; do not truncate the manifest or substitute an empty catalog. Verify the rejected write rolls back, the previous catalog remains readable, and errors remain valid IPC responses. If independently opening an already oversized legacy catalog is supported, its explicit recovery path needs coverage too.

## Verified contracts and remaining gaps

- The inspected actor appends/fsyncs each Output/Resize/SyncTimeout/Exit before applying and publishing that event. EOF checks process exit nonblockingly, keeping priority Stop serviceable. Dirty idle state and resize-only events participate in checkpoint cadence.
- Placement operations use one global CAS revision and commit per-tab projections with history in one SQLite transaction. Raw placement writes through CatalogApply are rejected. Transfer/undo does not invoke the launch path.
- `verify_recovery` compares complete checkpoint bytes plus frames and rejects storage warnings or a missing fast path. Its documented immutable-journal precondition remains caller responsibility; no immutable-file lock was established here.
- The checkpoint fallback branch at `lib.rs:280` logs a rejected watermark only to stderr rather than adding that reason to `View.storage_error`. No practical corrupt-file case for this branch was reproduced in this review; preserve it as a diagnostics follow-up, not a third confirmed blocker.
- `isolated-render.py` launches the actual app through `bwrap --unshare-all --clearenv`, private runtime/proc/dev/tmp mounts and software rendering. It verifies namespace changes, absence of active desktop/device paths, and compositor peer identity before app launch. It has no input injection or active-display fallback. **Only static review and Python compilation were performed; this reviewer did not execute a compositor, window, input, monitor command, or desktop configuration change.**
- Long-history/24h/VM reboot/real GPU, IME, clipboard, fractional scaling and accessibility acceptance remain open. Short tests do not certify those gates. No additional control application was introduced.

## Fresh diagnostics

- `cargo build -p pigade-session-host --bin pigade-session-host`: passed.
- `cargo test -p pigade-session-host -p pigade-storage --lib`: **33 passed**.
- `cargo clippy -p pigade-session-host -p pigade-storage --all-targets -- -D warnings`: passed.
- `cargo test -p pigade-session-host --test recovery --test slow_input`: **8 passed**, including real PTY resize/replay, priority Stop after EOF, quiet/resize-only checkpoint cadence, cross-workspace CAS/undo/reopen and saturated input.
- `python3 -m py_compile scripts/isolated-render.py`: passed.
- `git diff --check`: passed at review time.
- Two independent headless dynamic probes reproduced the HIGH findings above. All processes were owned by the probes and terminated with their exact Popen handles; temporary directories were removed. No broad process-argument or provider-private-file inspection occurred.

## Independent follow-up — both blockers repaired

**Latest verdict: APPROVE the reviewed fixes and output coalescing slice. Zero unresolved CRITICAL/HIGH findings in this follow-up scope.** This does not approve the overall stack or MVP acceptance gates. The initial findings above remain as historical evidence.

Reviewed `migrate_legacy_layout` (`crates/session-host/src/catalog.rs:23`), Host::open/Response notice propagation, `JsonByteCounter` and pre-commit shared reader validation (`crates/storage/src/catalog.rs:705`), and `coalesce_output` (`crates/session-host/src/lib.rs:655`).

Fresh dynamic probes against the rebuilt actual host:

- Created a valid project/workspace/tab, then inserted a **nonempty pane** through LayoutEdit. Stopped the owned host, dropped only `layout_revisions` and set schema2, then restarted. Catalog returned successfully, with the exact original projects/workspaces/tabs/selection (including pane ID and active focus) and `legacy layout imported; previous schema stored no undo history`. Verified the backup's user_version2 and original tab projection. A further host restart returned the same valid catalog without repeating the notice.
- Repeated the original escaping-growth scenario for all22 attempts. Attempts1–18 committed; attempts19–22 each returned an explicit `encoded JSON byte bound` error. After every rejected mutation, SQLite row count remained18 and a fresh Catalog request succeeded with the exact previous catalog. No EOF, silent truncation or post-commit oversized state occurred.

Coalescing review: only adjacent already-queued Output bytes are merged, up to16KiB. The first non-Output or oversized next Output is retained intact in the single deferred slot. The actor consumes that slot with `take()` before calling the coalescer; when an ordered timeout preempts an action, the coalescer receives SyncTimeout and leaves its deferred action untouched. Resize/EOF/ReadError cannot be crossed by the batch, and the helper never waits for future output. No byte loss, deferred overwrite, duplicate event application or unsafe fallback was found. The two new unit regressions cover adjacent output, size boundaries, Resize/EOF barriers and an empty queue. A synthetic queued SyncTimeout/ReadError barrier case is not separately asserted, but follows the same non-Output branch; the existing live timer/replay tests remain useful evidence.

Fresh follow-up checks: host binary build passed; **40 host/storage unit tests passed**; scoped all-targets clippy passed; **8 real PTY integration tests passed** after batching (1000resizes/replay, quiet/resize-only checkpoints, EOF Stop, saturated input, helper gate, host kill/recovery, UI-client reopen, cross-workspace undo/CAS). `git diff --check` passed. No GUI/compositor/input or desktop configuration command was executed. The lead owns the forthcoming60s run and longer acceptance evidence; no such run is claimed here.
