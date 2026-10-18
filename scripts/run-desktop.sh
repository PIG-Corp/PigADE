#!/usr/bin/env bash
set -euo pipefail

script_path="$(readlink -f -- "${BASH_SOURCE[0]}")"
repo_root="$(cd -- "$(dirname -- "$script_path")/.." && pwd -P)"
app_binary="$repo_root/crates/app/target/release/pigade-app"

if [[ ! -x "$app_binary" ]]; then
    message="PigADE build not found at $app_binary. Build the current app binary first."
    if command -v notify-send >/dev/null 2>&1; then
        notify-send "PigADE" "$message" || true
    fi
    printf '%s\n' "$message" >&2
    exit 127
fi

if [[ ! ${PIGADE_HOST_BINARY+x} ]]; then
    export PIGADE_HOST_BINARY="$repo_root/target/release/pigade-session-host"
fi

exec "$app_binary" "$@"
