# GPUI 322cd509 Linux dependency license metadata

This inventory follows the resolved package graph for `crates/app/Cargo.toml` and `crates/app/Cargo.lock` on `x86_64-unknown-linux-gnu`, with the app's configured GPUI Wayland and X11 features. The machine-readable package inventory is [gpui-322cd509-linux-licenses.json](gpui-322cd509-linux-licenses.json). Cargo's raw metadata was saved outside the repository at `/tmp/pigade-gpui-322cd509-linux-metadata.json`.

The exact command was:

```sh
CARGO_HOME=/tmp/pigade-gpui-cargo \
CARGO_TARGET_DIR=/tmp/pigade-gpui-source-check/target \
cargo metadata \
  --manifest-path crates/app/Cargo.toml \
  --locked \
  --format-version 1 \
  --filter-platform x86_64-unknown-linux-gnu
```

The report traverses `resolve.nodes` starting at `resolve.root`, so it includes the 481 packages reachable for that target and excludes unrelated locked packages. License expressions come from Cargo metadata. The report parses SPDX `AND`, `OR`, `WITH`, and parentheses with the standard `AND`-before-`OR` precedence; Cargo's legacy slash-separated alternatives are interpreted as `OR`. For a GPL/LGPL expression, an `AND` requires a copyleft branch if either child does, while an `OR` requires it only if every branch does.

The Linux closure contains one GPL alternative: `self_cell 1.3.0` declares `Apache-2.0 OR GPL-2.0-only`. The expression allows the Apache-2.0 alternative, so GPL is not required by the declared choice. No LGPL expression was found. `option-ext 0.2.0` declares MPL-2.0 and should receive a separate license review. `pigade-app` has no license expression because the product license remains undecided. These findings flag review points; they do not recommend automatically removing dependencies.

This is a package-metadata inventory, not legal advice, a review of source-file notices, or a distribution bundle audit. It describes only the Linux target and enabled dependency features. It does not verify Windows, macOS, other target architectures, alternate feature sets, or their resolved package closures. Cargo metadata itself did not compile or package the app; the locked Linux app build passed separately during integration.
