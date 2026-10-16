# Desktop input incident — 2026-10-17, 0.0.1-dev

During the native GPUI keyboard spike, a child agent focused test windows and used wtype to enter an EN/RU printf command on the user's active Wayland session. The user reported unexpected browser launches and desktop-wide cursor-following enlargement. These symptoms are consistent with injected input reaching global shortcuts; the exact modifier/key-routing mechanism is not established.

## Observed evidence and recovery

- Runtime `hyprctl getoption cursor:zoom_factor -j` returned1.3.
- User bindings map Super+Equal/Minus to global zoom increments and include application-launch shortcuts.
- The three physical monitor modes, positions and scale values matched the pre-test hardware manifest:1920×1080@180,1920×1080@143.981,1360×768@60.366, scale1.
- Root restored only runtime cursor zoom using `hl.config({cursor={zoom_factor=1.0}})` and verified zoom1.0 and no configuration errors. No monitor configuration files were changed.
- All agents stopped tests. Identity-scoped process checks found no remaining wtype/native canvas/PigADE host/benchmark processes. Unidentified user application windows were left intact.
- No global modifier state baseline exists; exact causal attribution remains an investigation gap. A matching active-window PID check did not sufficiently isolate input from global compositor bindings.

## Required prevention

The legacy native smoke entrypoint is disabled. Do not inject keyboard/mouse, manipulate focus/workspaces or launch automated windows on the active user desktop. Future UI automation must run in a verified isolated compositor/display or VM. An environment flag alone is not isolation. Product tests must not modify desktop zoom, monitor modes or user configuration. Main-agent ownership includes ensuring delegated test tooling follows this boundary.

The user subsequently authorized continuing development. That resumes local implementation/headless verification, not the unsafe desktop automation. Native input/platform acceptance remains open; earlier failed keyboard smoke is not a passing result.
