# FileMaker Server's log files

FileMaker Server records what happens in text files in its `Logs` folder.
The Admin Console's Log Viewer shows them ([admin-console.md](admin-console.md)
4.9). This document records their names, how they are rotated, the format of
a line, and the catalog of messages the lines come from. It covers
`Event.log` (server events), `Access.log` (client access) and
`scriptEvent.log` (script errors). OpenMaker Server's own
logs, written in this format, are described in
[../admin-console.md](../admin-console.md#the-event-log-and-alerts).

## Sources and provenance

- **The lab server's `Logs` folder, 2026-11-11, read only.** FileMaker
  Server 26.0.3 for Ubuntu. Over SSH: the folder's listing with sizes and
  times; the first and last 40 lines of `Event.log`, `Event-old.log`,
  `Access.log` and `Access-old.log`; the first 200 bytes of `Event.log` and
  `Access.log` and the last 120 of `Event.log` in octal. No other line was
  read and nothing was changed. The excerpts hold real names and stay in
  the git-ignored `re/work/server-logs/`. In the examples below, names are
  stand-ins: `server` for the server's 6-character host name, "Nightly" for
  a schedule, "Front Desk" for a user, `workstation` for a machine,
  192.0.2.x for addresses.
- **The test server's `Event.log`, 2026-11-25.** FileMaker Server 26.0.3
  on Ubuntu 24.04, a test server the maintainer set up for this work: a
  copy of a starter file in the Databases folder, owned by the server's
  user, opened with `fmsadmin open <id>` with mode 444 and with mode 664,
  and the lines each open wrote (events 18, 184, 654, 655, section 4;
  [#790](https://github.com/OpenMaker-Dev/openmaker/issues/790#issuecomment-7176757453)).
- **The test server's `scriptEvent.log`, 2026-11-26, read only.** FileMaker
  Server 26.0.3.309 for Ubuntu. The complete file had 24 event rows and one
  header. The rows include the errors from the read-only Perform Script on
  Server run in [#918](https://github.com/OpenMaker-Dev/openmaker/issues/918).
  The original file stays in local, git-ignored research data.
- **FileMaker Server's message table.** `lib/libFMSLib.so` of the 26.0.3.309
  package holds a table of 801 messages in `.data.rel.ro`, entries of 64
  bytes from 0xce3d10 to 0xcf0510 (section 4). It is data read from the
  package, not code; its strings are UTF-16LE. Every number seen in the
  lab server's lines has the same text in the table.

Observed facts are marked (obs); what follows from them without being seen
is marked (inf).

## 1. The Logs folder

FileMaker Server 26 on Linux keeps its logs in
`/opt/FileMaker/FileMaker Server/Logs/`. The lab server's folder held (obs):

| File | What it holds |
|---|---|
| `Event.log`, `Event-old.log` | server events: databases, schedules, administrators, the server itself |
| `Access.log`, `Access-old.log` | client access: connections opened and closed, databases opened and closed |
| `Stats.log` | server statistics |
| `TopCallStats.log`, `TopCallStats-old.log` | the slowest client calls |
| `scriptEvent.log`, `scriptEvent-old.log` | script events, unless "Script Log to Event Log" sends them to `Event.log` (admin-console.md 4.9) |
| `fmdapi.log`, `fmodata.log`, `fmshelper.log`, `fm_LLMOS_Debug.log` | other components |

The files belong to the server's user (`fmserver`, group `fmsadmin`). Most
had mode 0755 (obs).

**Rotation.** A log that reaches its maximum size is renamed to
`<name>-old.log`, replacing the previous one, and a new file is started, so
one previous file is kept (obs: one `-old` file each, and the messages
below). The last line of the renamed file and the first line of the new one
say so (obs):

```text
... Information  226  server  This log file reached maximum size, and was renamed. A new log file was created.
... Information  228  server  The previous log file reached maximum size, and was renamed to "Event-old.log". 
```

Message 228 ends with a space (obs, also in the table). The four `-old`
files were 41,943,106 to 41,946,501 bytes, just over 40 MiB (41,943,040)
(obs): the maximum size is 40 MiB, and a log is rotated when a write takes
it past that (inf). The lab server's `Event.log` (146,916,998 bytes) and
`Access.log` (42,940,004 bytes) were past 40 MiB and had not been rotated
for months; why is unknown.

## 2. An Event.log or Access.log line

One event per line (obs):

```text
2026-02-17 10:34:00.064 -0600<TAB>Information<TAB>148<TAB>server<TAB>Schedule "Nightly" running.
```

- **Fields.** Five fields separated by one tab each: timestamp, level,
  number, host, message (obs). The Log Viewer's columns are Timestamp,
  Level, Error, Host and Message, in that order (admin-console.md 4.9).
- **Timestamp.** `YYYY-MM-DD HH:MM:SS.mmm ±HHMM`: the server's local date
  and time to the millisecond, a space, and its offset from UTC (obs; the
  offset changed from -0500 to -0600 across the daylight-saving change).
  The Log Viewer shows it as `10/29/2026 18:04:00` (admin-console.md 4.9).
- **Level.** Only `Information` was seen. The message table has levels 1, 2
  and 3, and its messages 629, 630 and 631 read "Error", "Warning" and
  "Information" (obs), so the other levels are written `Error` and
  `Warning` (inf).
- **Number.** The message's number in the message table (obs: every number
  seen has the line's text in the table). The Log Viewer heads the column
  "Error", but it is not an error code: 148 above is information. A
  message reports an error code in its text, for example
  `... completed; last scripting error (802).`
- **Host.** The server's host name, the 6 characters of its first label
  (obs). The lab server's name in the console is the same text, so whether
  this is the host name or the server's name is not established.
- **Message.** The table's text with its `%1`, `%2`, ... replaced by the
  event's values (obs). Names are quoted with `"`.
- **Encoding and lines.** Each line ends with LF alone; there is no
  byte-order mark and no header line (obs). Text outside ASCII was not seen:
  UTF-8 is inferred.
- **Order.** Lines are appended as events happen and are not strictly in
  time order: the 228 line at the top of a new file was stamped 1 ms after
  the lines below it (obs).

## 3. Event.log and Access.log

Every client line seen was in `Access.log`, and every schedule,
administrator and rotation line in `Event.log` (obs). The excerpts of
`Event.log` show schedules starting a script session every minute without
any client line, while `Access.log`'s excerpts show such sessions'
connections. The message table gives each message a category (section 4):
the client messages are category 2. So category 2 goes to `Access.log` and
the others to `Event.log` (inf); 226 and 228 are written to each file as it
is rotated (obs).

A client's lines (obs, `Access.log`):

```text
... Information  638  server  Client "Front Desk" opening a connection from "workstation (192.0.2.7)" using "Pro 26.0.3 [fmapp]".
... Information  94   server  Client "Front Desk (workstation) [192.0.2.7]" opening database "Sample" as "Admin".
... Information  98   server  Client "Front Desk (workstation) [192.0.2.7]" closing database "Sample" as "Admin".
... Information  22   server  Client "Front Desk (workstation) [192.0.2.7]" closing a connection.
```

- The client is named by its user name, the session name FileMaker Pro
  sends in Login (network-protocol.md 5.4), then in later lines as
  `<user name> (<machine name>) [<address>]` (obs). 638 names the machine
  and address as `<machine name> (<address>)` and the client as its version
  string and the extended privilege it uses, `[fmapp]` for FileMaker Pro
  (obs).
- The database is named without its extension, and the account after
  `as` (obs).
- 638 and the first 94 were stamped in the same millisecond, though
  FileMaker Pro downloads part of the file between Connect and Login
  (network-protocol.md 5.4): both are written at the first accepted Login,
  and nothing at Connect (inf).
- When a client leaves with files open, a 98 for each file and a 22 are
  written in the same millisecond, in either order (obs).
- A server script's session is named `"<name> (FileMaker Script)"` and
  connects `from "FileMaker Script"` using `"Server <version> [fmapp]"`; a
  Data API client uses `"FileMaker Data API Engine <version> [fmrest]"`
  (obs).

A schedule's lines (obs, `Event.log`; one run of a script schedule that
runs every minute):

```text
... Information  148  server  Schedule "Nightly" running.
... Information  689  server  Schedule "Nightly" has started FileMaker script "Refresh".
... Information  150  server  Schedule "Nightly" completed.
... Information  126  server  Schedule "Nightly" scheduled for 02/17/2026 10:35 AM.
```

A run whose script ended with an error writes 644 instead of 150:
`Schedule "Nightly" completed; last scripting error (802).` (obs). 126 gives
the next run as `MM/DD/YYYY hh:mm AM` (obs).

The console's own sign-ins are logged as 518
`Administrator connected: "Admin [127.0.0.1]" (admin console).` and 520
`Administrator disconnected: ...` (obs).

## 4. The message table

Each entry of the table in `libFMSLib.so` is 64 bytes (obs):

| Offset | Size | Content |
|---|---|---|
| 0 | 8 | the message number in the low 16 bits; the top byte (bits 24-31) holds the level in its top two bits and a category in its low four |
| 8 | 8 | the level: 0, 1, 2 or 3, equal to the top byte's top two bits in all 801 entries |
| 16 | 48 | six pointers to the text: English, French, German, Spanish, Italian, Japanese |

- **Levels.** 1 Error, 2 Warning, 3 Information (inf from messages 629-631,
  section 2); 0 marks labels that are not log messages, such as `Stats.log`'s
  column names (obs: "Timestamp", "Pro Clients").
- **Categories** (inf from the messages in each): 1 databases, 2 clients, 3
  administrators and security, 4 settings, 5 directory service, 6 backups,
  7 schedules starting and changing, 8 schedules ending, 9 the server and
  its processes, and smaller groups 10-12.
- **Numbers.** 2 to 1300-odd, mostly even; no message is numbered 0 (obs).

The messages for events OpenMaker Server also has, and those around a
database's opening that it does not, as the table gives them (level: E
Error, W Warning, I Information). Seen in the lab server's lines: 22, 94,
98, 126, 148, 150, 226, 228, 518, 520, 638, 644, 689 (obs); in the test
server's: 18, 184, 654, 655 (obs, below); when FileMaker writes the others is
not observed.

| Number | Level | Category | Text |
|---|---|---|---|
| 18 | I | 1 | `Opened database "%1" read only.` |
| 22 | I | 2 | `Client "%1" closing a connection.` |
| 26 | W | 2 | `Client "%1" forced to close connection.` |
| 30 | W | 2 | `Client "%1" no longer responding; connection closed. (%2)` |
| 94 | I | 2 | `Client "%1" opening database "%2" as "%3".` |
| 98 | I | 2 | `Client "%1" closing database "%2" as "%3".` |
| 126 | I | 8 | `Schedule "%1" scheduled for %2.` |
| 140 | I | 1 | `Closing database "%1"...` |
| 146 | I | 7 | `Schedule "%1" run now by "%2".` |
| 148 | I | 7 | `Schedule "%1" running.` |
| 150 | I | 8 | `Schedule "%1" completed.` |
| 152 | I | 8 | `Schedule "%1" aborted; aborted by user.` |
| 168 | I | 1 | `Database "%1" closed.` |
| 184 | I | 1 | `Opened database "%1".` |
| 226 | I | 9 | `This log file reached maximum size, and was renamed. A new log file was created.` |
| 228 | I | 9 | `The previous log file reached maximum size, and was renamed to "%1". ` |
| 240 | E | 1 | `Database "%1" could not be opened: not a FileMaker Pro database.` |
| 250 | E | 1 | `Database "%1" could not be opened; file was not found.` |
| 282 | E | 1 | `Database "%1" could not be opened: Permission denied.` |
| 418 | I | 1 | `Opening database "%1"...` |
| 486 | I | 9 | `Database Server started.` |
| 488 | I | 9 | `Stopping Database Server...` |
| 490 | I | 9 | `Database Server stopped.` |
| 496 | W | 2 | `Client "%1" has been asked to disconnect. Message sent: %2` |
| 627 | E | 1 | `Unable to open database "%1" (%2)` |
| 654 | W | 1 (inf) | `Database "%1" cannot be shared; no account has the necessary extended privileges, and [fmapp] could not be added to the [Full Access] privilege set.` |
| 655 | I | 1 (inf) | `Database "%1" was made shareable by adding the [fmapp] extended privilege to the [Full Access] privilege set.` |
| 638 | I | 2 | `Client "%1" opening a connection from "%2" using "%3".` |
| 644 | I | 8 | `Schedule "%1" completed; last scripting error (%2).` |
| 661 | W | 2 | `SECURITY: Client "%1" authentication failed on database "%2" using "%3".` |
| 675 | I | 7 | `Schedule "%1" disabled by "%2".` |
| 689 | I | 7 | `Schedule "%1" has started FileMaker script "%2".` |
| 690 | W | 7 | `Schedule "%1" has exceeded its time limit.` |
| 691 | E | 8 | `Schedule "%1" aborted; time limit exceeded or server stopped.` |
| 732 | E | 7 | `Schedule "%1" aborted; invalid account name or password.` |
| 736 | I | 7 | `Schedule "%1" enabled by "%2".` |
| 742 | W | 2 | `Client "%1" has been asked to disconnect by "%2". Message sent: %3` |
| 1017 | E | 1 | `SECURITY: Database "%1" cannot be opened because it is insecure. Require Password-Protected Databases is enabled.` (local-files.md 3.2) |
| 1049 | E | 2 | `SECURITY: Client "%1" temporarily locked account "%3" on database "%2" because of too many failed attempts to log in.` |
| 1216 | W | 8 | `Schedule "%1" was skipped. Try again in 30 seconds.` |

When the test server wrote events 18, 184, 654 and 655, as it opened a
copy of a starter file from the command line (obs, 2026-11-25):

- **184** `Opened database "<name>".`: the file writable (mode 664). The
  folder does not change it: the same file in a folder the server's user
  may not write (another user's, mode 755) opened with 184 and status
  `Normal` as well, its bytes unchanged after open and close (obs,
  2026-11-26). A client's change to that file was not tried.
- **18** `Opened database "<name>" read only.`: the server's user may not
  write the file (mode 444). `fmsadmin list files -s` then shows it with
  status `Normal`, and its bytes stay unchanged (the same SHA-256).
- **655**, before 184: the file writable, and no account of it has the
  `fmapp` extended privilege; the server writes the privilege into the
  file's [Full Access] set as it opens it (its bytes change).
- **654**, before 18: the same file with mode 444; the server cannot write
  the privilege in, and opens the file read only and unshared (`fmapp` not
  among its enabled extended privileges). A mode-444 copy that already has
  it logs 18 alone and is shared.

## 5. Script errors

With script logging in its own file, `scriptEvent.log` starts with the
UTF-8 BOM (`ef bb bf`) and the header
`Timestamp<TAB>Error<TAB>Message<LF>` (obs). The header and every event
row end with LF alone. Each sampled row has three tab-separated fields
(obs):

| Field | Observed form |
|---|---|
| Timestamp | `YYYY-MM-DD HH:MM:SS.mmm ±HHMM`, as in Event.log |
| Error | Decimal FileMaker script error number |
| Message | `Schedule "<name>" scripting error (<error>) at "<file> : <script> : <line> : <step>".` |

The word `Schedule` also appears for client-invoked Perform Script on
Server (obs). In the read-only run, the affected steps at lines 3, 7 and 9
produced separate rows for Set Field, New Record/Request and Delete
Record/Request, each with error 6 in both the Error column and the
message. Commit Records returned 0 in that run and produced no row. The
complete sample also contains rows for errors 3 and 100. Some of the
observed script names have leading spaces inside the location. An event
with stand-in names reads:

```text
2026-11-26 00:50:00.000 +0000<TAB>6<TAB>Schedule "Sample" scripting error (6) at "Example : Edit Record : 3 : Set Field".
```

## 6. Unknowns

- What limit now applies to rotation: the lab server's `Event.log` and
  `Access.log` were far past 40 MiB without being rotated.
- Whether a new log file (not one started by rotation) has a header line.
- Whether the host field is the host name or the server's name.
- Text outside ASCII: encoding not observed.
- When FileMaker writes the table's messages that were not seen in the lab
  server's lines (section 4), and what `%2` of 30 and 627 holds.
- Script event formats other than the observed step errors, including
  the form when the server routes them to Event.log.
- Whether the Log Viewer reads anything besides the files themselves.
