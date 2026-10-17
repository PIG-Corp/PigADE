# Terminal cursor — 0.0.1-dev

The host now exports typed cursor shape and blink preference from its single
terminal model. DECSCUSR block/underline/beam and DECTCEM visibility reach the
native painter. The legacy debug string remains compatibility data; the app
does not parse it. Older frames without the typed field use the existing
visible-underline fallback. Checkpoint schema/profile and terminal semantics
are unchanged.

The painter clips to acknowledged grid and viewport geometry, normalizes a
wide-cell trailing half, and rejects invalid/outside cursor positions. A block
changes the cell backgrounds before glyph painting, preserving contrasting
text across its one/two-cell span. Beam and hollow strokes are one device pixel;
underline is two. Explicit dynamic cursor color index258 is honored. An inactive
pane/window uses a static hollow cursor, except a hidden cursor stays hidden.
Focused blinking cursors have a500ms on/off period; input, movement, shape,
generation and focus changes reset the phase. Unrelated panel output does not.
Reduced motion disables blinking.

Fresh verification:177 root-workspace tests,79 native-app tests and92 Python
tests passed; fmt, strict all-target Clippy, workspace release bins/examples and
native debug/release builds passed. Tests cover typed state, missing legacy
field, partial-CSI checkpoint continuation, wide/edge geometry, glyph color
contrast and blink boundaries/reduced motion.

Four source-bound private Weston/Pixman + Lavapipe runs exercised the actual
app and four real shell PTYs. Each shell emitted one steady cursor form; the
app selected each panel in turn, resized/reopened, and retained live process
identities. Root inspected all four captures. Content-only exact-RGB pixel
probes found: block9×23=207pixels, underline9×2=18, beam1×23=23, hollow60,
and hidden0. See [captures, binary/source hashes and metadata](../../crates/app/evidence/cursor-20261017/).

These captures use fixed scale1 and reduced motion with steady shapes. They do
not measure actual compositor blink timing, a block over a nonblank/wide glyph,
hardware GPU, X11, fractional scale, native input/IME or cross-platform parity.
Those remain acceptance gaps; a screenshot is not terminal checkpoint proof.

```sh
cargo test -p pigade-terminal
crates/app/bootstrap.sh test --locked --bin pigade-app
# Repeat with selected-pane0,1,2,3 and a distinct new output directory each time.
python3 scripts/isolated-render.py --output /absolute/new/private/run \
  --cursor-fixture --selected-pane 0 --reduced-motion --recovery-warning
```
