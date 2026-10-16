# ADR 0007 — Upgrade the existing GPUI application for native accessibility

Date: 2026-10-17. Status: integrated in development; Linux accessibility bridge verified in isolation, broader platform acceptance pending.

## Context

PigADE already has one native Rust/GPUI application. The pinned upstream revision `zed-industries/zed@69e2130295c2649963eb639fc70b4f2ee8ea1624` builds that application, but its GPUI implementation does not include AccessKit integration. The absence leaves the application without the native accessibility bridge needed for the Linux desktop target. A second application, a browser shell, or a separate control stand would add another runtime and would not resolve the accessibility gap in the product application.

## Decision

Upgrade the source pin used by the existing `crates/app` application to the verified clean upstream revision `zed-industries/zed@322cd509dcccaee1f6bc44725d270598ae8171ad`. Keep the existing Rust and GPUI product architecture and host protocol. Adapt the app to the upstream platform entry point, `gpui_platform::application().run(...)`, and add the `gpui_platform` path dependency with its `wayland` and `x11` features as required by the Linux build. Do not add Electron or a separate application.

Pin the source archive by commit and SHA-256:

- Archive: `https://codeload.github.com/zed-industries/zed/tar.gz/322cd509dcccaee1f6bc44725d270598ae8171ad`
- SHA-256: `27e6a724515cb6e6fa8a456098d075bfa2a50b4f6145491105555e56de2d256d`
- Archive size: 25,431,105 bytes

The commit-pinned [GPUI accessibility example](https://github.com/zed-industries/zed/blob/322cd509dcccaee1f6bc44725d270598ae8171ad/crates/gpui/examples/a11y.rs), [GPUI manifest](https://github.com/zed-industries/zed/blob/322cd509dcccaee1f6bc44725d270598ae8171ad/crates/gpui/Cargo.toml), [`gpui_platform` manifest](https://github.com/zed-industries/zed/blob/322cd509dcccaee1f6bc44725d270598ae8171ad/crates/gpui_platform/Cargo.toml), and [Linux platform manifest](https://github.com/zed-industries/zed/blob/322cd509dcccaee1f6bc44725d270598ae8171ad/crates/gpui_linux/Cargo.toml) document the API, features and Linux implementation selected here. The [upstream Apache license](https://github.com/zed-industries/zed/blob/322cd509dcccaee1f6bc44725d270598ae8171ad/LICENSE-APACHE) and package metadata are the primary licensing references. The archived GPUI and `gpui_platform` source files and their upstream Linux accessibility integration are part of this selected revision. This establishes that the capability is present in the source; it does not establish that PigADE exposes a complete or correct accessibility tree at runtime.

The observed component licensing is compatible with continued evaluation while PigADE's product license remains undecided: GPUI, `gpui_platform`, platform utilities and wgpu declare Apache-2.0; AccessKit declares MIT OR Apache-2.0; and zbus declares MIT. These declarations are recorded in the pinned upstream manifests and dependency metadata; the actual resolved graph must be rechecked during integration. Preserve upstream license and notice files with the source archive. Do not import Zed's GPL terminal application/integration crates.

## Consequences and acceptance boundary

The app follows the newer GPUI platform API and carries its new pinned source/lockfile graph. The locked Linux build, 17 app tests and strict Clippy pass. An isolated Weston/Pixman + Lavapipe run also renders and reopens four live generations without replacing processes. Headless unit tests, formatting, Clippy and builds can run without opening windows.

Wayland and X11 builds or source-level AccessKit presence are not accessibility acceptance. The project still needs isolated runtime checks that inspect the Linux accessibility tree and exercise it with AT-SPI and Orca on Wayland and X11. Those tests require a verified isolated compositor or VM. No keyboard or mouse injection, focus/workspace manipulation, or automated window launch may occur on the user's active desktop; see [the desktop incident report](../reports/desktop-incident.md). Until those checks pass, native Linux accessibility and the broader platform gate remain open.

This change does not claim WCAG conformance, full keyboard access, screen-reader usability, GPU correctness, or macOS/Windows support. DESIGN.md remains the product accessibility target; the new upstream dependency is an enabling platform change, not evidence that the target is met.

## Resolved dependency evidence

The [Linux dependency inventory](../research/gpui-322cd509-linux-licenses.md)
records 481 reachable packages, the exact metadata command, declared license
expressions and remaining notice/source obligations. This is a target-filtered
declaration inventory, not legal certification or a scan of every native library.
An isolated read-only AT-SPI probe observes the named PigADE semantic tree;
its exact role mapping and evidence are recorded separately. It does not test
Orca, keyboard navigation, all four panes or accessible history/search.
