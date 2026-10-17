# Terminal cell decorations — 0.0.1-dev

The native painter now renders host-owned underline, double underline, curl,
dotted/dashed underline and strikeout flags. Explicit underline colors use the
same bounded color resolver as cell colors. Decorated spaces take part in the
foreground pass; they were previously omitted with ordinary blank cells.
Hidden text and wide continuation/wrap spacers keep their existing suppression.
The renderer leaves backgrounds independent and completes every background
before glyphs/decorations. Full cells outside the visible acknowledged grid are
skipped; a wide span clips to remaining columns during resize.

`cell_style.rs` is a pure planner, with no ANSI parser or model mutation. It
uses fixed-size recipes and lazy device-pixel strokes. Cell dimensions are
bounded512pixels, spans1/2columns and clip widths1024pixels; malformed geometry
suppresses decorations. Pattern phase starts at the cell origin. Strikeout uses
the cell midline; curl is a stepped two-row wave. These are explicit rendering
choices, not font-metric-perfect decoration geometry. All strokes are one
physical pixel, including at scale2.

The blank-decoration regression failed before integration and passes afterward.
Fresh90 app tests, strict all-target Clippy, formatting and debug/release builds
passed. Sixteen isolated-render tests exercise fixture emission/metadata checks.
Two private actual-app Weston/Pixman + Lavapipe runs at fixed scales1/2 passed
resize, stable host metadata and UI reopen with four live PTY identities intact.
Root inspected both screenshots: all six styles, wide Han underline, underlined
spaces, custom green underline and hidden synthetic text with visible bookends.
Exact custom RGB17/238/119 occupies36pixels at scale1 and68 at scale2 (four
one-pixel strokes of the actual9/17pixel cell widths). The device-rounded grids
are69×13 and73×14; changing scale correctly changes the measured grid.

[Captures and binary/source/font/SDK hashes](../../crates/app/evidence/decorations-20261017/)
retain the scope. Pixel counts cover the custom underline, not a complete glyph
or decoration oracle. Hardware GPU, fractional/multi-monitor transitions,
font-exact strike/underline positions, native input/IME and platform parity
remain unverified. Hidden text still exists in terminal state/history and can
be selected/copied; SGR8 is a display attribute, not a secrecy boundary.

```sh
crates/app/bootstrap.sh test --locked --bin pigade-app
python3 -m unittest scripts/test_isolated_render.py
python3 scripts/isolated-render.py --output /absolute/new/private/run \
  --decoration-fixture --reduced-motion --scale 1
# Repeat with a fresh output directory and --scale 2.
```
