# ADR0016: Profile roster is independent metadata

Status: accepted for 0.0.1-dev.

A reusable profile describes future launches. Pinning and list order must not
change its launch revision, prepared snapshots, pane layout or process identity.
SQLite schema14 adds a singleton schema1 roster with its own monotonic revision,
a permutation of all current profile IDs and a unique subset of pinned IDs.
Existing databases receive a durable pre-v14 backup before migration; the
initial order matches the existing profile rowid order and starts unpinned.

An explicit host command checks the roster revision and exact membership inside
one immediate transaction. Profile creation appends its ID; deletion removes it
from order and pins in the same catalog transaction. Content edits leave the
roster unchanged. Invalid stored membership is rejected before profile changes,
not repaired implicitly. Bounds are 128 IDs, 8192 bytes per stored JSON list and
SQLite integer revisions; overflow refuses writes. Missing/corrupt metadata
never silently drops profiles. No new dependency, filesystem action or CLI
process is involved in a roster edit.

The UI projects pinned profiles first, retaining canonical order within each
group. Move changes only one group's order; it does not implicitly pin/unpin.
Selection follows the profile UUID. Late responses cannot reopen a replaced
library. A failed or uncertain write requires explicit read-only refresh;
mutations are not automatically retried. Old hosts omit the optional roster and
retain their original library order with new controls unavailable.

This follows the profile pin/order contract in the user's BridgeMind reference,
while keeping PigADE's profile, session and placement identities separate.

Evidence: storage tests cover migration/backups, membership, CAS, corrupt-state refusal, atomic profile lifecycle and integer overflow. Real IPC checks host restart and metadata preservation. The existing isolated release catalog fixture03 verifies keyboard pin/reorder and list reopen with unchanged terminal models/processes, followed by the existing Git/tab scenarios; root reviewed the actual library captures. These checks do not certify native Wayland, screen-reader operation or full product acceptance.

Profile access UI extension: validated roster pins also populate a bounded sidebar rail and launcher shortcuts. Missing/invalid roster never invents quick-launch IDs; the ordinary library remains accessible. Quick selection creates a UI preparation scope and uses the existing exact profile/revision/tab request receipt before ordinary launch review. Cancel/stale scope discards the preview without starting or retrying a process. Copying a profile creates an independent unsaved UUID/settings draft; it neither copies roster pins nor changes the source revision. No additional host/storage schema is needed for these presentation and draft operations.
