#!/usr/bin/env bash
# Install OpenMaker Server as a launchd daemon on macOS: the service account
# and operator group, the folders under /Library/OpenMaker Server, the
# settings and Admin Console password, the binaries and the job, started.
# shellcheck source=tools/server/macos/common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"

if [[ ${1:-} == --help ]]; then
    echo 'Usage: tools/server/macos/install.sh [directory containing om-server and om-server-pdf-worker]'
    echo 'Without a directory, builds om-server from this checkout first.'
    exit 0
fi
(($# <= 1)) || fail 'Usage: tools/server/macos/install.sh [binary directory]'
prepare_binaries "$@"
elevate "$script_dir/install.sh" "$binary_dir"
lock_installation

[[ ! -e "$plist_path" && ! -L "$plist_path" ]] && ! job_loaded ||
    fail 'A job is already installed. Use update.sh for an installation these scripts made.'

root_directory "$server_root/releases"
root_directory "$config"
root_directory "$data"

ensure_accounts
service_directories

check_root_file "$config/server.env"
if [[ ! -e "$config/server.env" ]]; then
    install -m 0644 -- "$script_dir/server.env" "$config/server.env"
    set_owner root:wheel "$config/server.env"
fi
admin_password

staged_release=
job_installed=false
cleanup() {
    local status=$?
    trap - EXIT
    if ((status != 0)); then
        if "$job_installed"; then
            # Leave the job's files if launchd cannot stop it.
            stop_job || {
                unlock_installation
                exit "$status"
            }
            rm -f -- "$plist_path"
        fi
        rm -f -- "$plist_path.new"
        if [[ -n $staged_release ]]; then
            remove_release "$staged_release"
        fi
    fi
    unlock_installation
    exit "$status"
}
trap cleanup EXIT

stage_release
write_plist "$staged_release" "$plist_path.new"
install_plist "$plist_path.new"
job_installed=true
start_job
wait_listening || startup_error
printf 'OpenMaker Server is running as %s:%s. Settings are in %s/server.env.\n' \
    "$service_user" "$service_group" "$config"
