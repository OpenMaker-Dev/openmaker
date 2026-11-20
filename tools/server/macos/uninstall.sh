#!/usr/bin/env bash
# Remove the launchd job and the binaries install.sh put in place. Data,
# logs, settings, the Admin Console password, the service account and the
# operator group (with its members) are kept for a later install.
# shellcheck source=tools/server/macos/common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"

if [[ ${1:-} == --help ]]; then
    echo 'Usage: tools/server/macos/uninstall.sh'
    echo 'Remove the job and binaries. Keep data, settings, the password, the account and the group.'
    exit 0
fi
(($# == 0)) || fail 'Usage: tools/server/macos/uninstall.sh'
require_macos
elevate "$script_dir/uninstall.sh"
if [[ ! -e "$plist_path" && ! -L "$plist_path" ]] && ! job_loaded; then
    echo 'OpenMaker Server job is already absent.'
    exit 0
fi
lock_installation
root_directory "$server_root"
root_directory "$server_root/releases"
release=$(installed_release)

stop_job
rm -- "$plist_path"
remove_release "$release"
echo 'Removed the OpenMaker Server job and binaries. Kept data, settings, the password, the account and the group.'
