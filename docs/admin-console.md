# OpenMaker Server's Admin Console

How OpenMaker Server is administered from a browser: an Admin Console with
FileMaker Server's tabs, URLs, pages and visual system, and the Admin API v2
beside it. FileMaker Server's own console and API are described in
[re/admin-console.md](re/admin-console.md), whose section numbers this note
cites. Code: `crates/om-admin` (the console and the Admin API),
`crates/om-server/src/console.rs` (both in the server) and
`crates/om-server/src/admin.rs` (`ServerHandle`).

Built so far:

- `om-admin` serves sign-in, sessions, Sign Out and the application shell
  (top bar with the clock and the alerts bell, the tab row, each tab's
  sidebar and views) over HTTPS, against a `Backend` trait whose model
  covers every view. The Dashboard, Databases, Backups, Logs,
  Configuration > General Settings, Folders, Schedules and SSL
  Certificate, and Administration > Administrator are built
  ([Backups and Logs](#backups-and-logs)); the other views say that they
  are not available yet. `cargo run -p om-admin --example mock_console`
  runs it on sample data (`MockBackend::sample`).
- `om-admin` serves the Admin API v2 beside the console, against the same
  backend ([Admin API](#admin-api)).
- `om-server` keeps a registry of the connected clients, snapshots of the
  server, its files and its clients, Disconnect (at once or with notice),
  Send Message, Block New Users, and Close, Open, Pause and Resume of a
  hosted file ([Administration in om-server](#administration-in-om-server)),
  and runs the console and the Admin API with `--admin-user`
  (`console::ServerBackend` answers both from `ServerHandle`; it does not
  pass on a disconnect with notice, Send Message, Block New Users or the
  file actions yet).
- `om-server` writes an event log in FileMaker Server's format and keeps
  alerts, and `ServerHandle` lists and reads the log's files for the Log
  Viewer ([The event log and alerts](#the-event-log-and-alerts));
  `console::ServerBackend` does not pass the alerts or the log on yet.
- `om-server` makes backups on backup schedules and on demand, keeps each
  schedule's last ones, and lists, preserves, labels and deletes them
  ([Backups](#backups)); `console::ServerBackend` does not pass them on
  yet.

The sections up to [Views](#views) record the design the rest follows.

## Scope

The console manages the server the way FileMaker Server's does. It keeps the
same tabs, URLs, page structure, controls and visual system (re 2-7), so an
administrator's habits and FileMaker Server's documentation carry over.

- **Faithful and honest.** Every view shows OpenMaker Server's real state. A
  tab or sub-view whose feature OpenMaker Server lacks keeps its place in the
  navigation and says plainly that the feature is not available yet, rather
  than showing controls that do nothing.
- **Left out:** Remote Backups and Claris MCP, which are Claris cloud
  services. They are not in the navigation.
- **Admin API v2.** OpenMaker serves the same API (re 8) with the same paths,
  JSON shapes and error codes, as each backing feature lands, so existing
  FileMaker Server automation keeps working.
- **Clean room.** No Claris console asset is copied: no code, style sheets,
  icons, logos or help text. OpenMaker draws its own marks and icons at the
  measured sizes, colors and positions and writes its own help text. It
  reuses functional labels ("Server Name", "Block New Users") so that
  administrators find things where they expect them.

## Architecture

- **Crate.** A crate of its own, `om-admin`, holds the HTTP server, sign-in
  and sessions, the console's pages and the Admin API. It does not depend on
  `om-server`: it asks a `Backend` trait for state and actions, with a model
  shaped like the Admin API's (server, databases, clients, settings,
  folders, certificate, schedules, backups, logs). A feature the server
  lacks answers `Unsupported`, which the view shows as not available.
  `om-server` implements `Backend` over `ServerHandle`; tests and design
  work use an in-memory mock backend.
- **Views.** Each view is a module of its own (`om-admin/src/views/`),
  rendered inside the shell. Its actions are form posts that carry the
  session's CSRF token, are logged with the administrator and address, and
  return to the page, which shows their outcome once. The outcome is kept
  in the session, not in the address, so that a link cannot make the
  console show text of someone else's choosing (inf).
- **Process.** `om-server` runs the console in its own process, on a thread
  of its own with a small async runtime, so the RCFMP12 listener's
  thread-per-connection design is untouched. The two share the server's
  state through the backend, and shutdown stops both.
- **HTTP.** `axum` on `tokio` and `hyper`, with `tokio-rustls` for TLS. The
  listener serves HTTPS only (HTTP/1.1), with the server's certificate and
  key and without client certificates. It serves at most 64 connections at
  once, 8 of them from one address (an IPv6 /64), and closes the rest as
  they arrive. A client gets 10 s for its TLS handshake, 30 s for a
  request's headers (which also ends an idle connection), and 30 s from
  the headers to the response, its body included; past that it gets 408
  and the connection closes. At shutdown, idle connections close at once
  and requests being answered get a second to finish.
- **Rendering.** Pages are rendered on the server as HTML with `maud`, which
  escapes all text. Icons are inline SVG, so the page loads no images. The page frame, sidebar, settings row, toggle, table,
  modal and chart are shared components, so every page has the same
  structure. Charts are SVG drawn on the server; no charting library.
- **Assets.** One style sheet and one small script written for OpenMaker,
  and OpenMaker's own SVG icons, compiled into the binary and served under a
  content-hash URL with long-lived caching. Each is joined from parts, the
  shell and the shared components first and then one part per area.
- **Without the script.** Every action is a regular form post that
  redirects back to the page, and menus such as the alerts panel are
  `<details>` elements. The script adds the running clock, immediate
  toggles, closing popovers on an outside click, modals and live refresh on
  top. The clock's SERVER pill switches to the browser's time zone and then
  reads LOCAL (inf).
- **Live refresh.** While a page is visible, the script re-fetches its live
  regions as HTML fragments every 5 seconds and replaces them: dashboard
  counters, the database list and the client list. It keeps the boxes
  checked in a region, and leaves a region alone while a modal or a menu is
  open or the administrator is typing in it. A region whose state the
  server cannot read answers an error status, and the script keeps what
  the region shows, checked boxes included, until a read works again;
  loading the page shows the error. Fetching a region does not
  count as using the session, so an unattended console still signs out
  after 30 idle minutes (inf); the page then reloads to the sign-in. FileMaker Server pushes
  these over socket.io (re 1); polling is OpenMaker's choice, and server
  push can come later without changing the pages. A page whose regions
  depend on its query (the Databases view's selection, search and sort,
  the Dashboard's chart) fetches them with the same query.
- **Statistics.** A sampler started with the console, on its runtime,
  reads the operating system on every minute and keeps ten samples, as
  FileMaker Server shows (re 4.1); its first sample comes a second after
  the console starts, so the chart is not empty for a minute. FileMaker
  Server does not define its figures; OpenMaker's are (inf):
  - CPU: the share of all processors' time that was busy (not idle or
    waiting for I/O) since the previous sample;
  - Memory: the share of memory in use, total less what the kernel reports
    as available;
  - Network: bytes received and sent per second by every interface but
    loopback;
  - Disk: MB (2^20 bytes) read and written per second by the physical
    disks, each counted once and not through its partitions or the
    virtual devices above it.

  They come from Linux's `/proc` (`stat`, `meminfo`, `net/dev`,
  `diskstats`); on another system the chart says they are not available.
  The reading runs on a blocking thread, one at a time, so a slow file
  system cannot hold up the pages. Volume Status shows the file system
  that holds the backend's data folder (`statvfs`, on Unix), as used and
  remaining space that add up to the total: space reserved for the
  superuser is left out, as `df` leaves it out (inf).

## URLs and options

- The console keeps FileMaker Server's URLs: `/admin-console/signin`,
  `/admin-console/app/<tab>/<view>` (re 2), and the Admin API under
  `/fmi/admin/api/v2`.
- `om-server --admin-user <name>` turns the console and the Admin API on.
  Without `--admin-user` neither runs.
- The password comes one of two ways (`development.md`, "The Admin
  Console"):
  - from a file, `--admin-password-file`;
  - or from `OM_ADMIN_PASSWORD`, which stays readable in the server's
    starting environment.

  It is never printed, and may not be empty. The console is given only its
  digest (`om_admin::PasswordDigest`).
- `--admin-bind <addr:port>` defaults to `127.0.0.1:16000`, this machine
  only; `0.0.0.0:16000` manages the server from elsewhere. 16000 was
  FileMaker Server's console port before it moved behind port 443.

## Security

**Sign-in**

- One administrator account, taken from the options.
- Passwords are compared through digests in constant time.
- A wrong user name and a wrong password give the same message.
- After 5 failures from one address within 5 minutes, that address waits
  5 minutes before it can sign in again, whatever it sends. An IPv6 address
  counts by its /64 network. At most 10,000 addresses are tracked; past
  that, the one heard from least recently is forgotten.

**Sessions**

- A random 256-bit identifier in a cookie with `HttpOnly`, `Secure`,
  `SameSite=Strict` and `Path=/admin-console`.
- A session ends after 30 idle minutes, or at Sign Out.
- At most 5 console sessions are open at once, as in FileMaker Server
  (re 1); a new sign-in ends the one idle longest (inf). FileMaker Server's
  string table has a message for refusing a sign-in past the limit ("Too
  many Admin Console sessions are open"; not observed), but its sessions
  end when their socket.io connection drops. A cookie session does not know
  that its browser closed, so refusing could lock the administrator out for
  30 minutes.
- Admin API tokens are separate: random, at most 5 at once, ending after 15
  idle minutes, as in FileMaker Server (re 8); see
  [Admin API](#admin-api).

**Requests**

- Every state-changing request is a POST, PATCH or DELETE.
- Console forms carry the session's CSRF token, and requests whose `Origin`
  is another site are refused, the Admin API's included. A browser that
  hides the origin (`Origin: null`) must say `Sec-Fetch-Site: same-origin`.
- A form returns only to a page of the console's application, never to an
  address the form names elsewhere.

**Response headers**

- A strict `Content-Security-Policy`: `default-src 'self'; base-uri 'none';
  form-action 'self'; frame-ancestors 'none'`, so no inline script or
  style, and `X-Frame-Options: DENY`.
- `X-Content-Type-Options: nosniff`.
- `Referrer-Policy: same-origin`, not `no-referrer`: under `no-referrer`
  browsers send `Origin: null` even with the console's own forms.
- `Cache-Control: no-store` on pages and API replies; the assets, whose
  URLs carry a hash of their content, are cached for good.

**Exposure and logging**

- The console listens on this machine unless configured otherwise.
- Sign-ins, failures and every administrative action are logged with the
  address they came from. Secrets are never logged.

## Views

What each of FileMaker Server's views (re 4) shows on OpenMaker Server, and
what backs it.

| View | What OpenMaker Server shows and does | Backing |
|---|---|---|
| Sign-in, shell, Sign Out | as in FileMaker Server, with OpenMaker's wordmark; the bell lists the server's alerts, such as a file that failed to open or save, each with OK to acknowledge it; it stays empty until `console::ServerBackend` passes them on ([Limits](#limits)) | `om-admin`; `ServerHandle::alerts`, `acknowledge_alert` |
| Dashboard | [below](#dashboard): counters, client types, server facts, certificate expiry, last backup, volume status and system statistics; in `om-server`, "Last Backup" reads "None" until `console::ServerBackend` passes the backups on | `server`, `databases`, `clients`, `certificate`, `backups`, `data_folder`; the sampler |
| Databases | [below](#databases): files with status, encryption and client count; clients with session name, files or account, address, sign-in time and client type; Disconnect and Send Message; Open, Pause, Resume and Close of a file or of all. In `om-server`, `console::ServerBackend` takes only the immediate Disconnect so far; the rest are refused when tried ([Limits](#limits)) | `databases`, `clients`, `disconnect_client`, `send_message`, `change_database` |
| Databases: Download, Remove, Verify, Flush, Clear Encryption Password | not available ([Limits](#limits)) | |
| Configuration > General Settings | Server Information (state, start time, name, id, addresses, version, display language); of the Startup Settings, Block New Users, which says it is not available until `console::ServerBackend` passes it to `ServerHandle::set_block_new_users` | `ServerHandle` |
| Configuration > Folders | Database Folders: the database folders searched at startup and the folders of files named elsewhere, in FileMaker's path syntax with Copy; Additional Database Folders: a note that the database folders are given when the server starts; Backup Folders: the backup folder, Progressive Backup not available. In `om-server`, `console::ServerBackend` does not pass the backup folder on yet | `Backend::folders` |
| Configuration > SSL Certificate | Certificate Information (status, issued to and by, expiry) and Certificate Details of the certificate the server presents; a note in place of Create CSR, Let's Encrypt, Import and Delete Files | `Backend::certificate`, certificate parsing |
| Configuration > Schedules | the schedules of `om-server --schedules` in FileMaker Server's grid, with Run Now for the checked ones and an Enabled toggle in each row; Last Run and Last Status refresh live; a note in place of Create Schedule, Duplicate, Delete and Edit | `Backend::schedules`, `run_schedule`, `enable_schedule` |
| Configuration > FileMaker Clients, Notifications | not available | |
| Backups | the sidebar's backup schedule (Repeat, Keep, Next Backup), Back Up Now and the running backup's status; Auto-Backup and On-Demand and Preserved with Preserve Backup, Edit Label and Delete; Backup Settings, whose three settings are not available. A backend without backups says so; `om-server`'s `console::ServerBackend` does not pass its backups on yet ([Limits](#limits)) | backend `backups`, `schedules`, `backup_plan`, `back_up_now`, `preserve_backup`, `label_backup`, `delete_backup`; `ServerHandle`'s backups once passed on ([Backups](#backups)) |
| Connectors, AI Services | not available | |
| Administration > Administrator | the console account's user name, without Change: it is set at startup | options |
| Administration > Licenses, Roles, External Authentication, Restrict Access | not available | |
| Logs | the Log Viewer: a log file's entries, newest first, a Block Size at a time, and Download; Log Settings, of which only the Block Size is OpenMaker's (for the session). Interactive View, Download All Logs and Reset All Filters are not available. `om-server` writes an event log, but its `console::ServerBackend` does not pass the log files on yet ([Limits](#limits)) | backend `log_files`, `read_log`; `ServerHandle::log_files` and `read_log` once passed on |

## Backups and Logs

The Backups and Logs tabs (re 4.3, 4.9) as `om-admin` shows them. Points
marked (inf) are OpenMaker's choices where FileMaker Server's behavior is
not known or not copied.

**Backups**

- **Sidebar.** Below the introduction, a tab can show content of its own
  above its sub-views (`views::sidebar`); Backups shows the backup schedule
  picker, its Repeat and Keep (`Backend::backup_plan`), Next Backup with
  Back Up Now (a confirmation, then `back_up_now`), and Backup Status as a
  live region. Cancel is always unavailable: the backend has no way to
  cancel a backup. The sub-views carry the number of backups and of
  preserved backups.
- **Choices in the address.** The schedule picker (`?schedule=<id>`), the
  Showing filter (`?showing=<schedule>`) and Filter by Label (`?label=`)
  are GET forms, so they work without the script; each carries the
  others' values. With the script, a select takes effect when changed and
  the label filter hides rows as it is typed; Select All never selects a
  hidden row, and a row shown again comes back unchecked. Showing offers the backup
  schedules and then any other schedule a listed backup came from, such as
  a deleted one (inf). After an action the page returns without these
  choices (inf: a form may return only to a page's path).
- **Actions.** Preserve Backup, Edit Label's OK and Delete's confirmation
  post the checked rows' folder names to one action. It refuses an empty
  selection, more than 1000 backups (a console form is at most 16 KiB,
  about 500 folder names), and a name that is empty, longer than
  255 bytes, `.` or `..`, or holds `/`, `\`, `:` or a control character,
  since a server may find the backup's folder by its name (inf). A label is
  trimmed and must be one line of 1 to 100 characters without control
  characters (inf: FileMaker Server's limit is not known, and a server
  that keeps a label on a line of a settings file must not be handed a
  line break). When some backups fail, the page says how many were done
  and why the first failed.
- **Preserved column.** A check mark in the accent color (inf: the
  reference shows no preserved backup).
- **Running status.** "Running: <schedule>" (inf); "No Backup Running" as
  FileMaker Server says, but only after the backups were read. A server
  without backups shows "Not available yet", and a failed read "Status
  unknown" with the reason as its tooltip (inf).

**Logs**

- **Log Viewer.** The file is chosen from the "Select" menu
  (`?file=<name>`); before that the grid says "No data to display", as
  FileMaker Server's does. Entries are shown newest first (inf: the
  reference lists the file's order), in pages of the Block Size with
  Previous and Next links (`?skip=`, at most 10,000,000), instead of
  FileMaker Server's scrolling grid. The time stamp reads
  `MM/DD/YYYY HH:MM:SS`, with the line's own time stamp as its tooltip.
  Columns other than Message end in an ellipsis rather than wrap (inf: no
  other column of the reference wrapped). Collapse Side Bar works without the script (a checkbox
  the style sheet reads).
- **Download.** The chosen file as `text/plain` UTF-8, oldest first, one
  entry a line with its fields separated by tabs, as an attachment named
  after the file with anything but ASCII letters, digits, `.`, `-` and `_`
  replaced by `_`. Only a file the backend lists is read. A log over 16 MiB
  gives its newest entries that fit (inf). The read runs on a blocking
  thread, off the console's request threads.
- **Block Size.** 100, 200 (the default, as in the reference), 500 or
  1000 rows, kept for the session (inf: FileMaker Server keeps one value on
  the server).

## Admin API

`om-admin` serves FileMaker Server's Admin API v2 (re 8) under
`/fmi/admin/api/v2` on the console's listener, over the same backend as the
views (`crates/om-admin/src/api/`). Paths, JSON field names, HTTP statuses
and error codes are FileMaker Server's, taken from its Admin API Reference;
no reply has been compared with a live FileMaker Server's. Points marked
(inf) are OpenMaker's choices where the reference is silent.

**Tokens**

- `POST /user/auth` with HTTP Basic credentials (the console's account)
  returns `{"token": ...}`: 256 random bits as 64 hex digits. Later calls
  send `Authorization: Bearer <token>`. `DELETE /user/auth/{token}` ends
  one and answers an empty `response`.
- Basic credentials on any other call also open a token, returned in the
  `X-FM-Access-Token` header, as the reference documents.
- The credentials are checked as the console's sign-in checks them: digests
  compared in constant time, and the same lockout per address with the same
  failure count, so failures at the console and the API add up. A
  locked-out address gets 429 with code 1703 and the console's lockout text
  (inf); tokens opened before go on working. A wrong token does not count
  as a failure: a script whose token has expired is not locked out (inf).
- At most 5 tokens are open at once, and a token ends after 15 idle
  minutes (re 1). A sixth ends the one idle longest (inf), as the console's
  sessions do: scripts often do not sign out, and refusing would lock the
  API for 15 minutes. Tokens are kept as SHA-256 digests.
- Missing, malformed or PKI credentials, and an unknown or expired token,
  get 401 and 1703, without a `WWW-Authenticate` challenge (inf).

**Requests**

- The API takes no cookies and sends no CORS headers or `WWW-Authenticate`
  challenge, so a browser holds no credentials for it, and another site's
  page cannot read its replies or send it a token or a JSON body (both need
  a CORS preflight, which the `Origin` check refuses). That check answers
  an API path with the API's envelope: 403 and code -1 with OpenMaker's
  own text, as FileMaker has no such refusal (inf).
- A body must be JSON (`Content-Type: application/json`, else 400 and 1708)
  and an object, at most 16 KiB (else 413). Invalid JSON is 400 and 1710.
  Documented fields are checked for type and range, `messageText` at most
  200 characters and `graceTime` 0-3600 s. A missing required field is
  1707, a wrong value 1708, each naming the field as FileMaker's texts do.
  A documented field given where the schema does not allow it, such as
  `force` with `PAUSED`, is a wrong value (1708; the schema does not give
  the code, inf). Fields the reference does
  not document, and `null` ones, are ignored (inf, as FileMaker Server's
  validator is expected to). A path id that is not a whole number at or
  above the schema's minimum, or that does not decode, is 400 and 1708.
- Every request that changes something is logged with the address and the
  token's account; tokens and passwords are not.

**Replies**

- Every reply is `{"response": {...}, "messages": [{"code": "0", "text":
  "OK"}]}`; an error has an empty `response` and FileMaker's code and text:
  1700 (404) no such client, file or schedule, 1703 (401), 1707 and 1708
  (400), 1709 (409) an operation the resource's state forbids, -1 (500) a
  failure.
- A feature the backend does not have (`Unsupported`) is a resource that
  does not exist when read (404, 1700) and an operation refused in the
  current state when changed (409, 1709) (inf). Other paths of FileMaker's
  API, and unknown ones, answer 404 and 1700, a known path with another
  method 405 and 1700 (inf).
- Ids are strings, times `2026-11-11T18:05:01` in the server's time zone,
  durations `HH:MM:SS`, folders in FileMaker's path syntax with a closing
  slash (`filelinux:/srv/Databases/`), and a time that never was
  `0000-00-00T00:00:00`, as in the reference's examples.

**Endpoints**

| Method and path | What OpenMaker Server answers |
|---|---|
| `GET /server/metadata` | `serverName`, `serverID`, `serverIP` (the first of the machine's addresses, or the listen address), `serverVersion` as the backend reports it (`OpenMaker Server 0.0.0`, not a dotted FileMaker number), `serverHostTime`, `timezone` (IANA, when known) and `offset` (minutes from local time to UTC, positive west of UTC, as JavaScript counts; inf) |
| `GET /server/status` | `RUNNING`: the API answers only while the server runs |
| `PATCH /server/status` | `RUNNING` answers `RUNNING`; `STOPPED` is refused with 409 and 1709, as OpenMaker Server cannot stop its Database Server and go on serving (inf) |
| `GET /databases` | `id`, `filename`, `folder`, `status`, `clients`, `size` (when known), `isEncrypted`, `decryptHint` (empty) and `hasSavedDecryptKey` (false): no encryption passwords are kept. `enabledExtPrivileges` is left out: OpenMaker Server reads a file's extended privileges (`om-server`'s `scripts/host/get.rs`) but does not report them here yet. `openDBCount` counts the files neither closed nor opening (inf) |
| `PATCH /databases/{id}` | `OPENED`, `PAUSED`, `RESUMED`, and `CLOSED` with `messageText` (required, may be empty): the file's clients get 90 s to close it (re 9), none with `force`. `key` and `saveKey` are taken only with `OPENED` (and not used: OpenMaker Server hosts no encrypted files), `messageText` and `force` only with `CLOSED`. `FLUSHED` and `PRESNAPSHOT` are refused (409, 1709). The reply has the file's status after the change |
| `PATCH /databases` | the change for every file whose state allows it, leaving the others; the reply lists every file (inf). `force` is taken only with `CLOSED`; `CLOSED` sends no message |
| `GET /clients` | `id`, `userName` (the session name), `computerName`, `ipaddress`, `appType`, `appVersion` (the version string the client sent), `connectTime`, `connectDuration`, `status` (`NORMAL`, or `DISCONNECTING`, inf), `concurrent` and `teamLicensed` (false: no licenses), `extpriv` (`fmapp` for FileMaker Pro and Go) and `guestFiles` (`id`, `filename`, `accountName`), with the counts per family. `appType` is `FMPRO` for FileMaker Pro; the others (`FMGO`, `FMWEBD`, `FMSASE`, `FMXDBC`, `FMMISC`) follow the count names (inf) |
| `DELETE /clients/{id}` | disconnects after asking the client to close its files with `messageText`, waiting `graceTime` (default 90 s); with no message and a grace time of 0, at once |
| `POST /clients/{id}/message` | sends `messageText` (1-200 characters, not only white space) |
| `GET`, `PATCH /server/config/blocknewusers` | `blockNewUsers` |
| `GET /server/currentfoldersettings` | the first of the Database Folders (above) as `defaultDatabaseFolder`, the next two as `additionalDatabaseFolder` and `additionalDatabaseFolder2` (`["enabled", path]`, else `["disabled"]`), and `backupPath`; secure, remote container and progressive backup folders do not exist |
| `GET /server/certificate/info` | `certificate` with `subjectName` and `issuerName`, the common names (the issuer's organization when it has none) |
| `GET /schedules`, `GET /schedules/{id}` | `id`, `name`, `enabled`, `status` (`IDLE` or `RUNNING`), `lastError` (0, or -1 after a failed run: the model has no number; inf), `lastRun`, `nextRun`, and the type member: `filemakerScriptType` with `resource` and `fmScriptAccount`, or an empty `backupType` |
| `PATCH /schedules/{id}` | `enabled`, and `status` `RUNNING` to run it now (`IDLE` asks nothing); edits of its name, timing or task are refused (409, 1709): schedules come from the server's configuration. Schedule ids start at 2, and id 1, which FileMaker Server reserves, is refused on every schedule path (400, 1708) |

### Dashboard

`views/dashboard.rs`, at FileMaker Server's measured layout (re 4.1).
System Overview, Volume Status and the chart are live regions.

- **Total Connections** counts every client. **Hosted Databases** is "open
  / total", where open is every file that is not closed or opening, paused
  ones included (inf).
- **Client types.** A client's `appType` (FileMaker's
  `ApplicationInfo::AppType`, re/calc-functions.md 6) gives its chip:
  FileMaker Pro for 0 and 1 (the former Pro Advanced, and Pro), FileMaker
  Go for 5, FileMaker WebDirect for 8, and Additional for every other
  type. FileMaker Server's help says Pro clients count against its
  license's client limit and Additional ones do not; which types it puts
  where is inferred, and OpenMaker Server has no license or limit.
- **Facts.** Server Name, IP Addresses (the configured addresses, else the
  listen address unless it is unspecified) and Version; FileMaker Server
  License Expiration "None"; SSL Certificate "Expires on <UTC time>" from
  the leaf certificate, with Details opening Configuration > SSL
  Certificate; Last Backup as "<schedule> (<date, time>)" of the newest
  backup, else "None", also when the server makes no backups. OpenMaker
  Server has no Data API, ODBC/JDBC or Script Engine connections to count:
  those rows read "Not available" (inf). Only a backend that answers
  `Unsupported` makes a fact "Not available"; any other failure, or a
  certificate the console cannot parse, reads "Could not be read" with
  the reason, as an error.
- **Volume Status**: a doughnut with the percentage used, then Total
  Space, Space Used and Space Remaining; "N/A" when the backend names no
  data folder, and also "Could not be read" with the reason when its file
  system cannot be read.
- **System Statistics**: one line chart of the ten samples, drawn on the
  server as SVG (`chart.rs`). The y axis runs from zero to the first round
  step (1, 2 or 5 times a power of ten) above the largest value, in at most
  eight steps; this matches the CPU and memory charts measured, not the
  network and disk ones (re 4.1). The line is a monotone curve, which never
  dips below a point (inf). The CPU, Memory, Network and Disk tabs are
  links (`?chart=memory`). On a phone the chart keeps a legible size and
  scrolls on its own.

### Databases

`views/databases.rs`, in FileMaker Server's two columns (re 4.2). The
view's state is its query, so that it works without the script and a
reload or an action returns to it: `db` (the selected file), `folder`, `q`
and `sort` (the files), `cq` and `csort` (the clients); `close` opens the
Close dialog. Menus are `<details>`; the client list is one form whose
checked rows Disconnect and Send Message act on, with their dialogs inside
it. Both lists are live regions and keep their checked boxes.

- **Files.** The folder row selects all files; its select filters them by
  folder: each database folder (`Backend::folders`), then the folders
  below it that hold files, indented, then folders outside every database
  folder; when the server's folders cannot be read, the menu says why and
  lists the files' folders. A folder shows its files and those of its
  subfolders (inf). Each
  file shows its status icon, an encryption lock, its name without
  `.fmp12`, and its client count; selecting it filters the clients and
  turns the Databases column into Account Name. Sorting is by Filename,
  Status (open, then transitions, paused, closed), Clients (most first)
  or Folder (inf for the last three); search matches the name, ignoring
  case, when Enter is pressed.
- **File menus.** A file's menu offers Open (when closed), Pause (when
  open) or Resume (when paused) and Close (when open or paused); the
  folder row's menu offers Open All, Pause All, Resume All and Close All
  for the files of the folder filter that the action applies to (inf).
  FileMaker Server's other items (Download, Remove, Verify, Flush, Clear
  Encryption Password, Show Extended Privileges) are shown disabled, as
  not available.
- **Clients.** Session Identifier (the Login's session name, else "Not
  signed in"), Databases (a count, the names in its tooltip), Host Name /
  IP Address (the address, with the computer's name in the tooltip),
  Sign-in (relative, absolute in the tooltip) and Client (FileMaker
  Server's tag for the `appType`, such as Pro or Go, with the version in
  the tooltip). With no file selected and a folder filter, the list shows
  the clients of that folder's files (inf). Search matches the session
  name, computer name, address, version, file names and account names;
  sorting is by Session Identifier, Client or Sign-in (oldest first) (inf).
- **Dialogs** (fields from FileMaker Server's string table, re 4.2). Send
  Message takes a required message; Disconnect Client and Close Database
  (or Close All Databases) take an optional message and a Delay Time in
  minutes, 0 to 60. A message is at most 200 characters, counted in UTF-16
  units as the browser's `maxlength` counts them, without control
  characters other than line breaks and tabs; the browser's CR LF line
  breaks become LF (inf). A Disconnect with neither a message nor a delay
  disconnects at once; with either it gives the backend a notice, whose
  grace time is the delay, or 90 s, the Admin API's default, when only a
  message is given (inf). Close always gives one. The server checks every
  field again; the script only keeps OK disabled until the fields are
  valid.
- **Outcomes.** Several clients or files are acted on one by one. The
  outcome names what succeeded and each failure, up to three; when all
  fail the same way, as when the server lacks the action, it is that
  error.

**Configuration views.** Points marked (inf) are OpenMaker's choices
where FileMaker Server's behavior is not observed or does not apply.

- **Folders.** OpenMaker Server hosts the files named on its command line,
  wherever they are, and those it finds in its database folders at
  startup (docs/development.md, "Hosting a file and opening it"), so
  Database Folders lists the database folders it looked in, whether or
  not they hold a hosted file, and the folders of the files named
  elsewhere, in path order. The Databases filter uses the same resolved
  folder paths, so a file opened through a directory symlink appears
  under its reported database folder. Labels read
  "Database Folder Path" for one, "Database Folder 1 Path" and on for
  several (inf), at most 100, then "Showing n of m". There is no Secure
  Database Folder Path. A path is shown as FileMaker writes a folder
  (re 4.5): `filelinux:/srv/Databases/` on Linux, `filemac:/<volume>/...`
  on macOS (the startup disk by its name under `/Volumes`),
  `filewin:/C:/...` or `filewin://computer/share/...` on Windows, always
  ending with `/`. Copy puts the path on the clipboard and then reads
  "Copied" for a moment (inf); where the browser does not let the page
  write to the clipboard, or without the script, there is no Copy. A path
  that is not valid Unicode (bytes that are not UTF-8, or an unpaired
  surrogate on Windows) cannot be written exactly: it is shown with
  replacement characters, says so, and has no Copy, which would name
  another folder (inf). The backup folder has Copy rather than FileMaker Server's Change (inf);
  without one, the row says that the server makes no backups.
- **SSL Certificate.** The leaf of the chain the server presents. Status
  is "Certificate installed", or "Self-signed certificate installed" with
  a help popover on what clients do with one (inf). Expires is the time in
  UTC as FileMaker Server writes it, with "Expires in n days" ("Expires
  today" in its last day) beneath, in the warning color within 30 days
  (inf: FileMaker Server's own alert warns 30 days ahead). An expired
  certificate's row reads "Expired On" in red (inf: the string exists, its
  use is not observed); a certificate not yet valid says "Not valid until"
  under Status. Certificate Details lists Country, State/Province,
  Locality and Organization when the subject has them, and Name. A chain
  that cannot be read says so.
- **Schedules.** The grid is FileMaker Server's: equal columns after the
  checkboxes, cells cut short with an ellipsis and their full text in a
  tooltip. Database is the backend's text for the target, `file:<name>`
  for a script schedule as in FileMaker Server; N/A without one. User Name
  is the account, or "Guest" for a script schedule without one (inf).
  Scheduled Time is the next run, or "Disabled". A schedule that has not
  run has empty Last Run and Last Status (inf); a run's detail (why it
  failed) is a gray line under its status (inf). Run Now runs each checked
  schedule once, at most 500 at a time, disabled ones too (inf), and says
  which started and which could not. The grid refreshes every 5 seconds,
  keeping the checked boxes.

## Administration in om-server

`ServerHandle` keeps what the console's live views need. Points marked (inf)
are OpenMaker's decisions rather than measured FileMaker behavior.

**Connection registry.** One entry per connection that has a session, by its
connection id. An entry holds:

- the client's address, the connection's TCP peer;
- from `Connect`'s machine info (re/network-protocol.md 5.2): `machineName`,
  `versionString` (for example `Pro 26.0.3`) and the `appType` byte;
- when `Connect` started the session;
- the session name of the client's latest accepted `Login`, which FileMaker
  Server shows as "Session Identifier";
- the files the connection has open, each with the account logged in to it.

The connection keeps its entry current:

- `Connect` adds it. A `Connect` on a connection that has a session ends that
  session first, so its entry is replaced.
- Open (op78) adds the file without an account; opening it again clears the
  account, as the connection does.
- An accepted Login (op80) sets the file's account and the session name. A
  refused one changes nothing.
- Logout (op81) clears the file's account and leaves it open, as the
  connection does.
- An op62 Unlock of the file's root (the notify lock, transaction 0,
  attention `0x2000`) closes the file, as FileMaker Pro closes one
  (re/write-path.md 4.3): what the session holds in it (locks, its
  notification registration, waiting notifications, staged records,
  transfers) is released, and the session goes on. A root unlock in
  another form changes nothing. A transfer of the file that the client
  continues after this, or after Logout, fails with a CORBA system
  exception, as one dropped at a limit does; the session's transfers on
  its other files go on.
- The end of the session removes it: `Disconnect`, the client closing its
  connection, the administrator's disconnect, or a panic while the server
  answers one of its requests. Its locks, notification registrations,
  waiting notifications and script callback results are released first.
  After a panic the client gets no reply: its connection is closed, as when
  the client leaves, and the panic is logged at error level with the
  client's address and the panic's message (inf).

Perform Script on Server sessions are not clients (inf); the running scripts
list shows them.

**Snapshots.** `ServerHandle::overview`, `files` and `clients`:

- The overview: name, version string, server id, start time (when
  `Server::bind` loaded the files), the RCFMP12 listen address, the number
  of hosted files and how many of them are served (open, paused or
  closing).
- Each hosted file: id, name, absolute path, folder, size on disk, status
  (`FileStatus`: FileMaker Server's `NORMAL`, `CLOSING`, `CLOSED`,
  `OPENING`, `PAUSED`, re 7), the number of connected clients that have it
  open, and why it is closed when it failed to open again. A file that is
  served but takes no more changes, because a save of it renamed its new
  copy over the file and could not flush the folder, gives that reason in
  the same field (`FileInfo::error`) with its status unchanged; closing it
  and opening it again clears it (docs/server-storage.md 5, "Failed
  saves"). The Admin API's and the console's database lists do not show
  the field. A file opened with something at its save's copy name
  (`<file>.omsave`) is served, and an alert names the entry, which
  nothing removes: each change fails at its save, taken back, until the
  administrator removes it, and then the next one is saved
  (docs/server-storage.md 5, "A leftover copy"). Every hosted file is
  opened at startup; the administrator closes, opens, pauses and resumes
  it (below).
- Each client as its entry holds it, oldest session first, and whether it is
  being disconnected (at once, or with notice during its grace time;
  FileMaker Server's Admin API lists such a client as `NORMAL`, re 9). A
  client disconnected with notice that has closed its files is not listed,
  as FileMaker Server lists it no more.

A snapshot takes only the registry's lock, and sizes come from the file
system, so it never waits for a file that a request is using. The `appType`
byte is passed on as it is; the console maps FileMaker's application types
(re/calc-functions.md 6) to the client families of re 7.

**Disconnect.** `ServerHandle::disconnect_client` marks the connection and
returns at once. The mark is checked at every read of the client's socket,
below TLS. Once it is set, the read fails, so the connection stops within one
read: at the latest at the 100 ms read timeout it uses to send back-channel
calls. A client that goes on sending cannot hold this off, whether it sends
between requests, inside a fragmented GIOP message (empty fragments add
nothing and never end it) or inside a TLS record. Writes are not checked.

- A request it is answering is finished first, also a Perform Script on Server
  that waits for its script. What arrives after the mark is not answered.
- The session then ends as when the client leaves, and the TLS connection is
  closed with close_notify. Until then the client is listed as being
  disconnected.
- Each disconnect is logged at info level with the client's address and
  session id, and in `Access.log`
  ([The event log and alerts](#the-event-log-and-alerts)).

**Disconnect with notice.** `ServerHandle::disconnect_client_with_notice(id,
&Notice { message, grace })` disconnects the way FileMaker
Server does for `DELETE /clients/{id}` with `messageText` and `graceTime`
(re 9, re/network-protocol.md 6.5). The message has 0 to 200 characters
and the grace time is at most 3600 s (90 s by default); otherwise, and for
an unknown client, a client already being disconnected or one not taking
messages, it fails with a `DisconnectError`. It returns at once, and the
client's connection takes each step when it is due: after each message the
client sends, whatever it is, or within its 100 ms read poll when the
client is quiet, so a client that never pauses cannot hold it off:

1. The client is sent the request to close its files (`0x0850`, its
   countdown 30), naming the file it has open with the lowest id (inf), as
   a message is sent. FileMaker Pro shows "Close File" and closes its
   files 30 s later, whatever the grace time.
2. Each file the client closes is closed in its session (op62, above).
   Once it has closed them all it is not listed.
3. When the grace time ends, a client with a file still open is sent the
   forced notice (`0x0870`, the same text), on which FileMaker Pro logs
   out and disconnects at once.
4. `ConnectionLimits::logout_wait` (30 s) later, a client that still has a
   file open is disconnected at once, as above. One that has closed its
   files but not left is called `Disconnect` on its callback object and
   its session ends; its connection stays open for a new session. This is
   what FileMaker Server did 90 s after a disconnect with a 60 s grace time
   (one observation: the 30 s rule is inferred).

A grace time of 0 sends the forced notice at once, a client with no file
open is called `Disconnect` at once, and a disconnect with notice is never
repeated (all inf: FileMaker Server's are unmeasured, re 10). A later
`disconnect_client` ends one under way at once. A grace time shorter than
FileMaker Pro's 30 s countdown ends before the client closes its files,
so it gets the forced notice. OpenMaker's own client closes a file the same
way (`Session::close_file`), and ends its session when the host calls
`Disconnect`.

**Closing and opening a file.** `ServerHandle::close_file(id, &Notice {
message, grace })` closes a file as FileMaker Server does for `PATCH
/databases/{id}` with `CLOSED` (re 9, re/network-protocol.md 6.5). Only an
open file is closed; the message, grace time and unknown files are checked
as for a disconnect, and a file in another state fails with
`FileError::NotAllowed` (the Admin API's 1709). The file is `CLOSING`, and
an Open of it is refused with status 4 as for a file the server does not
host (inf). Each connection that has the file open takes the steps for its
client within its read poll:

1. The request to close the file (`0x0040`, its countdown 30), naming it by
   the id the client registered it with in op63; a client that never
   registered it is named the host's id with `0x0840` (inf).
2. The same request halfway through the grace time: FileMaker Server
   repeated it 45 s into its default 90 (the halfway rule is inferred).
3. The forced notice (`0x0870`) when the grace time ends.
4. `ConnectionLimits::logout_wait` later, a client that still has the file
   open is disconnected at once.

A client closes the file with op62, or leaves. When the grace time ends,
the file's Perform Script on Server scripts and schedule runs are aborted,
and so is one that starts later, admitted just before the close began.
The file is `CLOSED` once nothing holds it: no session has it open, and no
script, schedule run or request uses its store. Until then it stays
`CLOSING`: new sessions are refused (above), a new Perform Script on Server
is answered as for a file not open (100, inf), and a schedule fails as for
a file the server does not host (802). Clients, scripts and schedules are
given the file under its own lock, the one that finishes the close, so
once the file is `CLOSED` nothing holds its old store: an old save can
never land on a file opened again. A grace time of 0 sends the forced
notice at once (inf).

`ServerHandle::open_file(id)` opens a closed file again: `OPENING` while it
loads from disk, with the checks the server makes at startup (the
administrator may have replaced it), then `NORMAL`. A file still closing
cannot be opened (`FileError::NotAllowed`). One that fails to load stays
`CLOSED` with the reason, in `FileInfo::error` and in `FileError::Load`.

**Pausing a file.** `ServerHandle::pause_file(id)` and `resume_file(id)`
(the Admin API's `PAUSED` and `RESUMED`). Only an open file is paused and
only a paused one resumed. While a file is paused, each request a client
makes on it (an Open naming it included) is held unanswered, as FileMaker
Server holds one (re 9). The connection goes on reading: it answers the
client's Tickles and its requests on other files at once (6.1), and a
request on a file with a request already held waits behind it, so each
file's requests keep their order. Within a read poll of Resume the held
requests are answered in the order they came. Clients are sent nothing. A
request being answered when the file is paused finishes first. One
connection holds at most 64 requests of 8 MiB in all; one more is refused
with the CORBA system exception `IMP_LIMIT` (inf). A held request is
dropped when its session ends (its client leaves or is disconnected): it
is never answered. A server that shuts down leaves its connections open
(`ServerHandle::shutdown`), so held requests wait for Resume, or end with
the process.

**Send Message.** `ServerHandle::send_message(id, text)` sends client `id`
a message from the administrator, which FileMaker Pro shows as "Message
from “<server name>”" (re 9). It checks the Admin API's rules and fails
with a `MessageError` the console can show:

- the text has 1 to 200 characters (`MAX_MESSAGE_CHARS`, counted as
  Unicode characters) and is not only white space (`EmptyText`,
  `TextTooLong`);
- no connected client has that id (`UnknownClient`, the Admin API's 404);
- the client is being disconnected (`Disconnecting`);
- the client has no file open, since a message names one (`NoOpenFile`,
  inf);
- 16 messages already wait for the client (`Busy`, inf): one that answers
  the LocateRequest below takes each within a read poll.

A client with several files open gets one message, naming the file with
the lowest id (inf): FileMaker Pro's dialog names the server, not the
file, and what FileMaker Server sends such a client is unknown (re 10).

The message is the `Notify` call of re/network-protocol.md 6.5: code
`0x0810`, the host's id for the file, the text, the server's name, 0. It is
queued for the client's connection, whose thread alone writes to the
client: within its next read poll (about 100 ms) it sends a GIOP
LocateRequest for the client's callback object, once per session, and
each waiting message in a `Notify` call of its own once the client
answers OBJECT_HERE. Calls take odd request ids from 3, counting across
the connection's sessions, shared with Perform Script on Server's
callback results. The client's answers are read like its requests and
only logged; nothing waits for them, so `Ok` means the message was
queued. A client that answers the LocateRequest otherwise has no callback
object: its waiting messages are dropped and logged. The end of a session
drops what still waits for it. Each message is logged at info level with
the client's address and session id, without its text (inf).

OpenMaker's own client (`om-session`) answers each call at once and keeps
the administrator's messages apart from change notifications
(`Session::take_admin_messages`). The desktop reads the host's calls each
second while idle and shows Send Message as FileMaker Pro does
(desktop-client.md, "Messages from the host"); it does not act on the
requests to close files or the forced disconnect yet
([#156](https://github.com/OpenMaker-Dev/openmaker/issues/156)).

**Block New Users.** `ServerHandle::set_block_new_users(bool)` and
`block_new_users()`. While it is on, an Open (op78) is refused with
FileMaker's error 1638, "The host is not allowing new connections", with
the reply FileMaker Server sends (re/network-protocol.md 5.6.4), whatever
file it names (inf). `Connect` still succeeds and nothing is sent to the
clients connected. A session that already has a file open is not a new
user and may open more (inf: FileMaker Server's own description says
current sessions are not affected, but whether one can open another file
is not measured, re 10). FileMaker Pro opens a solution's related files in
the session it has (re/network-protocol.md 5.5), so they still open.
OpenMaker's desktop opens a hosted file's related files in its session too
([#229](https://github.com/OpenMaker-Dev/openmaker/issues/229)), so by the rule above they open as well. Turning it on or off is
logged.
It starts off.

## The event log and alerts

`om-server` records what happens in FileMaker Server's log format
([re/server-logs.md](re/server-logs.md)), so the Log Viewer shows the same
columns and tools that read FileMaker Server's logs read OpenMaker
Server's. Code: `crates/om-server/src/eventlog.rs` and `log_folder.rs`.
Points marked (inf) are OpenMaker's decisions.

**Files.**

- `om-server --log-folder <dir>` names the folder. By default it is `Logs`
  in the state folder that also keeps the certificate (`--state-dir`, else
  systemd's `$STATE_DIRECTORY`, else the user's state folder;
  [development.md](development.md#the-servers-certificate)). Without
  either, the server warns and writes no log. The library's
  `Server::with_event_log` does the same.
- The folder is opened once, and the logs are made, opened, renamed,
  listed and read relative to it, never through a symbolic link (on Unix
  with `openat` and `O_NOFOLLOW`). A missing folder is made with mode
  0700, and a new log with 0600: the lines name accounts and client
  addresses (inf).
- On Unix, an existing folder must belong to the server's user, and no
  other user may write in it. A folder that does not pass, a folder that is
  a link, or one that cannot be made stops the server at its start,
  saying why: whoever can write in it could swap a log for a link (inf, as
  for the certificate's folder). Links in the path above the folder are
  the administrator's. The folder's path is taken without trailing
  separators and `.` components, so `linked/` and `linked/.` are the link
  `linked`.
- A log must be a regular file of the server's user with no other link:
  a link, a FIFO or a second name of another file is refused, not written
  or read. A log already open that a rotation has since unlinked is still
  read. A log that other users may read or write is made 0600, the active
  logs when they are opened and the `-old` files when the folder is, and
  the process log says so (inf). Elsewhere than on Unix,
  the folder's inherited permissions apply, and links are refused by
  looking at each name before it is opened.
- `Event.log` takes the server, its files and its schedules; `Access.log`
  takes clients, as FileMaker Server splits them (re/server-logs.md 3).
- A log that holds 40 MiB is renamed to `Event-old.log` or
  `Access-old.log`, replacing the previous one, and a new one is started;
  the renamed file ends with message 226 and the new one starts with 228,
  as FileMaker Server's do. So each log takes at most about 80 MiB. A log
  that cannot be renamed takes no more lines until it can (inf).

**Lines.** Timestamp in the server's time zone with milliseconds and
offset, level, number, host and message, separated by tabs
(re/server-logs.md 2). The host is the first label of the machine's host
name (inf: on the lab server it is also the server's name). Where
FileMaker Server has a message for the event, the line has its number,
level and text; OpenMaker's own messages have the number 0, which no
FileMaker message has (inf). Each name or error text in a message is cut
at 256 characters and its control characters escaped (`\t`, `\n`), so that
nothing a client or a file names can add a field or a line (inf).

| Event | Line |
|---|---|
| The server starts accepting connections | 486 `Database Server started.` |
| `ServerHandle::shutdown` (on Unix, `om-server` at SIGTERM or SIGINT) | 488 `Stopping Database Server...` |
| `ServerHandle::close_event_log`, after the stop and the connections' end | 168 `Database "<name>" closed.` for each file still served (open, paused or closing; one the administrator closed is not), then 490 `Database Server stopped.` |
| Each hosted file at startup | 418 `Opening database "<name>"...`, then 184 `Opened database "<name>".`, or 18 `... read only.` for a file served read-only |
| A hosted file that cannot be opened | 250 (not found), 282 (permission denied), 240 (not a FileMaker file), else OpenMaker's own `Database "<name>" could not be opened: <error>.` at level Error; and an alert |
| A failed save | OpenMaker's own, level Error: `Saving database "<name>" failed: <error>.` and what became of the changes (kept in memory, kept unsaved because the file cannot hold them, or the commit taken back); and an alert |
| Events dropped because the log could not keep up | OpenMaker's own warning with their count |

`Access.log`, for each client session:

| Event | Line |
|---|---|
| The session's first accepted Login | 638 `Client "<session name>" opening a connection from "<machine> (<address>)" using "<version> [fmapp]".`, then 94 |
| Each accepted Login | 94 `Client "<client>" opening database "<name>" as "<account>".` |
| Logout, the client closing the file (op62), or the file opened or logged in to again | 98 `Client "<client>" closing database "<name>" as "<account>".` |
| The session ends | 98 for each file still logged in to, then 22 `Client "<client>" closing a connection.` when the client left or the administrator disconnected it; else OpenMaker's own line saying why: at level Information, the server closed its connection (`close_connections`, which `om-server` calls when it stops; a client that leaves on its own after `shutdown` gets 22); at level Warning, no request for the idle limit, its connection failed (with the error), too many change notifications waited for it; at level Error, the server failed answering its request (a panic) |
| A Login refused after its password was checked | 661 `SECURITY: Client "<client>" authentication failed on database "<name>" using "<account>".` (the account name as given; never the password) |
| That refusal used up the account's allowance (`login_limits`) | 1049 `SECURITY: Client "<client>" temporarily locked account ...` |
| That refusal used up the address's allowance | OpenMaker's own, level Error |
| `ServerHandle::disconnect_client` | OpenMaker's own, level Warning: `Client "<client>" disconnected by the administrator.` |

`<client>` is `<session name> (<machine>) [<address>]`, the session name
of the session's first accepted Login (or, for a refused Login, the one it
gave; before any, the machine's name), as FileMaker Server names clients
(re/server-logs.md 3). A session keeps that name in every line, the
administrator's disconnect included, even when a later Login gives another
session name (which the client list shows) (inf). Apart from refused Logins, the server writes
nothing for a session until its first accepted Login, as FileMaker Server
appears to (inf from the timestamps of the lab server's lines), so a
session that never logs in leaves only its refused Logins and what the
administrator did to it.

`Event.log`, for schedules, scripts and the administrator:

| Event | Line |
|---|---|
| A schedule's run starts | 148 `Schedule "<name>" running.` |
| Its script starts | 689 `Schedule "<name>" has started FileMaker script "<script>".` |
| The run ends | 150 `Schedule "<name>" completed.`, or 644 `... completed; last scripting error (<n>).`; then 126 `Schedule "<name>" scheduled for MM/DD/YYYY hh:mm AM.` when it runs again on its own |
| The run passes its time limit and goes on | 690, level Warning |
| The run is aborted | 152 (by the administrator), 691 (at its own time limit, or the server stopping), 732 (its account cannot log in); else OpenMaker's own with the reason and error, level Error (the file is not hosted, its accounts could not be read (802), the server's limits, the server refused or failed to run it) |
| A run was due while the last one ran | OpenMaker's own, level Warning: `... was skipped: its last run has not ended.` |
| `ServerHandle::run_schedule`, `enable_schedule` | OpenMaker's own: `Schedule "<name>" run now by the administrator.`, `... enabled by` / `disabled by the administrator.` |
| A Perform Script on Server script is aborted (by the administrator, its file closing, the server stopping, or the server's step or time limit) | OpenMaker's own, level Warning, with the script, file, account, reason and error |

Where FileMaker Server's message names the administrator who acted (146,
675, 736, 742), OpenMaker writes its own: `ServerHandle` does not know who
the administrator is.

Names of databases are given without `.fmp12`, as FileMaker Server gives
them.

**Writing.** An event is formatted where it happens and handed to a
thread that writes the log, through a queue of 4096 lines (inf). Recording
never waits for the disk and never fails what recorded it: when the queue
is full the line is dropped and counted, and the writer logs the count
once it catches up. A failed write is reported once in the process log
(`env_logger`), and again only after a write worked in between. Lines
recorded before the folder is known (the files opened while the server
binds) are written first. The log stays open after
`ServerHandle::shutdown`, which leaves open connections serving, so what
they do is still recorded; `ServerHandle::close_event_log` writes its last
lines and closes it, giving the writer up to its timeout to write what is
queued. Lines recorded after it are not written.

**Startup with a file that cannot be opened.** The file is not hosted, its
id stays unused, and the server serves the others, as FileMaker Server
does. It gets no slot, so it is not in `ServerHandle::files` and
`open_file` does not know it (`FileError::UnknownFile`). When none of the
files opens the server does not start (inf: a server with nothing to host
is a mistake to report at once).

**Alerts.** `ServerHandle::alerts` lists them newest first, each with an
id, time, level and the event's message; `acknowledge_alert(id)` removes
one. A hosted file that could not be opened at startup or saved raises
one. An alert not yet acknowledged with the same message is replaced, so a
condition that goes on is one alert, at the top; at most 100 are kept, the
oldest going first (inf). Alerts live in memory only.

**Reading for the Log Viewer.** `ServerHandle::log_files` lists the four
files that exist, with size and modification time.
`ServerHandle::read_log(name, before, limit)` reads one of them by its
exact name (never a path) from disk: up to `limit` lines, at most 1000,
newest first, ending before the byte offset `before` (the end of the file
when `None`). Each line comes as its fields, as written, and its time
parsed; a line that is not five fields is whole in the message. The page's
`next` is the offset to give for the next, older page, `None` at the start
of the file. A last line still being written is left for a later page.
Offsets refer to the file as it is: after the log is renamed, a viewer
starts again from the end.

One call reads at most 4 MiB of the file, so a page may hold fewer lines
than asked and still have a `next` (inf). A line keeps its first 16 KiB;
the server's own lines are far shorter. A longer line, or one a page's 4
MiB end inside, is marked `cut`. A line longer than 4 MiB comes in parts,
newest part first, each page going on where the last stopped: a page's
`next` is then inside the line, and the next page starts with the rest of
it, cut too. A last line still being written waits for a later page, unless
it is longer than 4 MiB; then it comes in cut parts like any other.

**Stopping.** On Unix, `om-server` stops at the first SIGTERM or SIGINT as
`ServerHandle::shutdown` does, on a thread of its own: it stops accepting
connections and the schedules, aborts running scripts and gives them 10 s
to revert (inf), so that the stop is in the event log. It then closes the
clients' connections, so that their sessions end and are recorded,
waits up to 5 s for them, and closes the event log
(`close_event_log`, 5 s) before it exits. A second signal, also during
these steps, ends the process at once.

## Backups

OpenMaker Server writes backups the way FileMaker Server does
([re/backups.md](re/backups.md)), so an administrator's restore habits and
tools carry over. Points marked (inf) are OpenMaker's decisions. Code:
`crates/om-server/src/backups/`.

**Options.**

- `--backup-folder <dir>` is the default backup folder (FileMaker's Default
  Backup Folder Path). Without it, backups go to `Backups` in the state
  folder (`--state-dir`, the folder that keeps the server's certificate) (inf:
  FileMaker's is `Data/Backups` under its install root, and OpenMaker Server
  has none). The server makes a missing backup folder owner-only (0700),
  since it holds whole databases (inf).
- A backup folder holds only backups (inf). The server refuses to start
  when the backup folder, or a schedule's `target`, holds a hosted file or
  its container data, or lies in a hosted file's `RC_Data_FMS` folder.
  Retention deletes old backups from it, and a hosted file inside one would
  go with it. A folder beside the hosted files, or below the folder that
  holds them, is fine. Should a hosted file come to lie in a backup anyway
  (a link moved after startup), neither retention nor Delete removes that
  backup.
- Backup schedules are sections of the `--schedules` file with `type =
  backup` (syntax in `crates/om-server/src/schedules/file.rs`):

  ```text
  [Nightly]
  type = backup
  # all (the default), folder <folder> or file <name>
  back up = all
  # 1 to 99 backups that are not preserved; default 1
  keep = 7
  # Default: the --backup-folder.
  target = /srv/openmaker/Backups
  repeat = daily
  at = 00:00
  ```

  The timing keys are a script schedule's. `back up = folder <folder>` takes
  the hosted files in that folder or a folder below it (inf). The name begins
  each backup's folder name, so it must be a valid folder name on Linux,
  macOS and Windows, of at most 100 characters.
- There is no default schedule (inf,
  [#159](https://github.com/OpenMaker-Dev/openmaker/issues/159)).
  FileMaker Server ships one, "FMS", daily at midnight, keeping 7 (re 6).
  The section above recreates it with `[FMS]`.

**A run.**

- **The folder.** It is FileMaker's: `<schedule>_YYYY-MM-DD_HHMM` in the
  target folder, named for the run's start in the server's local time. It
  holds `backup.config` (`Label`, `TimeStamp` in UTC, `IsPreserved` and a
  random `ID`, in FileMaker's form) and `Databases/`. A scheduled run's label
  is `Auto-backup`, and Back Up Now's and Run Now's is `On-demand` (inf for
  Run Now).
- **`Databases/`.** OpenMaker Server's hosted files need not lie in one
  database folder (files named on its command line are hosted wherever
  they are), so `Databases/` holds the files as they lie below the deepest
  folder that holds every hosted file. Copying it back restores them
  (inf).
- **Container data.** A file's `RC_Data_FMS/<file>/` folder of externally
  stored container data is copied beside its copy, where FileMaker Server
  puts it (re 2.1, read in its code). Every container file is copied on every
  run, while FileMaker links those unchanged since the last backup (re 2.3,
  [#178](https://github.com/OpenMaker-Dev/openmaker/issues/178)).
- **Reading container data.** The run walks the folder through open folder
  handles, opening each name below the one above it without following a
  link (`O_NOFOLLOW`), so a folder swapped for a link during the copy fails
  it rather than lead it elsewhere (inf).
  - Links and special files inside are left out and logged.
  - A missing `RC_Data_FMS/<file>/` means the file has none.
  - If that folder, or `RC_Data_FMS`, is a link or cannot be read, the run
    fails before the backup is published. No older backup is deleted, and
    no backup lacks data that exists.
- **Consistency.** OpenMaker Server writes each change as a complete new file
  renamed over the old one, never in place. So a run opens each file while it
  holds the file's store for reading, and lets go once the file is open. The
  open descriptor keeps reading that saved version whatever later saves
  rename over it, so each copy is one saved version of the file. The copy is
  checked to be as long as the file was when it was opened. Where the
  backup folder's file system makes unnamed files (Linux), it is written
  without a name and linked to its name once complete; elsewhere, and where
  the file system has no hard links, it is written under its name, as
  before ([server-storage.md](server-storage.md) 5, "Exports"). A folder
  flush that fails after a copy is named is logged, and the run's flush of
  every folder before the backup is named decides, as before. A copy is
  not read back to verify it, as before; FileMaker's Verify option would
  add that (below).
- **What clients see.** The store is held for one `open` call, never while
  the file is copied. Clients see nothing: a write waits at most for that
  call, and a save in progress delays the backup, not the client.
  FileMaker Server instead pauses each file at the end of its copy (re 3).
- **Unsaved changes.** Changes a failed save left in memory are not in the
  copy, and the run logs a warning.
- **Unchanged files.** A file not saved since the schedule's last complete
  backup made in this process becomes a hard link to that backup's copy, as
  in FileMaker Server (re 2.3), while that copy is still the file the run
  made. After a restart, a schedule's first run copies every file.
  FileMaker decides instead by the newest earlier backup on disk: same size,
  no unwritten changes and a modification time not later than the copy's (re
  2.3). OpenMaker counts its own saves, since a save replaces the file whole
  (inf).
- **Completion.** A run writes `<name>_InProgress` and flushes every file and
  folder. It then renames the folder to `<name>`, so a crash never leaves a
  backup that looks finished. A run that fails or is cancelled removes its
  folder (FileMaker's Keep Cancelled Backups off).
- **Unfinished backups.** While a run writes, its folder holds
  `.openmaker-in-progress`, OpenMaker's own marker, which names the process
  (`pid=` and its start time) and is removed once the backup is complete
  (inf).
  - The schedule's next run removes an `_InProgress` folder whose marker
    names this process or a process that no longer runs, such as one left by
    a crash.
  - A marker counts only when it is a regular file of at most 256 bytes
    holding exactly what the server writes.
  - The run leaves alone, and logs, a folder without such a marker, one
    another running process writes, and one hosted data lies in.
  - If the system has since given the crashed process's id to another
    process, the leftover stays until that process ends. This errs on the
    side of keeping data.
- **Flushing the target folder.** If the target folder cannot be flushed
  after the rename, the backup may not survive a crash. The run fails with
  the error, the backup stays where it is, no older backup is deleted, and
  the next run does not link to its copies (inf).
- **One minute, one backup.** A second backup of a schedule in the same
  minute fails with error 800 and leaves nothing (inf; FileMaker's behavior
  is unknown).
- **One at a time.** One run copies at a time across the server
  (FileMaker's Parallel Backup off). A run that comes due meanwhile waits for
  its turn.
- **Retention.** After a successful run, the schedule's oldest backups past
  its keep count are deleted, counting only those that are not preserved
  (inf). Only folders in the target folder with that schedule's name and a
  readable `backup.config` are deleted, never through a link, and never one
  that holds a hosted file or its container data. Files they share with
  newer backups by hard links stay with those backups.
- **Folder handles.** Every operation resolves its target folder once
  (the administrator's links followed) and opens it a name at a time from
  the root without following a link (inf). Listing, the final rename,
  reading markers and configs, and deleting all go through that handle,
  never through a link.
  - A folder is removed through the handle it was checked on: its contents
    first, then its name, which fails unless the name still holds an empty
    folder.
  - So a link changed on the configured path, or above it, during the
    operation cannot redirect a deletion.
  - Whether hosted data lies in a folder is decided by device and inode
    numbers. The run finds each hosted file's folder, its `RC_Data_FMS` and
    its `RC_Data_FMS/<file>` where the server reads them, links followed,
    and walks `..` up from each. A hosted file's folder is both the folder
    its configured path names and the folder of the file that path leads
    to, since the path may be a link to a file elsewhere.
  - A target that has come to lie in container data is not used: no backup
    is written there, and nothing in it is listed or deleted.
- **Errors.** A run's error is FileMaker's code (inf): 1 when cancelled, 802
  when no hosted file is chosen or one cannot be read, and 800, 809 (disk
  full) or 810 (read-only volume) when the backup cannot be written. The
  schedule's last run holds it, and the backup's folder name once it is
  complete.
- **The log.** Each run logs FileMaker's event lines where they hold (re 4):
  `Schedule "S" running.`, the start line naming the `_InProgress` folder,
  each file's start and completion or its hard link, `Deleted backup
  folder`, and `Schedule "S" completed.`. Failures and cancels use OpenMaker's
  own wording.

**ServerHandle.**

| Method | What it does |
|---|---|
| `schedules` | every schedule, backup schedules included, with its next and last run (`ScheduleTask::Backup` holds its files, folder and keep count) |
| `back_up_now(schedule)` | an on-demand backup with a backup schedule, also a disabled one; `run_schedule` does the same |
| `running_backups` | the runs under way, the one copying first, with files and bytes done; empty means "No Backup Running" |
| `cancel_backup` | stop the copying run within 8 MiB; it removes its folder and ends with error 1 |
| `backups` | the backups in the default folder and the schedules' folders, newest first: id, schedule, folder name, path, start time, label, preserved and size. Backups of removed schedules are listed; unfinished ones and folders without a `backup.config` are not |
| `preserve_backup(id, preserved)` | preserve a backup or stop preserving it |
| `set_backup_label(id, label)` | one line of at most 100 characters (inf) |
| `delete_backup(id)` | delete a preserved backup; the others go by their keep count |
| `backup_folder` | the default backup folder |

- Backups are named by the `ID` of their `backup.config`.
- The list is read from the folders each time, so a folder copied in or
  removed by hand is seen as it is.
- Preserved state and labels are written to `backup.config`, where FileMaker
  keeps them. A new file is renamed over the old, so a crash leaves one or
  the other.
- Shutdown cancels the runs under way.

## Limits

- **A first console.** Of the console's views only the Dashboard,
  Databases, Backups, Logs, General Settings, Folders, Schedules, SSL
  Certificate and Administrator are built, and the console shows neither
  the server's
  alerts nor its event log
  (`console::ServerBackend` answers no alerts and leaves the log files
  unsupported). "Server IP Address" shows the listen address, or, when the
  server listens on every address, the one the machine's routing table
  uses to reach other networks (inf). Schedule ids in the console are
  their place in the `--schedules` file, from 2, since FileMaker Server
  reserves 1 (inf). `console::ServerBackend` does not use the server's
  disconnect with notice, Send Message, Block New Users or Close, Open,
  Pause and Resume yet: it disconnects a client at once and answers the
  others as unsupported, so the Databases view refuses them when they are
  tried ("Actions the server lacks") and General Settings shows Block New
  Users as not available. It
  reports each file's state (Closing, Closed, Opening and Paused as well
  as Open) under FileMaker Server's names. Nor does it pass the backups on:
  the backups list, Back Up Now, preserve, label and delete are
  unsupported, while Run Now on a backup schedule starts its backup, as
  `ServerHandle::run_schedule` does for any schedule.
- **Configuration is read from the command line.** Hosted files, the
  certificate and the schedules are set when the server starts, so the
  console cannot add database folders, turn on Progressive Backup, change
  the backup folder, create a certificate signing request, request a
  Let's Encrypt certificate, import or delete certificate files, or
  create, duplicate, edit or delete a schedule. Run Now and Enabled are
  the schedule actions; whether an Enabled change outlasts a restart is
  the backend's (the schedules file keeps its own `enabled`). The
  Schedules grid has no column filters, Save or Load of its layout, or
  Column Settings.
- **Actions the server lacks.** The `Backend` does not say beforehand
  which actions it takes, so the Databases view offers Send Message, Open,
  Pause, Resume and Close by the files' and clients' states, and a server
  without one answers "OpenMaker Server does not do this yet." when it is
  tried.
- **Databases view.** Search applies when Enter is pressed, not as the
  administrator types. The view's state must fit in the 2,048 bytes of
  query a return address may carry: a search is cut to 480 bytes encoded,
  and a folder whose path takes more than 960 is listed but cannot be the
  filter. Extended privileges, Download, Remove, Verify,
  Flush and encryption passwords are not available. One action can carry
  about a thousand checked clients: a form is at most 16 KB.
- **Statistics** are read on Linux only, Volume Status on Unix systems.
- **Backups tab.** `om-server`'s `console::ServerBackend` does not pass
  the server's backups on yet, so the tab works only against a backend
  that has them, such as the mock. Cancel
  and Keep Cancelled Backups
  ([#167](https://github.com/OpenMaker-Dev/openmaker/issues/167)),
  Progressive Backup
  ([#160](https://github.com/OpenMaker-Dev/openmaker/issues/160)) and
  Parallel Backup
  ([#168](https://github.com/OpenMaker-Dev/openmaker/issues/168)) are not
  available. Only the backup status refreshes live; the lists and their
  counts change when the page is loaded again. An action returns to its
  view without the chosen schedule and filters.
- **Logs tab.** Interactive View, Download All Logs, column filters and Reset
  All Filters are not available
  ([#169](https://github.com/OpenMaker-Dev/openmaker/issues/169)), nor
  are the logging options of Log Settings. A download holds at most the
  newest 16 MiB of a log.
- **Admin API.** Of FileMaker Server's 112 paths, the ones in
  [Admin API](#admin-api) are served; the rest answer 404. `om-server`
  serves them with the console, from the same `console::ServerBackend`:
  what that leaves unsupported reads as missing (404, 1700) and is refused
  when changed (409, 1709), so Send Message, Block New Users, a disconnect
  with a message and a file's status change (`PATCH /databases`) are
  refused there too. No reply has been compared with FileMaker Server's
  own, so the inferred points there are unverified
  ([#163](https://github.com/OpenMaker-Dev/openmaker/issues/163)).
  #494 (merged as
  re 8.1, read in the console's server code, not compared with a live
  server) gives FileMaker Server's own answers for several of them: the
  sixth token, the lockout, missing and PKI credentials, undocumented and
  `null` fields, other methods, unsupported features, `appType` and client
  `status` values, and the fields a database status does not take,
  `CLOSED` without `messageText` included. Issues
  [#179](https://github.com/OpenMaker-Dev/openmaker/issues/179),
  [#180](https://github.com/OpenMaker-Dev/openmaker/issues/180) and
  [#181](https://github.com/OpenMaker-Dev/openmaker/issues/181) track the
  differences, which are not resolved: OpenMaker keeps its answers until
  they are. A schedule's reply lacks its script name, parameter and
  timing, and a backup schedule's what and where, which the backend's
  model does not have yet
  ([#162](https://github.com/OpenMaker-Dev/openmaker/issues/162)). PKI
  tokens are not accepted.
- **Pause holds client requests only.** Perform Script on Server scripts
  already running and schedules go on using a paused file, so copying a
  paused file can catch a save in progress
  ([#166](https://github.com/OpenMaker-Dev/openmaker/issues/166)).
- **File operations not built.** Download, Remove, Verify, Flush and
  Clear Encryption Password, and the files' extended privileges.
- **Block New Users is not kept.** It lives in memory and is off when the
  server starts; FileMaker Server's is one of its configuration settings
  (`/server/config/blocknewusers`, re 8).
- **Event log.** Script sessions (Perform Script on Server and schedules'
  runs) are not in `Access.log`, where FileMaker Server lists them as
  clients connecting "from FileMaker Script"; their scripts' errors other
  than aborts are not logged (FileMaker Server's `scriptEvent.log` form is
  unread). Lines that would name the administrator do not, logins refused
  unchecked past a limit (error 214) are not logged one by one, and the
  messages FileMaker Server writes at startup and stop beyond those above
  are unmeasured. Of the administrator's actions only Disconnect, Run Now
  and enabling or disabling a schedule are recorded: a disconnect with
  notice, Send Message, Block New Users and a file's Close, Open, Pause and
  Resume write no line of their own, and a session that a disconnect with
  notice ends gets 22, as one its client ended. `Stats.log` and
  `TopCallStats.log` are not written, and Log Settings has nothing to set:
  the size at which logs are renamed is fixed. A file that could not be
  opened is not in the Databases view.
- **Backups.** These are not implemented, and the console shows them as not
  available:
  - FileMaker's Verify, Clone and Clone Only options
    ([#158](https://github.com/OpenMaker-Dev/openmaker/issues/158)): a
    backup's copy is not verified by reading it back, which is what Verify
    would add; progressive backups
    ([#160](https://github.com/OpenMaker-Dev/openmaker/issues/160)),
    Parallel Backup and Keep Cancelled Backups. The schedules file refuses
    their keys.
  - The Admin API's `/schedules/backup` and backup settings.
  - Schedules are fixed at startup, so the console cannot create or edit one.
  - FileMaker's automatic preservation of the default schedule's on-demand
    backups, since there is no default schedule
    ([#159](https://github.com/OpenMaker-Dev/openmaker/issues/159)).
  - Hard links for unchanged container files: each run copies them all
    ([#178](https://github.com/OpenMaker-Dev/openmaker/issues/178)).
- **Backups compared with FileMaker.** The folder layout, `backup.config` and
  hard links for database files follow what the lab server wrote. Where
  container data goes follows FileMaker Server's code (re 2.1), not a live
  backup. Not compared with FileMaker Server: a keep count of 0, two runs in
  one minute, and whether preserved backups count toward the keep count. No
  copy has been opened in FileMaker Pro.
- **Shared copies.** A copy linked into several backups is one file. Opening
  a backup copy in place with FileMaker Pro changes it in every backup that
  links it, as on FileMaker Server: restore from a copy of the copy.
- **Hard links.** Where the backup folder's file system has no hard links
  (some network shares), every run copies every file.
- **Linux and macOS only.** Backups need calls that open a folder without
  following a link and work relative to it. On Windows every run fails with
  that reason, and no backup is listed or deleted
  ([#197](https://github.com/OpenMaker-Dev/openmaker/issues/197)).
- **Moves during a deletion.** A hosted file moved into a backup folder
  while that folder is being deleted can go with it. Nothing can rule that
  out.
- **Backups and the hosted files' states.** A backup copies the files that
  are open or paused when it starts, taken as a client's session takes
  them; one closing or closed is left out. It holds what it copies until
  it ends, so a file's close waits for a backup under way: the close does
  not cancel it (inf). Every file the server was given stays protected
  from deletion, closed or not, and so does what can be found of one that
  failed to open at startup; while such a file cannot be found, backups
  are still made but none is deleted (retention, Delete, or the cleanup of
  an earlier run's unfinished folder), since where its data lies is
  unknown. The backups' layout follows every hosted file, open or not.
- **Backups in the event log.** A backup schedule gets the scheduler's
  lines (run now, enabled or disabled, skipped, its next run, a run that
  could not start), but its runs write no line of their own: FileMaker
  Server's backup messages are not written.
