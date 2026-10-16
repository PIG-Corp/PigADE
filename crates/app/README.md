# PigADE application — 0.0.1-dev

The native GPUI application displays cell replicas from the independent Rust session host. It contains no ANSI parser or terminal core dependency. Host-owned frames carry generation/sequence identity; same-generation sequence regression is rejected. Closing UI leaves host and CLI processes running. Disconnection preserves the last frame as read-only history. Input is never automatically retried.

This is the application under development, not a separate comparison stand. Terminal/recovery/platform gates remain open; the fixed session view is not the completed project/workspace/tab interface.

## Build and headless verification

Install Rust and the official GPUI Linux prerequisites documented in [the recorded source check](../../docs/research/gpui-source-check/README.md). From the repository root:

```sh
crates/app/bootstrap.sh build --locked
crates/app/bootstrap.sh test --locked
crates/app/bootstrap.sh clippy --locked --all-targets -- -D warnings
crates/app/bootstrap.sh fmt -- --check
```

Bootstrap verifies the pinned archive SHA-256, extracts it into ignored `.upstream`, and uses the application's own Cargo.lock. The separate dependency workspace preserves GPUI's compatible dependency graph; it does not introduce a second terminal implementation. Default output is `crates/app/target`. `PIGADE_GPUI_CACHE`, `PIGADE_GPUI_CARGO_HOME` and `PIGADE_GPUI_TARGET_DIR` can override build cache locations. None of these commands opens a window.

For deliberate manual use, first start the host and its sessions using the [root instructions](../../README.md), then run:

```sh
crates/app/target/debug/pigade-app /absolute/state/host.sock
```

Click a panel to focus it. Ctrl+Shift+Space enters PigADE management mode; 1–4 selects a pane and Escape exits. Outside management mode ordinary CLI keys go to the focused running generation. Printable text is delivered through GPUI's committed-text input handler; preedit stays local. Keyboard encoding is tested headlessly, but actual EN/RU/IME routing still needs an isolated platform test.

## Pins and licensing

- GPUI clean commit: `zed-industries/zed@69e2130295c2649963eb639fc70b4f2ee8ea1624`, Apache-2.0. No Zed GPL terminal integration is imported.
- [Source archive](https://codeload.github.com/zed-industries/zed/tar.gz/69e2130295c2649963eb639fc70b4f2ee8ea1624), SHA-256 `3954c515b37623af616253da41a6d25882e2323bcb0af2c4a2d1bdec790355e8`.
- `Cargo.lock` includes libc0.2.177: resolving newest libc exposes an upstream xattr0.2 compatibility failure, recorded in research. Transitive license audit is incomplete.
- Provisional font: installed Liberation Mono14 logical px, with measured GPUI advances/ascent/descent rounded to device pixels. No font is bundled or certified; a pinned font/fallback profile is still required for consistent platform behavior.

## Known limits

The UI displays at most four sessions, with one/two/three/four-pane projection. Full tiling, projects/workspaces/tabs, drag/drop, terminal selection, clipboard, mouse reporting and indexed history are pending integration. Full JSON frames are polled every50ms; there is no release input-to-present, frame-time, idle CPU/GPU or RSS acceptance measurement. Cell flags/basic colors are rendered, but palette overrides, precise decorations/cursor shapes and Unicode shaping require fixtures and pixel checks. Light theme, accessibility tree, real IME, X11, scaling/multi-monitor and GPU-loss checks are open. Historical Wayland smoke evidence predates the isolation guard and is not platform certification.

Automated windows, focus manipulation and keyboard/mouse injection on the active user desktop are forbidden. The unsafe smoke script has been removed. Future UI automation requires a verified isolated compositor or VM. Read [the incident report](../../docs/reports/desktop-incident.md) before platform work; do not reuse its historical actions as test instructions.
