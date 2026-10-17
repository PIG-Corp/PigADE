# Avoid copying discarded combining marks — 0.0.1-dev

`Cell::clear_wide` currently calls `Arc::make_mut` before discarding the
combining vector. With a shared CellExtra, this copies all combining marks
even though the resulting cell retains none. A wide-cell overwrite can thus
create a large temporary allocation unrelated to its short input.

First lock the existing behavior with regression tests for shared and unique
extras: cleared character/wide flag and marks; preserved other flags, colors,
underline and hyperlink identity; unchanged shared sibling; unchanged unique
Arc identity. Also cover absent and empty extras. Then, for shared extras,
construct only the retained attributes with an empty vector. The unique path
continues to drop its old vector in place. Do not change Cell layout, semantic
checkpoint bytes, ANSI handling or the sibling's contents.

Verify the regression before and after the repair, the full vendor library and
45 recordings, workspace tests, strict lint, formatting and release build.
This removes one discarded copy. It is not a whole-event reservation, allocator
bound, RSS measurement or completion of the global memory gate.
