# Steady cursor rendering and reopen

Four actual debug-app/release-host runs inside verified device-free private
namespaces with Weston/Pixman + Lavapipe. Each subdirectory retains run.json
with exact app/host/SDK/font hashes, result.json with typed host presentations
and stable session/generation/PID identities, and the captured PNG. Source hashes
are recorded separately. No keyboard/mouse injection or active desktop access.

Root visually inspected all four PNGs: filled selected block, two-pixel
underline, one-pixel beam, static inactive hollow outlines, and no cursor in
the hidden fourth pane even when selected. Pixel-probe counts exact accent
RGB125/175/255 or inactive96/104/117 inside content rectangles, excluding borders.
The fixtures are steady and use reduced motion. Actual blink timing and the
other platform/GPU/input acceptance gaps remain unverified.
