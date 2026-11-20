#!/usr/bin/env bash
# Update an installation install.sh made: new binaries, and the job written
# again from server.env. Data, settings, the password, the account and the
# operator group's members are kept; a server that does not come back up
# is rolled back to the previous release and job.
# shellcheck source=tools/server/macos/common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"

if [[ ${1:-} == --help ]]; then
    echo 'Usage: tools/server/macos/update.sh [directory containing om-server and om-server-pdf-worker]'
    echo 'Also run it after changing server.env: the job is written again from it.'
    exit 0
fi
(($# <= 1)) || fail 'Usage: tools/server/macos/update.sh [binary directory]'
prepare_binaries "$@"
elevate "$script_dir/update.sh" "$binary_dir"
lock_installation

root_directory "$server_root"
root_directory "$server_root/releases"
root_directory "$config"
check_root_file "$config/server.env"
previous_release=$(installed_release)
ensure_accounts
service_directories
admin_password

scratch=$(mktemp -d)
cp -- "$plist_path" "$scratch/$label.plist"
staged_release=
was_loaded=false
switch_started=false
job_loaded && was_loaded=true
cleanup() {
    local status=$?
    trap - EXIT
    if ((status != 0)); then
        if "$switch_started"; then
            if ! stop_job || ! cp -- "$scratch/$label.plist" "$plist_path.new" ||
                ! install_plist "$plist_path.new"; then
                unlock_installation
                fail "Could not roll back. The previous job is saved in $scratch/$label.plist."
            fi
            if "$was_loaded"; then
                start_job && wait_listening || startup_error
            fi
            echo 'Restored the previous OpenMaker Server release.' >&2
        fi
        rm -f -- "$plist_path.new"
        if [[ -n $staged_release ]]; then
            remove_release "$staged_release"
        fi
    fi
    rm -rf -- "$scratch"
    unlock_installation
    exit "$status"
}
trap cleanup EXIT

stage_release
write_plist "$staged_release" "$plist_path.new"
stop_job
switch_started=true
install_plist "$plist_path.new"
if "$was_loaded"; then
    start_job
    wait_listening || startup_error
fi
trap - EXIT
rm -rf -- "$scratch"
remove_release "$previous_release"
unlock_installation
echo 'OpenMaker Server updated.'
