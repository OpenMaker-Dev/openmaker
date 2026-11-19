# Shared by the Linux systemd installer scripts.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
server_root='/opt/OpenMaker/OpenMaker Server'
config=/etc/openmaker
unit=openmaker-server.service
unit_path="/etc/systemd/system/$unit"

fail() {
    printf 'OpenMaker Server: %s\n' "$*" >&2
    exit 1
}

require_systemd() {
    [[ $(uname -s) == Linux && -d /run/systemd/system ]] ||
        fail 'These scripts require Linux booted with systemd.'
}

# Build as the caller. Elevation starts only after the binaries exist.
prepare_binaries() {
    require_systemd
    command -v ss >/dev/null || fail 'Install ss from iproute2 before installing or updating.'
    if (($# == 0)); then
        local repo
        repo=$(cd -- "$script_dir/../.." && pwd)
        (
            cd -- "$repo"
            CARGO_TARGET_DIR="$repo/target" cargo build --release --locked -p om-server
        )
        binary_dir="$repo/target/release"
    else
        binary_dir=$(cd -- "$1" && pwd)
    fi
    for binary in om-server om-server-pdf-worker; do
        [[ -f "$binary_dir/$binary" && -x "$binary_dir/$binary" ]] ||
            fail "Expected an executable $binary in $binary_dir"
    done
}

lock_installation() {
    require_systemd
    umask 022
    exec 9>>/run/openmaker-server-install.lock
    flock -x 9
}

# All paths used for installing executable code and config must be controlled by root.
root_directory() {
    local directory=$1
    [[ ! -L "$directory" ]] || fail "Refusing symlink: $directory"
    if [[ ! -e "$directory" ]]; then
        install -d -o root -g root -m 0755 -- "$directory"
    fi
    [[ -d "$directory" && $(stat -c %u -- "$directory") == 0 ]] ||
        fail "Expected a root-owned directory: $directory"
    local mode
    mode=$(stat -c %a -- "$directory")
    (( (8#$mode & 022) == 0 )) || fail "Directory is writable by other users: $directory"
}

managed_installation() {
    root_directory /opt/OpenMaker
    root_directory "$server_root"
    root_directory "$server_root/releases"
    [[ -f "$unit_path" && ! -L "$unit_path" &&
        $(stat -c %u -- "$unit_path") == 0 ]] &&
        grep -Fxq '# Managed by the OpenMaker Server service scripts.' "$unit_path" ||
        fail 'Expected a service installed by the OpenMaker Server scripts.'
    previous_release=$(readlink -- "$server_root/bin") ||
        fail 'The managed binary link is missing.'
    [[ $previous_release =~ ^releases/release\.[[:alnum:]]{8}$ &&
        -d "$server_root/$previous_release" && ! -L "$server_root/$previous_release" ]] ||
        fail 'The managed binary link points outside the release directory.'
    [[ $(systemctl show -p FragmentPath --value "$unit") == "$unit_path" ]] ||
        fail 'systemd is using a different unit file.'
}

stage_release() {
    local directory help_output status=0
    directory=$(mktemp -d -- "$server_root/releases/release.XXXXXXXX")
    staged_release="releases/${directory##*/}"
    install -m 0755 -- "$binary_dir/om-server" "$binary_dir/om-server-pdf-worker" "$directory"
    chmod 0755 -- "$directory"
    help_output=$(runuser -u omserver -- "$directory/om-server" --help 2>&1) || status=$?
    [[ $status == 0 || $status == 64 ]] || fail "Cannot run $binary_dir/om-server on this host."
    [[ $help_output == *--database-folder* && $help_output == *--admin-password-file* ]] ||
        fail 'Use an om-server build with database folder discovery and the Admin Console.'
}

activate_release() {
    local link="$server_root/.bin.$$"
    ln -s -- "$1" "$link" || return 1
    if ! mv -Tf -- "$link" "$server_root/bin"; then
        rm -- "$link"
        return 1
    fi
}

install_unit() {
    local staged_unit
    staged_unit=$(mktemp -- "$unit_path.XXXXXXXX") || return 1
    if ! install -m 0644 -- "$1" "$staged_unit" || ! mv -Tf -- "$staged_unit" "$unit_path"; then
        rm -f -- "$staged_unit"
        return 1
    fi
}

remove_release() {
    local directory="$server_root/$1"
    rm -f -- "$directory/om-server" "$directory/om-server-pdf-worker"
    rmdir -- "$directory"
}

start_and_wait() {
    local pid attempt
    systemctl start "$unit" || return 1
    # Type=exec proves execve succeeded. Wait for both listeners to belong to the service.
    for ((attempt = 0; attempt < 60; attempt++)); do
        systemctl is-active --quiet "$unit" || return 1
        pid=$(systemctl show -p MainPID --value "$unit") || return 1
        if [[ $pid =~ ^[1-9][0-9]*$ ]] &&
            ss -Hltnp | awk -v pid="$pid" 'index($0, "pid=" pid ",") { count++ } END { exit count < 2 }'; then
            return 0
        fi
        sleep 1
    done
    return 1
}

startup_error() {
    journalctl -u "$unit" -n 20 --no-pager >&2 || true
    fail 'The server did not start both listeners. See the service journal.'
}
