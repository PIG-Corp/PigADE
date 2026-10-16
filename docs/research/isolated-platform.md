# Isolated Linux application checks — 0.0.1-dev

Research date: 2026-10-17. Owner: isolated_platform_plan. This is a read-only investigation and an execution handoff, **not a passing platform test**. No compositor, application window, input injector or namespace was launched. No packages or desktop configuration were changed. Read [the incident report](../reports/desktop-incident.md) before implementation.

The test target is the actual `crates/app` binary with the actual session host. A headless display is test infrastructure; it does not introduce a second product/control application.

## Verified prerequisites

Read-only `command -v`, `pacman -Q/-Si`, filesystem inspection and `bwrap --help/--version` found:

| Component | This machine |
|---|---|
| Weston, weston-info, weston-screenshooter, wayland-info | Not installed / not found on PATH |
| Xvfb, cage, sway | Not installed / not found on PATH |
| Bubblewrap | `/usr/bin/bwrap`, 0.11.2-1 |
| D-Bus | `dbus-run-session`, dbus 1.16.2-1 |
| Wayland libraries | 1.26.0-1; client/server pkg-config both 1.26.0 |
| wayland-protocols | 1.49-1 |
| Mesa / EGL | Mesa 26.2.1-1; libEGL_mesa.so.0.0.0 present |
| Vulkan | Loader 1.4.357.0-1; only `nvidia_icd.json` in `/usr/share/vulkan/icd.d`; `vulkan-swrast` absent, no Lavapipe ICD found |
| Capture/input tools | grim 1.5.0-2 and wtype available, but their compatibility with Weston is **not established** |
| User namespaces | max_user_namespaces=159589; unprivileged_userns_clone=1; actual namespace creation not tested |

Local package metadata offers Weston **15.0.1-3**, MIT. That is repository-cache information, not a claim that 15.0.1 is the newest Weston: the official release page also lists 16.0.0. Use one pinned toolchain and record its exact build. The live Weston website currently describes 16.0.90; flags below were checked against the **15.0.1 source** instead.

## Official source pin and behavior

