# Electron / xterm control prototype (stage 0)

This is the bounded comparison required by SPEC §10.4, **not an approved production stack or the PigADE MVP**. There is one ANSI parser per session, in the independent Node host. The Electron renderer receives complete cell frames and never imports xterm or interprets ANSI. Real shell and Python PTYs are available in four panels.

## Reproduce

Requirements: Linux x86_64, Node 26.8.1/npm 11.19.0 (tested), a working Wayland or X11 desktop, `/bin/bash`, `/bin/sh`, `/usr/bin/python3`. Electron may require the distribution's normal Chromium shared libraries. Dependencies and tarball integrity are pinned in `package-lock.json`.

```sh
cd prototypes/electron-control
npm ci
npm rebuild node-pty
node node_modules/electron/install.js
npm run check
npm test
npm run smoke -- --ozone-platform=wayland
```

The explicit Electron installer is useful with npm installations which suppress dependency install scripts. `node-pty` must load successfully; installation/build failures are blockers, never silently replaced with pipes.

Run the host in its own terminal, then open the UI from another terminal:

```sh
npm run host
npm start -- --ozone-platform=wayland
```

Click “Открыть shell” / “Открыть Python” to create up to four sessions. Closing the Electron window leaves the separately started host alive. Reopening reconnects to the same session/generation/PID. Host state defaults to `$XDG_STATE_HOME/pigade-electron-control` (or `~/.local/state/pigade-electron-control`). The directory is mode 0700, socket/journals mode 0600. `PIGADE_CONTROL_STATE` selects an isolated directory. Do not point it at production data.

Ctrl+C in the **host terminal** stops its PTYs. After an unclean host death an existing socket prevents a replacement host from silently starting. Inspect the old process/socket and choose a fresh test state directory. There is no automatic process restart, conversation resume or provider task submission.

## Implemented boundary

- Independent host, local Unix socket, protocol version, session/generation IDs and monotonic event sequences.
- Raw PTY bytes (`encoding:null`), ordered output/resize journal, SHA-256 checksums, fsync before parsing and publishing a frame.
- Full replay into a fresh headless model with no terminal-query response handler, clipboard, URL or bell integration.
- Hot xterm history capped at 10,000 lines. PTY is paused while each output batch is durably parsed. Session count is capped at four, operation admission at 1,024, input message at 1 MiB and pending IPC output at 4 MiB. Slow clients are disconnected and request full snapshots on reconnect. Journal data is not discarded.
- Four fixed cell-replica panels, keyboard input, a textarea IME delivery path, bracketed paste, standard text selection/copy, dark/light themes, integer rows/cols from font metrics and no terminal-layer scale transform.

These are prototype limits, not evidence that the full SPEC performance/compatibility budgets pass. The full-frame JSON/DOM renderer is intentionally a small comparison surface and is not the production binary delta/GPU renderer.

## Recovery finding

`npm test` compares live model, complete journal replay, and `SerializeAddon.serialize()` + tail. The comparison includes **both** buffers, all public cell characters/codepoints/widths/attributes/colors, wrap metadata, cursors, viewport offsets, active buffer and public modes. Continuation probes expose state not exported by the public snapshot API.

| Fixture | Full replay = live | Serialize + tail = live |
| --- | --- | --- |
| Completed plain text | Yes | Yes |
| Partial UTF-8 (`d0` / `96`, Ж) | Yes | **No** |
| Partial CSI truecolor parameters | Yes | **No** |
| Partial OSC hyperlink payload | Yes | **No** |
| Normal → alternate → normal | Yes | **No** (new normal text inherits red) |
| Saved cursor | Yes | **No** |
| Custom tabstop | Yes | **No** |
| Scroll region | Yes | **No** |
| Bracketed paste/mouse/focus/cursor/insert modes | Yes | Yes |
| Unicode + resize | Yes | Yes |

Passing tests assert these **known negative outcomes** so a green diagnostic suite cannot be mistaken for a successful checkpoint gate. `evidence/checkpoint-comparisons.json` records SHA-256 hashes of each complete compared observable model plus concrete first differences. Fixtures are in `test/checkpoint.test.mjs` and reproduce failures without CLI credentials.

