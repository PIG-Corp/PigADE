# Terminal mouse input — 0.0.1-dev

The app encodes pointer events from acknowledged host mode bits. It does not
parse terminal output. DECSET1000 click,1002 drag,1003 any motion,1006 SGR and
1005 UTF-8 coordinate encoding are covered. Legacy coordinates outside their
representable range are refused rather than wrapped. A release outside that
range uses the last reported cell. Shift suppresses new reports; an already
reported press still receives its matching release. Local Shift selection and visible-frame copy are described in
[selection and paste](selection-and-paste.md).

Native GPUI handlers route click, motion, release and scroll to the cell hit by
the pointer. Headers/padding are excluded; coordinates use the intersection of
visible pane and acknowledged frame geometry during resize. Click selects a
pane; ordinary hover does not. A held button captures stable session and process
generation identity, including when crossing pane boundaries. Outside releases
are clamped to the captured pane, never sent to a newly hovered generation.
Window blur/deactivation and entering management mode close remembered buttons
when the input path remains available. Replaced/stopped generations discard old
local capture. If an outside release was missed, motion with no physical button
closes the remembered press. Actual compositor-level capture remains a platform
verification item.

Encoder transitions are prepared on a clone and committed only after admission
to the bounded UI queue. A queue rejection preserves the previous button state
and reports an error. Explicit prefix+Escape acknowledgement attempts a
compensating release that was previously rejected before local admission;
a shared unknown-outcome error continues to block this path. The worker does not retry ambiguous input. IPC failure
freezes further input. This is not end-to-end acknowledgement that the child has
consumed a click.

Wheel conversion retains a fractional line remainder and emits at most32 steps
per native event; excess whole steps are discarded. Pixel events use the current
cell height. This bounds mouse input, not terminal output: journal output is
never dropped for frame rate. Wheel remainder and motion deduplication belong
to the same session/generation route. Shift+wheel is reserved for local behavior.

The pure encoder and app tests cover exact wire bytes, modes/modifiers, bounds,
multiple buttons, motion deduplication, wheel limits, identity reordering,
rejected candidate transitions, hit testing and fractional scale. A real raw PTY
child acknowledges exact received bytes after enabling modes and after resize,
then disables reporting. This proves wire delivery through the host, not that a
particular provider interprets every mouse gesture correctly.

```sh
crates/app/bootstrap.sh test --locked --bin pigade-app
crates/app/bootstrap.sh clippy --locked --all-targets -- -D warnings
cargo test -p pigade-session-host --test mouse_input
```

Primary wire reference: [xterm mouse tracking](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#Mouse_Tracking).
Native pointer injection has not been run against the user's desktop. Wayland,
X11, fractional-scale native pointer gestures and accessibility remain open
acceptance checks. The same-app isolated render/reopen smoke is a separate
non-input check.
