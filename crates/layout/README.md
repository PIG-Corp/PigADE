# pigade-layout — 0.0.1-dev

Pure, platform-neutral tiling transactions. No PTY, process identity, provider data,
I/O, UI framework, or terminal parser is present. Passing these tests does not
certify the native UI, storage durability, or any operating system.

## Integration

`LayoutState` holds a `BTreeMap<TabId, TabLayout>`. The catalog owns project and
workspace membership; the layout model owns placement across those tabs.
All IDs are UUIDs. `Node` is a pane or a split with stable split ID, explicit
`Axis`, normalized positive weights, and at least two children. Call
`LayoutModel::new` to validate persisted state, or `from_history` for a versioned
history. Never traverse deserialized trees before validation.

`Command` supports empty-tab insertion, four-way dock/split, move, swap, transfer,
close, normalized resize, adjacent-boundary resize, equalize, rotate, focus and
focus mode. Equalizing two neighbors is `ResizeBoundary { fraction: 0.5, .. }`.
A transfer detaches and inserts within one validated snapshot; a failed command
cannot partially modify either tab. Empty/single-child splits collapse.
Caller-supplied split UUIDs make preview and commit deterministic.

`preview(command)` returns a state without mutation. For a durable host operation,
clone the model, apply the command to that candidate, serialize its
`export_history()` into the host metadata transaction, then publish the candidate
only after commit. Use the same candidate approach for undo/redo. The model's
`apply`, `undo`, and `redo` do not themselves write durable data.

History retains 100 completed placement transactions, including both sides of
cross-tab transfers. `LayoutHistory` schema 1 serializes current state, undo, and
redo. Import validates all states, history capacity and placement continuity;
focus clicks can occur between transactions without consuming undo entries or
invalidating redo. `add_tab_metadata` adds a catalog-owned empty tab to the current
state and every history snapshot, preserving undo/redo without making catalog tab
creation an undoable placement edit. Undo restores placement and pane identity only: it cannot
restart a stopped process, repeat input, or recreate a deleted worktree.

## Geometry and focus

`LayoutState::project(tab, viewport, minimum, gap)` returns `CanvasGeometry` with
pane rectangles in stable depth-first order and split-divider rectangles. The UI
provides minimums derived from 40 columns × 8 rows plus chrome and actual font
metrics. Small viewports expand the scrollable canvas to preserve minimum sizes;
window resize never modifies the tree, axes, order or weights. The caller clamps
drag boundary fractions against available child minimums before committing.
Focus mode projects only the focused pane; the exact underlying tree remains.

`CanvasGeometry::neighbor` chooses a pane in the requested center half-plane,
prioritizes perpendicular overlap, then nearest edge, nearest center and stable
tree order. It does not wrap. The UI converts a neighbor into `Focus`, `Move`, or
`Swap`; no ordinary CLI key handling belongs to this crate.

State validation limits 256 tabs, 4,096 nodes and depth 64. Geometry rejects
invalid/overflowing logical bounds rather than emitting zero terminal sizes.
Host IPC still needs its own byte/allocation limits before serde decoding. JSON
readers use serde_json `float_roundtrip` to preserve fractional weights exactly.
The host preflights encoded history against its enabled JSON recursion and size
limits before committing; model-valid depth alone is not proof of readable JSON.

## Checks

```sh
cargo test -p pigade-layout
cargo clippy -p pigade-layout --all-targets -- -D warnings
cargo fmt -p pigade-layout --check
```

Tests cover four-way dock, move/swap/collapse, atomic preview and failed transfer,
minima, focus projection and ties, explicit resize/rotate, 100 undo/redo,
history reopen/invalid imports and 8,000 deterministic transactions across eight
panes. These headless model checks make no visual or platform runtime claim.
