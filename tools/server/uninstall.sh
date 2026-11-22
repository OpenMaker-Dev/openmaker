#!/usr/bin/env bash
# shellcheck source=tools/server/common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"

if [[ ${1:-} == --help ]]; then
    echo 'Usage: tools/server/uninstall.sh'
    echo 'Remove the service and binaries. Keep data, settings, credentials, and the service account.'
    exit 0
fi
(($# == 0)) || fail 'Usage: tools/server/uninstall.sh'
if ((EUID != 0)); then
    exec sudo -- "$script_dir/uninstall.sh"
fi
lock_installation
uninstall_service
