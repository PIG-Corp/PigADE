# IME target-generation barrier: implementation notes

Status: source review only, 2026-10-17. No UI, X server, Fcitx process, or input
was run for this investigation. The current isolated X11 IME success establishes
composition on the current target; it does not establish stale-composition
rejection across a logical pane/session switch.

## Finding

The app needs a target identity independent of its currently selected view. A
native IME callback must be attributable to the target for which its input
context was created, or it must be rejected after that target changes. Current
GPUI callbacks carry text and the platform window, while `EntityInputHandler`
routes accepted text through the app's current selection. Local preedit
cancellation on target change therefore does not by itself prevent an already
queued native commit from reaching the newly selected pane.

### X11: `DestroyIC` is necessary, but an IC ID is not a generation

The XIM protocol assigns `input-context-id` on `CreateIC` and requires the
client to wait for the synchronous `DestroyIC_REPLY`. The protocol explicitly
allows DestroyIC to produce preedit callbacks before that reply. It also says
callbacks may be sent asynchronously in response to prior requests. Thus the
reply is a resource-lifecycle boundary, but the spec does not state that it
flushes every previously queued asynchronous callback. `ResetIC_REPLY` is
synchronous too, but its contract is only to return the reset preedit string;
it is not a generation token or a general callback-drain acknowledgment.

The exact Fcitx transport dependency in the current environment is
`xcb-imdkit 1.0.9-1`. Its upstream 1.0.9 source allocates IC IDs per XIM client:
`_xcb_im_new_input_context` pops `client->ic_free_list` before incrementing
`client->icid` (lines 2919-2985), and `_xcb_im_destroy_ic` adds the just-freed
IC to that list (lines 3822-3852). Therefore the next IC for the same XIM
client can immediately reuse the just-destroyed numeric ID. The XIM wire
callbacks identify an IC only by that ID. The GPUI patch's `focused_ic_id`
filter can reject callbacks while no IC is focused, but after a replacement IC
is focused with the reused ID it cannot distinguish a hypothetical late
callback from the previous logical target.

This source establishes ID reuse; it does **not** prove that Fcitx 5.1.23 will
emit a stale commit after the corresponding DestroyIC reply. The protocol does
not give enough guarantee to use that absence as a portable invariant.

### XIM transport endpoint rotation gives a usable discriminator

`xim-rs`'s X11 client allocates a dedicated hidden `client_window` in
`X11rbClient::init`. Its `filter_event` consumes XIM client messages only when
`msg.window == self.client_window` (`src/x11rb.rs`, around lines 399-410 and
488-568). On this transport, callbacks to the old endpoint and new endpoint
are addressed to different X windows, even if both XIM clients reuse the same
IC number. An endpoint generation can therefore identify the callback source
without changing the XIM wire protocol.

The strongest bounded X11 design is to rotate the XIM client endpoint on a
logical target-generation change:

1. App increments an opaque target generation, marks native text input
   unavailable for that target, and clears local preedit immediately.
2. GPUI records the old XIM client-window identity as retired, stops dispatching
   its callbacks to application windows, and creates a new XIM client endpoint
   (fresh hidden `client_window`) for the new generation.
3. The new endpoint creates a fresh IC for the actual focused OS window, waits
   for `CreateIC_REPLY`, then sends `SetICFocus`. Only then does it report the
   new target as locally ready. Forwarded keys and callbacks are accepted only
   from that endpoint and its focused IC.
4. Old endpoint teardown is completed separately. Destroy/close errors or
   unknown completion leave the new target input-locked; do not retry blindly.

This design does not need to assume distinct IC numbers across endpoints.
Late old messages carry the old `client_window` and cannot be interpreted by
the new endpoint. The old endpoint must stay identifiable until its pending
X11 event delivery is harmless. This is a small GPUI/xim-rs surface expansion:
xim-rs currently does not expose the client-window ID or an explicit
destroy/retire operation, and `X11Client` currently stores one `ximc`/handler.
An implementation should expose a narrow endpoint identity/retirement method
and route each incoming event through the matching endpoint; do not merely
replace the handler's IC ID in place.

An alternative is an explicit `Close` followed by `Open` on the same XIM
endpoint. This removes the IM's old logical context, but it leaves callback
messages addressed to the same client window and allows the same ID namespace
to be reused. It is weaker than endpoint rotation unless a tested event-drain
rule is added. IC recreation on the same endpoint is therefore not the
recommended generation boundary.

### Wayland text-input-v3 has no target-generation acknowledgment

