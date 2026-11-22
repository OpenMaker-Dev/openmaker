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

# --- The directory search's answer, as `dscl /Search -search` prints it: a
# name with spaces, a name with tabs, and two records of one name.
sample=$(printf 'omserver\t\tUniqueID = (\n    452\n)\nOther Service\t\tUniqueID = (\n    452\n)\nTabbed\t\tService\t\tUniqueID = (\n    452\n)\nomserver\t\tUniqueID = (\n    452\n)\n')
[[ $(OPENMAKER_PREFIX=$work/parse bash -c 'source "$1"; search_headers UniqueID' _ "$here/common.sh" <<<"$sample") == $'omserver\nOther Service\nTabbed\t\tService\nomserver' ]] ||
    failed 'the directory search answer was misread'
echo 'directory search answer: names with spaces, tabs and repetitions kept'

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
    "root:wheel	/Library/LaunchDaemons/dev.openmaker.server.plist.new"; do
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

# A directory service holding omadmin (members alice and bob) and omserver
# beneath `root`, with `shell`.
accounts() {
    local root=$1 shell=$2
    mkdir -p -- "$root/.ds/Groups/omadmin" "$root/.ds/Users/omserver"
    echo 451 >"$root/.ds/Groups/omadmin/PrimaryGroupID"
    printf 'alice\nbob\n' >"$root/.ds/Groups/omadmin/GroupMembership"
    echo 452 >"$root/.ds/Users/omserver/UniqueID"
    echo 451 >"$root/.ds/Users/omserver/PrimaryGroupID"
    echo "$shell" >"$root/.ds/Users/omserver/UserShell"
    echo '*' >"$root/.ds/Users/omserver/Password"
}

# --- Existing accounts are used, and their members kept.
existing=$work/existing
accounts "$existing" /usr/bin/false
run "$existing" install.sh "$bin" >/dev/null
[[ ! -e "$existing/.ds/log" ]] || failed 'install changed existing accounts'
[[ $(cat "$existing/.ds/Groups/omadmin/GroupMembership") == $'alice\nbob' ]] ||
    failed "install changed omadmin's members"
echo 'existing account and group: used as they are, members kept'

# --- Unsuitable existing accounts, or folders others may change, stop the
# install before anything is installed.
refused() {
    local name=$1 why=$2
    if run "$work/$name" install.sh "$bin" >"$work/$name.out" 2>&1; then
        failed "install accepted $why"
    fi
    [[ ! -e "$work/$name/Library/LaunchDaemons/dev.openmaker.server.plist" &&
        ! -e "$work/$name/.launchctl" ]] || failed "a refused install ($why) left a job"
    echo "refused, nothing installed: $why ($(sed 's/^OpenMaker Server: //' "$work/$name.out" | tail -n 1))"
}
accounts "$work/login" /bin/zsh
refused login 'an omserver account that can log in'
accounts "$work/password" /usr/bin/false
echo 'a stored verifier' >"$work/password/.ds/Users/omserver/AuthenticationAuthority"
refused password 'an omserver account with a password'
accounts "$work/shared" /usr/bin/false
mkdir -p -- "$work/shared/.ds/Users/other"
echo 452 >"$work/shared/.ds/Users/other/UniqueID"
refused shared "another user with omserver's id"
accounts "$work/admin" /usr/bin/false
mkdir -p -- "$work/admin/.ds/Groups/admin"
echo 80 >"$work/admin/.ds/Groups/admin/PrimaryGroupID"
echo omserver >"$work/admin/.ds/Groups/admin/GroupMembership"
refused admin 'an omserver account in the admin group'
mkdir -p -- "$work/acl/Library"
chmod +a 'everyone allow add_file,add_subdirectory,delete_child' "$work/acl/Library"
refused acl 'a parent folder an access control entry opens to writes'

# --- An update refuses kept files and folders more open than it makes them.
kept=$work/kept
mkdir -p -- "$kept"
run "$kept" install.sh "$bin" >/dev/null
kept_server="$kept/Library/OpenMaker Server"
chmod 0444 "$kept_server/Config/admin-password"
if run "$kept" update.sh "$bin" >"$work/kept.out" 2>&1; then
    failed 'update accepted a password file others may read'
fi
grep -q 'admin-password is open to other users' "$work/kept.out" || failed "update refused it for another reason: $(cat "$work/kept.out")"
chmod 0400 "$kept_server/Config/admin-password"
chmod 0777 "$kept_server/Data/Backups"
if run "$kept" update.sh "$bin" >"$work/kept.out" 2>&1; then
    failed 'update accepted a backup folder others may write'
fi
grep -q 'Backups is more open than' "$work/kept.out" || failed "update refused it for another reason: $(cat "$work/kept.out")"
chmod 0750 "$kept_server/Data/Backups"
# The same through access control entries, and a job others may change.
kept_job="$kept/Library/LaunchDaemons/dev.openmaker.server.plist"
for opened in "$kept_server/Config/admin-password|everyone allow read|admin-password is open" \
    "$kept_server/Data/Backups|everyone allow add_file|Backups is open to other users' writes" \
    "$kept_job|everyone allow write|may be changed by other users"; do
    path=${opened%%|*}
    rest=${opened#*|}
    entry=${rest%%|*}
    reason=${rest#*|}
    chmod +a "$entry" "$path"
    if run "$kept" update.sh "$bin" >"$work/kept.out" 2>&1; then
        failed "update accepted $path with \"$entry\""
    fi
    grep -qF -- "$reason" "$work/kept.out" ||
        failed "update refused $path for another reason: $(cat "$work/kept.out")"
    chmod -a "$entry" "$path"
done
[[ ! -e "$kept_server/.install.lock" ]] || failed 'a refused update left its lock'
run "$kept" update.sh "$bin" >/dev/null
echo 'update: a password, folder or job others may read or change (modes or ACLs) refused; restored, it updates'

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
