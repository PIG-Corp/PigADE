#!/bin/sh
# Temporary compatibility for a running pre-fix host whose binary was replaced.
# Install only as target/release/pigade-session-host\ (deleted). New hosts use
# /proc/<host-pid>/exe directly and do not need this shim.
set -eu

[ "${1-}" = "__helper" ] || exit 126
parent_pid=$PPID
shim_dir=$(CDPATH= cd -- "${0%/*}" && pwd -P)
image=$(/usr/bin/readlink "/proc/$parent_pid/exe") || exit 126
[ "$image" = "$shim_dir/pigade-session-host (deleted)" ] || exit 126

# exec preserves the child PID expected by the existing durable nonce gate.
# No fallback to a different host image or direct CLI execution is permitted.
exec "/proc/$parent_pid/exe" "$@"
