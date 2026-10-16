# Native Linux accessibility bridge probe — 2026-10-17

The opt-in `--accessibility` mode of `scripts/isolated-render.py` completed a
read-only AT-SPI tree inspection against the actual PigADE process, inside the
existing private Wayland compositor and device-free namespace. The overall
isolated render/reopen check passed. This demonstrates that the pinned GPUI
Linux bridge registers and exports the inspected semantics in this harness; it
does not establish screen-reader usability or accessibility conformance.

The run used app SHA-256
`8b24806d114a33d279d3676f318f9b273a1836b8a079e6573d45273c6f837966` and host
SHA-256
`d8d4b58171ee378527dbbfaa44c5f4b3cc368123f73c4d8a69353226acfa041a`. The
full isolated runtime, logs, and state remain under
`/home/afterburner/.local/state/pigade/isolated-accessibility/atspi-20261017-pass-final2/`;
only the summary JSON files and screenshot are retained here:
[`result.json`](../../crates/app/evidence/accessibility-atspi-20261017/result.json),
[`run.json`](../../crates/app/evidence/accessibility-atspi-20261017/run.json),
[`preflight.json`](../../crates/app/evidence/accessibility-atspi-20261017/preflight.json),
and [the synthetic Wayland screenshot](../../crates/app/evidence/accessibility-atspi-20261017/wayland-screenshot-2026-10-16_22-50-47.png).

The probe starts `at-spi-bus-launcher` and `at-spi2-registryd` only after the
namespace preflight. It sets `GSETTINGS_BACKEND=memory` for the launcher process
only, enables `ScreenReaderEnabled` on the private session bus after the app
initializes, and confirms that both private status properties read true. It
then waits up to 10 seconds for GPUI's asynchronous first-frame accessibility
tree update. No actions are invoked. The preflight records private mount, PID,
user, IPC, and network namespaces; no host runtime directory, X11 socket,
input device, GPU device, or desktop environment variable is available inside
the test namespace.

The registry returned PigADE's unique bus connection and root path
`/org/a11y/atspi/accessible/root`. Read-only calls were `Registry.GetChildren`,
`org.freedesktop.DBus.GetConnectionUnixProcessID`, `Properties.Get` for `Name`,
`AccessibleId`, and `ChildCount`, `Accessible.GetRole`,
`Accessible.GetInterfaces`, and `Accessible.GetChildAtIndex`. The exact bounded
call counts and the complete six-node tree are in `result.json`.

The tree contained:

- A bridge-owned application root named `app`, role 75 (`application`), with
  `Accessible` and `Application` interfaces.
- PigADE's semantic root `PigADE`, accessible ID `pigade-root`, role 78
  (`embedded`), with `Accessible` and `Component` interfaces.
- A `PigADE` heading, role 83 (`heading`), with `Accessible` and `Component`.
- An AT-SPI frame, role 23 (`frame`), contains PigADE's semantic subtree.
- The selected terminal node `Терминал Bash 1 · running`, accessible ID
  `terminal-canvas`, role 60 (`terminal`), with `Accessible` and `Component`.
- A status node named `PigADE 0.0.1-dev · 4 сессий · Ctrl+Shift+Space, 1–4 — фокус`,
  role 54 (`status-bar`), with `Accessible` and `Component`.

The role 78 result is expected for the app's GPUI/AccessKit `Role::Application`:
the pinned [`accesskit_atspi_common` 0.19.1 mapping](https://docs.rs/crate/accesskit_atspi_common/0.19.1/source/src/node.rs)
translates that role to AT-SPI `Embedded` (the source marks this mapping as a
TODO). The separate bridge root is the AT-SPI `Application` role. Role values
and the `GetRole` contract are defined by the [GNOME AT-SPI Accessible
interface documentation](https://gnome.pages.gitlab.gnome.org/at-spi2-core/devel-docs/doc-org.a11y.atspi.Accessible.html).

The terminal node currently exposes only `Accessible` and `Component`; the
probe did not observe `Text` or `Action` interfaces. It did not start Orca or
another screen reader, request focus or keyboard input, invoke accessibility
actions, inspect X11, or test speech, navigation, contrast, or WCAG criteria.
Those remain unverified; this report is evidence of native bridge presence and
the listed tree only.
