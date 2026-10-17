# Local composition bridge — 0.0.1-dev

The GPUI input bridge now exposes the local undelivered composition as a UTF-16
document. Range queries return the requested scalar-aligned substring and its
actual range. Mark/selection callbacks retain partial replacement and selection;
commit delivers the resulting local document once. Preedit is drawn over the
acknowledged cursor with selection, underline and caret, clipped to the pane.
Candidate geometry uses the same shaped text and cursor origin. Host cells,
geometry and terminal history are not edited during composition.

Focus loss, management mode, generation/input-availability changes and an
unsupported keyboard mode cancel composition and request native text-service
invalidation through GPUI. On this pinned Linux backend that platform hook is
a no-op; it does not reset an active Wayland/XIM composition. A fresh native
preedit/commit remains usable without a preceding GPUI key event, since IBus and
Fcitx can consume that key. Successful commit clears the
local buffer and consumes Meta once. Direct terminal controls cancel preedit;
ordinary EN/RU input still has one platform commit path.

Fresh app verification: 33 unit tests, strict all-target Clippy, formatting and
debug build pass. Tests cover surrogate interiors, clamped/reversed UTF-16
ranges, idempotent queries, partial replacement, selected range, cancellation,
fresh preedit after cancellation without KeyDown, Meta, generation and unsupported-mode changes.
The first independent review found missing native invalidation on poll/prefix
cancellation and retained preedit on unsupported-mode rejection; both are fixed.

The explicit same-app `--diagnostic-preedit` fixture stages `日本😀` locally;
it does not call a PTY input route. The verified Bubblewrap/private
Weston/Pixman/Lavapipe harness renders and reopens the app at 100% and 200%.
Root inspected both final screenshots: the selected pane shows the highlighted
Japanese characters, emoji, underline and caret. At scale1, all four child ACKs
and a nonfatal recovery warning remain visible while composition follows the
cursor. Terminal frames/journals do not contain the fixture. PIDs and generations
survive UI reopen. Source/binary hashes, namespace/FD preflight and screenshots
are in [the curated evidence](../../crates/app/evidence/ime-composition-20261017/).
The earlier first run is retained outside Git and is superseded by these final
cancellation-aware builds.

```sh
crates/app/bootstrap.sh test --locked
crates/app/bootstrap.sh clippy --locked --all-targets -- -D warnings
crates/app/bootstrap.sh build --locked
python3 scripts/isolated-render.py --output /path/to/new/private/run \
  --ime-preedit --recovery-warning --accessibility
```

This proves local document/overlay behavior, not a real input method. IBus/Fcitx,
Wayland/X11 protocol operation, candidate-window placement by an actual IME and
cross-platform behavior are not certified. Native callbacks carry no PigADE
generation token: a native callback from an old composition arriving after
target cancellation remains an ordering question for platform testing. No
app-only latch proves a native reset boundary. Emoji outside the
bundled font coverage still use the documented unpinned platform fallback.

An independent pinned-source review found synchronous dispatch into the current
handler in Wayland `gpui_linux/src/linux/wayland/{client,window}.rs` and X11
`gpui_linux/src/linux/x11/{client,window}.rs`. Wayland Done serials do not filter
commit delivery by app session. X11 has a reset-IC operation used for a mouse
click during composition, but no app-driven generation reset is exposed.
`gpui/src/window.rs::notify_text_input_changed` calls the platform hook;
`gpui/src/platform.rs::text_input_state_changed` defaults to no-op and has no
Linux override. This is an API/validation gap, not evidence that reordering
actually occurs. A stronger boundary needs a tested native reset operation or
a proven input-method event-order contract. The speculative cancel-until-KeyDown
latch was removed before integration: it would reject legitimate fresh IME-only
gestures and did not prove the missing boundary.