**Decision: stock serialize addon is rejected as a complete checkpoint.** Parser/UTF-8 state, saved cursor, scroll regions and tabstops are not recovered by this path. Replay remains a test oracle. No raw journal compaction is permitted. There is no bounded long-history recovery proof; this prototype does not authorize choosing Electron as the fallback production stack. Next step: prove and cost a maintained full-state core patch or evaluate another core under a separately approved ADR, then repeat all recovery tests.

## Fresh evidence and its limits

`npm test` runs 16 diagnostic tests: ten checkpoint/replay comparisons, a 300,000-byte parse timing, real bash/Python PTYs, 1,000 requested / 999 applied resizes (first size is unchanged), tail/corruption handling, four PTYs across ten client reconnects with unchanged IDs/PIDs, and killing a separate client/host followed by journal replay. It does **not** claim REC-01's 100 UI cycles or all TERM/REC acceptance tests.

`npm run smoke` opens the actual Electron renderer, launches two bash and two Python PTYs, captures dark/light screenshots, closes/reopens the actual window, and checks the same four session/generation/PID tuples. It disables hardware acceleration for deterministic capture; this is not a GPU acceptance result. The first attempt using the current default Vulkan/Wayland path emitted a Vulkan incompatibility warning and capture failed with `UnknownVizError`; software capture succeeded. Actual compositor-managed window size is reported by the screenshot, and is not assumed equal to the requested 1280×800 bounds.

Machine-readable observations are in `evidence/short-replay.json`, `pty-resize.json`, `ui-smoke.json`; screenshots are `ui-dark.png` / `ui-light.png`. They are single smoke observations on the current machine, **not** reference-hardware p50/p95/p99 results. Electron's `workingSetSize` fields are KiB and do not include the independent Node host or the CLI processes. The observed UI process footprint already exceeds the empty-product 250 MiB target; the four-panel run is not an empty benchmark or a fair native comparison. AI provider CLI/auth/resume, IME correctness, screen reader, clipboard matrix, fractional scaling, real X11, GPU context loss/recovery, reboot VM, 1 GiB histories and 24 h soak remain unverified.

## Deliberate blockers

The prototype has no SQLite metadata registry, durable launch helper/intent protocol, OS start/boot identity registry or generation locks across host restart. Its resize call and journal commit are not crash-atomic. It therefore refuses to claim safe automatic recovery of an interrupted launch or reattach after host death. A previous process is never started again from replay. Terminal timers/synchronized-output deadlines are not journaled. OSC8 hyperlink cell metadata is not fully exposed by the compared public API. The current font is the system monospace, not a bundled/pinned font profile. These block production acceptance.

The host is separately started by the operator: independent process lifetime across UI termination is tested, cgroup/logout service guarantees are not. The IPC endpoint permissions bind the prototype to a private user directory; robust peer-credential negotiation remains a production task. Browser input encoding covers a basic tested path; full keyboard extension/mouse reporting semantics are not certified.

## Upstream/version/license evidence

- xterm headless **6.0.0**, upstream commit `f447274f430fd22513f6adbf9862d19524471c04`, MIT. Its public typings expose buffers and cells, not a complete parser checkpoint.
- Serialize addon **0.14.0**, MIT. [Pinned upstream README](https://github.com/xtermjs/xterm.js/blob/6.0.0/addons/addon-serialize/README.md) describes framebuffer export and marks the addon experimental. [API](https://github.com/xtermjs/xterm.js/blob/6.0.0/addons/addon-serialize/typings/addon-serialize.d.ts) documents serialization options. A serialization API is not a complete recovery proof.
- node-pty **1.1.0**, MIT. [Pinned upstream](https://github.com/microsoft/node-pty/tree/v1.1.0) documents real PTYs, pause/resume and resize. Installed typings were checked for raw Buffer (`encoding:null`) delivery.
- Electron **44.8.0**, MIT; bundled Chromium **152.0.7977.167**, Node **24.22.0**, measured by the smoke. [Official process model](https://www.electronjs.org/docs/latest/tutorial/process-model) explains renderer lifetime and isolation. The host deliberately uses system Node 26, so node-pty does not need Electron ABI rebuilding.

Registry metadata/integrity and license-file SHA-256 values are recorded in `evidence/*-package.json` / `dependencies.json`; upstream licenses are under `evidence/licenses/`. The published xterm npm artifacts omit a standalone LICENSE file, so the MIT text was obtained from the pinned upstream tag. Electron redistribution would also need its bundled Chromium third-party notices; this source prototype is not a distributable package. No Zed terminal implementation or other copyleft implementation was copied.
