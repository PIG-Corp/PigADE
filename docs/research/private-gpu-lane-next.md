# Private hardware-render lane: evidence and next checks

This note records a read-only review on 2026-10-17. No compositor, UI, device node, or GPU workload was launched.

## Current test boundary

- [`scripts/isolated-render.py`](../../scripts/isolated-render.py) starts Weston with `--backend=headless --renderer=pixman`, confirms those choices in its log and checks the compositor has no open `/dev/dri`, `/dev/nvidia`, `/dev/input`, or host runtime-directory descriptors. Its app sandbox has a private `/dev` and sets `LIBGL_ALWAYS_SOFTWARE=true`.
- The X11 and Wayland IME harnesses likewise force `VK_DRIVER_FILES` to the prepared Lavapipe ICD and set software rendering. They are software-path evidence, not hardware-render evidence.
- The [desktop incident guard](../reports/desktop-incident.md) remains in force: no active-desktop input/focus/window automation; the user’s authorization to continue local work does not authorize that testing.

## Local hardware metadata

`/proc/driver/nvidia/version` reports NVIDIA Open Kernel Module **610.57.04**. Read-only node/sysfs metadata maps `/dev/dri/renderD128` (226:128) and `/dev/dri/card1` (226:1) to PCI function `0000:05:00.0`, vendor `0x10de`, device `0x2882`; the driver link is `nvidia`. `/dev/nvidia0`, `/dev/nvidiactl`, and `/dev/nvidia-modeset` also exist. No node was opened and no active-display relationship was inspected, so this does not establish whether the device is in use by the desktop.

Linux documents that DRM render nodes expose rendering while disallowing modesetting and privileged ioctls ([DRM UAPI](https://www.kernel.org/doc/html/v4.14/gpu/drm-uapi.html)). That property alone does not prove that this NVIDIA Vulkan driver can run using only `renderD128`, or that the physical GPU is isolated from use by the desktop.

## GPUI selection behavior

Pinned GPUI source is `crates/app/.upstream/zed-322cd509dcccaee1f6bc44725d270598ae8171ad/crates/gpui_wgpu/src/wgpu_context.rs`. Its native instance enables Vulkan and GL. Surface selection enumerates adapters and sorts candidates by `ZED_DEVICE_ID` match, compositor-GPU hint, device type, then backend, trying surface configuration. `ZED_DEVICE_ID` changes priority; it does not restrict enumeration to one adapter. A headless compositor may also lack a compositor-GPU hint. The code logs adapter name, vendor/device IDs, backend, and type, which can support post-run attestation, but current harness result checks do not require that the chosen adapter be hardware.

The Vulkan loader’s `VK_DRIVER_FILES` overrides driver-manifest discovery ([Khronos loader docs](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md#overriding-the-default-driver-discovery)); it does not select or attest a physical adapter. NVIDIA’s installed-component docs identify its Vulkan ICD and vendor libraries ([NVIDIA docs](https://download.nvidia.com/XFree86/Linux-x86_64/590.48.01/README/installedcomponents.html)), but do not settle the exact device-node requirements for this installed build.

## Next verifiable step

Keep the present Lavapipe test path unchanged. Before a same-host hardware lane, verify against documentation for the exact 610.57.04 package which device nodes its Vulkan path requires, and whether it works with only the corresponding render node. Then define a separate opt-in isolated path that exposes only those proven nodes, retains the private headless compositor and absent host display/input/config endpoints, and fails unless structured output attests the expected adapter vendor/device/backend. If those node requirements or physical-device isolation cannot be established, test on a dedicated-GPU VM instead. This is an unresolved verification gap, not a claim that hardware rendering is impossible or inherently unsafe.
