# ADR0003 — One native application, no separate control stands

Date:2026-10-17. Status: accepted development direction; platform/recovery acceptance remains pending.

The user requires cross-platform behavior and asks the lead to choose an optimized implementation. Later clarification explicitly removes separate control stands. This overrides SPEC's original mandatory comparative-application requirement and ADR0001's corresponding evidence bullet, without relaxing terminal correctness, bounded recovery or performance budgets. SPEC10.4 records the scope change.

Continue with Rust, GPUI, portable-pty and one host-owned alacritty/VTE model. UI choice and terminal core are independent: this uses the alacritty_terminal library, not the Alacritty desktop application. The same pinned core/parser/profile provides common terminal semantics across target platforms; PTY, text input, fonts, clipboard and GPU backends still require actual OS-specific verification. Linux is the first tested target, not evidence of Windows/macOS support.

GPUI avoids embedding a browser runtime and allows direct control of grid geometry and rendering. These architectural properties motivate the direction; they are not measured proof that PigADE meets memory/input/presentation budgets. Tauri's system WebViews differ by OS and Electron brings its own Chromium runtime. Neither is added to PigADE while the native path has actionable repairs. Reconsider only against a concrete gate failure, documented in a new ADR.

The initial Electron experiment is retained in Git commit6b9ed5c and its historical findings are summarized in research. Its application/dependencies are removed from the working product. The native surface moves into crates/app. Unit/integration tests, fixtures, fault injection and measurement scripts remain necessary and are not separate demonstration applications.

GPUI source is pinned to the verified clean archive and compiled as part of the actual app. A separate Cargo dependency workspace preserves a compatible locked graph; it does not run a second application. Linux builds and short functional checks do not certify IME/accessibility, X11/Wayland scaling, long-history, VM reboot, soak or other OSes. Those gates remain release blockers.
