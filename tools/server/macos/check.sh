#!/usr/bin/env bash
# Check the macOS launchd scripts on disposable installs, as the current
# user: each scenario installs beneath a temporary folder standing for `/`
# (OPENMAKER_PREFIX), where ownership, the service account and group, and
# launchd are recorded instead of changed. Nothing on this Mac is changed
# and no administrator is needed. Then the job's own command line runs the
# server, as the current user, on a staged blank database.
#
#   tools/server/macos/check.sh [directory with om-server, om-server-pdf-worker, om-author]
#
# Without a directory it builds the three (debug) from this checkout.
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$here/../../.." && pwd)
if (($# == 0)); then
    (cd -- "$repo" && cargo build --locked -p om-server -p om-author)
    bin="${CARGO_TARGET_DIR:-$repo/target}/debug"
else
    bin=$(cd -- "$1" && pwd)
fi
work=$(mktemp -d)
server_pid=
finish() {
    local status=$?
    [[ -z $server_pid ]] || kill "$server_pid" 2>/dev/null || true
    rm -rf -- "${work:?}"
    if ((status == 0)); then
        echo 'PASS'
    else
        echo 'FAIL' >&2
    fi
}
trap finish EXIT

failed() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}
mode() { stat -f %Lp -- "$1"; }
expect_mode() {
    [[ $(mode "$1") == "$2" ]] || failed "$1 is $(mode "$1"), expected $2"
}
job_value() { plutil -extract "$2" raw -o - "$1"; }
job_args() {
    local plist=$1 i=0 arg
    while arg=$(plutil -extract "ProgramArguments.$i" raw -o - "$plist" 2>/dev/null); do
        printf '%s\n' "$arg"
        i=$((i + 1))
    done
}
run() {
    # One scenario's script, beneath `root`.
    local root=$1
    shift
    OPENMAKER_PREFIX=$root "$here/$1" "${@:2}"
}

# --- A fresh install: the account and group made, the folders, the job.
fresh=$work/fresh
mkdir -p -- "$fresh"
run "$fresh" install.sh "$bin" >/dev/null
server="$fresh/Library/OpenMaker Server"
plist="$fresh/Library/LaunchDaemons/dev.openmaker.server.plist"
grep -q '^create Groups omadmin PrimaryGroupID ' "$fresh/.ds/log" || failed 'no omadmin group made'
gid=$(cat "$fresh/.ds/Groups/omadmin/PrimaryGroupID")
uid=$(cat "$fresh/.ds/Users/omserver/UniqueID")
((gid >= 300 && gid < 500 && uid >= 300 && uid < 500)) || failed "ids $uid:$gid outside 300-499"
[[ $(cat "$fresh/.ds/Users/omserver/PrimaryGroupID") == "$gid" &&
    $(cat "$fresh/.ds/Users/omserver/UserShell") == /usr/bin/false &&
    $(cat "$fresh/.ds/Users/omserver/Password") == '*' ]] ||
    failed 'omserver is not a non-login account of omadmin'
expect_mode "$server" 755
expect_mode "$server/Config" 755
expect_mode "$server/Data/Databases" 770
expect_mode "$server/Data/Backups" 750
expect_mode "$server/Data/State" 700
expect_mode "$server/Logs" 750
expect_mode "$server/Config/server.env" 644
expect_mode "$server/Config/admin-password" 400
expect_mode "$plist" 644
[[ -s "$server/Config/admin-password" ]] || failed 'no Admin Console password'
for owned in "omserver:omadmin	/Library/OpenMaker Server/Data/Databases" \
    "omserver:omadmin	/Library/OpenMaker Server/Config/admin-password" \
    "root:wheel	/Library/OpenMaker Server/Config" \
    "root:wheel	/Library/LaunchDaemons/dev.openmaker.server.plist"; do
    grep -qxF -- "$owned" "$fresh/.owners" || failed "not given: $owned"
done
plutil -lint -s "$plist" || failed 'the job is not a valid property list'
[[ $(job_value "$plist" Label) == dev.openmaker.server &&
    $(job_value "$plist" UserName) == omserver &&
    $(job_value "$plist" GroupName) == omadmin &&
    $(job_value "$plist" Umask) == 23 &&
    $(job_value "$plist" KeepAlive.SuccessfulExit) == false ]] ||
    failed 'the job does not run the server as omserver:omadmin with umask 027'
first=$(job_value "$plist" ProgramArguments.0)
[[ $first == "/Library/OpenMaker Server/releases/release."*/om-server ]] ||
    [[ $first == "$server/releases/release."*/om-server ]] || failed "the job runs $first"
grep -q '^launchctl bootstrap system ' "$fresh/.launchctl" || failed 'the job was not started'
if run "$fresh" install.sh "$bin" >/dev/null 2>&1; then
    failed 'a second install went ahead'
fi
echo 'fresh install: account, group, folders, password and job'

# --- An update keeps data, the password, the account and the operators.
printf 'staged by an operator' >"$server/Data/Databases/Kept.fmp12"
echo alice >"$fresh/.ds/Groups/omadmin/GroupMembership"
password=$(cat "$server/Config/admin-password")
accounts=$(cat "$fresh/.ds/log")
sed -i '' 's/^OPENMAKER_NAME=.*/OPENMAKER_NAME=renamed/' "$server/Config/server.env"
run "$fresh" update.sh "$bin" >/dev/null
second=$(job_value "$plist" ProgramArguments.0)
[[ $second != "$first" && -x "$second" ]] || failed 'the update did not switch releases'
[[ ! -e "$first" && ! -e "$(dirname -- "$first")" ]] || failed 'the previous release stayed'
grep -qx renamed <<<"$(job_args "$plist")" || failed 'the job was not written from server.env'
[[ $(cat "$server/Data/Databases/Kept.fmp12") == 'staged by an operator' ]] ||
    failed 'the update changed a staged database'
[[ $(cat "$fresh/.ds/Groups/omadmin/GroupMembership") == alice ]] ||
    failed "the update changed omadmin's members"
[[ $(cat "$fresh/.ds/log") == "$accounts" ]] || failed 'the update changed the accounts'
[[ $(cat "$server/Config/admin-password") == "$password" ]] || failed 'the update made a new password'
grep -q "^launchctl bootout system/dev.openmaker.server" "$fresh/.launchctl" ||
    failed 'the update did not stop the job'
echo 'update: new release and job; data, password, account and operators kept'

# --- Uninstall keeps everything but the job and the binaries.
run "$fresh" uninstall.sh >/dev/null
[[ ! -e "$plist" && -z $(ls -A -- "$server/releases") ]] || failed 'uninstall left the job or binaries'
[[ -f "$server/Data/Databases/Kept.fmp12" && -f "$server/Config/server.env" &&
    -f "$server/Config/admin-password" && -d "$fresh/.ds/Users/omserver" &&
    $(cat "$fresh/.ds/Groups/omadmin/GroupMembership") == alice ]] ||
    failed 'uninstall removed data, settings, the account or the operators'
grep -q 'already absent' <<<"$(run "$fresh" uninstall.sh)" || failed 'a second uninstall did not stop'
echo 'uninstall: job and binaries removed; data, settings and accounts kept'

# --- Existing accounts are used, and their members kept.
existing=$work/existing
mkdir -p -- "$existing/.ds/Groups/omadmin" "$existing/.ds/Users/omserver"
echo 451 >"$existing/.ds/Groups/omadmin/PrimaryGroupID"
printf 'alice\nbob\n' >"$existing/.ds/Groups/omadmin/GroupMembership"
echo 452 >"$existing/.ds/Users/omserver/UniqueID"
echo 451 >"$existing/.ds/Users/omserver/PrimaryGroupID"
echo /usr/bin/false >"$existing/.ds/Users/omserver/UserShell"
run "$existing" install.sh "$bin" >/dev/null
[[ ! -e "$existing/.ds/log" ]] || failed 'install changed existing accounts'
[[ $(cat "$existing/.ds/Groups/omadmin/GroupMembership") == $'alice\nbob' ]] ||
    failed "install changed omadmin's members"
echo 'existing account and group: used as they are, members kept'

# --- An unsuitable existing account stops the install before anything runs.
unsafe=$work/unsafe
mkdir -p -- "$unsafe/.ds/Groups/omadmin" "$unsafe/.ds/Users/omserver"
echo 461 >"$unsafe/.ds/Groups/omadmin/PrimaryGroupID"
echo 462 >"$unsafe/.ds/Users/omserver/UniqueID"
echo 461 >"$unsafe/.ds/Users/omserver/PrimaryGroupID"
echo /bin/zsh >"$unsafe/.ds/Users/omserver/UserShell"
if run "$unsafe" install.sh "$bin" >/dev/null 2>&1; then
    failed 'install accepted a login account'
fi
[[ ! -e "$unsafe/Library/LaunchDaemons/dev.openmaker.server.plist" &&
    ! -e "$unsafe/.launchctl" ]] || failed 'a refused install left a job'
echo 'a login omserver account: refused, nothing installed'

# --- The job's command line serves a staged database copy and writes its
# state and logs, as the current user standing for omserver. A disposable
# install's job names the folders beneath its prefix.
rm -f -- "$server/Data/Databases/Kept.fmp12"
"$bin/om-author" create-file --path "$work/Staged.fmp12" >/dev/null
install -m 0660 -- "$work/Staged.fmp12" "$server/Data/Databases/Staged.fmp12"
run "$fresh" install.sh "$bin" >/dev/null
args=()
while IFS= read -r line; do
    args+=("$line")
done < <(job_args "$plist")
[[ ${args[0]} == "$server/releases/release."*/om-server ]] || failed "the job runs ${args[0]}"
# A blank file's Admin has no password: FileMaker Server would not host it,
# and neither does om-server unless told to. Ephemeral ports, so that the
# check runs beside a server on the defaults.
"${args[@]}" --allow-insecure-files --bind 127.0.0.1:0 --admin-bind 127.0.0.1:0 \
    >"$work/server.log" 2>&1 &
server_pid=$!
opened='Opened database "Staged".'
for ((attempt = 0; attempt < 60; attempt++)); do
    if grep -qF -- "$opened" "$server/Logs/Event.log" 2>/dev/null &&
        [[ -s "$server/Data/State/tls/cert.pem" ]]; then
        break
    fi
    kill -0 "$server_pid" 2>/dev/null || {
        cat "$work/server.log" >&2
        failed 'the server stopped'
    }
    sleep 1
done
grep -qF -- "$opened" "$server/Logs/Event.log" 2>/dev/null || {
    cat "$work/server.log" >&2
    failed 'the server did not open the staged database for writing'
}
[[ -s "$server/Data/State/tls/cert.pem" ]] || failed 'the server wrote no state'
echo 'the job command line: the staged database opened for writing, state and logs written'
