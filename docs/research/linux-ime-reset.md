# GPUI Linux native IME reset boundary

## Conclusion

The pinned GPUI revision exposes no application-level Linux IME reset operation. A small `Window::reset_text_input` forwarding API could request local reset behavior on both backends, but the available backend operations do not establish a boundary that lets PigADE reject every callback from the prior session generation. Therefore this note intentionally proposes no tracked upstream source patch and makes no claim that reset prevents stale native commits.

## Evidence in the pinned source

The source reviewed is `crates/app/.upstream/zed-322cd509dcccaee1f6bc44725d270598ae8171ad`.

- `crates/gpui/src/window.rs:5425-5434` implements `Window::notify_text_input_changed` by calling `PlatformWindow::text_input_state_changed(ContentChanged)` only when the same component is still focused. `crates/gpui/src/platform.rs:1385-1386` supplies a no-op default. There is no Linux override under `crates/gpui_linux/src`.
- Wayland's current reset pattern is `disable_ime(); enable_ime();` in `crates/gpui_linux/src/linux/wayland/client.rs:2522-2528`. The source comment states that text-input-v3 has no reset function. `disable_ime` sets local `composing = false`, then sends `disable` and `commit` (`client.rs:731-740`); `enable_ime` sends `enable`, content type, and `commit` (`client.rs:705-729`). Neither waits for a compositor acknowledgement.
- The Wayland text-input callback handles `CommitString` immediately through the currently focused window (`client.rs:2298-2321`). The protocol `Done { serial }` is handled separately (`client.rs:2327-2349`); its serial is used for the input-method serial tracker and cursor rectangle update, not to associate/filter a commit. The callback data has no PigADE session generation.
- X11 provides `reset_ime`, which calls XIM `reset_ic` and clears local `composing` (`crates/gpui_linux/src/linux/x11/client.rs:861-872`). Focus-out invokes it (`client.rs:1071-1085`), and a click during composition invokes it (`client.rs:1243-1247`). XIM commit/preedit callbacks still route to the window's current `input_handler` (`client.rs:1555-1578`, `crates/gpui_linux/src/linux/x11/window.rs:1224-1247`) without an origin generation.
- GPUI's `PlatformInputHandler` carries only the focused GPUI `FocusId` (`crates/gpui/src/platform.rs:1987-2010`). PigADE reuses its component focus across session generations, so that identifier cannot separate old and new session callbacks.

## Boundary and recommendation

The event callbacks are dispatched synchronously by the platform event handlers into the currently installed GPUI input handler. The source does not demonstrate an app-side delayed callback queue or prove that a backend will deliver an old composition after fresh input. It also does not provide a cross-generation callback token or a reset acknowledgement. A later callback after re-arming is indistinguishable from current composition input at the `InputHandler` API.

PigADE's cancellation latch can reject marks and commits until a fresh key event. That policy has an IME usability cost: IBus/Fcitx can consume the initiating key and report composition through preedit/commit callbacks without a GPUI printable-key callback, leaving input rejected until another key arrives. Re-arming on the first preedit would avoid that stuck state but would also accept an untagged callback if it were stale. The pinned API does not let app code distinguish these cases.

The minimal API proposal for a follow-up GPUI change is a distinct `Window::reset_text_input(focus_handle)` and `PlatformWindow::reset_text_input()` method, separate from content-changed notification. Wayland could use its existing disable/enable reset pattern and X11 could call `reset_ic`; however this API alone must be documented as best-effort reset, not a generation barrier. A stronger guarantee requires backend-specific evidence that reset discards prior callbacks before re-enable (including callbacks already queued client-side), plus tests against real Wayland IBus/Fcitx and X11 XIM implementations. If the upstream protocol cannot establish this, the product contract must retain the limitation rather than claim strict rejection.

No native desktop was driven for this review, and no real compositor/IME ordering guarantee is claimed.

Root integration decision: remove the speculative cancel-until-KeyDown latch.
The pinned dispatch path explicitly allows IME-only preedit/commit gestures;
blocking those is a known usability defect, whereas late cross-target delivery
has not been observed. Keep local cancellation, native notification requests,
input-availability checks and an explicit unverified native reset/ordering gate.
Add a regression accepting a new composition after cancellation without an
intervening synthetic printable key. This decision does not certify Linux IME.
