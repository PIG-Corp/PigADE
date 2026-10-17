# Local IME composition — 0.0.1-dev

The native canvas currently forwards committed text once, but returns the whole
preedit for every UTF-16 range, reports a fixed zero selection, ignores marked
replacement ranges, and never paints the composition. This is an incomplete
input-method bridge, not certified IME support.

Repair the composition document only; terminal cells remain authoritative in
the host. Retain the platform's replacement and selection ranges in UTF-16,
normalize them to scalar boundaries, expose actual queried ranges, and commit
the local document exactly once. No operation edits already delivered PTY input.
Cancellation/focus management discards local preedit and pending Meta state.
Paint a clipped local overlay at the acknowledged terminal cursor; do not
resize, scale or modify the underlying grid. Candidate geometry follows that
overlay. The API reference is the pinned Apache-licensed GPUI example
`crates/gpui/examples/input.rs` and `crates/gpui/src/input.rs`; no GPL terminal
implementation is imported.

Verify UTF-16 surrogate boundaries, partial replacement, selection, cancellation,
single commit/Meta delivery and unchanged ordinary EN/RU input with pure tests.
Then build/test/Clippy the actual app and inspect its overlay inside the verified
private compositor. This does not certify a real IBus/Fcitx input method,
Wayland/X11 protocol interaction or cross-platform IME.