Pinned GPUI (`322cd509dcccaee1f6bc44725d270598ae8171ad`) keeps one
`zwp_text_input_v3` proxy in `WaylandClientState`. Its dispatch handler updates
`keyboard_focused_window` from current state when handling `commit_string`,
`preedit_string`, and `done`; it does not bind these events to an app logical
target. The text-input-v3 protocol's `done(serial)` serial equals the number of
client `commit` requests already issued on that object. It batches/apply
preedit/commit/delete events and says state requests are applied by the
compositor after `commit`; it is not a callback-generation ID and does not
acknowledge that a prior logical target has been drained.

Object rotation could provide a discriminator analogous to X11: retire the old
`zwp_text_input_v3` proxy, create a new proxy, attach a per-proxy generation in
its dispatch data, and reject events from retired proxies. But this is not yet
proven safe for the pinned backend. GPUI stores only one proxy; the protocol
allows only one enabled text-input object per seat/surface, and `disable` plus
`commit` has no explicit completion reply. A compositor test must show that
retiring/replacing the proxy neither leaves stale events routed to the new
target nor creates a gap/duplicate active input context. Until that is
implemented and tested, do not describe a common cross-platform target barrier.

## API shape and truthful behavior

Avoid an app-only `ime_epoch` attached to the currently selected view as the
sole defense: current XIM and Wayland events do not carry it. Also avoid a
platform trait method named `reset_ime()` returning success when the platform
cannot prove target separation.

Prefer an explicit target-binding API at the platform boundary, conceptually:

```text
replace_text_input_target(target_generation, focused_native_window)
    -> Pending | Ready(target_generation) | Unsupported | Failed
```

Callbacks must carry the generation/endpoint identity that was bound when the
input context/proxy was created. The app accepts `Commit`/`Preedit` only when
that identity equals its current target generation; `Pending`, `Unsupported`,
and `Failed` never route text to a newly selected target. On X11 the backend
can implement this using endpoint rotation. For Wayland, expose `Unsupported`
or retain an explicitly weaker behavior until proxy rotation and compositor
tests establish an equivalent boundary. Keep the current tested X11 focus
patch's readiness/focus state machine, but do not call its same-endpoint
DestroyIC sequence a generation barrier.

The generation should identify at least `(app_window, selected_session,
selected_view_generation, monotonically increasing target epoch)`. It is a
local routing identity only; it must not be copied into host journal or
checkpoint schemas. A generation change cancels local preedit before any
backend work. No retry, restart, or automatic text replay follows a backend
failure.

## Required implementation and test boundary

1. Add a pure app test for generation changes: old `Preedit` and `Commit` are
   rejected; only an event tagged with the exact active generation can modify
   the selected terminal; asynchronous pending/failure states stay locked.
2. Add GPUI/X11 state tests for endpoint rotation, not only IC focus: old and
   new client-window identities remain distinct; old endpoint callback after
   switch is discarded even when its IC number equals the new endpoint's ID;
   create reply for a retired endpoint cannot mark the current target ready;
   errors/unknown lifecycle results do not open the gate.
3. Extend the existing isolated X11/Fcitx fixture with two logical targets in
   one app window. Start `nihao` preedit, switch target before Space/commit,
   prove the second target receives no composition or committed bytes, then
   prove a fresh composition on the second target works. If a test shim can
   deterministically delay the old endpoint's preedit/commit until after new
   endpoint readiness, inject those protocol events in the isolated fixture
   and assert they remain discarded. Do not use the active desktop.
4. Add a private-compositor Wayland-v3 test before claiming equivalent support:
   delayed `commit_string`/`preedit_string`/`done` from the retired proxy must
   be rejected while the new proxy receives new events; include `done` serial
   match and mismatch cases. Verify actual protocol-object retirement ordering.
5. Keep the capability explicit in diagnostics/evidence: X11 target-generation
   barrier proven, pending, or unsupported per backend. Startup/composition
   success alone is not a switch-barrier test.

## Source references

- XIM protocol, CreateIC/DestroyIC/SetFocus/ResetIC/callback ordering:
  <https://xorg.freedesktop.org/archive/current/doc/libX11/XIM/xim.html>
- xcb-imdkit 1.0.9 IC allocation and destruction:
  <https://github.com/fcitx/xcb-imdkit/blob/1.0.9/src/imdkit.c>
- xim-rs event dispatch and X11 client endpoint filtering:
  `~/.cargo/git/checkouts/xim-rs-f2169dfc4c0688a9/16f35a2/src/client.rs`
  and `src/x11rb.rs` (local dependency checkout, git revision in Cargo.lock).
- Pinned GPUI X11/Wayland implementation:
  `/tmp/gpui-source-review/zed-322cd509dcccaee1f6bc44725d270598ae8171ad/crates/gpui_linux/src/linux/{x11,wayland}/client.rs`.
- text-input-v3 protocol XML used in the local build:
  `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/wayland-protocols-0.32.13/protocols/unstable/text-input/text-input-unstable-v3.xml`.
