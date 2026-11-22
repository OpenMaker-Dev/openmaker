#!/usr/bin/env bash
# Exercise uninstall with a temporary installation and mocked systemd.
set -euo pipefail
umask 022

common=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/common.sh
scratch=$(mktemp -d)
trap 'find "$scratch" -depth -delete' EXIT

stat() {
    # The test runner owns the fixtures, standing in for root.
    if [[ $2 == %u ]]; then
        [[ -O "$4" ]] && printf '0\n'
    elif command stat -c %a -- . >/dev/null 2>&1; then
        command stat "$@"
    else
        command stat -f %Lp -- "$4"
    fi
}

mv() {
    # BSD mv lacks -T. The fixture destination is absent at this step.
    [[ $1 == -T && $2 == -- && ! -e "$4" && ! -L "$4" ]] || return 1
    command mv -- "$3" "$4"
}

systemctl() {
    local unit=${unit_path##*/}
    case "$*" in
        "show -p FragmentPath --value $unit") printf '%s\n' "$unit_path" ;;
        "show -p LoadState --value $unit")
            [[ -f "$fixture/reloaded" ]] || return 1
            printf 'not-found\n'
            ;;
        "disable --now $unit")
            [[ -f "$unit_path" && -L "$server_root/bin" ]] || return 1
            touch "$fixture/stopped"
            ;;
        daemon-reload)
            [[ -f "$fixture/stopped" && ! -e "$unit_path" ]] || return 1
            if [[ -f "$fixture/fail-reload" ]]; then
                printf 'Mock daemon-reload failed\n' >&2
                return 1
            fi
            touch "$fixture/reloaded"
            ;;
        *) printf 'Unexpected systemctl call: %s\n' "$*" >&2; return 1 ;;
    esac
}
export -f stat mv systemctl

run_uninstall() {
    bash -c '
        source "$1"
        fixture=$2
        server_root="$fixture/opt/OpenMaker Server"
        unit_path="$fixture/$unit"
        uninstall_service
    ' bash "$common" "$fixture" >"$fixture/output" 2>&1
}

for failure in reload release; do
    fixture="$scratch/$failure"
    server_root="$fixture/opt/OpenMaker Server"
    release=releases/release.ABC12345
    pending="$server_root/.uninstall-release"
    unit_path="$fixture/openmaker-server.service"
    mkdir -p -- "$server_root/$release" "$server_root/releases/release.OTHER123" \
        "$server_root/Data/Databases" "$fixture/config"
    printf '# Managed by the OpenMaker Server service scripts.\n' >"$unit_path"
    touch "$server_root/$release/om-server" "$server_root/$release/om-server-pdf-worker" \
        "$server_root/releases/release.OTHER123/om-server"
    printf 'database\n' >"$server_root/Data/Databases/fixture.fmp12"
    printf 'settings\n' >"$fixture/config/server.env"
    ln -s -- "$release" "$server_root/bin"

    if [[ $failure == reload ]]; then
        touch "$fixture/fail-reload"
    else
        # The installer owns the executables, but rmdir must refuse an extra file.
        printf 'operator file\n' >"$server_root/$release/keep"
    fi
    if run_uninstall; then
        printf 'Expected uninstall to fail during %s\n' "$failure" >&2
        exit 1
    fi
    [[ ! -e "$unit_path" && ! -L "$server_root/bin" && $(readlink "$pending") == "$release" ]]
    if [[ $failure == reload ]]; then
        grep -Fq 'Mock daemon-reload failed' "$fixture/output"
        [[ -f "$server_root/$release/om-server" && -f "$server_root/$release/om-server-pdf-worker" ]]
        rm -- "$fixture/fail-reload"
    else
        [[ -f "$fixture/reloaded" && ! -e "$server_root/$release/om-server" &&
            ! -e "$server_root/$release/om-server-pdf-worker" &&
            $(cat "$server_root/$release/keep") == 'operator file' ]]
        command mv -- "$server_root/$release/keep" "$fixture/keep"
    fi

    run_uninstall || { cat "$fixture/output" >&2; exit 1; }
    grep -Fq 'Removed the OpenMaker Server service and binaries.' "$fixture/output"
    [[ ! -e "$server_root/$release" && ! -e "$pending" && ! -L "$pending" &&
        -f "$server_root/releases/release.OTHER123/om-server" &&
        $(cat "$server_root/Data/Databases/fixture.fmp12") == database &&
        $(cat "$fixture/config/server.env") == settings ]]
    run_uninstall || { cat "$fixture/output" >&2; exit 1; }
    grep -Fq 'OpenMaker Server service is already absent.' "$fixture/output"
    printf 'Uninstall resumed after %s failure\n' "$failure"
done
