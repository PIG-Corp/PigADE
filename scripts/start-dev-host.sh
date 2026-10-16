#!/usr/bin/env bash
set -euo pipefail
PIGADE_REPO_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PIGADE_STATE_DIR="${PIGADE_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/pigade/dev-0.0.1}"
PIGADE_UNIT="${PIGADE_UNIT:-pigade-dev-host}"
if [[ ! -x "$PIGADE_REPO_DIR/target/release/pigade-session-host" ]]; then
  printf '%s\n' 'Build first: cargo build --release --workspace --bins --examples' >&2
  exit 1
fi
mkdir -p -- "$PIGADE_STATE_DIR"
chmod 700 -- "$PIGADE_STATE_DIR"
exec systemd-run --user --no-ask-password --collect --unit="$PIGADE_UNIT" \
  --service-type=exec --property=KillMode=control-group \
  --working-directory="$PIGADE_REPO_DIR" \
  "$PIGADE_REPO_DIR/target/release/pigade-session-host" serve "$PIGADE_STATE_DIR"
