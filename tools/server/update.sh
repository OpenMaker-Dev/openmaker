#!/usr/bin/env bash
# shellcheck source=tools/server/common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"

if [[ ${1:-} == --help ]]; then
    echo 'Usage: tools/server/update.sh [directory containing om-server and om-server-pdf-worker]'
    exit 0
fi
(($# <= 1)) || fail 'Usage: tools/server/update.sh [binary directory]'
prepare_binaries "$@"
if ((EUID != 0)); then
    exec sudo -- "$script_dir/update.sh" "$binary_dir"
fi
lock_installation
managed_installation

scratch=$(mktemp -d)
cp -- "$unit_path" "$scratch/$unit"
staged_release=
was_active=false
switch_started=false
systemctl is-active --quiet "$unit" && was_active=true
cleanup() {
    local status=$?
    trap - EXIT
    if ((status != 0)); then
        if "$switch_started"; then
            if ! systemctl stop "$unit" ||
                ! activate_release "$previous_release" ||
                ! install_unit "$scratch/$unit" ||
                ! systemctl daemon-reload; then
                fail "Could not roll back. Previous unit saved in $scratch/$unit."
            fi
            if "$was_active"; then
                start_and_wait || startup_error
            fi
            echo 'Restored the previous OpenMaker Server release.' >&2
        fi
        if [[ -n $staged_release ]]; then
            remove_release "$staged_release"
        fi
    fi
    rm -rf -- "$scratch"
    exit "$status"
}
trap cleanup EXIT

stage_release
systemctl stop "$unit"
switch_started=true
activate_release "$staged_release"
install_unit "$script_dir/$unit"
systemctl daemon-reload
if "$was_active"; then
    start_and_wait || startup_error
fi
trap - EXIT
rm -rf -- "$scratch"
remove_release "$previous_release"
echo 'OpenMaker Server updated.'
