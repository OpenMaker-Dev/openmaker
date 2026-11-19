#!/usr/bin/env bash
# Run a GUI command against a headless Xvfb display, for building/screenshotting
# the OpenMaker GUI on a server with no desktop. Usage:
#   tools/run-headless.sh <command...>
#   tools/run-headless.sh screenshot <out.png>   # capture the current :99 screen
set -euo pipefail
DISPLAY_NUM=:99
if ! pgrep -x Xvfb >/dev/null; then
  Xvfb "$DISPLAY_NUM" -screen 0 1600x1000x24 >/tmp/xvfb.log 2>&1 &
  sleep 2
fi
export DISPLAY="$DISPLAY_NUM"
export LIBGL_ALWAYS_SOFTWARE=1   # llvmpipe; no GPU
if [[ "${1:-}" == "screenshot" ]]; then
  scrot "${2:?usage: screenshot <out.png>}"
else
  exec "$@"
fi