Weston 15.0.1 resolves to commit **a4939fd123f509766e60b2d310d801b215c9822d**, verified through the upstream GitLab tag API. The [official release archive](https://gitlab.freedesktop.org/wayland/weston/-/releases/15.0.1/downloads/weston-15.0.1.tar.xz) was downloaded into memory, inspected without extraction, and measured at 3,324,344 bytes, SHA-256 **551d039bfb0c837ba5a4d027cdb8ee16bded0eedb789821f8025d8a64b791f6d**. This matches the release announcement. PGP verification was not performed. Its `COPYING` is MIT/Expat; preserve notices if redistributing it.

- [Official release index](https://wayland.freedesktop.org/releases.html) establishes the release/download provenance.
- [wayland/weston@a4939fd123f509766e60b2d310d801b215c9822d:man/weston.man](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/man/weston.man#L70-72): the headless backend runs in memory and supports client tests/capture.
- [Same source, frontend/main.c:L710-L774](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/frontend/main.c#L710-774): explicit backend, `--renderer=pixman`, socket, `--no-config`, width/height, refresh rate and `--fake-seat` flags. Avoid automatic backend selection: an ordinary `weston` command may nest in the user's display.
- [Same source, libweston/backend-headless/headless.c:L604-L613](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/libweston/backend-headless/headless.c#L604-613): fake seat creates an internal pointer/keyboard; it does not mean physical device input is required or that an injection tool is available.
- [Same source, man/weston.man:L213-L217](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/man/weston.man#L213-217): `--no-config` prevents user configuration/modules being loaded.

## Isolation contract for the execution owner

The following is an implementation recipe, **not a run-certified shell script**. Complete a harmless namespace preflight before starting Weston. If a required boundary fails, stop; do not retry on the live display.

1. Construct a fresh Bubblewrap mount/user/PID/IPC/network namespace with `--unshare-all --new-session --die-with-parent --clearenv`. Start from the empty root, not `--ro-bind / /`. Bind only required `/usr` libraries/binaries, loader symlinks and selected font/loader configuration read-only; mount private `/proc`, `/dev`, `/tmp`, `/run`, test config/cache/home directories. Bind a fresh test artifact/state directory for output and the actual application binaries read-only. Do not bind the user's home, `/run/user`, `/tmp/.X11-unix`, system/session D-Bus sockets, `/dev/input`, `/dev/uinput`, `/dev/dri`, `/dev/nvidia*`, or host `/proc`. Close inherited file descriptors other than intentional stdio; stdin should be `/dev/null`.
2. Set an owner-only `XDG_RUNTIME_DIR` inside the namespace, e.g. `/run/pigade-test` with mode 0700, and a unique simple `WAYLAND_DISPLAY` name. Set private XDG config/cache/data directories. Explicitly omit `DISPLAY`, `WAYLAND_SOCKET`, `XAUTHORITY`, `DBUS_SESSION_BUS_ADDRESS`, `DBUS_SYSTEM_BUS_ADDRESS`, `HYPRLAND_INSTANCE_SIGNATURE`, `SWAYSOCK`, portal and other desktop IPC variables. A clean environment is easier to audit than a denylist. `WAYLAND_SOCKET` is particularly important: an inherited fd overrides the display name in libwayland.
3. Run `dbus-run-session` **inside** the namespace if GPUI requires a bus. The bus and any activated services must remain inside the private namespace. Do not mount or proxy the existing user bus. For a stricter harness, provide a private D-Bus config with no host service activation directories.
4. Before compositor startup, record namespace identities/mount table; assert that the host display socket paths, input devices and GPU devices are inaccessible and the private runtime directory is owned by the test UID with mode 0700. Then use only an explicit headless command such as the following **inside the already verified namespace**:

   ```text
   weston --backend=headless --renderer=pixman --shell=kiosk \
     --no-config --socket=pigade-test-UNIQUE --width=1280 --height=800 \
     --scale=1 --refresh-rate=60000 --idle-time=0 --fake-seat \
     --debug --log=/artifacts/weston.log
   ```

5. Gate application launch on fresh evidence: the exact child PID/start identity is alive; its log reports the headless backend and Pixman; its socket is a Unix socket in the private runtime dir; connecting to that exact path and reading `SO_PEERCRED` returns the owned Weston PID/UID in the relevant namespace. Confirm only the private virtual output exists. Audit the compositor's open descriptors for external display/device connections. Missing evidence is a failed gate, not a warning to ignore.
6. Launch the actual PigADE host with fresh private state and app with the private socket. Retain their exact child identities. Capture exit status/logs/artifacts and terminate only owned children. User-namespace support and the binary dependency closure still require actual preflight verification.

Bubblewrap's [upstream documentation](https://github.com/containers/bubblewrap/blob/v0.11.0/README.md) explicitly says the caller's mount/namespace arguments determine isolation; merely using `bwrap` is not proof. That documentation is v0.11.0; the installed 0.11.2 executable's help separately confirmed all listed CLI switches. The [Wayland client API](https://wayland.freedesktop.org/docs/html/apb.html) documents the inherited `WAYLAND_SOCKET` behavior. [dbus-run-session documentation](https://dbus.freedesktop.org/doc/dbus-run-session.1.html) describes a private regression-test bus and its lifetime.

## Rendering and capture

Pixman selects the **compositor** CPU renderer, not necessarily the application's renderer. [Mesa's environment documentation](https://docs.mesa3d.org/envvars.html) describes `LIBGL_ALWAYS_SOFTWARE=true` for OpenGL. This is not a Vulkan selector. If the pinned GPUI path needs Vulkan, a verified Lavapipe installation and explicitly selected software ICD are prerequisites; the present machine's NVIDIA-only ICD inventory does not satisfy that. Keep real GPU nodes inaccessible and fail instead of falling back to hardware or the host display. Renderer compatibility with GPUI has not been executed here.

For the private virtual output use **Weston's matching `weston-screenshooter`**, not an assumption that grim supports the required protocol. Weston `--debug` grants capture authority to clients; this is suitable only inside the private test namespace. [frontend/main.c:L5512-L5516](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/frontend/main.c#L5512-5516) establishes that authorization. [clients/screenshot.c:L607-L608](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/clients/screenshot.c#L607-608) defaults to framebuffer + shared memory; [L435-L439](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/clients/screenshot.c#L435-439) writes a timestamped PNG under `XDG_PICTURES_DIR`. Set that directory to `/artifacts/screenshots` inside the namespace and capture only after the actual app has presented. Kiosk shell plus one output/app makes this the app's virtual surface; assert dimensions and nonblank content, retain the logs and image hash.

## Input and acceptance limits

No ready, verified Weston input driver is available in this investigation. Do not run wtype merely because it exists. Weston's own [tests/weston-test.c:L470-L504](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/tests/weston-test.c#L470-504) provides seat focus/key operations, but [L896-L897](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/tests/weston-test.c#L896-897) hooks its own testsuite launch. Do not assume that loading its test module alone is a supported general external-app driver. A compatible private-compositor input protocol/test harness remains a separate bounded implementation task. Surface IDs belong to client connections; another client cannot simply reuse PigADE's `wl_surface` object ID.

A successful software headless screenshot would prove only that this exact application/toolchain can render into this private virtual output. It would not prove physical GPU performance, presentation latency, X11, real compositor shortcuts, clipboard integration, IME composition, accessibility, fractional-scale text sharpness, Windows/macOS support, reboot recovery or the 24h soak. Those acceptance items remain open.

Next concrete step: the lead resolves the missing pinned Weston/screenshooter and software renderer prerequisites without changing desktop configuration, implements the namespace/socket preflight, and only then executes a bounded actual-app render/capture test. No live-desktop fallback is permitted.

## Local SDK preparation follow-up — 2026-10-17

The lead authorized downloading/extracting prerequisites under `/tmp/pigade-platform-sdk`, with no system installation and no display/namespace/application execution. That preparation is now complete. Reproduce with:

```sh
python3 scripts/prepare-platform-sdk.py
```

The script pins official Arch archive URLs, archive SHA-256 values and signer fingerprint; verifies detached signatures using the installed Arch keyring converted into a private binary keyring; extracts package payload without running installer scripts; inspects required objects with `ldd`; and writes `/tmp/pigade-platform-sdk/manifest.json`. Initial gpgv verification rejected the armored system keyring format; explicit local dearmoring resolved that, and **both signatures then verified successfully**. Nothing was extracted before verification.

| Signed package | Download bytes | SHA-256 |
|---|---:|---|
| [weston 15.0.1-3](https://archive.archlinux.org/packages/w/weston/weston-15.0.1-3-x86_64.pkg.tar.zst) | 1,524,891 | `92fe0bfe2cf1ae02105b0e6573f6695c6bbd4637c9a6b20299a6fea8c3d23b4d` |
| [vulkan-swrast 1:26.2.1-1](https://archive.archlinux.org/packages/v/vulkan-swrast/vulkan-swrast-1:26.2.1-1-x86_64.pkg.tar.zst) | 2,436,943 | `08b9775e4743a64a94fd9591095cf4895d7ae17c3d43c2ba08fb9c8a1a91e752` |

Both archives are signed by Jan Alexander Steffens / heftig, fingerprint `83BC8889351B5DEBBB68416EB8AC08600F108CDF`, present in the installed Arch keyring. Signatures and verification transcripts remain under the private SDK. Weston license: MIT. Mesa package license expression: MIT AND BSD-3-Clause AND SGI-B-2.0. Full notices are retained under `root/usr/share/licenses`. Lavapipe 26.2.1 deliberately matches the installed Mesa 26.2.1; the cache's newer 26.2.2 is not silently substituted.

Prepared paths:

- Weston: `/tmp/pigade-platform-sdk/root/usr/bin/weston`
- Screenshooter: `/tmp/pigade-platform-sdk/root/usr/bin/weston-screenshooter`
- Explicit software ICD: `/tmp/pigade-platform-sdk/lavapipe-icd.json`
- Driver: `/tmp/pigade-platform-sdk/root/usr/lib/libvulkan_lvp.so`
- Manifest, verified package metadata/signatures and library closure reports: `/tmp/pigade-platform-sdk/manifest.json`, `/tmp/pigade-platform-sdk/evidence/`

The manifest's `private_environment` contains the exact `LD_LIBRARY_PATH`, `VK_DRIVER_FILES`, compatibility `VK_ICD_FILENAMES`, and `WESTON_MODULE_MAP`. These are **inputs to the isolated launcher**, not instructions to export them into the user's shell. Bind the SDK at the same absolute path inside the private namespace, or deliberately regenerate paths before launch. The local ICD names an absolute Lavapipe library path, preventing an accidental same-name host library lookup. Khronos' [loader documentation](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md#overriding-the-default-driver-discovery) specifies `VK_DRIVER_FILES` as the replacement driver list and prioritizes it over deprecated `VK_ICD_FILENAMES`; this is current documentation, not a pinned source claim.

Weston module relocation is supported by [wayland/weston@a4939fd123f509766e60b2d310d801b215c9822d:libweston/compositor.c:L10348-L10364](https://gitlab.freedesktop.org/wayland/weston/-/blob/a4939fd123f509766e60b2d310d801b215c9822d/libweston/compositor.c#L10348-10364): `WESTON_MODULE_MAP` uses semicolon-separated name=path entries. The prepared map includes only `headless-backend.so` and `kiosk-shell.so`; Pixman is built into libweston. No DRM/X11/Wayland nested backend is selected.

Fresh verification: the preparation script completed; Python compilation and scoped whitespace checks passed. `ldd` reported no unresolved libraries for Weston, screenshooter, Lavapipe, headless backend and kiosk shell with the SDK library directories. Existing LLVM 22.1.8-2 and other host libraries satisfy that link closure, so no dependency downloads were necessary. An exploratory check of unused modules found missing VNC libraries `libneatvnc.so.1`/`libaml.so.1`; the headless recipe does not use that backend. Total downloaded archives+signatures are 3,962,072 bytes; SDK artifacts including evidence are approximately 27.1 MB, below the 1 GiB limit.

This is a host-ABI-dependent SDK, not a portable sysroot. `ldd` proves link resolution only, not Vulkan initialization, protocol compatibility, rendering or isolation. **No Weston/screenshooter/app/input process, namespace, or Vulkan initialization was run.** The lead still owns the mandatory isolation preflight and actual bounded rendering test.
