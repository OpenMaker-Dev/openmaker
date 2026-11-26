#!/bin/sh
# Run a command on a private headless X display, then stop the display.
#
#   tools/xvfb-run.sh [-s "<Xvfb arguments>"] <command> [arguments...]
#
# For GUI tests and runs, in place of `xvfb-run -a`. That picks the first
# display number without a lock file in /tmp and only then starts Xvfb, so
# jobs started at about the same time pick the same number and all but one
# Xvfb exit. Debian's xvfb-run then tries the next number; Arch's does not
# notice, runs the command on the other job's display, and that display
# stops when the other job ends (a test then fails with XOpenDisplayFailed).
#
# Here each try starts Xvfb on a number, and Xvfb's lock file decides: the
# loser exits and the next number is tried. Xvfb signals USR1 to this shell
# once it accepts connections. Not -displayfd: with it Xvfb ignores lock
# files, and takes over a display that only has one, as XWayland's :0 under
# Hyprland.
#
# The default screen is 1600x1000x24; -s replaces it, as xvfb-run's does.
# WAYLAND_DISPLAY is unset so that GTK and winit use the display, not a
# Wayland desktop the command was started from.
set -eu

screen='-screen 0 1600x1000x24'
if [ "${1-}" = -s ]; then
    screen=${2:?"-s needs Xvfb's arguments"}
    shift 2
fi
if [ $# -eq 0 ]; then
    echo "usage: $0 [-s \"<Xvfb arguments>\"] <command> [arguments...]" >&2
    exit 2
fi

dir=$(mktemp -d "${TMPDIR:-/tmp}/om-xvfb.XXXXXX")
pid=
stop() {
    if [ -n "$pid" ]; then
        kill "$pid" 2>/dev/null || :
        wait "$pid" 2>/dev/null || :
    fi
    rm -rf "$dir"
}
trap stop EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
ready=
trap 'ready=1' USR1

cookie=$(mcookie)
: >"$dir/auth"
# From :200, clear of desktops' displays and of xvfb-run's (from :99).
number=200
while :; do
    if [ "$number" -ge 300 ]; then
        echo "$0: no free X display from :200 to :299" >&2
        exit 1
    fi
    if [ -e "/tmp/.X$number-lock" ] || [ -e "/tmp/.X11-unix/X$number" ]; then
        number=$((number + 1))
        continue
    fi
    xauth -q -f "$dir/auth" add ":$number" . "$cookie"
    # Started with USR1 ignored, Xvfb signals its parent when it is ready.
    # $screen is a list of arguments.
    # shellcheck disable=SC2086
    (
        trap '' USR1
        exec Xvfb ":$number" -nolisten tcp -auth "$dir/auth" $screen 2>"$dir/log"
    ) &
    pid=$!
    waited=0
    while [ -z "$ready" ] && kill -0 "$pid" 2>/dev/null; do
        if [ "$waited" -ge 600 ]; then
            echo "$0: Xvfb :$number did not start in 60 s" >&2
            exit 1
        fi
        sleep 0.1
        waited=$((waited + 1))
    done
    if [ -n "$ready" ]; then
        break
    fi
    # Another server holds the number, or Xvfb failed.
    wait "$pid" 2>/dev/null || :
    pid=
    if ! grep -Eq 'already (active|running)' "$dir/log"; then
        echo "$0: Xvfb :$number did not start:" >&2
        tail -n 5 "$dir/log" >&2
        exit 1
    fi
    number=$((number + 1))
done

unset WAYLAND_DISPLAY
DISPLAY=:$number XAUTHORITY=$dir/auth
export DISPLAY XAUTHORITY
status=0
"$@" || status=$?
exit "$status"
