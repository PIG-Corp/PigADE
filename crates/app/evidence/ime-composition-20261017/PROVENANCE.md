# Curated local composition evidence

Final app SHA-256: `a3559318f4c596c57ad287130ff2e78e2f6cb798c5a9c461b75902fe9e5da705`.
Exact source hashes are in `source-sha256.json`; each run binds its own host,
fonts and SDK hashes. Only metadata and screenshots are tracked. Source runtime
state remains outside Git under `~/.local/state/pigade/isolated-ime-20261017-integrated-scale{1,2}`.

Root inspected both final screenshots. Scale1 contains all four distinct child
ACKs, a nonfatal recovery warning and the local highlighted composition at the
acknowledged cursor. Scale2 shows the same overlay at device scale2. Composition
does not occur in host frames or journals. Both UI reopen checks preserve PIDs
and generations. This uses a verified device-free namespace, private
Weston/Pixman and Lavapipe; no user-desktop input or actual IME is driven.

The earlier latch-aware capture/source bundle remains at
`~/.local/state/pigade/curated-ime-before-latch-removal-20261017`; it is superseded.
The speculative latch was removed because it blocked native IME-only gestures.
Native reset/ordering remains an explicitly unverified platform gate.
