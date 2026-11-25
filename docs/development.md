# Development guide

Building, running, checking and debugging OpenMaker. Rules for what to
change and how to submit it are in [AGENTS.md](../AGENTS.md); the design is
in [architecture.md](architecture.md).

## Prerequisites

- **The latest stable Rust** through rustup (`rustup update stable`).
  `rust-toolchain.toml` selects the stable channel with `rustfmt` and
  `clippy`; an older stable refuses to build the workspace (see
  [Toolchain and dependencies](#toolchain-and-dependencies)). In a
  non-login shell `cargo` may not be on `PATH`:
  `export PATH="$HOME/.cargo/bin:$PATH"`.
- **Linux desktop** (Ubuntu is the first platform). The GUI needs an X11 or
  Wayland session; its tests run headless under `xvfb-run` (Debian/Ubuntu
  package `xvfb`).
- **Optional:**
  - Web viewers (`--features web-viewer`): WebKitGTK 4.1 and GTK 3
    development files and `pkg-config` (Debian/Ubuntu:
    `libwebkit2gtk-4.1-dev`). On a Wayland session they also need
    XWayland and the X11 libraries winit opens (Debian/Ubuntu:
    `libxkbcommon-x11-0`, `libx11-xcb1`, `libxcursor1`, `libxi6`).
    Without them the windows stay on Wayland and web viewers show a note.
    On macOS they use the system's WKWebView, with nothing to install.
  - Saving a FileMaker client license: an unlocked desktop Secret Service
    on Linux (Keychain on macOS, Credential Manager on Windows).
  - Print: CUPS (`lp`).
  - OpenMaker Go: Xcode and an iOS SDK on an Apple silicon Mac for iPhone
    and iPad; JDK 21 and the Android SDK and NDK for Android. Building
    and trying the apps is in [mobile/README.md](../mobile/README.md).

## Build and run

```sh
cargo run -p om-gui                                  # desktop client
OPENMAKER_PROVIDER=mock cargo run -p om-gui          # labeled sample data, no server
cargo run -p om-gui --features web-viewer            # with embedded web viewers (Linux: X11, or XWayland on Wayland; macOS)
cargo run -p om-server -- --list-all <file.fmp12>... # OpenMaker Server (see Hosting a file below)
OM_ADMIN_PASSWORD=<password> cargo run -p om-admin --example mock_console  # Admin Console on sample data, https://localhost:16000
cargo run -p om-client                               # protocol probe: prints isa/connect/open/login/files usage
cargo run -p om-author -- help                       # agent authoring CLI and MCP server
```

PDF data in container fields (`GetThumbnail`, `GetTextFromPDF`, a PDF's
size, Browse's first page) is read by a worker executable beside the
program, never in it: `om-gui-pdf-worker` and `om-server-pdf-worker`
(`om_calc::functions::pdf_worker`, #566). `om-gui-pdf-worker --svg` also
draws SVG pictures and icons, in reports and in Browse
(`om_gui::report::svg_worker`, `om_gui::svg_loader`); without it report SVG
pictures refuse with error 3 and Browse shows its error mark for them. `cargo build -p om-gui` and
`cargo build -p om-server` build the worker with the program, and the
packages install it; `cargo run` builds only the program it runs, so build
the package once first. A missing worker is reported at startup, and
reading PDF data then fails with error 3. `OPENMAKER_PDF_WORKER` names
another worker executable (om-calc's `om-pdf-worker`, for example, for a
host without its own).

**Desktop client.** **File > Open...** opens a `.fmp12` file on this
computer through a private embedded host (no server or license needed);
Home's Hosts and File > Hosts > Open Network File... connect to FileMaker
Server or OpenMaker Server. Useful
environment variables:

| Variable | Effect |
|---|---|
| `OPENMAKER_HOST`, `OPENMAKER_PORT`, `OPENMAKER_FILE`, `OPENMAKER_ACCOUNT` | Prefill the connection form (the password is always typed) |
| `OPENMAKER_PROVIDER=mock` | Use the mock provider |
| `OPENMAKER_CLIENT_CERT`, `OPENMAKER_CLIENT_KEY` | PEM client identity (default: an in-memory one is generated) |
| `OPENMAKER_CA_CERT` | Extra trusted certificate: a private CA's, or a server's self-signed one |
| `WEBKIT_DISABLE_DMABUF_RENDERER` | Overrides OpenMaker's choice for web viewers on X servers without DRI3 (Xvfb, VNC) |
| `WINIT_X11_SCALE_FACTOR` | The windows' scale on X11 (winit's), also for a web-viewer build on a Wayland session, which runs through XWayland |
| `OPENMAKER_PDF_WORKER` | The PDF worker executable to use instead of the one beside the program (above) |
| `OPENMAKER_USER_NAME` | `Get ( UserName )` and the session name, whatever Settings' User Name says |
| `OPENMAKER_SETTINGS` | The settings file to read and write instead of the OS account's ([desktop-client](desktop-client.md#settings)) |

**OpenMaker Server.** `om-server --help` lists every flag. Without
`--cert`/`--key` it uses the operating system's self-signed certificate or
one it makes once and keeps ([The server's
certificate](#the-servers-certificate)). Clients log in with each hosted
file's own accounts and passwords, as with FileMaker Server ([Accounts and
insecure files](#accounts-and-insecure-files)). `--record <dir>` saves every connection's
GIOP messages for later decoding (use a new directory per run). It serves
at most 4096 connections at once, 1024 from one address, and closes a
silent one after an hour, or 145 s without a session (`--connections`,
`--connections-per-address`, `--idle-seconds`); it raises its open-file
limit to match. Logging follows `RUST_LOG` (default `info`). Beside it,
the server writes an event log in FileMaker Server's format, `Event.log`
and `Access.log` in `--log-folder` (default `Logs` in its state folder,
[The server's certificate](#the-servers-certificate)); on Unix, SIGTERM or
SIGINT stops it as an administrator's stop does, and a second ends it at
once ([admin-console.md](admin-console.md#the-event-log-and-alerts)). The
server rewrites hosted files on every change, so host copies, not
originals.
Backup schedules (`type = backup` in the `--schedules` file) write
FileMaker Server's backup folders to `--backup-folder`, by default
`Backups` in the state folder ([Backups](admin-console.md#backups)).

**Admin Console and Admin API.** The `mock_console` example serves both on
sample data, as user `admin` with the password in `OM_ADMIN_PASSWORD`
([admin-console](admin-console.md)). Its certificate is a new self-signed
one, hence `-k`:

```sh
API=https://localhost:16000/fmi/admin/api/v2
TOKEN=$(curl -sk -X POST -u "admin:$OM_ADMIN_PASSWORD" $API/user/auth | jq -r .response.token)
curl -sk -H "Authorization: Bearer $TOKEN" $API/clients
curl -sk -X DELETE "$API/clients/12?messageText=Closing%20soon&graceTime=30" \
  -H "Authorization: Bearer $TOKEN"
curl -sk -X DELETE $API/user/auth/$TOKEN
```

### Hosting a file and opening it

The client verifies the server's certificate, so tell it to trust the one
the server logs at startup (`TLS certificate: ... kept as <path>`; on most
machines `~/.local/state/openmaker-server/tls/cert.pem`):

```sh
cp <file.fmp12> /tmp/hosted.fmp12
cargo run -p om-server -- --bind 127.0.0.1:5003 --list-all /tmp/hosted.fmp12
OPENMAKER_CA_CERT=~/.local/state/openmaker-server/tls/cert.pem OPENMAKER_HOST=localhost \
  OPENMAKER_FILE=hosted.fmp12 OPENMAKER_ACCOUNT=Admin cargo run -p om-gui
```

Open Network File opens over Home, filled in: type the account's password
and choose Open. Saved under Home's Hosts (**+**, `localhost`), the host
lists the hosted file.

Files can also be put in a database folder instead of named on the
command line. At startup the server hosts the `.fmp12` files at the
folder's root and in its subfolders, one level deep, as FileMaker Server
opens its database folders' (Claris: "Database folders"). The default
folder is `Databases` in the state folder
(`~/.local/state/openmaker-server/Databases`, or
`$STATE_DIRECTORY/Databases` under systemd). The server makes it for its
own user only, and does not use it when it is a link, belongs to another
user, or others may write in it. It is searched also when files are named
on the command line. `--database-folder <dir>`, given once or more,
replaces it; a folder named this way is resolved once, links and all,
since the administrator chose it. Inside a database folder only regular
files and folders are used, never a link, so a database elsewhere is
hosted by naming its folder or the file itself. On Unix, a file named on
the command line may be a link to a regular file. On Windows, the server
refuses file symlinks at open, before recovery or saves. Name the regular
database file directly. Other non-regular files (such as FIFOs and devices)
are refused without waiting.

Files named on the command line come first. A found file with the name of
one already hosted is left out and logged. The backup and log folders in
use (`--backup-folder` and `--log-folder`, or their defaults) and
everything inside them are not searched; a schedule's own backup target is
not excluded. At most 200,000 entries are read across all the folders and
10,000 files found. The entry limit applies as entries are read, before
they are sorted, so past it which files are found depends on the order the
system lists them. A file copied in after startup is not hosted until the
server starts again. With no files at all the server still starts, with an
empty file list; when files are found or named and none of them opens, it
does not start.

Discovery checks each entry when it reads the folder. Afterwards the
server finds a hosted file by its path each time it reads, saves, backs
up or reopens it, and works through the folder handle it opened then, by
the file's name, never following a link at the name
([#468](https://github.com/OpenMaker-Dev/openmaker/issues/468)):
- **A save or a backup copy** goes ahead only while the name is a regular
  file and the file the server read or last saved (same device and
  inode). It takes the file's permissions from that same look.
- **A reopen**, which may find a file the administrator replaced, goes
  ahead only while the folder is the one the file was first hosted from.

So a folder of the path swapped for a link to other content, to an empty
folder, or to a folder whose entry links back to the file, makes those
fail instead of writing or reading there, and a save's changes wait in
memory. Backup deletion protects a slot's last saved file even while the
slot is closed or opening. If the saved location cannot be verified,
retention and manual deletion stop. A schedule can still back up other
files if the target passes the known container-folder checks. If an
administrator moves a hosted file's folder and makes its path lead to
the same file, saves and backups can continue. A reopen then refuses
until the server restarts.

On Linux and macOS a path whose last part is a link (allowed on the
command line) is resolved once, when the file is hosted: the file is read
and saved where the link led, and backup deletion checks both the
configured and saved locations. A file already hosted under another
name, through a link or a second hard link, is refused. The desktop
keeps the path it opened.
Outside Linux, a folder the server may search but not read cannot hold a
hosted file. Some things still go by the path alone:
- the time between discovery and the file's load;
- server scripts' external container reads and Documents folder
  ([#594](https://github.com/OpenMaker-Dev/openmaker/issues/594));
- on Windows, which has no inode identities here, everything;
- the moment between retention's check and its removal of an old backup
  ([#602](https://github.com/OpenMaker-Dev/openmaker/issues/602)).

Keep database folders, their subfolders and the folders above them
writable by the server's user only all the same. Once the entry limit is reached, the
log says how many folders were left unread and names the first.

Failed logins are limited. After 5 failures an address (an IPv6 address by
its /64) may try once a minute; after 30 failures of an account of a file
from all addresses, it may be tried once every 30 s, except from addresses
it logged in from in the last 30 days. A login past a limit gets error 214
(Too many login attempts) without its password being checked, and the
server logs a warning naming the peer and the account. Logins with an
empty password are counted apart, by address only, at the same rate, and
a login accepted on a connection takes back that connection's. FileMaker
Pro's first Login on each file it opens is the file's "Log in using"
account with an empty password, refused before the user's own, and would
otherwise lock out a user opening several files (#440). The limits are
OpenMaker's choice (FileMaker Server's are unmeasured), live in memory
only, and do not apply to files opened from this computer; the reasoning
is in `crates/om-server/src/login_limits.rs`.

### Accounts and insecure files

The server has no accounts of its own. A login is checked against the
hosted file's accounts, as FileMaker Server checks it: the password against
the hash the file stores, then the account's state, and it must be active,
a FileMaker-file account (not [Guest] or an external one), with no password
change due, in [Full Access], [Data Entry Only] or [Read-Only Access] as
FileMaker stores them. An account in a custom privilege set is refused
with error 9: the server does not yet hold a client that sends its own
requests to every member of one ([#227](https://github.com/OpenMaker-Dev/openmaker/issues/227)). An account in [Full Access] may
run any Perform Script on Server script and make `om-author` edits. One in
[Data Entry Only] or [Read-Only Access] runs any script under its set: it
reads every record, and under [Data Entry Only] creates, edits and deletes
them, while [Read-Only Access]'s writes are refused with 200. In a server
script session (Perform Script on Server or a schedule), an account in a
custom set (admitted by a local file's private host) runs the scripts its
set makes executable. One its set gives no access runs only when a script
running with full access performs it, and then with the account's own
privileges. Its steps read and write no record or field unless the script
is set to run with full access privileges, since the set's data limits
are not applied to server scripts yet. Desktop scripts apply the set's
data limits themselves. Only [Full Access] authors. The rules and their error codes are in
[docs/re/local-files.md](re/local-files.md) 2. A desktop's private host
for a local file applies the same ones, but admits the custom sets
OpenMaker enforces, since its one client is the desktop itself. A password,
account or privilege set changed over the wire counts from each client's
next login: a signed-in session keeps the account and privileges it logged
in with, deleted or not, as FileMaker keeps a client's until it opens the
file again ([docs/re/session-identity.md](re/session-identity.md) 1).
Perform Script on Server, a schedule's run and `om-author` edits act with
the same privileges as the session or the run's login.

After Login, FileMaker Pro requests the signed-in account's whole member.
The server lets a restricted account download that member with known
metadata, omitting the stored password verifier, its derived checksum
and unknown children. Other whole account members and their parent
subtrees remain restricted. As before, a direct read of any account
member's child is allowed except for `04` and `06`.
In the native #735 check, Contact Details drew for [Data Entry Only] and
[Read-Only Access]. FileMaker Pro allowed record creation and deletion
for the first, disabled both for the second, and disabled Manage Database
for both. FileMaker Server's exact account-member reply remains
unmeasured.

Like FileMaker Server, the server does not host a file anyone could open
with full access: an active account in [Full Access] (or in a set that
manages accounts) whose password is empty, whose password the file
stores for File Options' "Log in using", or that is [Guest]. Such a file
is left closed with an alert and FileMaker Server's event 1017, and the
log names the account. With no other file to host the server does not
start, and writes no event log (as for a file that cannot be opened at
all). New files and the starter solutions are like this
(Admin, no password): give the account a password in FileMaker Pro, or
start the server with `--allow-insecure-files` (FileMaker Server's
`securefilesonly=false`), which lets anyone who reaches the port open
such a file with full access.

A script schedule names one of the file's accounts that a client could log
in as (a stored password included), and runs as it without the password:
the schedules file comes from the server's administrator, who holds the
files anyway (FileMaker Server keeps a password with each schedule, [#226](https://github.com/OpenMaker-Dev/openmaker/issues/226)).

### The Admin Console

`--admin-user <name>` serves the Admin Console, a web page for
administering the running server, at `https://127.0.0.1:16000/admin-console`,
and the Admin API v2 at `/fmi/admin/api/v2` on the same address
(`--admin-bind <addr:port>` to change; `0.0.0.0:16000` to administer from
other machines). Its password may not be empty and comes one of two ways;
once read, the server keeps only its SHA-256 digest, and no password
appears in its log or error messages.

- **A file, `--admin-password-file <file>`.** This is the recommended
  way. The password is the file's one line, without its line ending. Other
  users may not read the file: mode 0600, or 0640 for a group the
  server's user is in. A looser mode, a second line or text that is not
  UTF-8 stops the server. Under systemd, give the file as a credential:
  systemd hands it only to the service, as mode 0400 in a private folder.

  ```ini
  [Service]
  LoadCredential=om-admin-password:/etc/openmaker/admin-password
  ExecStart=/usr/local/bin/om-server --admin-user admin --admin-password-file %d/om-admin-password ...
  ```

- **The `OM_ADMIN_PASSWORD` environment variable.** The server removes the
  variable from its environment once read, so nothing it starts inherits
  it. But the environment it started with stays readable in
  `/proc/<pid>/environ` to its user and to root, and the server logs a
  warning saying so.

Giving both is refused. The console presents the server's own
certificate. What it shows and its limits:
[admin-console.md](admin-console.md).

```sh
(umask 077 && read -rsp 'Console password: ' pw && printf '%s\n' "$pw" > admin-password)
cargo run -p om-server -- --admin-user admin --admin-password-file admin-password \
  /tmp/hosted.fmp12
```

The desktop's private hosts for local files are built without the console
(`om-server`'s `admin-console` feature, on by default).

### The server's certificate

The server's certificate is `--cert` with its key `--key`, or else one that
stays the same across restarts, so clients can be told to trust it:

1. The operating system's own self-signed certificate, when there is one
   and the server's user may read its key. Only Debian and Ubuntu have
   one: the `ssl-cert` package's `/etc/ssl/certs/ssl-cert-snakeoil.pem`,
   whose key `/etc/ssl/private/ssl-cert-snakeoil.key` root and the
   `ssl-cert` group may read (`adduser <server user> ssl-cert`). It names
   the computer's full host name when the package was installed. Other
   Linux distributions, macOS and Windows have none the server can use.
   One that clients would refuse (expired, marked as a CA) is passed over
   with a warning.
2. Otherwise one the server makes at its first start and keeps as
   `tls/cert.pem`, its key `tls/key.pem` readable by the server's user
   only, in its state folder: `--state-dir`; else `$STATE_DIRECTORY` (a
   systemd unit's `StateDirectory=openmaker-server` gives
   `/var/lib/openmaker-server`); else `$XDG_STATE_HOME/openmaker-server`
   or `~/.local/state/openmaker-server` on Linux, `~/Library/Application
   Support/OpenMaker Server` on macOS, `%LOCALAPPDATA%\OpenMaker Server`
   on Windows. It is valid for ten years and names `localhost`,
   `127.0.0.1`, `::1`, the `--bind` address (unless it is every address),
   the `--name` when that is a host name, and each `--cert-name`.

At startup the server logs which certificate it uses and its SHA-256
fingerprint, in the form `openssl x509 -noout -fingerprint -sha256 -in
<cert>` prints, to compare with what a client or tool shows.

- **Trusting it.** Copy the certificate file, never the key, to each
  client. OpenMaker clients trust it through `OPENMAKER_CA_CERT` and must
  connect by a name it holds (`openssl x509 -noout -ext subjectAltName -in
  <cert>` lists them). FileMaker Pro warns that it cannot verify a
  self-signed certificate.
- **New names.** A kept certificate keeps the names it was made with. The
  server warns about each `--cert-name` its certificate does not hold. When
  the host name changes, remove the `tls` folder and start the server with
  the new `--cert-name`: it makes a new certificate, which clients refuse
  until they are given it.
- **Replacing it.** Give `--cert`/`--key` (a certificate from a CA the
  clients trust needs no copying), or remove the `tls` folder to have a new
  self-signed one made.
- **When it cannot be used.** If a kept file cannot be read or is missing,
  the key is not the certificate's, or the certificate has expired, the
  server says why and does not start. It never replaces the certificate on
  its own, since clients trust it. The `tls` folder must be a folder (not a
  link) of the server's user or root that no one else may write.

A certificate of your own for `--cert` must not be marked as a CA (rustls
refuses a CA certificate as a server's own), for example:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 30 \
  -subj /CN=localhost -addext subjectAltName=DNS:localhost \
  -addext basicConstraints=critical,CA:FALSE -keyout server.key -out server.pem
```

### OpenMaker Server as a Linux service

On Linux with systemd 247 or newer, run these scripts from a source checkout.
They build the server and its PDF worker with Cargo, then use `sudo` for
installation. Linux needs Bash, coreutils, util-linux, account management
commands (`useradd`, `groupadd`), OpenSSL and `ss` from iproute2. The
source build also needs stable Rust and a C linker.

```sh
tools/server/install.sh
tools/server/update.sh      # after checking out the version to install
tools/server/uninstall.sh
```

Install and update also accept a directory containing the matching Linux
`om-server` and `om-server-pdf-worker` executables. For example,
`tools/server/install.sh /path/to/openmaker/bin` skips the source build.
The binaries need database folder discovery and the default Admin Console
feature enabled.

The installer starts and enables `openmaker-server.service` as the
non-login user `omserver` and group `omadmin`. The service can start with
an empty database directory.

| Path | Contents |
| --- | --- |
| `/opt/OpenMaker/OpenMaker Server/bin/` | Link to the installed server and PDF worker |
| `/opt/OpenMaker/OpenMaker Server/Data/Databases/` | Hosted databases |
| `/opt/OpenMaker/OpenMaker Server/Data/Backups/` | Backups |
| `/opt/OpenMaker/OpenMaker Server/Logs/` | FileMaker-format logs |
| `/etc/openmaker/server.env` | Server name, database bind address, console bind address and console user |
| `/etc/openmaker/admin-password` | Generated password for the Admin Console |
| `/etc/openmaker/tls/` | TLS certificate and private key |
| `/var/lib/openmaker-server/` | Server state directory managed by systemd |

The default listeners are `127.0.0.1:5003` and `127.0.0.1:16000`. Sign in
as `admin` at `https://localhost:16000/admin-console`. Read the password
with `sudo cat /etc/openmaker/admin-password`. For remote access, edit the
bind addresses in `server.env` and supply a certificate covering the
address clients use. The generated certificate is self-signed and covers
localhost. The service reads the private key and password through systemd
credentials. Run `sudo systemctl restart openmaker-server.service` after
changing the settings or replacing credentials.

Stage working copies in the database folder. The server discovers eligible
`.fmp12` files at startup, including one level of subdirectories.
FileMaker Server's `Data/Secure/` and `Data/Scripts/` have no equivalent
hosting or system-script features in this layout.

```sh
sudo install -o omserver -g omadmin -m 0660 Tasks.fmp12 \
  '/opt/OpenMaker/OpenMaker Server/Data/Databases/Tasks.fmp12'
sudo systemctl restart openmaker-server.service
```

Use `sudo systemctl edit openmaker-server.service` for unit overrides.
When replacing a manual installation, stop its service first. The
installer expects root-owned parent directories and manages only the
unit and binaries it installs.

Before updating, take a current backup of the hosted databases and keep it
separate from the live database folder. Update keeps the settings and the
service's active or stopped state. If startup fails, the script attempts
to restore the previous binaries and unit. The server can save deferred
recovery to hosted files before both listeners are ready. Those writes
persist through binary and unit rollback. Use the pre-update backup if you
need to restore the previous database contents.

Uninstall stops and disables the service and removes its unit and
binaries. It keeps databases, backups, logs, settings, credentials, and
the service account for a later install. If uninstall is interrupted,
correct the reported error and rerun `tools/server/uninstall.sh` before
reinstalling or updating.

### Packages for Ubuntu 24.04

`tools/package-ubuntu.sh` builds release `om-gui` (with web viewers) and
`om-server` in an `ubuntu:24.04` container (docker or podman; the host
needs neither Ubuntu 24.04 nor WebKitGTK's development files) and writes a
`.deb` and a tarball for amd64 to `target/package/`:

```sh
tools/package-ubuntu.sh              # or: tools/package-ubuntu.sh <output folder>
sudo apt install ./target/package/openmaker_<version>_amd64.deb
```

The container keeps its toolchain and build in `target/ubuntu-24.04/`. The
version is the workspace's plus the commit (`0.0.0+git<hash>`). Both
packages hold the two binaries, a desktop entry (OpenMaker, running
`om-gui`), the icon, and in `share/doc/openmaker/` the licenses of what the
binaries embed (OpenMaker's, the Inter, Liberation Sans and egui fonts',
and the Unicode collation data's). They contain nothing of Claris's: open
starters and other files from outside.

The `.deb` declares what it needs: `Depends` lists the libraries the
binaries link (`dpkg-shlibdeps`) and the ones `om-gui` opens itself at run
time, which `dpkg-shlibdeps` cannot see: `libxkbcommon-x11-0` (without it
`om-gui` stops at startup), Vulkan and EGL. The script records those by
starting `om-gui` under Xvfb with `LD_DEBUG`, and stops if it looks for one
the build image lacks. `Recommends` lists what those libraries open in turn
(Mesa's drivers), which a desktop with other drivers does not need. The
run-time libraries are measured under X11 only. The tarball installs for
one user, with the same packages listed in
`share/doc/openmaker/apt-packages`:

```sh
tar -xzf openmaker-<version>-ubuntu24.04-amd64.tar.gz -C ~/.local --strip-components=1
xargs -a ~/.local/share/doc/openmaker/apt-packages sudo apt install
```

### OpenMaker Server as a macOS service

On macOS, run these scripts from a source checkout. They build the server
and its PDF worker with Cargo, then use `sudo`. They need only what macOS
ships (bash 3.2, `dscl`, `launchctl`, `plutil`, `openssl`, `lsof`); the
source build also needs stable Rust and the Xcode command line tools.

```sh
tools/server/macos/install.sh
tools/server/macos/update.sh      # after checking out the version to install,
                                  # or after changing server.env
tools/server/macos/uninstall.sh
```

Install and update also take a folder holding matching macOS `om-server` and
`om-server-pdf-worker` executables, to skip the build. The binaries need
database folders and the Admin Console (the default features).

The installer makes the service account `omserver` and the operator group
`omadmin`, unless they exist. The account is a hidden, non-login local user
(shell `/usr/bin/false`, no password, home `/var/empty`) whose group is
`omadmin`, with a free id below 500. FileMaker Server uses `fmserver` and
`fmsadmin` the same way. Existing records are used as they are, after a
check of what the server would run with. The install stops, with nothing
installed, when the account:

- is root, can log in or has another group than `omadmin`;
- has a password or another way to sign in (`AuthenticationAuthority`);
- shares its id, or the group's, with another record in any directory this
  Mac searches (local or network);
- is in a privileged group (`admin`, `wheel`, `staff`, `operator`,
  `_developer`, `_lpadmin`, or the remote-access groups).

The group's members are never changed. The installer then installs
and starts the launchd daemon `dev.openmaker.server`, which runs the server
as `omserver:omadmin` with umask 027 and restarts it if it stops on an
error.

| Path | Owner, mode | Contents |
| --- | --- | --- |
| `/Library/OpenMaker Server/releases/release.*/` | root, 0755 | The server and PDF worker the job runs |
| `/Library/OpenMaker Server/Config/server.env` | root, 0644 | Server name, bind addresses, console user |
| `/Library/OpenMaker Server/Config/admin-password` | `omserver`, 0400 | Generated Admin Console password |
| `/Library/OpenMaker Server/Data/Databases/` | `omserver:omadmin`, 0770 | Hosted databases, staged by operators |
| `/Library/OpenMaker Server/Data/Backups/` | `omserver:omadmin`, 0750 | Backups |
| `/Library/OpenMaker Server/Data/State/` | `omserver`, 0700 | The server's certificate (`tls/`) |
| `/Library/OpenMaker Server/Logs/` | `omserver:omadmin`, 0750 | FileMaker-format logs and `launchd.log` |
| `/Library/LaunchDaemons/dev.openmaker.server.plist` | root, 0644 | The launchd job |

The folders above the service's own are root's, 0755. The scripts run as
root, so they refuse to work in a folder another user could change:
`/Library`, `/Library/LaunchDaemons`, the server's root folders and the
installed job must be root's, not writable by others, and without an
access control entry granting writes. Kept service folders and the
password file must be the service account's and no more open than the
installer makes them, through modes or access control entries (only
`Databases` lets `omadmin` write); their contents are never changed. The scripts lock against each other with
`/Library/OpenMaker Server/.install.lock`. They never read the service's
logs or run its binaries as root: the release's `--help` check runs as
`omserver`. launchd has no
credentials store, so the password file belongs to the service account,
which alone may read it, in a folder only root may change. The default
listeners are `127.0.0.1:5003` and `127.0.0.1:16000`. Sign in as `admin` at
`https://localhost:16000/admin-console`, with the password from
`sudo cat '/Library/OpenMaker Server/Config/admin-password'`. The server
makes its own certificate in `Data/State/tls/`, as [above](#the-servers-certificate).
Give clients `Data/State/tls/cert.pem` in `OPENMAKER_CA_CERT`. For remote
access, change the bind addresses in `server.env` and run `update.sh`,
which writes the job again from it.

Operators in `omadmin` stage databases, with group read and write so the
server can save them. The server's own saves leave a file `0640`, which
operators may still read. To make someone an operator, or to stop:

```sh
install -g omadmin -m 0660 Tasks.fmp12 '/Library/OpenMaker Server/Data/Databases/'
sudo launchctl kickstart -k system/dev.openmaker.server   # host it now
sudo dseditgroup -o edit -a <user> -t user omadmin        # add an operator
sudo dseditgroup -o edit -d <user> -t user omadmin        # remove one
```

Update keeps the data, settings, password, account and the group's members.
It stops the job, puts the new release and job in place, loads the job
again if it was loaded (launchd then starts the server, as at boot), and
restores the previous release and job if the server does not come back with
both listeners. Uninstall removes the job
and the binaries and keeps everything else for a later install. The
account and group are left for an administrator to remove
(`sudo dscl . -delete /Users/omserver`, then `/Groups/omadmin`).

`tools/server/macos/check.sh` checks the scripts on disposable installs as
the current user, with no administrator and no change to the Mac. Each
scenario installs beneath a temporary folder standing for `/`
(`OPENMAKER_PREFIX`), where ownership, the account and group, and launchd
are recorded instead of applied. The scenarios:

- a fresh install;
- an update that must keep a staged database, the password, the account
  and an operator's membership;
- an uninstall;
- existing accounts with members;
- accounts that are refused: one that can log in, one with a password,
  one whose id another record shares, and one in `admin`;
- a parent folder whose access control entry grants writes, which is
  refused;
- an update refusing a password file others may read and a backup folder
  others may write, by mode and by access control entry, and a job others
  may change, then going ahead once they are restored.

It then runs the job's own command line, as the current user, on a staged
blank database: the server must open it for writing and write its state
and logs.

What `check.sh` cannot show needs a real install by an administrator:

- that launchd starts the server as `omserver:omadmin`
  (`ps -o user=,group= -p <pid>`, the pid from `sudo launchctl print
  system/dev.openmaker.server`);
- that a database an operator stages opens (`Opened database` in
  `Logs/Event.log`);
- that the group's members may read the logs and backups and stage files.

## Checks

The gate before a pull request (what merged PRs report):

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
xvfb-run -a cargo test --locked --workspace --no-fail-fast
```

While iterating, check only what you touched: `cargo test -p <crate>`,
`cargo test -p <crate> --test <file>` for one integration-test file, or
`cargo test -p <crate> --lib` to skip examples and integration tests. The
GUI's integration tests need a display, so run `om-gui` tests under
`xvfb-run -a` (or in a desktop session).

### Toolchain and dependencies

OpenMaker builds with the latest stable Rust and the latest stable release
of each dependency, unless there is a concrete reason not to.

- **Rust.** `rust-version` in the root `Cargo.toml`, inherited by every
  crate, is the minimum compiler accepted by the project. It matches the
  stable release used for builds and checks; older compilers are not
  supported. When a new stable comes out, update the toolchain, raise
  `rust-version` to it and run the gate in the same change. The declared
  version must never be above the installed stable.
- **Clippy.** Raising the compiler baseline enables suggestions for newer
  std APIs. Four style lints are temporarily allowed in the workspace
  manifest while [#138](https://github.com/OpenMaker-Dev/openmaker/issues/138)
  coordinates that migration across active feature branches:
  `unnecessary_map_or`, `chunks_exact_to_as_chunks`,
  `manual_is_multiple_of` and `manual_repeat_n`. Remove those specific
  allows as the migration lands. The general Clippy group has lower
  priority so these explicit settings take effect; the compiler baseline
  remains the actual stable release.
- **Dependencies.** Requirements are ordinary (`"0.4"`, not `"=0.4.42"`),
  and the lockfile follows the latest compatible releases. A pin, or a
  hold below a crate's latest major release, needs its concrete reason in
  a comment beside it. The exact pins today are `webkit2gtk = "=2.0.2"`
  and `javascriptcore-rs = "=1.1.2"` (`om-gui`, `web-viewer`): they must
  match the versions `wry` uses, because `om-gui` installs its own page
  message handler (wry's IPC drops messages from `data:` URLs).
- **Checking.** `rustup check` shows a newer stable.
  `cargo update --dry-run --verbose` lists compatible updates, and as
  `Unchanged ... (available: ...)` the crates behind a newer major
  release; after an update, read the lockfile diff and name every crate
  that moved. When a change touches `Cargo.lock`, check it against the
  RustSec advisory database with `cargo audit` (install it once with
  `cargo install cargo-audit --locked`). Any vulnerability it lists needs
  an update or an issue.

## Kinds of tests

- **Default tests** run anywhere: unit tests, checked-in vectors
  (`crates/*/tests/data/`, `docs/re/catalogs/`) and end-to-end tests that
  host a synthetic file on an in-process `om-server`.
- **Ignored tests** (`#[ignore]`, about 170) need inputs that are not in
  git: recorded captures and working files under `re/`, files from the
  FileMaker installers, the oracle harness container, or a live server. A
  few are measurements run on demand, such as release-build timings. The
  test's `#[ignore = "requires ..."]` reason names its inputs.
  Run one with `cargo test -p <crate> --test <file> -- --ignored`. A fresh
  clone or worktree has no `re/` directory; link the one from a checkout
  that has it.
- **Oracle runs** (`tools/fms-*`) execute FileMaker Server's own engine to
  produce new vectors. See each tool's README.

## Verifying UI changes

FileMaker Pro 26 is the visual reference, down to the pixel. Look at the
result instead of trusting a passing test:

- `cargo test -p om-gui --test screenshots` renders the main screens to
  PNGs in `crates/om-gui/screenshots/` (git-ignored). Several other GUI
  tests write images when an environment variable names a directory (for
  example `OM_PDF_ARTIFACT_DIR`, `OM_WEB_VIEWER_SHOTS`).
- Run the app (`cargo run -p om-gui`, mock provider or a hosted file) and
  compare it side by side with FileMaker Pro.
- Measure alignment in the PNG rather than eyeballing it when a pixel
  matters.
- An integration GUI test that renders builds its harness through `tests/common`
  (`harness`, `mock`) or passes `.renderer(common::renderer())`. Unit tests
  pass `.renderer(crate::test_renderer::renderer())`. Both use the same helper
  source in `src/test_renderer.rs`, compiled once per test binary, so every
  harness in that process draws on one wgpu device with its own egui renderer
  and textures. The lazy, process-wide device stays alive through test teardown;
  test parallelism and the default backend and adapter selection stay unchanged.
  Otherwise
  each harness creates and destroys its own Vulkan instance, and with tests
  on parallel threads, Vulkan loaders before 1.4.345 can crash the whole
  test binary with SIGSEGV in libvulkan before any result ([#142](https://github.com/OpenMaker-Dev/openmaker/issues/142)).

### egui and kittest traps

Each of these cost a debugging round:

- Inside popups and menus never allocate `ui.available_width()`: the first
  (sizing) pass is unbounded and the menu balloons. Allocate a minimal
  width and let the justified layout stretch it.
- Popups and `Modal`s spend their first frame on a sizing pass: they are
  invisible and absent from the AccessKit tree, so kittest needs two extra
  `run_steps` before querying or rendering them. `Modal::should_close()`
  fires on kittest's synthesized click that opened it.
- `Popup::from_response` makes the popup an AccessKit child of its
  widget. For a widget in the document's zoom layer, whose AccessKit node
  carries the zoom transform, the popup's items (already in window points)
  are then moved a second time, and kittest clicks miss them: build it with
  `Popup::new(id, ctx, &response, response.layer_id)` instead.
- Toggle widgets surface as AccessKit `toggled`, not `is_selected`.
  Painted text (`painter.text`) is invisible to tests and screen readers;
  use real labels for anything a test must find.
- `TextEdit::margin()` is ignored once `.frame(...)` is set: put the inset
  in `Frame::inner_margin`.
- `ui.columns(n)` splits evenly and `set_width` cannot shrink a column. For
  a fixed-width side list use `horizontal_top` +
  `allocate_ui_with_layout(vec2(w, h), Layout::top_down_justified(..))`.
- kittest embeds `show_viewport_immediate` viewports as `egui::Window`s in
  the root, sharing its input, and any `Id::unique("x")` clashes between
  windows: derive ids from `ui.scope_id().with("x")`.
- kittest delivers queued events one per frame and advances time each
  frame, so two `node.click()` calls are never a double click: push both
  press/release pairs into `app.input_mut().events` in one frame. Copied
  text appears in `app.output().platform_output.commands` the frame after
  `Event::Copy`.
- Decide "a text field has the keyboard" at the start of the frame: the
  Return that closes a `TextEdit` is still `key_pressed` for handlers drawn
  later in the same frame.

## Debugging

- **Logs.** `om-server` uses `env_logger`; `RUST_LOG=debug` (or a module
  filter) shows each request. For timing without a profiler, initialize
  `env_logger` in a throwaway test and read the timestamps between log
  lines.
- **Timings.** Quote release-build numbers (`--release`); debug builds of
  the server's commit paths are several times slower. Measurements and
  their method go in `docs/performance/`.
- **Traffic.** `om-server --record <dir>` and the TLS relay in
  `tools/capture/` save GIOP streams; `tools/capture/giop_dump.py` decodes
  them and masks license keys and auth tokens.
- **Inspecting a file.** Read-only examples print what OpenMaker decodes
  from a local `.fmp12` (`cargo run -p <crate> --example <name> -- <file>`;
  `styles` also needs a layout id, and each example's first doc comment
  gives its arguments):

  | Crate | Example | Shows |
  |---|---|---|
  | `om-hbam` | `dump_keys`, `dump_calcs`, `file_metadata`, `design_paths` | the key tree, stored formulas, file metadata; the paths in each path group, those no rule covers, and the design objects with their Secret paths and design revisions (`--objects`) |
  | `om-layout` | `inspect`, `objects`, `styles`, `library_images` | layouts, objects with bounds and resolved styles, images |
  | `om-provider` | `schema_catalog`, `field_options` | tables, occurrences, relationships; field options |
  | `om-scripts` | `inspect_scripts`, `step_params` | the script catalog, re-encoded exactly; step parameters |
- **The alpha scorecard.** What still blocks a user of given `.fmp12` files,
  per [alpha gate](../ROADMAP.md#alpha-gate) criterion, from the client's
  own decisions: whether each file opens (the account it suggests, an empty
  password), why the record planner refuses each base table, why Browse does
  not let the user change each field object FileMaker does, which steps of
  the scripts and buttons reach a script host capability the desktop host
  lacks (error 3), why the report builder refuses each layout for Preview
  and Save Records as PDF, which objects Browse does not draw or open as
  FileMaker does, and which functions the file's calculations call that
  OpenMaker has no implementation of. It opens copies, draws nothing and
  runs no step. Its Build measure (criterion 7, the desktop route) makes
  scripts, a layout, buttons, an object's new frame, field options, Layout
  Setup's name, views and General options, a layout's width, a field's
  control style and a part's options in another copy through the
  operations the developer tools call, closing and
  reopening the file after each, and once a run makes a blank file with
  File > Create New's operation, adds a field and two records to it and
  reads them back after reopening (left in `OM_SCORECARD_TRUST` as
  `Create New.fmp12` for FileMaker's consistency check). Its Trust measure (criterion 8) writes records in every
  base table of another copy as Browse writes them, through the table's
  layout or a portal, closes the file and checks after reopening that they
  read back as written; with `OM_SCORECARD_TRUST` set to a folder it
  leaves the written copies there, for FileMaker's consistency check. It
  writes `scorecard.md`, the ranked summary, and `scorecard.json`, the
  detail, to the folder you name; keep both out of git. The module docs in
  `crates/om-gui/src/scorecard.rs`, `scorecard/build.rs` and
  `scorecard/trust.rs` say what each measure leaves out.

  ```sh
  OM_SCORECARD_FILES="a.fmp12:b.fmp12" OM_SCORECARD_OUT=/tmp/scorecard \
    OM_SCORECARD_TRUST=/tmp/scorecard-trust \
    cargo test -p om-gui --lib scorecard::alpha_scorecard -- --ignored --exact --nocapture
  ```

  Hosted by OpenMaker Server: `alpha_scorecard_hosted` (in
  `scorecard/hosted.rs`) starts `om-server` as a separate process on
  loopback (`cargo build -p om-server` first, or set `OM_SERVER_BIN`),
  hosting copies of the same files and signing in to each as its own
  `Admin` with an empty password, as the starter solutions have it (the
  server is started with `--allow-insecure-files`, since FileMaker Server
  would not host such files by default). Over
  the wire, by reading calls only, it runs the scorecard, finds and sorts,
  and counts each layout's Preview and PDF pages. Then it commits a record
  that a second connection reads back and performs a script on the host
  that changes a record, and deletes both records and the script; it writes
  `hosted.md`. `OM_STARTER_FINDS_HOSTED=1` runs `starter_finds` against the
  same kind of server through Open Network File.

  ```sh
  cargo build -p om-server
  OM_SCORECARD_FILES="a.fmp12:b.fmp12" OM_SCORECARD_OUT=/tmp/scorecard \
    cargo test -p om-gui --lib scorecard::hosted -- --ignored
  OM_STARTER_FINDS_HOSTED=1 OM_TEMPLATES=<DynamicTemplates folder> \
    xvfb-run -a cargo test -p om-gui --test starter_finds -- --ignored
  ```

  A file hosted by FileMaker Server, or by any server already running: with
  `OM_SCORECARD_HOST` set, the same test measures the one file
  `OM_SCORECARD_FILE` on that host instead of starting `om-server`. The
  values come from the environment only and are not printed or written to
  `hosted.md` or `hosted.json`; messages that carry one have it masked.

  | Variable | Meaning |
  |---|---|
  | `OM_SCORECARD_HOST` | the host name or address; setting it selects this mode |
  | `OM_SCORECARD_PORT` | its port, 5003 when unset |
  | `OM_SCORECARD_FILE` | the hosted file's name |
  | `OM_SCORECARD_ACCOUNT`, `OM_SCORECARD_PASSWORD` | the file's account and its password (which may be empty) |
  | `OM_SCORECARD_LICENSE` | the client license key, for a server that requires one |
  | `OM_SCORECARD_LOCAL_COPY` | a copy of the same file on disk, for its table definitions; without one, Edit has no tables |
  | `OM_SCORECARD_READ_ONLY` | `1`: reads only |
  | `OPENMAKER_CA_CERT`, `OPENMAKER_CLIENT_CERT`, `OPENMAKER_CLIENT_KEY` | TLS, as the desktop reads them: a CA to trust beside the public roots, and a client identity (one is made for the run when both are unset) |

  The run signs in once, and in the full mode once more for the second
  connection, before anything is written. A failed sign-in ends the run
  with its message and is not tried again, so a run cannot lock an account
  out by itself. With `OM_SCORECARD_READ_ONLY=1` only reading calls reach
  the host: the measure holds the connection through a wrapper that
  forwards the layout list, layouts, records, record capabilities, the
  calculation schema, the script catalog, finds and sorts, and nothing
  else. Every other provider call falls to the trait's default, which has
  no connection, so nothing writes a record, a field, a layout or a script,
  and nothing performs one. No window opens, so no script trigger runs. A
  find is the one read that can still write: on a field without its index,
  the host builds the index and stores it in the file (FileMaker Server too,
  `docs/re/schema-path.md` 3.7). Read-only finds use only a text field whose
  definition keeps its value and word indexes (indexing All), and skip the
  find when there is none. Against `om-server`, a read-only run left the
  hosted file byte-identical. In the full mode, the records and the script
  it makes are deleted afterwards, and `hosted.md` lists anything it could
  not delete, including a field's indexes the find may have made the host
  build.

  ```sh
  OM_SCORECARD_HOST=<host> OM_SCORECARD_FILE=<file.fmp12> \
    OM_SCORECARD_ACCOUNT=<account> OM_SCORECARD_PASSWORD=<password> \
    OM_SCORECARD_READ_ONLY=1 OM_SCORECARD_OUT=/tmp/scorecard-external \
    cargo test -p om-gui --lib scorecard::hosted::alpha_scorecard_hosted -- --ignored --exact
  ```

  Criterion 1's local half, on two Contacts copies FileMaker Pro 26 gave
  accounts and privilege sets to (kept out of git; #156, [#7](https://github.com/OpenMaker-Dev/openmaker/issues/7)): a wrong
  password, `reader`, `entry`, `limited`, `nolayout`, `viewonly` and
  `editonly`, through the provider calls and the window; it writes
  `privileges.md`.

  ```sh
  OM_ACCOUNTS_FILE=<156 Contacts.fmp12> OM_CUSTOM_SETS_FILE=<137 Contacts-custom.fmp12> \
    OM_SCORECARD_OUT=/tmp/scorecard \
    xvfb-run -a cargo test -p om-gui --test privilege_measure -- --ignored
  ```

  For FileMaker Pro's side of criterion 7, `build_step_files` applies each
  layout and field step of the Build measure alone to a fresh copy of one
  file, and all of them to one more, and leaves the closed copies with a
  README saying where each change is:

  ```sh
  OM_BUILD_STEP_SOURCE=Tasks.fmp12 OM_BUILD_STEP_FILES=/tmp/build-steps \
    cargo test -p om-gui --lib build_step_files -- --ignored
  ```

  `build_script_step_files` does the same for the Script Workspace's
  operations on the Tasks starter's own scripts (new, rename, add a step,
  duplicate, move, delete), with the same variables.

  Manage Database's steps the Build measure leaves out (`scorecard/md_steps.rs`):
  - **Tables:** create, rename and delete a table.
  - **Fields:** add a field of each kind (the five stored types, a container,
    a stored and an unstored calculation, a summary, a global); rename a
    field, change its type, delete it.
  - **Options for Field:** a serial, a calculated value, a lookup from
    another table, validation (not empty, unique, by calculation), indexing.
  - **Relationships:** add an occurrence; add, edit (a second predicate, a
    sort, allow creation, delete related records) and delete a relationship.

  Each step goes through Manage Database's operations on a copy of each file
  and a File > Create New file, and is read back from the schema after
  reopening; it writes `md-steps.md` and `md-steps.json`. `md_step_files`
  writes one blank file per step, `MD01.fmp12` onward, with a README, for
  FileMaker's check.

  ```sh
  OM_SCORECARD_FILES="a.fmp12:b.fmp12" OM_SCORECARD_OUT=/tmp/scorecard \
    cargo test -p om-gui --lib scorecard::md_steps::md_steps_scorecard -- --ignored --exact
  OM_MD_STEP_FILES=/tmp/md-steps \
    cargo test -p om-gui --lib scorecard::md_steps::md_step_files -- --ignored --exact
  ```

  Entry (criterion 2) types a record through the window, as a user would.
  For each base table it takes the layout, among those the layout menu
  offers, with the most field objects FileMaker lets the user change. It
  adds a record there with New Record and fills each such object by its
  control: typing by the field's type, the first item of a drop-down list
  or pop-up menu, a checkbox, the 15th in a drop-down calendar, a picture
  inserted into a container, and the fields of a portal's creation row.
  Then Ctrl+Enter commits the record, and it compares what each object
  shows with what the provider reads after reopening and with a new window
  at that record.
  Two more rows click a text field, type, press Tab and type again, on a
  new record and in a find request, and expect the second text in the next
  field of the layout's tab order.
  It writes `entry.md` and `entry.json`, and keeps each file's copy as
  the window saved it in `saved/<n>/`, for FileMaker Pro to open; the
  module doc of `crates/om-gui/src/scorecard/entry.rs` says what it
  leaves out.

  ```sh
  OM_SCORECARD_FILES="a.fmp12:b.fmp12" OM_SCORECARD_OUT=/tmp/scorecard \
    cargo test -p om-gui --lib scorecard::entry::starter_entry -- --ignored --exact --nocapture
  ```

  Portal rows also retain their visible control candidates, geometry,
  toolbar position and settled state before close and before and after
  pending reopen reads complete. Revealing a tab may start portal reads;
  the measure waits for the window to settle and locates the control again
  before judging its value. A blank settled value still fails. To investigate
  one portal pop-up through the same entry/save/reopen path, set
  `OM_ENTRY_FILE` to its input file, `OM_ENTRY_FIELD` to its accessible
  field name, and `OM_SCORECARD_OUT` to a new folder, then run the ignored
  `scorecard::entry::portal_popup_readback` test. It makes 20 independent
  copies with the same solution name as the full Entry measure (or
  `OM_ENTRY_RUNS`, from 1 to 20), stops at the first failure, and keeps
  each run's JSON and saved file. It enters only the first matching plan
  on a fresh copy; earlier layouts entered by the full measure can affect
  portal value lists.

  Record transfer (criterion 6, its second half) runs through the
  window, as a user would, with the same variables: for each file, in the
  table of the layout it opens on, File > Import Records of a generated
  three-record seed into a copy, Export Records as CSV and Save Records as
  Excel of the imported records, then Import Records of that CSV into a
  fresh copy and Export Records again. It writes `transfer.md` and
  `transfer.json`, with the first error of each step and the first
  difference between seed, exports and workbook (read back with
  `om_transfer`'s readers), and leaves its copies and files in
  `transfer-<n>/` there. The module doc of
  `crates/om-gui/tests/scorecard_transfer.rs` says what it leaves out.

  ```sh
  OM_SCORECARD_FILES="a.fmp12:b.fmp12" OM_SCORECARD_OUT=/tmp/scorecard \
    xvfb-run -a cargo test -p om-gui --test scorecard_transfer -- --ignored
  ```

  Building a solution (criterion 7) through `om-author` on OpenMaker
  Server: each file's copy hosted on loopback by an in-process `om-server`,
  each build step (a table, a field of each type, a relationship, a layout,
  a field and a button on it, a script, the button's action) tried with the
  command that would do it, or answered with what `capabilities` says. It
  writes `build.md` and `build.json`.

  ```sh
  OM_SCORECARD_FILES="a.fmp12:b.fmp12" OM_SCORECARD_OUT=/tmp/scorecard \
    cargo test -p om-author --test scorecard_build -- --ignored
  ```

### Error reports

`om-server` and `om-gui` can report errors and panics to a Sentry project
(`om_server::reporting`). Reports are off unless `SENTRY_DSN` is set when
the program starts: without it no Sentry client is made and nothing is
sent. No build carries a DSN.

| Variable | Meaning |
|---|---|
| `SENTRY_DSN` | The project's DSN, where reports go. Unset or empty: no reports |
| `SENTRY_ENVIRONMENT` | The reports' environment; default `development` for a debug build, `production` for a release build |
| `HTTPS_PROXY`, `HTTP_PROXY` | The reports' proxy: `HTTPS_PROXY` for an `https` DSN, else `HTTP_PROXY` (lower-case names too), as Sentry's SDK picks it. `ALL_PROXY` and `NO_PROXY` are not read |

Both programs report to one project, told apart by the `app` tag
(`om-server` or `om-gui`). The release is `openmaker@<crate version>`,
without the commit, which the build does not record. What is sent:

- A panic, with its location and stack trace. Its message is sent only
  when the program wrote it out in full (`panic!("text")`, `unwrap` on
  `None`); a message formatted at run time (`expect`, `unwrap` on `Err`, an
  index out of bounds, `panic!("{value}")`) can hold any value, such as a
  field's text, and is withheld. A panic on one of Sentry's own threads is
  not reported.
- Each error-level log line, as an event with the stack of the thread that
  logged it and, as breadcrumbs, the last 100 warning and info lines logged
  on that thread. Warning and error lines of OpenMaker's own crates also go
  to Sentry's log. Debug lines, `ureq`'s and Sentry's own are not read.
- The error a program ends with: `om-server`'s once reports are on, which
  is after the command line and the Admin Console's password are read;
  `om-gui`'s window's.
- The host name, the operating system, the architecture and the Rust
  version.

Every message is scrubbed first, going by how values look in it: a quoted
string (a field's value, a file, account, script, schedule or machine
name, a caught panic's message) becomes `"…"`; a bracketed list (key
paths, index keys, which are made from field values, lists of values)
becomes `[…]`; a key path (`3/5/x6b6579…`, as `om-hbam` writes it in
its errors) becomes `…`; everything from the first `{` on (the `Debug`
output of a structure, such as an operation with the records it commits)
becomes `{…}`; an IP address, also in a URL, becomes `<address>`. Default
personal data (`send_default_pii`) is off, and no request, record or file
is attached. The console shows what it showed without Sentry, and each
line before Sentry reads it.

Each request to Sentry, its name lookup included, ends after 5 seconds, so
an endpoint that does not answer delays a program's end by seconds, not
indefinitely: about 12 seconds, a few more when a panic is being reported
as the program ends.

Limits: a value a log line gives unquoted and outside a list (an error
text from a lower layer, `{e}`) is sent as it reads; the Admin Console's
own account name and file names and paths given unquoted are sent; a
second signal ends `om-server` at once, without sending what is queued.
Stack frames give file and line only for a build with debug info, and
Sentry symbolicates them only with the build's debug files uploaded; a
panic's location is in its message either way.

## Reverse-engineering and oracle tools

Everything under `tools/` is research tooling for writing specs, not part
of the product. Run Python tools with `python3 -I`, and treat extracted
binaries and captures as untrusted data.

| Path | Purpose |
|---|---|
| `tools/fms-calc-harness/` | FileMaker Server's calculation engine in a network-less container: evaluate and compile formulas |
| `tools/fms-script-harness/` | The same container running FileMaker scripts, with probe batteries |
| `tools/fms-sql/` | ExecuteSQL probe batteries and the FQL grammar extraction |
| `tools/fms-transfer-oracle/` | Import and Export Records probes |
| `tools/calc-oracle/` | Calculation probes in FileMaker Pro itself (macOS) |
| `tools/capture/` | TLS relay for recording FileMaker Pro sessions, GIOP decoder |
| `tools/ghidra/`, `tools/re-network/` | Headless Ghidra scripts and helpers for querying pre-analyzed binaries |
| `tools/*.py` | Format and binary analysis helpers (HBAM blocks, protobuf extraction, catalogs) |

The tools read and write working data in three git-ignored folders:
`re/extracted/` (unpacked installers), `re/captures/` (recorded sessions;
they contain credentials) and `re/work/` (analysis output, Ghidra projects,
probe files). Only these folders are ignored, not the rest of `re/`, so
keep research output inside them, and never commit anything from them.

Ghidra note: pre-analyzed projects can mark common functions (`memcpy`,
`operator new`, ...) as non-returning, so decompiled output silently stops
at the first call to one. Clear that flag before trusting a truncated
function. Decompile client-side code paths from the macOS `DBEngine`
rather than the Linux server library, whose DWARF prototypes omit a hidden
return pointer.
