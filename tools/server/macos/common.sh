# Shared by the macOS launchd scripts for OpenMaker Server (install.sh,
# update.sh, uninstall.sh). macOS ships bash 3.2 and BSD tools: no
# associative arrays, `stat -f`, no `mv -T`.
#
# With OPENMAKER_PREFIX set to a folder, the scripts install beneath it as
# the current user, standing for `/`: ownership changes, the service
# account and group, and launchd are recorded there instead of applied
# (check.sh's disposable installs). Without it they change this Mac and
# need an administrator.
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
prefix=${OPENMAKER_PREFIX:-}
server_root="$prefix/Library/OpenMaker Server"
config="$server_root/Config"
data="$server_root/Data"
logs="$server_root/Logs"
label=dev.openmaker.server
plist_path="$prefix/Library/LaunchDaemons/$label.plist"
service_user=omserver
service_group=omadmin
# The uid that owns what only the administrator may change: root's, or in a
# disposable install the current user's.
if [[ -n $prefix ]]; then
    root_uid=$(id -u)
else
    root_uid=0
fi

# Stop with `message`. The lock is released by the script's exit trap, after
# its cleanup has put things back; a cleanup that stops unlocks first.
fail() {
    printf 'OpenMaker Server: %s\n' "$*" >&2
    exit 1
}

require_macos() {
    [[ $(uname -s) == Darwin ]] || fail 'These scripts are for macOS; see tools/server for Linux.'
}

