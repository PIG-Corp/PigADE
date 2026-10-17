# ADR0004 — One durable layout history

Date:2026-10-17. Status: accepted; native UI integration implemented, full MVP acceptance open.

The pure layout model owns placement and100-step undo/redo across tabs. Catalog owns project/workspace/tab membership. A cross-workspace transfer must remain one transaction and cannot be represented by independent per-tab undo stacks.

Store one schema-versioned LayoutHistory document plus current per-tab projections in the existing sole SQLite writer. Every edit loads and validates the document, applies to a candidate, checks membership/projections and exact JSON roundtrip, then commits document, changed tabs and selection through CAS revisions in one transaction. Publish only the committed result. Unknown/malformed state remains intact and yields an explicit error. Metadata tab creation rebases that empty tab through history without consuming placement undo; catalog membership cannot disappear through undo. Close/undo affects placement only and never restarts processes.

A bounded2MiB history and4MiB catalog envelope prevents unbounded IPC growth. Model IDs/depth/node/history limits still apply; normal serde recursion protection remains enabled. A structurally valid but JSON-too-deep candidate is rejected before persistence. The existing serde_json float_roundtrip feature preserves exact fractional split weights; default float parsing reproduced a one-ULP persistence mismatch in a new regression. Both application and host enable it.

Schema3 upgrades v1/v2 only after SQLite-generated consistent backups, including committed WAL pages, are checked and fsynced. Launch locks and original backup data survive. A reader rejects a newer schema. No raw input/credentials are stored in these catalog documents.

Headless validation covers deterministic invariant sequences, cross-workspace CAS rollback, saved50undo/redo after reopen, and a real shell unchanged across60 transfers plus host restart. This does not certify drag/drop, accessibility, scale or the finished application hierarchy.

## Saved geometry layouts — 2026-10-18

Schema8 added a bounded library (128 records) on the sole catalog writer, with
CAS save/delete and the existing durable migration backup. `GeometryTemplate`
schema1 stores axes, weights and anonymous leaf slots, omitting pane/split UUIDs,
PIDs, credentials and conversation identities. Applying it requires an exact
current pane count; depth-first order assigns the existing stable pane IDs to
slots. Focus is retained and focus mode exits. The regular layout transaction
creates one undo step and preserves other tabs, CLI processes and generations.
Identical geometry outside focus mode is a no-op. The UI captures geometry when
opening Save, and never silently replaces an existing named template.

Schema9 extends those records with optional per-slot `LaunchSettings` and adds
bounded durable template-run records. Missing launch settings are valid and
preserve geometry-only templates. Four UI presets cover one agent, implementer
plus reviewer, two agents plus shell, and four panels. Configuration does not
replace ordinary pane/CLI creation or require a launch form. Existing-tab
geometry application remains placement-only: it preserves pane/process identity
and never launches a CLI.

A template run freezes its geometry and launch plans against the selected
workspace and saved-template revision (or identity-free custom recipe). Prepare
is review-only: it creates neither a tab nor a process. The review exposes the
new-process count, workspace and effective working directory, plus each slot's
provider and frozen context. Context delivery is explicit; memory, credentials,
provider conversation IDs and user configuration are not copied. Every later
advance is a user-authorized step and starts at most one CLI. Stored plan IDs,
generations and run bindings are allocated before persistence and remain fixed.
Attempting/unknown outcomes stay locked and cannot be retried; reopening does
not auto-advance. Cancel prevents later advances but does not stop launched
processes. Final geometry placement and run completion commit atomically through
the catalog writer; normal placement undo applies. Thus a crash can reconcile
an already-started generation or refuse to proceed, never spawn it a second time.

Metadata rename/archive is separate from placement undo. Archive changes
visibility and selection only; restore must retain the archived cwd/mode and
pass the host's original checkout/Git common-directory check. An older host
without the optional launch-schema capability reports launch-template support
unavailable; the UI does not restart it.

## Pane aliases and palette activation — 2026-10-18

Schema10 adds a separate revisioned `pane_labels` table. Aliases are bounded to
128 Unicode scalar values and512 UTF-8 bytes and reject control characters or
surrounding whitespace; an empty label is a retained tombstone that clears the
override and returns the displayed title to the CLI/OSC title. These labels do
not modify terminal title state, launch snapshots/context, process generations,
placement, or layout undo. Rename is compare-and-swap and only applies while the
logical pane remains placed in a Ready workspace. An older host omits the
optional pane-label catalog capability, so the UI leaves rename/search disabled
without restarting the host.

Panel search is read-only and returns at most64 entries per page for currently
placed panes in Ready workspaces. It matches alias/display title, provider,
frozen role, launch state, project, workspace task and tab name. The palette
provides explicit next-page and return-to-first-page actions; mouse and
arrow/Enter use the same visible ordering. Requests are correlated to the
palette dialog, query and editor revision so a stale response cannot replace a
newer search. Activation rechecks the current layout revision and pane-to-tab
membership, then atomically updates focused pane, tab projection and selected
project/workspace/tab; a focus-only change is excluded from placement undo and
does not start a process. Existing generation pins are preserved.
