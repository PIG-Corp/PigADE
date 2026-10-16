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
# GPUI inherits source-workspace dependencies. Isolate its compatible lockfile
# from the host workspace, without adding a second terminal/parser/runtime.
export CARGO_HOME="${PIGADE_GPUI_CARGO_HOME:-${XDG_CACHE_HOME:-$HOME/.cache}/pigade/cargo}"
export CARGO_TARGET_DIR="${PIGADE_GPUI_TARGET_DIR:-$app_dir/target}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
command="${1:-build}"
if test "$#" -gt 0; then shift; fi
exec cargo "$command" --manifest-path "$app_dir/Cargo.toml" "$@"
