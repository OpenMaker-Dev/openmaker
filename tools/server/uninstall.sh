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
if [[ ! -e "$unit_path" && ! -L "$unit_path" && ! -L "$server_root/bin" &&
    $(systemctl show -p LoadState --value "$unit") == not-found ]]; then
    echo 'OpenMaker Server service is already absent.'
    exit 0
fi
managed_installation

systemctl disable --now "$unit"
rm -- "$unit_path"
systemctl daemon-reload
rm -- "$server_root/bin"
remove_release "$previous_release"
echo 'Removed the OpenMaker Server service and binaries. Kept data, settings, credentials, and the service account.'
