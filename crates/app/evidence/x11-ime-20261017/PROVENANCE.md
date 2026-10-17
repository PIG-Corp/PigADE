# Actual PigADE X11/XIM evidence

Baseline is app source33bd5dd and clean upstream GPUI, frozen0cecf217… binary;
its window rendered on private Xvfb but Fcitx CurrentInputMethod stayed empty.
Debug/release use the identical first-party app source with the tracked
pigade-x11-ime-focus-v1 GPUI patch. Exact hashes are in configs/results and the
source manifest; the same release host9c22265d… serves the fixture.

Root inspected native candidate/preedit popup and release ACK/exited state.
No inline composition/caret-placement claim follows. PNGs are lossless PPM
conversions, without resizing or content edits. Captures are from private Xvfb,
not the user desktop. The debug final capture is before UI ACK repaint; release
waits250ms after host ACK and includes it. Namespace, XRes/socket/PID evidence,
fixed synthetic raw/no-echo child protocol and exact-once UTF8 counts establish
this fixture's scope. SDK preparation status/limits describe preparation only;
the later run separately exercises the runtime. No credentials/history copied.