# Build as the caller, or take the binaries from a folder. Elevation starts
# only after the binaries exist.
prepare_binaries() {
    require_macos
    if (($# == 0)); then
        local repo
        repo=$(cd -- "$script_dir/../../.." && pwd)
        (
            cd -- "$repo"
            CARGO_TARGET_DIR="$repo/target" cargo build --release --locked -p om-server
        )
        binary_dir="$repo/target/release"
    else
        binary_dir=$(cd -- "$1" && pwd)
    fi
    local binary
    for binary in om-server om-server-pdf-worker; do
        [[ -f "$binary_dir/$binary" && -x "$binary_dir/$binary" ]] ||
            fail "Expected an executable $binary in $binary_dir"
    done
}

# Run the calling script again as root, unless installing beneath a prefix.
elevate() {
    if [[ -z $prefix ]] && ((EUID != 0)); then
        exec sudo -- "$@"
    fi
}

lock_path="$server_root/.install.lock"

# One script at a time: a lock folder in the server's root-owned folder
# (made first, as root's), removed when the script ends.
lock_installation() {
    umask 022
    [[ -z $prefix ]] || mkdir -p -- "$prefix/Library/LaunchDaemons"
    trusted_parents "$server_root"
    trusted_parents "$plist_path"
    root_directory "$server_root"
    mkdir -- "$lock_path" 2>/dev/null ||
        fail "Another install, update or uninstall is running (or remove $lock_path)."
    locked=true
    # Scripts that set their own exit trap unlock in it.
    trap unlock_installation EXIT
}

unlock_installation() {
    if [[ ${locked:-false} == true ]]; then
        rmdir -- "$lock_path" 2>/dev/null || true
        locked=false
    fi
}

uid_of() { stat -f %u -- "$1"; }
mode_of() { stat -f %Lp -- "$1"; }

# Give `path` to `owner:group`; recorded in a disposable install.
set_owner() {
    local owner=$1 path=$2
    if [[ -n $prefix ]]; then
        printf '%s\t%s\n' "$owner" "${path#"$prefix"}" >>"$prefix/.owners"
    else
        chown -- "$owner" "$path"
    fi
}

# Whether `path` has an access control entry that lets someone change it
# or what it holds (`ls -le` lists entries after the first line).
write_acl() {
    grep -Eq "$write_rights" <<<"$(acl_entries "$1")"
}

# `path`'s access control entries, one per line (`ls -le` lists them after
# the first line). Read whole, so that no pipe ends early under pipefail.
acl_entries() {
    local listing
    listing=$(ls -led -- "$1")
    tail -n +2 <<<"$listing"
}

write_rights=' allow .*(write|append|add_file|add_subdirectory|delete|delete_child|writeattr|writeextattr|writesecurity|chown)'


# The folders above `path` up to `/` (or the prefix): root's, not writable
# by others and with no entry granting writes, so nothing in them can be
# swapped under the scripts.
trusted_parents() {
    local directory
    directory=$(dirname -- "$1")
    while [[ $directory != / && $directory != "$prefix" && $directory != . ]]; do
        [[ ! -L "$directory" && -d "$directory" && $(uid_of "$directory") == "$root_uid" ]] ||
            fail "Expected a root-owned folder: $directory"
        (((8#$(mode_of "$directory") & 022) == 0)) ||
            fail "Folder is writable by other users: $directory"
        ! write_acl "$directory" || fail "An access control entry lets others change $directory"
        directory=$(dirname -- "$directory")
    done
}

# A folder only the administrator may change: made root-owned 0755 when
# missing, refused when it is a link, another user's, writable by others or
# open to writes by an access control entry.
root_directory() {
    local directory=$1
    [[ ! -L "$directory" ]] || fail "Refusing symlink: $directory"
    if [[ ! -e "$directory" ]]; then
        mkdir -m 0755 -- "$directory"
        set_owner root:wheel "$directory"
    fi
    [[ -d "$directory" && $(uid_of "$directory") == "$root_uid" ]] ||
        fail "Expected a root-owned folder: $directory"
    (((8#$(mode_of "$directory") & 022) == 0)) ||
        fail "Folder is writable by other users: $directory"
    ! write_acl "$directory" || fail "An access control entry lets others change $directory"
}

# A file only the administrator may change, when it exists.
check_root_file() {
    local path=$1
    [[ ! -L "$path" ]] || fail "Refusing symlink: $path"
    if [[ -e "$path" ]]; then
        [[ -f "$path" && $(uid_of "$path") == "$root_uid" ]] ||
            fail "Expected a root-owned file: $path"
        (((8#$(mode_of "$path") & 022) == 0)) || fail "File is writable by other users: $path"
        ! write_acl "$path" || fail "An access control entry lets others change $path"
    fi
}

# --- The directory service: dscl, or a record of it beneath the prefix.

ds_root="$prefix/.ds"

# `kind` (Users or Groups) `name`'s attribute `key`, empty when absent.
ds_read() {
    local kind=$1 name=$2 key=$3
    if [[ -n $prefix ]]; then
        local file="$ds_root/$kind/$name/$key"
        [[ -f "$file" ]] && cat -- "$file"
        return 0
    fi
    dscl . -read "/$kind/$name" "$key" 2>/dev/null | sed -n "s/^$key: //p" | head -n 1
}

# Whether `kind` `name` has attribute `key` at all (a value may span lines).
ds_has() {
    local kind=$1 name=$2 key=$3
    if [[ -n $prefix ]]; then
        [[ -f "$ds_root/$kind/$name/$key" ]]
    else
        # dscl answers 0 for a missing key too, with only a message on
        # stderr: the key's own line says it is there.
        local out
        out=$(dscl . -read "/$kind/$name" "$key" 2>/dev/null) || return 1
        [[ $out == "$key:"* ]]
    fi
}

ds_exists() {
    local kind=$1 name=$2
    if [[ -n $prefix ]]; then
        [[ -d "$ds_root/$kind/$name" ]]
    else
        dscl . -read "/$kind/$name" RecordName >/dev/null 2>&1
    fi
}

# Every value of `key` over `kind`'s records, one per line.
ds_values() {
    local kind=$1 key=$2
    if [[ -n $prefix ]]; then
        cat -- "$ds_root/$kind"/*/"$key" 2>/dev/null || true
        return 0
    fi
    dscl . -list "/$kind" "$key" | awk '{ print $2 }'
}

ds_create() {
    local kind=$1 name=$2 key=${3:-} value=${4:-}
    if [[ -n $prefix ]]; then
        mkdir -p -- "$ds_root/$kind/$name"
        if [[ -n $key ]]; then
            printf '%s\n' "$value" >"$ds_root/$kind/$name/$key"
        fi
        printf 'create %s %s %s %s\n' "$kind" "$name" "$key" "$value" >>"$ds_root/log"
        return 0
    fi
    if [[ -n $key ]]; then
        dscl . -create "/$kind/$name" "$key" "$value"
    else
        dscl . -create "/$kind/$name"
    fi
}

# A free id for both a user and a group, below 500 (macOS's hidden range
# for service accounts).
# Set `free` to an id below 500 (macOS's hidden range for service
# accounts) that no user and no group has, in any directory searched.
# Called in the script's own shell, so that a failed search stops it.
free_id() {
    local id taken user_holders
    taken=$( (ds_values Users UniqueID; ds_values Groups PrimaryGroupID) | sort -n | uniq)
    for ((id = 300; id < 500; id++)); do
        grep -qx -- "$id" <<<"$taken" && continue
        id_holders Users UniqueID "$id"
        user_holders=$holders
        id_holders Groups PrimaryGroupID "$id"
        if [[ -z $user_holders && -z $holders ]]; then
            free=$id
            return 0
        fi
    done
    fail 'No free id below 500 for the service account.'
}

# Set `holders` to the names of the `kind` records with `key` `id` in every
# directory this Mac searches (local and network: LDAP, Active Directory),
# one line per record, records of the same name kept apart. Called in the
# script's own shell: a failed search stops the script.
id_holders() {
    local kind=$1 key=$2 id=$3 file found
    holders=
    if [[ -n $prefix ]]; then
        for file in "$ds_root/$kind"/*/"$key"; do
            [[ -f $file ]] || continue
            if [[ $(cat -- "$file") == "$id" ]]; then
                holders+="$(basename -- "$(dirname -- "$file")")"$'\n'
            fi
        done
        holders=${holders%$'\n'}
        return 0
    fi
    found=$(dscl /Search -search "/$kind" "$key" "$id") ||
        fail "Cannot search the directory for $kind with $key $id."
    holders=$(search_headers "$key" <<<"$found") ||
        fail "Cannot read the directory's answer for $kind with $key $id."
}

# The record names in `dscl /Search -search` output on stdin for `key`: each
# match starts a line `<name><tab><tab><key> = (`, its values on the lines
# below. Match the suffix so the name can contain spaces or tabs.
# Records with the same name stay apart.
search_headers() {
    awk -v key="$1" '
        BEGIN { suffix = "\t\t" key " = (" }
        {
            start = length($0) - length(suffix) + 1
            if (start > 1 && substr($0, start) == suffix)
                print substr($0, 1, start - 1)
        }
    '
}

# Make the group and the user when they are missing, and check them when
# they exist. Existing records, and the group's members (the operators
# an administrator added), are never changed.
ensure_accounts() {
    local gid uid shell
    if ds_exists Groups "$service_group"; then
        gid=$(ds_read Groups "$service_group" PrimaryGroupID)
        [[ $gid =~ ^[0-9]+$ ]] || fail "The existing $service_group group has no group id."
    else
        free_id
        gid=$free
        ds_create Groups "$service_group"
        ds_create Groups "$service_group" PrimaryGroupID "$gid"
        ds_create Groups "$service_group" RealName 'OpenMaker Server operators'
        ds_create Groups "$service_group" Password '*'
    fi
    if ! ds_exists Users "$service_user"; then
        uid=$gid
        id_holders Users UniqueID "$uid"
        if grep -qx -- "$uid" <<<"$(ds_values Users UniqueID)" || [[ -n $holders ]]; then
            free_id
            uid=$free
        fi
        ds_create Users "$service_user"
        ds_create Users "$service_user" UniqueID "$uid"
        ds_create Users "$service_user" PrimaryGroupID "$gid"
        ds_create Users "$service_user" UserShell /usr/bin/false
        ds_create Users "$service_user" NFSHomeDirectory /var/empty
        ds_create Users "$service_user" RealName 'OpenMaker Server'
        ds_create Users "$service_user" Password '*'
        ds_create Users "$service_user" IsHidden 1
    fi
    uid=$(ds_read Users "$service_user" UniqueID)
    shell=$(ds_read Users "$service_user" UserShell)
    [[ $uid =~ ^[0-9]+$ && $uid != 0 &&
        $(ds_read Users "$service_user" PrimaryGroupID) == "$gid" &&
        ($shell == /usr/bin/false || $shell == /usr/sbin/nologin) ]] ||
        fail "The existing $service_user account must be a non-root, non-login user whose group is $service_group."
    # Its ids its own, in every directory searched: another record with
    # them would share its files.
    local users groups
    id_holders Users UniqueID "$uid"
    users=$holders
    id_holders Groups PrimaryGroupID "$gid"
    groups=$holders
    [[ $users == "$service_user" && $groups == "$service_group" ]] ||
        fail "Another user or group has $service_user's or $service_group's id."
    # No way to sign in: no password and no other authentication.
    [[ $(ds_read Users "$service_user" Password) == '*' ]] &&
        ! ds_has Users "$service_user" AuthenticationAuthority ||
        fail "The existing $service_user account has a password or another way to sign in."
    local privileged
    service_groups
    privileged=$(grep -Ex 'admin|wheel|staff|operator|_developer|_lpadmin|com\.apple\.access_(ssh|screensharing|remote_ae)' <<<"$member_of" || true)
    [[ -z $privileged ]] ||
        fail "The existing $service_user account is in privileged groups: $(tr '\n' ' ' <<<"$privileged")"
}

# Set `member_of` to the groups the service account is in, by name, one
# per line. Called in the script's own shell: a failed lookup stops it.
service_groups() {
    member_of=
    if [[ -n $prefix ]]; then
        local file
        for file in "$ds_root"/Groups/*/GroupMembership; do
            [[ -f $file ]] || continue
            if grep -qx -- "$service_user" "$file"; then
                member_of+="$(basename -- "$(dirname -- "$file")")"$'\n'
            fi
        done
        member_of=${member_of%$'\n'}
        return 0
    fi
    local names
    names=$(id -Gn "$service_user") || fail "Cannot read $service_user's groups."
    [[ -n $names ]] || fail "Cannot read $service_user's groups."
    member_of=$(tr ' ' '\n' <<<"$names")
}

# --- The service's folders.

# Databases: operators stage files (group write). Backups, logs and state:
# the service's alone, operators may read backups and logs.
service_directories() {
    local path mode
    for path in "$data/Databases" "$data/Backups" "$data/State" "$logs"; do
        [[ ! -L "$path" ]] || fail "Refusing symlink: $path"
        case $path in
        "$data/Databases") mode=0770 ;;
        "$data/State") mode=0700 ;;
        *) mode=0750 ;;
        esac
        if [[ ! -e "$path" ]]; then
            mkdir -m "$mode" -- "$path"
            set_owner "$service_user:$service_group" "$path"
        fi
        [[ -d "$path" ]] || fail "Expected a folder: $path"
        # A kept folder is used as it is, its contents untouched, but it is
        # the service's and no more open than the installer makes it.
        [[ $(uid_of "$path") == "$(service_uid)" ]] ||
            fail "$path is not $service_user's; restore its owner."
        (((8#$(mode_of "$path") & ~8#$mode & 8#777) == 0)) ||
            fail "$path is more open than $mode; restore its mode."
        local writers="user:$service_user"
        [[ $path != "$data/Databases" ]] || writers="$writers|group:$service_group"
        ! acl_grants_others "$path" "$write_rights" "$writers" ||
            fail "$path is open to other users' writes; restore its permissions."
    done
}

# Whether an access control entry on `path` grants `rights` to anyone but
# `allowed` (a `user:name|group:name` alternation).
acl_grants_others() {
    local granted
    granted=$(grep -E "$2" <<<"$(acl_entries "$1")" || true)
    [[ -n $granted ]] && grep -Evq "($3) allow" <<<"$granted"
}

# The service account's uid: in a disposable install, the current user's,
# who stands for it.
service_uid() {
    if [[ -n $prefix ]]; then
        id -u
    else
        ds_read Users "$service_user" UniqueID
    fi
}

# The Admin Console password: made once, readable by the service alone.
admin_password() {
    local path="$config/admin-password"
    [[ ! -L "$path" ]] || fail "Refusing symlink: $path"
    if [[ ! -e "$path" ]]; then
        (
            umask 077
            openssl rand -hex 24 >"$path"
        )
        chmod 0400 "$path"
        set_owner "$service_user:$service_group" "$path"
    fi
    [[ -f "$path" && -s "$path" ]] || fail "Expected the Admin Console password in $path."
    [[ $(uid_of "$path") == "$(service_uid)" ]] &&
        (((8#$(mode_of "$path") & 077) == 0)) &&
        ! acl_grants_others "$path" ' allow ' "user:$service_user" ||
        fail "$path is open to other users or not $service_user's; make it 0400 and the service's."
}

# --- Settings and the launchd job.

# Read server.env's settings (KEY=value lines; # comments).
read_settings() {
    local line key value
    OPENMAKER_NAME=openmaker
    OPENMAKER_BIND=127.0.0.1:5003
    OPENMAKER_ADMIN_BIND=127.0.0.1:16000
    OPENMAKER_ADMIN_USER=admin
    while IFS= read -r line || [[ -n $line ]]; do
        [[ -z $line || $line == \#* ]] && continue
        key=${line%%=*}
        value=${line#*=}
        case $key in
        OPENMAKER_NAME | OPENMAKER_BIND | OPENMAKER_ADMIN_BIND | OPENMAKER_ADMIN_USER)
            [[ -n $value ]] || fail "server.env: $key is empty."
            printf -v "$key" '%s' "$value"
            ;;
        *) fail "server.env: unknown setting $key." ;;
        esac
    done <"$config/server.env"
}

# Write the launchd job for release folder `release` to `out`: the server
# as the service account and group, its settings from server.env.
write_plist() {
    local release=$1 out=$2 arg
    read_settings
    rm -f -- "$out"
    plutil -create xml1 "$out"
    plutil -insert Label -string "$label" "$out"
    plutil -insert ProgramArguments -array "$out"
    for arg in "$server_root/$release/om-server" \
        --name "$OPENMAKER_NAME" --bind "$OPENMAKER_BIND" \
        --admin-bind "$OPENMAKER_ADMIN_BIND" --admin-user "$OPENMAKER_ADMIN_USER" \
        --admin-password-file "$config/admin-password" \
        --state-dir "$data/State" \
        --database-folder "$data/Databases" \
        --backup-folder "$data/Backups" \
        --log-folder "$logs" --list-all; do
        plutil -insert ProgramArguments -string "$arg" -append "$out"
    done
    plutil -insert UserName -string "$service_user" "$out"
    plutil -insert GroupName -string "$service_group" "$out"
    plutil -insert InitGroups -bool true "$out"
    # 027: what the server makes is not other users'.
    plutil -insert Umask -integer 23 "$out"
    plutil -insert WorkingDirectory -string "$data" "$out"
    plutil -insert RunAtLoad -bool true "$out"
    plutil -insert KeepAlive -dictionary "$out"
    plutil -insert KeepAlive.SuccessfulExit -bool false "$out"
    plutil -insert ThrottleInterval -integer 10 "$out"
    plutil -insert StandardOutPath -string "$logs/launchd.log" "$out"
    plutil -insert StandardErrorPath -string "$logs/launchd.log" "$out"
    plutil -lint -s "$out" || fail "The launchd job written is not valid: $out"
    chmod 0644 "$out"
}

# Put the job `staged` (a file beside the job's path) in place: root's
# before it takes the job's name.
install_plist() {
    set_owner root:wheel "$1" || return 1
    mv -f -- "$1" "$plist_path"
}

launchd() {
    if [[ -n $prefix ]]; then
        printf 'launchctl %s\n' "$*" >>"$prefix/.launchctl"
        return 0
    fi
    launchctl "$@"
}

job_loaded() {
    if [[ -n $prefix ]]; then
        [[ -f "$prefix/.loaded" ]]
    else
        launchctl print "system/$label" >/dev/null 2>&1
    fi
}

start_job() {
    launchd bootstrap system "$plist_path" || return 1
    [[ -z $prefix ]] || : >"$prefix/.loaded"
}

stop_job() {
    if job_loaded; then
        launchd bootout "system/$label" || return 1
        [[ -z $prefix ]] || rm -f -- "$prefix/.loaded"
    fi
    return 0
}

# Wait until the job's process listens on its two ports (clients' and the
# Admin Console's).
wait_listening() {
    [[ -z $prefix ]] || return 0
    local attempt pid listeners
    for ((attempt = 0; attempt < 60; attempt++)); do
        pid=$(launchctl print "system/$label" 2>/dev/null | awk '/^\tpid = / { print $3; exit }')
        if [[ $pid =~ ^[1-9][0-9]*$ ]]; then
            listeners=$(lsof -nP -a -p "$pid" -iTCP -sTCP:LISTEN 2>/dev/null | tail -n +2 | wc -l)
            ((listeners >= 2)) && return 0
        fi
        sleep 1
    done
    return 1
}

# The log is the service account's: the scripts, as root, do not read it.
startup_error() {
    fail "The server did not start both listeners. See $logs/launchd.log and Event.log."
}

# A release folder of the server's binaries, root-owned.
stage_release() {
    local directory
    directory=$(mktemp -d -- "$server_root/releases/release.XXXXXXXX")
    chmod 0755 "$directory"
    staged_release="releases/${directory##*/}"
    install -m 0755 -- "$binary_dir/om-server" "$binary_dir/om-server-pdf-worker" "$directory"
    set_owner root:wheel "$directory"
    local help
    # --help answers 0 or 64 (EX_USAGE), as the build has it. Run as the
    # service account, never as root.
    if [[ -n $prefix ]]; then
        help=$("$directory/om-server" --help 2>&1) || true
    else
        help=$(sudo -u "$service_user" -- "$directory/om-server" --help 2>&1) || true
    fi
    [[ $help == *--database-folder* && $help == *--admin-password-file* ]] ||
        fail 'Use an om-server build with database folders and the Admin Console.'
}

remove_release() {
    local directory="$server_root/$1"
    rm -f -- "$directory/om-server" "$directory/om-server-pdf-worker"
    rmdir -- "$directory"
}

# The release the installed job runs, from its program path.
installed_release() {
    local program release
    [[ -f "$plist_path" && ! -L "$plist_path" && $(uid_of "$plist_path") == "$root_uid" ]] ||
        fail 'Expected a job installed by these scripts.'
    (((8#$(mode_of "$plist_path") & 022) == 0)) && ! write_acl "$plist_path" ||
        fail "The installed job may be changed by other users: $plist_path"
    program=$(plutil -extract ProgramArguments.0 raw -o - "$plist_path") ||
        fail 'The installed job names no program.'
    release=${program#"$server_root/"}
    release=${release%/om-server}
    [[ $release =~ ^releases/release\.[[:alnum:]]{8}$ && -d "$server_root/$release" &&
        ! -L "$server_root/$release" ]] ||
        fail 'The installed job runs a program outside the release folders.'
    echo "$release"
}
