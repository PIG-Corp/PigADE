# Dynamic terminal palette — 0.0.1-dev

The cell replica now includes sorted typed `palette_overrides` for explicitly
set terminal colors. OSC4 palette changes and OSC10/11 foreground/background
changes reach the painter; OSC104/110/111 reset entries to the unchanged app
baseline. Named ANSI and indexed references use the same override. Truecolor
cells keep their own RGB. Terminal padding follows the terminal background;
product chrome retains its independent surface.

The authoritative model still owns color state. The app resolves serialized
cell attributes, not raw ANSI. It does not parse the legacy `Frame.colors`
debug string. That field remains for backward compatibility; old frames without
the new field decode to no overrides. The protocol decoder admits at most269
unique sorted entries, indices0–268, with exact three-byte RGB channels. Invalid
updates fail before replacing cached frames or recovery diagnostics. Parsing
cell color names/specifications is bounded64bytes and allocates nothing per
cell. The existing ANSI16/foreground/background defaults are unchanged; dim
named fallback is explicitly66% of the corresponding baseline color.

No checkpoint schema, terminal profile, default model palette or query response
policy changes. The host answers color queries only when it knows an explicit
model color. Reporting the UI's default theme as a terminal query response and
configurable shared render profiles remain separate unfinished work. New typed
frame fields change JSON frame bytes; earlier frozen-binary measurements retain
their exact original hashes and scopes.

Verification passed26 terminal tests,70 app tests,176 workspace tests, strict
Clippy, fmt and host release build. Python79 tests include the opt-in fixture.
The existing private software-Wayland test emitted set/reset/set through four
real shell PTYs; overrides matched after UI resize and repeated polls, and UI
reopen preserved process identities. Root inspected the PNG and measured exact
supplied RGB pixels: background779,693, foreground1,666, named/indexed sample340.
See [source-bound capture](../../crates/app/evidence/palette-20261017/).

This proves this isolated font/software-render profile. It does not certify
hardware GPU, X11, light theme, native input/clipboard, cross-platform parity or
all remaining cell decorations/cursor shapes.

```sh
cargo test -p pigade-terminal
crates/app/bootstrap.sh test --locked --bin pigade-app
python3 scripts/isolated-render.py --output /absolute/new/private/run \
  --palette-fixture --recovery-warning
```
