# GPUI pinned-source baseline

Research date: 2026-10-17 local (UTC logs 2026-10-16). This is an upstream example validation, not PigADE UI or the four-panel gate. No product UI was authored.

- Clean source: `zed-industries/zed@69e2130295c2649963eb639fc70b4f2ee8ea1624`.
- [Untouched upstream example](https://github.com/zed-industries/zed/blob/69e2130295c2649963eb639fc70b4f2ee8ea1624/crates/gpui/examples/hello_world.rs).
- Example SHA-256: `72db5f3a91a3d3daec3ff433fd1f3e12b3f978b4893e7a74da08185dae91d060`.
- [Commit archive](https://codeload.github.com/zed-industries/zed/tar.gz/69e2130295c2649963eb639fc70b4f2ee8ea1624): 21,435,307 bytes, SHA-256 `3954c515b37623af616253da41a6d25882e2323bcb0af2c4a2d1bdec790355e8`.
- Rust 1.98.1 (`48a229cea`, Arch Linux package 1:1.98.1-1).
- Preflight pkg-config: xkbcommon 1.13.2, wayland-client 1.26.0, fontconfig 2.18.3, libzstd 1.5.7.
- Session: `XDG_SESSION_TYPE=wayland`, `WAYLAND_DISPLAY=wayland-1`, `DISPLAY=:0`.
- Build uses isolated `CARGO_HOME=/tmp/pigade-gpui-cargo`, `CARGO_BUILD_JOBS=2`; root workspace manifests are untouched.

The initial Cargo Git fetch was cancelled to release the shared Cargo cache lock. A second isolated Git fetch was replaced by the immutable source archive, avoiding full repository history. The dependency points to the GPUI crate inside that archive; this validates the same clean source tree, not Git transport or the dirty published artifact.

## Fresh dependency resolution: failed

Resolving dependencies on the research date produced 709 lockfile packages. `cargo build` exited 101 with E0425 in xattr 0.2.3: `::libc::ENOATTR` was unavailable in libc 0.2.190. This is a real compatibility failure, not a missing GPUI system library or missing Rust toolchain. See [full build log](build-latest-resolution.log) and [resolved lock](Cargo.latest-resolution.lock).

The clean upstream commit's own Cargo.lock pins libc 0.2.177. A second build was initiated from that lockfile, allowing Cargo to prune unrelated Zed packages for the standalone example. Its resulting lock has 681 packages. That retry uses upstream source unchanged and does not patch xattr or libc.

## Reproduction

In a new temporary directory, download the archive above, verify its SHA-256, extract it, and copy the upstream example to `src/main.rs`. Create this manifest:

```toml
[package]
name = "pigade-gpui-source-check"
version = "0.0.0"
edition = "2024"
[dependencies]
gpui = { path = "zed-69e2130295c2649963eb639fc70b4f2ee8ea1624/crates/gpui" }
```

For the failing attempt, copy `Cargo.latest-resolution.lock` to `Cargo.lock`. For the upstream-lock retry, use the archived upstream Cargo.lock (or the final retry lock when attached). Run:

```sh
CARGO_HOME=/tmp/pigade-gpui-cargo CARGO_BUILD_JOBS=2 cargo build --locked
```

The first generation of a standalone lock from upstream's workspace lock requires omitting `--locked`; subsequent reproductions should use the final standalone lock and `--locked`.

## What this cannot establish

Hello World contains no editable terminal surface or accessibility acceptance harness. Even a successful build/window proves neither four-pane terminal rendering nor EN/RU/IME, Unicode grid geometry, clipboard integration, fractional scaling, screen reader, GPU recovery, input latency or durable terminal performance. Those remain separate SPEC.md 10.4 gates. No X11 desktop session is certified by an XWayland DISPLAY in a Wayland session.

## Upstream-lock retry: build passed; native window mapped

The second build exited 0 in 3 min 08 s in the **debug** profile. This is not a release benchmark. [Build log](build-upstream-lock.log), [final standalone lockfile](Cargo.upstream-resolution.lock). GPUI produced two future-compatibility float-literal warnings; Cargo also reported future incompatibility notices for num-bigint-dig 0.8.4 and proc-macro-error2 2.0.1. These were not hidden or patched.

The untouched example was then run for 40 seconds under `timeout`. It registered a native Wayland window (`xwayland:false`) with Hyprland; [observed window metadata](window.json) records its PID, address and geometry. Runtime stderr/stdout was empty ([log](run-wayland.log)); timeout exited 124 after intentionally ending the smoke run. The registered window was 1912×1046 under the user's tiling compositor, despite the example requesting 500×500 bounds.

Visual rendering was **not verified**: the region capture showed the currently visible workspace instead of the example's workspace. Attempts to focus the specific window encountered this compositor's changed Lua dispatcher API. That image was not retained as proof. No input/IME, clipboard, screen reader, scale, pixel-grid or visual-design claim follows from a mapped surface. A next platform verification step must deliberately display the example and a terminal-specific input harness, then capture the correct surface.

The source build baseline is therefore viable with the recorded lockfile, while the native product gate remains open. The fresh-resolution failure also demonstrates why pinning only GPUI's Git commit is insufficient: retain and verify the complete dependency lock.
