#!/usr/bin/env bash
set -euo pipefail
# This builds PigADE itself. No separate comparison application is created.
app_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
commit=322cd509dcccaee1f6bc44725d270598ae8171ad
cache="${PIGADE_GPUI_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/pigade/gpui}"
mkdir -p "$cache" "$app_dir/.upstream"
archive="$cache/$commit.tar.gz"
checksum=27e6a724515cb6e6fa8a456098d075bfa2a50b4f6145491105555e56de2d256d
if ! test -f "$archive"; then
  download="$(mktemp "$cache/download.XXXXXXXX")"
  trap 'rm -f -- "$download"' EXIT
  curl --fail --location "https://codeload.github.com/zed-industries/zed/tar.gz/$commit" --output "$download"
  printf '%s  %s\n' "$checksum" "$download" | sha256sum --check
  mv -- "$download" "$archive"
  trap - EXIT
fi
printf '%s  %s\n' "$checksum" "$archive" | sha256sum --check
source_dir="$app_dir/.upstream/zed-$commit"
if ! test -d "$source_dir"; then
  extract="$(mktemp -d "$app_dir/.upstream/extract.XXXXXXXX")"
  trap 'rm -rf -- "$extract"' EXIT
  tar -xzf "$archive" -C "$extract"
  mv -- "$extract/zed-$commit" "$source_dir"
  rmdir -- "$extract"
  trap - EXIT
fi
python3 "$app_dir/../../scripts/apply-gpui-patches.py" "$source_dir" \
  "$app_dir/patches/gpui-x11-ime/manifest.json" --archive "$archive"
# Keep the existing pinned XIM transport, with its synchronous preedit reply fix.
xim_commit=16f35a2c881b815a2b6cdfd6687988e84f8447d8
xim_checksum=a1a2163b226b6103e1ac39932832439ae7003c1e7bd559ce63dc50b6d68610cc
xim_archive="$cache/xim-$xim_commit.tar.gz"
if ! test -f "$xim_archive"; then
  xim_download="$(mktemp "$cache/xim-download.XXXXXXXX")"
  trap 'rm -f -- "$xim_download"' EXIT
  curl --fail --location "https://codeload.github.com/zed-industries/xim-rs/tar.gz/$xim_commit" --output "$xim_download"
  printf '%s  %s\n' "$xim_checksum" "$xim_download" | sha256sum --check
  mv -- "$xim_download" "$xim_archive"
  trap - EXIT
fi
printf '%s  %s\n' "$xim_checksum" "$xim_archive" | sha256sum --check
xim_source="$app_dir/.upstream/xim-rs-$xim_commit"
if ! test -d "$xim_source"; then
  xim_extract="$(mktemp -d "$app_dir/.upstream/xim-extract.XXXXXXXX")"
  trap 'rm -rf -- "$xim_extract"' EXIT
  tar -xzf "$xim_archive" -C "$xim_extract"
  mv -- "$xim_extract/xim-rs-$xim_commit" "$xim_source"
  rmdir -- "$xim_extract"
  trap - EXIT
fi
python3 "$app_dir/../../scripts/apply-gpui-patches.py" "$xim_source" \
  "$app_dir/patches/xim-preedit/manifest.json" --archive "$xim_archive"
# GPUI inherits source-workspace dependencies. Isolate its compatible lockfile
# from the host workspace, without adding a second terminal/parser/runtime.
export CARGO_HOME="${PIGADE_GPUI_CARGO_HOME:-${XDG_CACHE_HOME:-$HOME/.cache}/pigade/cargo}"
export CARGO_TARGET_DIR="${PIGADE_GPUI_TARGET_DIR:-$app_dir/target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
command="${1:-build}"
if test "$#" -gt 0; then shift; fi
if test "$command" = test-xim; then
  exec cargo test --manifest-path "$xim_source/Cargo.toml" --package zed-xim --lib --features x11rb-client "$@"
fi
exec cargo "$command" --manifest-path "$app_dir/Cargo.toml" "$@"
