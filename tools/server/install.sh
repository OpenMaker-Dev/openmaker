#!/usr/bin/env bash
# shellcheck source=tools/server/common.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/common.sh"

if [[ ${1:-} == --help ]]; then
    echo 'Usage: tools/server/install.sh [directory containing om-server and om-server-pdf-worker]'
    exit 0
fi
(($# <= 1)) || fail 'Usage: tools/server/install.sh [binary directory]'
prepare_binaries "$@"
if ((EUID != 0)); then
    exec sudo -- "$script_dir/install.sh" "$binary_dir"
fi
lock_installation
require_no_pending_uninstall

managed_directories
root_directory "$server_root/Data"
root_directory "$config"
root_directory "$config/tls"

[[ ! -e "$unit_path" && ! -L "$unit_path" &&
    $(systemctl show -p LoadState --value "$unit") == not-found ]] ||
    fail 'A service already exists. Use update.sh for a script-managed installation.'
[[ ! -e "$server_root/bin" && ! -L "$server_root/bin" ]] ||
    fail "A binary directory already exists under $server_root."

getent group omadmin >/dev/null || groupadd --system omadmin
if ! getent passwd omserver >/dev/null; then
    useradd --system --gid omadmin --home-dir /var/lib/openmaker-server \
        --no-create-home --shell "$(command -v nologin)" omserver
fi
[[ $(id -gn omserver) == omadmin && $(id -u omserver) != 0 &&
    $(getent passwd omserver | cut -d: -f7) == */nologin ]] ||
    fail 'The existing omserver account must be a non-root, non-login omadmin member.'

for path in "$server_root/Data/Databases" "$server_root/Data/Backups" "$server_root/Logs"; do
    [[ ! -L "$path" ]] || fail "Refusing symlink: $path"
    if [[ ! -e "$path" ]]; then
        # Operators stage databases. The server requires sole write access to its logs.
        mode=0750
        if [[ "$path" == "$server_root/Data/Databases" ]]; then
            mode=2770
        fi
        install -d -o omserver -g omadmin -m "$mode" -- "$path"
    fi
    if [[ ! -d "$path" ]] || ! runuser -u omserver -- test -w "$path"; then
        fail "Service account cannot write to $path."
    fi
done

scratch=$(mktemp -d)
staged_release=
unit_installed=false
cleanup() {
    local status=$?
    trap - EXIT
    if ((status != 0)); then
        if "$unit_installed"; then
            # Leave files in place if systemd cannot stop the service.
            systemctl disable --now "$unit" || exit "$status"
            rm -f -- "$unit_path"
            systemctl daemon-reload || true
        fi
        if [[ -n $staged_release ]]; then
            rm -f -- "$server_root/bin" "$server_root/.bin.$$"
            remove_release "$staged_release"
        fi
    fi
    rm -rf -- "$scratch"
    exit "$status"
}
trap cleanup EXIT

for path in "$config/server.env" "$config/admin-password" "$config/tls/cert.pem" "$config/tls/key.pem"; do
    [[ ! -L "$path" ]] || fail "Refusing symlink: $path"
    if [[ -e "$path" ]]; then
        [[ -f "$path" && $(stat -c %u -- "$path") == 0 ]] ||
            fail "Expected a root-owned file: $path"
        mode=$(stat -c %a -- "$path")
        (( (8#$mode & 022) == 0 )) || fail "File is writable by other users: $path"
    fi
done
if [[ ! -e "$config/server.env" ]]; then
    install -m 0644 -- "$script_dir/server.env" "$config/server.env"
fi
if [[ ! -e "$config/admin-password" ]]; then
    openssl rand -hex 24 >"$scratch/admin-password"
    install -m 0600 -- "$scratch/admin-password" "$config/admin-password"
fi
if [[ ! -e "$config/tls/cert.pem" && ! -e "$config/tls/key.pem" ]]; then
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -sha256 -days 365 \
        -subj /CN=localhost -addext 'subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1' \
        -addext 'basicConstraints=critical,CA:FALSE' -addext 'extendedKeyUsage=serverAuth' \
        -out "$scratch/cert.pem" -keyout "$scratch/key.pem"
    install -m 0644 -- "$scratch/cert.pem" "$config/tls/cert.pem"
    install -m 0600 -- "$scratch/key.pem" "$config/tls/key.pem"
fi
[[ -s "$config/tls/cert.pem" && -s "$config/tls/key.pem" ]] ||
    fail "Supply both $config/tls/cert.pem and $config/tls/key.pem."
chmod 0600 -- "$config/admin-password" "$config/tls/key.pem"

stage_release
activate_release "$staged_release"
install_unit "$script_dir/$unit"
unit_installed=true
systemctl daemon-reload
start_and_wait || startup_error
systemctl enable "$unit"
printf 'OpenMaker Server is running. Settings are in %s/server.env.\n' "$config"
