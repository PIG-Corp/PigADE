# PigADE application — 0.0.1-dev

The native GPUI application displays cell replicas from the independent Rust session host. It contains no ANSI parser or terminal core dependency. Host-owned frames carry generation/sequence identity; same-generation sequence regression is rejected. Closing UI leaves host and CLI processes running. Disconnection preserves the last frame as read-only history. Input is never automatically retried. A proven host queue-full rejection can be acknowledged with Ctrl+Shift+Space then Escape; later queued requests are counted as unsent, and unknown outcomes stay locked. See the [command rejection contract](../../docs/reports/app-command-rejection.md).

This is the application under development, not a separate comparison stand. Terminal/recovery/platform gates remain open; the fixed session view is not the completed project/workspace/tab interface.

Idle updates use conditional polling: the host returns only selection metadata
when generation/sequence/lifecycle state is unchanged. Cached cell allocations
are retained, and the UI checks a lightweight revision before cloning or
notifying GPUI. This requires a current host with the additive `poll` command;
an older host produces an explicit protocol/capability error, never an automatic
host restart. This is not yet the full hierarchy/visibility subscription model.

## Build and headless verification

Install Rust and the official GPUI Linux prerequisites documented in [the recorded source check](../../docs/research/gpui-source-check/README.md). From the repository root:

```sh
crates/app/bootstrap.sh build --locked
crates/app/bootstrap.sh test --locked
crates/app/bootstrap.sh clippy --locked --all-targets -- -D warnings
crates/app/bootstrap.sh fmt -- --check
```

Bootstrap verifies the pinned archive SHA-256, extracts it into ignored `.upstream`, applies the [manifest-pinned X11 focus profile](patches/gpui-x11-ime/) with exact pre/postimage checks, and uses the application's own Cargo.lock. Mixed or unexpected patched sources fail closed; an already applied exact profile is accepted. The separate dependency workspace preserves GPUI's compatible dependency graph; it does not introduce a second terminal implementation. Default output is `crates/app/target`. `PIGADE_GPUI_CACHE`, `PIGADE_GPUI_CARGO_HOME` and `PIGADE_GPUI_TARGET_DIR` can override build cache locations. None of these commands opens a window.

For deliberate manual use, first start the host and its sessions using the [root instructions](../../README.md), then run:

```sh
crates/app/target/debug/pigade-app /absolute/state/host.sock
```

Click a panel to focus it. Ctrl+Shift+Space enters PigADE management mode; 1–4 selects a pane; A selects visible cells, C copies selection, V pastes, and Escape exits/acknowledges rejected local input. Footer buttons also copy/paste. Left-drag selects cells; Shift overrides CLI mouse reporting for local selection. See [mouse input](../../docs/reports/mouse-input.md) and [selection/paste](../../docs/reports/selection-and-paste.md) for exact behavior and verification limits. Outside management mode ordinary CLI keys go to the focused running generation. Printable text is delivered through GPUI's committed-text input handler; preedit stays local. A fixed EN/RU and CLI shortcut sequence now passes in the actual private X11 release app, including a repaired Alt+X path; see the [keyboard report](../../docs/reports/x11-keyboard-20261017.md). One actual X11/Fcitx Pinyin commit passes in private debug/release runs with no precommit bytes or duplicate input; The full shortcut matrix, inline preedit and cross-session composition barriers remain open. See the [X11 report](../../docs/reports/x11-ime-20261017.md).

## Pins and licensing

- GPUI upstream commit: `zed-industries/zed@322cd509dcccaee1f6bc44725d270598ae8171ad`, Apache-2.0. The app enters through `gpui_platform::application().run(...)`. No Zed GPL terminal integration is imported.
- Downstream profile: `pigade-x11-ime-focus-v1`, two Apache-2.0 X11 backend files; [ADR0010](../../docs/adr/0010-gpui-x11-ime-focus-lifecycle.md). `python3 scripts/test_gpui_xim_focus.py` checks exact pre/postimages and executes nine embedded lifecycle tests without a display.
- [Source archive](https://codeload.github.com/zed-industries/zed/tar.gz/322cd509dcccaee1f6bc44725d270598ae8171ad), SHA-256 `27e6a724515cb6e6fa8a456098d075bfa2a50b4f6145491105555e56de2d256d` (25,431,105 bytes).
- The current `Cargo.lock` resolves libc0.2.190 and has no xattr dependency; the earlier xattr0.2.3 compatibility failure belongs to the previous GPUI pin and is recorded in [the source-check report](../../docs/research/gpui-source-check/README.md). The locked Linux build passes. The [Linux Rust license inventory](../../docs/research/gpui-322cd509-linux-licenses.md) covers 481 reachable packages; system libraries and off-platform distribution remain outside it.
- Terminal fonts use embedded, uniquely named derivatives of JetBrains Mono v2.304 (four static styles) and Noto Sans Mono CJK JP v2.004 Regular, at 14 logical px. [ADR0008](../../docs/adr/0008-bundled-terminal-font-identities.md) and [the render profile](assets/fonts/render-profile.json) record aliases, exact source hashes, OFL notices and unsupported-glyph limits. Cell dimensions round measured advances/ascent/descent to device pixels. `python3 scripts/prepare-terminal-fonts.py --verify` reproduces the derived files with fontTools4.63.0; it is a developer check, not a runtime dependency.

## Known limits

The UI displays at most four sessions, with one/two/three/four-pane projection. Full tiling, projects/workspaces/tabs, drag/drop and indexed history are pending integration. Visible frame selection, explicit bounded clipboard paste and mode-aware mouse input are implemented; native platform gestures/clipboard remain unverified. Selection clears on a new frame sequence, and paste is limited to one16KiB input command including brackets. Host metadata is polled with a50ms idle interval; commands wake the worker immediately and state/receipt notifications wake the UI. Complete JSON cell frames are sent only after the generation watermark changes. There is no release input-to-present, frame-time, idle CPU/GPU or RSS acceptance measurement. Cell flags/basic colors and typed dynamic palette overrides are rendered (see [palette report](../../docs/reports/dynamic-palette.md)), and typed block/underline/beam/hidden cursor state is rendered (see [cursor report](../../docs/reports/terminal-cursor.md)); underline variants, explicit underline color and strikeout also render (see [decoration report](../../docs/reports/terminal-decorations.md)). Font-exact decoration metrics and full Unicode coverage remain open. A private Wayland AT-SPI probe sees named application, heading, selected terminal and status nodes. It does not expose complete terminal Text/Action interfaces or certify Orca. One native X11 IME sequence is verified; the complete X11/Wayland input and reset matrix, fractional scaling/multi-monitor, GPU-loss and light-theme checks remain open. Historical active-desktop smoke evidence is not platform certification. See [ADR0007](../../docs/adr/0007-gpui-accessibility-platform-upgrade.md) for the source-upgrade decision and acceptance boundary.

Automated windows, focus manipulation and keyboard/mouse injection on the active user desktop are forbidden. The unsafe smoke script has been removed. Future UI automation requires a verified isolated compositor or VM. Read [the incident report](../../docs/reports/desktop-incident.md) before platform work; do not reuse its historical actions as test instructions.
