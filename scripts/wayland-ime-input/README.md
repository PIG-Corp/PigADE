# Private Wayland IME keyboard helper

`ime_input.c` is a test-only, persistent `zwp_virtual_keyboard_v1` client for
`isolated-ime-wayland.py`. It exposes no text or key arguments. Its only
commands are the bounded control messages `COMPOSE` (physical US keys n/i/h/a/o),
`COMMIT` (Space), and `QUIT`. One virtual keyboard object and fixed US keymap
remain alive across both input phases, allowing the compositor's keyboard seat
capability and the application's text-input-v3 context to remain stable.

The helper accepts one `COMPOSE`, then one `COMMIT`; `QUIT` is allowed from any
state. Duplicate or out-of-order input commands fail closed before sending any
key. Control reads share one absolute `CLOCK_MONOTONIC` deadline, including
partial messages and interrupted polls. Wayland key timestamps remain the
protocol's wrapping 32-bit millisecond values, separate from control timing.

The protocol XML is copied unchanged from `atx/wtype` tag `v0.4`, path
`protocol/virtual-keyboard-unstable-v1.xml`, SHA-256
`7ad7870003ecd592cae47dc19d277a609b7f18fd7b7be012623cf3225a7294f5`. The XML
contains its own copyright and permissive license notice. The C helper is a
small first-party implementation; it does not copy wtype implementation code.
`WTYPE-LICENSE.txt` retains the source project's MIT license and attribution
from tag `v0.4`; it is recorded separately from the protocol XML's embedded
license. The isolated runner records the generated helper, source, XML, license,
scanner, compiler, and resolved `ldd` library closure (including dependency
hashes) in its private run evidence. Both Fcitx and the helper receive
`XKB_CONFIG_ROOT` pointing to the verified `xkeyboard-config-2` files from the
signed X11/Fcitx SDK profile; the helper preflights its fixed keycodes, US
symbols, and rules files before connecting to Wayland.

The test suite only compiles the helper and runs its no-display fixed-command
self-test, including deadline wrap arithmetic and one-shot command ordering. It
does not start a compositor, connect to a Wayland server, or send input. The
runner requires explicit `--run` and performs it only inside its verified
private namespace path.
