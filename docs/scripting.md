# OpenMaker scripting: calculation and script engines

Goal: a 1:1 compatible clean-room implementation of FileMaker's calculation
language (every function in the Claris functions reference, every `Get`
selector) and script engine (every script step), usable by the desktop
client, OpenMaker Server and the tools.

## Where FileMaker runs scripting

From architecture.md section 7: the client links the whole engine. Stored,
unstored, auto-enter and validation calculations, portal filters, ExecuteSQL
planning and scripts run on the client. The host runs Perform Script on
Server (PSOS), schedules, Data API scripts and the recalculation of stored
calculations after a schema change. OpenMaker therefore needs the same
engines on both sides, with a host abstraction for what differs (windows,
dialogs, `Get` selectors that describe the session).

## Crates

```
om-formula  formula syntax: the expression tree, the stored codec (Polish
  |           and formatted streams, byte for byte), the source tokenizer,
  |           parser and compiler, the printer, and a name-resolution trait
  |           (fields, table occurrences, custom functions, plug-ins)
om-calc     value model (text with styles, decimal numbers, date, time,
  |           timestamp, container), operators, the evaluator over
  |           om-formula trees, the function library, and a CalcHost trait
  |           the host implements (fields, related records, variables, Get)
om-scripts  script catalog codec, per-step parameter schemas, the runtime
  |           (control flow, error capture, variables, results, triggers,
  |           transactions) and a ScriptHost trait for steps that touch
  |           windows, records, files or the network
om-transfer Import Records, Export Records and Save Records as Excel: stored
  |           specs, file formats and character sets, with source and sink
  |           traits the hosts implement
om-server   PSOS and server-side recalculation on top of both
om-gui      calculation dialog, Script Workspace editing and the desktop
            ScriptHost
```

`om-formula` and `om-calc` have no dependency on the protocol or GUI crates.
`om-formula` knows nothing about values: number literals stay in their
stored decimal form, and `om-calc` converts them.

## Oracles

- `tools/fms-calc-harness`: FileMaker Server's own engine in a network-less
  Docker container (fast; file context, no records).
- `tools/calc-oracle`: FileMaker Pro 26.0.3 on this Mac, driven through an
  isolated capture copy (client behavior, field context).
- `tools/fms-script-harness`: FileMaker Server's own script engine in the
  same container, running probe scripts as a schedule does and reporting
  each step's error, `Get(LastError)`, `$$` values and the result.
- `tools/fms-sql`: ExecuteSQL batteries and the FQL grammar dump on the
  same engine.
- `tools/fms-transfer-oracle`: Import and Export Records on a real
  FileMaker Server, because the harness cannot load the steps' handlers.
  It runs on the lab FileMaker Server with a separate probe database
  only, which is removed afterwards. The maintainer approved this on
  2026-10-29. It logs every action, and credentials stay in the
  development machine's `.env`.

## Specifications (docs/re)

| Spec | Covers |
|---|---|
| `calc-encoding.md` | stored formulas: opcodes, operands, function tokens, formatted stream, context, source grammar |
| `calc-values.md` | value model, decimal arithmetic and precision, conversions, comparisons, dates and times, errors, evaluation rules and limits |
| `calc-functions.md` | every function and `Get` selector: signature, behavior, edge cases, client/server availability, test vectors |
| `script-steps.md` | every step: parameter schema, storage, options |
| `script-runtime.md` | control flow, errors and `Get(LastError)`, variables, script results, triggers, transactions, PSOS |

## Verification

Every claim is checked against FileMaker itself:

- Codec: decode, and re-encode byte for byte, every formula in the 66
  shipped templates and test project.
- Semantics: a differential oracle runs expressions through FileMaker Pro
  and compares its results with `om-calc` (tools/calc-oracle).
- Scripts: script catalogs in the same files decode and re-encode exactly;
  step behavior is checked against FileMaker Pro runs.

## Status (2026-10-29)

Done, verified against FileMaker:

- `om-formula`: all 5,304 template formulas (and 55,269 MBS example
  formulas) decode, re-encode, print and compile back byte for byte.
- `om-calc`: the value core with FileMaker's own decimal arithmetic and
  transcendental algorithms (calc-values.md 14), every pure function
  category (text, text formatting, number, trigonometric, financial,
  aggregate, date, time, timestamp, Japanese, JSON, container encoding,
  embedding), and the evaluator with the `CalcHost` boundary. All 2,179
  value vectors run through compile and evaluate and match FileMaker
  Server's engine exactly; of 8,198 engine result lines (incl. 7,504
  arithmetic battery lines at precisions 16-400), 8,179 match and 7 are
  known deviations (a glibc near-tie in `Sin(1529)`, cartesian `Lookup`,
  one `While`/`Let` slot case). Per-category fixtures pass apart from
  listed known rows.
- `om-scripts`: lossless, typed step parameters for every step and an edit
  API (3,639 definitions in 746 sample files re-encode exactly), and the
  runtime with `ScriptHost`; all 200 template scripts run to the end
  against the in-memory host. The desktop client runs scripts on its
  provider worker (dialogs, alerts, pause/abort, layout and object
  triggers, finds with every find operator and sorts by value list, summary
  or language, transactions, data files, windows, PSOS, Insert from URL)
  and evaluates unstored calculations and layout calculations; record
  writes use om-calc for auto-enter, stored calculations and validation.
- OpenMaker Server answers Perform Script on Server (op30, op105 callback)
  with a server script host over the hosted file: record rules,
  transactions that commit atomically, privilege checks, limits (which
  also stop a calculation in the middle of a step), related records, Go to
  Related Record, stored sorts and finds, repetitions and the file's
  locale. It recomputes stored calculations after a Manage
  Database save changes them (op73), as FileMaker Server does.
- `om-sql`: FileMaker's SQL engine behind `ExecuteSQL`/`ExecuteSQLe`: the
  FQL grammar with FileMaker's error messages and positions, execution over
  an `SqlSource`, the system tables, and a local `.fmp12` source; 1,617 of
  1,625 engine vectors match (8 text-order rows for `ı` and `Œ` remain).
  The desktop client (layouts, record pages, scripts) and server scripts
  answer it through one source over hosted data (batched reads, host-side
  finds for indexed `field = constant`); all 1,244 runnable sample-file
  vectors match on the hosted path.

Perform Script on Server with Callback reads the slots FileMaker Pro
writes (script-steps.md 12.2): the callback by name (`[00 6]` = 1, slot 7)
or from the list (the key literal in slot 3), and its parameter in slot 4;
the step text shows and writes the same. These are OpenMaker's choices,
not compared with FileMaker: a callback script the file does not have is
error 104 and the server is not called, and a callback in another file is
error 3 ([#372](https://github.com/OpenMaker-Dev/openmaker/issues/372)). The step text writes the list key's hex digits
a-f in upper case ([#371](https://github.com/OpenMaker-Dev/openmaker/issues/371)), and pasting the step as FileMaker XML
refuses its callback ([#370](https://github.com/OpenMaker-Dev/openmaker/issues/370)).

Send Mail (63) through the email client, with dialog, opens a new message
in the system's mail client on the desktop: a `mailto:` URL (RFC 6068,
`om_url::mailto`) with the To, CC, BCC, subject and message the step's
calculations give in the current record, for the user to look over and
send. The URL goes to the application the system registers for
`mailto:`, never to a web browser: on Linux the default handler of
`x-scheme-handler/mailto` (`xdg-mime`), launched from its desktop entry's
command (not through `xdg-open`, which exits 0 when the handler fails and
falls back to a browser when none is registered); on macOS `open`. The
step waits up to 10 seconds for the client's exit: zero is the message
taken, non-zero a refusal, and a client that has not exited by then is
unconfirmed, also 3. That client is left running, so its compose window
may still appear after the step returned 3: a client launched fresh keeps
running while the message is written, and only one already running
answers within the wait. On Windows
the step is 3 for now
([#609](https://github.com/OpenMaker-Dev/openmaker/issues/609)): its
`url.dll` launcher reports no failure.
Address lists split at returns, semicolons and commas outside a quoted
local part (`"Sales, West"@example.org` stays one address). FileMaker Pro
hands the message to the email client too; a URL is OpenMaker's means,
which cannot carry the rest, so through the email client these fail with
3: no dialog (sending unseen), and a URL longer than 32,000 characters
(OpenMaker's limit), no registered mail client, and a client that refuses
the message or cannot be started.

Send Mail by SMTP sends the message itself, on the desktop and on
OpenMaker Server (`om_url::send_smtp`), with the step's SMTP options
(script-steps.md 12.3): the sender's name and address, Reply-To, the
server and port, Encryption (none; SSL, TLS from the start; TLS,
STARTTLS) and Authentication (none; plain password, AUTH PLAIN or else
LOGIN; CRAM-MD5), the calculations evaluated in the current record. An
empty port is 25: Claris documents 25 as the
[SMTP Options](https://help.claris.com/en/pro-help/content/smtp-options.html)
dialog's default, and that an empty calculated port means it is
OpenMaker's reading. A stored option calculation that does not decode is
3. TLS checks the server's certificate against the webpki roots, as
Insert from URL does. For a certificate it cannot verify, Claris
documents ([Send Mail](https://help.claris.com/en/pro-help/content/send-mail.html))
that FileMaker Pro offers to connect anyway or cancel, that with error
capture on the step behaves as if the server were unavailable, and that
a server-side script behaves as if the user cancelled. OpenMaker offers
no choice: the desktop answers as with error capture on, 1502 (which
code FileMaker gives is unmeasured), and OpenMaker Server 1. With
Encryption none a plain password goes in clear, as the step says.

Esc, Cmd-. and a closing window end the session on the desktop (1), and
the script's time limit and abort on the server. Either shuts the
connection where it is, during the message's body too, unless the
message's final `.` is already sent. From then on the server's answer to
it is the step's result, and the step waits up to ten minutes for it, RFC
5321's timeout for that reply, as a shorter wait could report a message
the server took as unsent and a retry send it twice. No answer by then,
or a closed connection, is 1506 with the message possibly sent. A
deadline already passed when the `.` would be committed withholds it; one
passing after the commit, even before the `.` is written, does not. On
OpenMaker Server a script that ends past its time limit reports 1 unless
an earlier error stands, as any script that runs past it does, also when
its last step was a Send Mail the server took. The result is given before
QUIT's reply. Connecting, each other reply, each
write and the TLS handshake have a minute each, every socket read and
write within it. The system resolver has no limit of its own here, so a
lookup that does not answer holds its session until the resolver gives
up.

The message is plain text in UTF-8. A non-ASCII name or subject becomes
RFC 2047 encoded words of at most 76 characters a line, and the body is
base64 unless it is ASCII without NUL in short lines. BCC addresses go in
the envelope only. One recipient the server refuses stops the message
for all before DATA (FileMaker's behavior is unmeasured). Addresses must
be RFC 5321 mailboxes in ASCII (an address literal an IPv4 or IPv6
address). The client names itself to the server by its address
(`EHLO [address]`), not by the computer's name. The server's replies are
parsed strictly (one code per reply, CRLF lines without CR or NUL inside),
and a reply already received before the command it would answer is
refused, the message's final `.` withheld for it; one still in transit
then cannot be told from the answer. Its reply to a login is reported by
its code only, since a server may echo what it was sent.

The codes are FileMaker's SMTP codes matched by their names, OpenMaker's
reading ([#89](https://github.com/OpenMaker-Dev/openmaker/issues/89)):
- 1501: the server refused the user name or password (535).
- 1502: no connection, the greeting refused one, or (desktop) a
  certificate TLS cannot verify.
- 1503: TLS failed, or the server offers no STARTTLS for TLS.
- 1504: the server asks for encryption first: 538, or a 530 that names
  TLS or encryption. A plain connection whose server lists STARTTLS and
  no usable AUTH is read as one too [I].
- 1505: none of the mechanisms the step's authentication can use is
  offered, or the server finds it too weak (534).
- 1507: another login failure, or a login the server wants (530) and the
  step does not make.
- 1506: the server refused the sender, a recipient or the message, did
  not answer in time, or ended the session; also a message whose end was
  sent and never confirmed, which may have been sent.
How FileMaker composes its message, and which code it gives for which
reply, were not compared.

Either way these fail with 3
([#89](https://github.com/OpenMaker-Dev/openmaker/issues/89)):
- OAuth;
- attachments;
- one email per record;
- addresses collected across a found set of more than one record;
- an address with characters beyond ASCII (no SMTPUTF8);
- a stored option or attachment list OpenMaker cannot read;
- an address whose quote does not end.
On OpenMaker Server the email client is 3, as a server has none.

The desktop host answers the Get selectors, layout-object attributes
and Design functions for other files as FileMaker Pro 26.0.3 does
(calc-functions.md 6.3, measured on local files in the isolated capture
copy with `tools/calc-oracle/runner.py --scripts`): window and machine
selectors from the desktop's usable area, focus and selection, find
requests, sort and open state, `ModifiedFields`, and layout objects in
screen points at every zoom and toolbar state. Battery `s33` settled the
server's focus, `ModifiedFields`, `RequestCount` and extended privilege
answers.

Go to Related Record's "Table by Calculation" (FileMaker Pro 26.1: step
flag `0x400` with the occurrence's name in calculation slot 8,
script-steps.md 12.4) goes to the occurrence that name gives, found as
calculations find one, then as the step does with a stored occurrence; a
name no occurrence of the file has is 106. The step text shows the
calculation in place of a quoted occurrence name. The 106, the text and the
layout choices kept as they are stored for other steps are OpenMaker's,
since no step FileMaker Pro 26.1.1 wrote has been read
([#352](https://github.com/OpenMaker-Dev/openmaker/issues/352)). The Script
Workspace's options do not offer the choice yet, and keep a stored one as it
is, with no table picker. As for every Go to Related Record step, OpenMaker
Server shows the records on the stored layout or the related table's first
([#481](https://github.com/OpenMaker-Dev/openmaker/issues/481)), and the step
text does not show a layout chosen by calculation
([#482](https://github.com/OpenMaker-Dev/openmaker/issues/482)).

Desktop scripts have portal rows, a script's own write rules, real windows
and OnWindowTransaction, as FileMaker Server's engine answered them
(script-runtime.md 13.24-13.27, batteries `s35-*`):
- Go to Portal Row, Go to Object, Delete Portal Row, Set Field into the
  active row (or a new related record), and `Get` of the active row; a
  parent's related edits commit and revert with it.
- A step into a field of a portal row gives that row's control the
  keyboard: its edit box, or its pop-up menu with the list closed (also
  after Go to Field [Select/perform]), as FileMaker Pro 26 leaves the
  Assets starter's Used By menu (#866). What Pro does with keys typed
  into the closed pop-up menu then was not measured. Go to Object brings
  the tab panel holding its object to the front, and a tab panel it names,
  as Pro does (#903); a panel inside another one fronts both (not
  compared). Slide panels and Go to Field into a field on a back panel are
  left as they are (unmeasured in Pro). Go to Object on a portal leaves no
  visible focus, where Pro focuses the portal as a whole ([#259](https://github.com/OpenMaker-Dev/openmaker/issues/259)).
- Set Field fails on the rules checked at entry, only for fields validated
  Always; typing into a global and Set Field By Name into one work.
- New Window opens an OS window with its own layout, found set, mode and
  record; Select Window and Close Window work by name.
- New Window's Card style opens a card over its parent window's layout
  area (dimmed, and taking no input until the card closes; other windows
  stay usable), and Dialog a window in front that blocks every other one;
  closing either returns to its parent or opener, and closing a parent
  closes its card first (script-runtime.md, Window styles). Floating
  Document and unmapped styles stop the script with a message.
- OnWindowTransaction runs after the script that committed, with
  FileMaker's JSON.

Show Custom Dialog's "Close after (seconds)" (FileMaker Pro 26.1, numeric
slot 6 and calculation slot 13, script-steps.md 12.4) closes the desktop's
dialog by itself once the time is up, as no button: `Get ( LastMessageChoice )`
is 0, as Claris documents, and no input is stored. A time that is not a
positive number leaves the dialog to the user (OpenMaker's choice). No step
FileMaker Pro 26.1.1 wrote has been read
([#348](https://github.com/OpenMaker-Dev/openmaker/issues/348)), so the
Boolean's byte width is unknown: the Script Workspace's options show the
option and its seconds as stored and take no input, and pasting the step as
FileMaker XML refuses the option, whose XML shape is not recorded.

Root-file layouts backed by a readable related hosted table can edit existing
records. Browse and Set Field write their ordinary stored text, number, date,
time, timestamp and container fields, a container's data going to the owning
file's library (record-writes.md, Limits). The
owning provider supplies field rules, auto-entry, custom functions, stored
calculations, version checks, locks, commit and verified readback. The root
layout and script remain the caller's context, and returned calculations and
related caches refresh through the owning route. A refused or unverifiable
save reports the owning file and retains the GUI draft for correction/revert.
When the owning table's field rules are supported, New Record and Duplicate
Record (Records menu, toolbar and script steps) create the record with the
same owning engine: the owner issues the record id, enters its auto-entry
values and stores the ordinary scalar values; the new record joins the
caller window's found set and becomes current, as a local creation does.
Duplicate Record copies a container reference too: the copy's reference is
counted in the owning file's library, as a local duplicate counts it in its
own. A copy that fails gives its references back; one whose answer is lost
counts as given back, so that no later transaction's revert gives it back
again. When the owner did not apply it, or the connection was lost in
between, the member keeps one reference too many: its data is kept longer,
never lost. Its auto-entered values are copied as the window's duplicate copies them
([#473](https://github.com/OpenMaker-Dev/openmaker/issues/473)). A script's transaction includes these layouts and commits all or
none: Commit Transaction prepares every file before any record data goes, and
a refusal in any file reverts every file's part; only a connection lost, or a
host refusing a later file, after an earlier file's commit leaves that earlier
file committed (record-writes.md, "A script's transaction across files").
A script's Delete Record inside one is held there too; the user's own edits
and deletions on such a layout while the script is paused are refused
([#411](https://github.com/OpenMaker-Dev/openmaker/issues/411)).
Import, writes to the layout's related records in other tables, globals and
repetitions remain refused on these layouts
([#43](https://github.com/OpenMaker-Dev/openmaker/issues/43)). The owning file
is reached through its data source's paths, on its own host
(desktop-files.md); native FileMaker acceptance is unmeasured.

Go to List of Records finds its record ids in calculation slot 7, and
Show in new window (`0x20000`) opens a window with the step's name, bounds
and style first, as Go to Related Record's does (script-steps.md 12.2).
OpenMaker Server refuses the new window with error 3. Neither is compared
with FileMaker, which has not been measured running the step.

Fixes from a review of import data safety:
- Each imported row is checked once with the import's own auto-enter and
  validation options, and neither the import's commit nor Commit
  Transaction re-plans it.
- Imports and exports stop at a script's abort or time limit, and an
  aborted import writes nothing.
- Commits and grouped exports are linear: a 20,000-row import takes
  0.73 s, and a 4,000-record grouped export 42 ms.
- The desktop imports through a write path of its own, with every field
  type and repetition.
- Matching ignores case and compares numbers by value (oracle x04).
- A non-repeating field reads its value in every repetition's validation
  (script-runtime.md 13.22).
- A panic holding the store rolls back and leaves the file usable.

Performance fixes from the maintainer's agent (#3-#6): indexed SQL lookups,
value-list sorts, unmatched import rows, typed `UniqueValues`.

Design, layout object and Get functions answer as FileMaker Server's
engine does (calc-functions.md 5.8 and 6, batteries `s30`-`s32`). The
desktop and server-script hosts both replay:
- 4,199 Design-function answers over seven files;
- 324 `GetLayoutObjectAttribute` answers.

All 140 Get selectors were measured on the server across 19 script
states and in file context, where every Design function but
`DatabaseNames` answers empty. calc-functions.md 6 holds the host matrix:
the desktop, server and file-context answer for each selector. Catalogs
follow their stored display order, and layout objects with ids from 128
on keep their names, labels, calculations and triggers.

Server scripts open files only beneath their folders' handles
(`om-server` `scripts/host/files.rs`):
- Each name is walked with `openat` and `O_NOFOLLOW`, so any symbolic link
  is refused, and the file opened is the file checked.
- Exports write a new file and rename it over the name, so a hard link's
  other names keep their contents.
- Imports refuse files with more than one link (error 100).
- Session temporary folders are created 0700 and used only when they are
  real folders owned by the server's user.

Every import reader is bounded against hostile sources and returns its
format's error (708, 718 or 730) instead of panicking. The limits cover
file size, parsed memory, columns, worksheet references, zip entries and
inflation, XML depth and entities, and dBase headers, and are
OpenMaker's choices (script-steps.md, "Implementation notes
(import/export)"). A fuzz test feeds truncated and mutated copies of
every fixture through every reader.

Server scripts read container data from the hosted file's library:
- `GetAsText` shows the file name, and `VerifyContainer` checks the stored
  MD5; ExecuteSQL returns the file name.
- A script's commit keeps the library's reference counts: deleted records
  release their data, and copies take a reference, in the same saved
  commit.

Validation in the shared planner covers every rule, with FileMaker's
codes and timing (script-runtime.md 13.21, 291 engine runs):
- Rules checked on entry fail Set Field itself, and only for fields
  validated Always: maximum characters 511, numeric 502, 4-digit year
  500, time 501, range 503, value list 506.
- Existing value is Commit's 505.

Server Find mode:
- An unstored calculation with all-empty references reads empty, on the
  desktop too.
- A find without criteria (400) returns to Browse.
- Perform Quick Find (150) searches the window's layout by the
  desktop's rules ([Quick Find](desktop-client.md#quick-find)), among
  all the table's records, reading dates in the file's date order as
  the server's other finds do. A word that a Quick Find field of another
  occurrence would be searched for fails with 3, as a related find
  criterion does on the server
  ([#101](https://github.com/OpenMaker-Dev/openmaker/issues/101)). What
  FileMaker Server does is not measured.
- Find Matching Records (155), here and on the desktop, finds the records
  whose field holds the current record's whole value: `=` for an empty
  value, else `==` and the value, a text value's find operators taken
  literally (`om_data::found::matching_criterion`), so `a*b` finds only
  `a*b`. This request is OpenMaker's reading of Claris's description; the
  one FileMaker builds is not captured. On the server a number is written
  out in full with `.` (`1000000000`, not `1.0E+9`) and a time or
  timestamp with its seconds, so the criterion is that second, not the
  whole minute. The server's criteria compare whole seconds: a record
  holding a fraction of a second within it is found too (`9:05:00` finds
  `9:05:00.250`), and a value with a fraction of a second fails with 3,
  as no criterion holds it. A number is matched by its index key, which
  keeps 200 digits as FileMaker's does (query-program.md 7.3), so numbers
  whose keys are equal (the same sign, count of integer digits and first
  200 digits) match each other; whether FileMaker then compares the
  stored values is not measured. From Find mode the
  server switches to Browse first, as
  Claris documents, and reads the record current when Find mode was
  entered (inferred: FileMaker's record after Find mode is not measured).
  A field of another occurrence, a global and a summary field fail with 3
  on the server.
- Globals read their value with no current record, and sessions start
  from the file's stored globals.
- A criterion longer than 10,000 characters cannot be read (3), on the
  desktop too. This limit is OpenMaker's choice; FileMaker's is not
  measured. It bounds what one criterion costs (`om_query::criteria`).
- A find checks the script's abort and time limit at every record it
  tests and between batches of its index finders (one per word of a
  criterion).

The desktop keeps a script's global values for the session over the
stored ones, as Claris documents for hosted files.

ExecuteSQL builds cross products row by row behind WHERE, under a memory
budget (256 MiB by default; past it, error 2). A statement nested more
than 500 levels deep (about 100 parentheses, 24 to 44 subqueries or a
chain of 485 operators) is a syntax error, OpenMaker's limit, so that
hostile text cannot overflow a script thread's stack (execute-sql.md,
"Implementation notes (om-sql)").

Import Records, Export Records and Save Records as Excel (`om-transfer`)
run in server scripts and the desktop client. Formats: tab- and
comma-separated, merge, HTML table, FMPXMLRESULT, Excel `.xlsx` and
dBase, and for imports Excel 97-2004 `.xls` too.
- Exports cover related fields, repetitions, group-by summaries, every
  export character set and FileMaker's line endings and quoting.
- Without group-by fields each row holds its record's summaries, read in
  one pass over the found set (`om_data::summary::Summary::values`): a
  total or count on every row, the running total so far and the record's
  fraction of total, as FileMaker Pro 26.0.3 wrote them without restart
  or subtotal. Restarting running totals, subtotaled fractions, running
  counts and FileMaker Server's own values follow the same rule but are
  unmeasured (script-steps.md, "Implementation notes (import/export)",
  Exports). In a grouped export a summary without a break field shows
  `GetSummary ( s ; s )` on the first row only, in server scripts as on
  the desktop: a running total's found-set total and a fraction's 1, as
  FileMaker Pro 26.0.3 wrote them without restart or subtotal. Restart,
  subtotal and FileMaker Server's own values are unmeasured.
- A group's summary its values cannot give (the sample standard
  deviation of a group with one value) is `?` in that group only, written
  as FileMaker Server writes it: nothing in tab, an unquoted empty field
  in comma-separated text, a `COL` without `DATA` in FMPXMLRESULT, `?` in
  HTML and a text cell `?` in Excel (script-steps.md, Exports). Without
  groups the same summary is empty on every row, as FileMaker Pro 26.0.3
  wrote it in Excel. Unmeasured and OpenMaker's inference: merge files
  (as comma), dBase (empty), the grand summary of a grouped export (`?`),
  a calculation field whose value is `?` (as a summary), and a `?` among a
  field's repetitions or related values in FMPXMLRESULT (an empty `DATA`,
  so the values after it keep their places).
- Imports cover every action, mapping by position or name, field names
  rows, and auto-enter and validation through `om_data::rules`. A row
  whose rule OpenMaker does not support checking (a value list it cannot
  key, #571) ends the import with 3 rather than being skipped (729):
  OpenMaker Server then imports none of its rows; the desktop keeps the
  rows it committed before it (outside a transaction, where each row is
  committed as it is imported). A row's other errors, 3 among them (a
  record changed meanwhile, a lookup it cannot read), skip that row.
- Stored specs decode and re-encode byte for byte.
- On FileMaker Server: export probes match byte for byte, except
  Shift-JIS, Big-5 and EUC-KR and `.xlsx` time cells; 49 of 51 import
  probes match (script-steps.md, "Implementation notes (import/export)").
- Server paths are limited to the Documents and per-session temporary
  folders, and Save Records as Excel returns 3 there, as on FileMaker
  Server. Server scripts run the data file steps in the same folders;
  a session's data files close when its script ends or is aborted
  (script-steps.md, Paths on OpenMaker Server).
- The desktop shows FileMaker's export, import mapping and summary
  dialogs. The export dialog's Save as shows the step's first stored path
  this machine reads (another platform's line is skipped), or the file's
  name with the type's extension. It takes a native absolute path, which it
  passes on in FileMaker's form (on macOS `/Macintosh HD/...`), a path
  relative to Documents, or a path list line; the line it showed, left as
  it was or with only its type changed, is passed on as it is.
- In Browse mode, File > Import Records > File opens a source chooser
  before the shared import mapping dialog. Enter a native absolute path
  or a path relative to Documents, choose TSV, CSV or XLSX explicitly,
  then choose the text character set or one-based worksheet number.
  Mapping supports adding, updating the found set in order, and updating
  matching records with separate match keys and imported columns. The
  current window's file, table and found set own the import. Pending edits
  pass through the normal commit/veto boundary before source selection;
  canceling either dialog imports no rows. Provider refusals and row
  validation errors use the existing script error/summary path.
  This chooser does not offer folder, ODBC, FileMaker, legacy XLS, named
  ranges, new-table creation, custom delimiters or remembered per-file
  options. Other stored script profiles retain their existing reader paths.
- Step path lists are read as FileMaker stores them: a binary stream, not
  text. This also fixes paths in the data-file steps and Export Field
  Contents.

A review's five server-script bugs are fixed, each with a test that
reproduced it:
- Every edit is checked against the privileges of the step making it.
- A commit counts only once the hosted file is saved. A failed save puts
  memory back and gives the script an error: 809 disk full, 810 read-only,
  otherwise 800. These codes are OpenMaker's choice, since FileMaker's are
  not observed.
- Validation applies to every repetition, as FileMaker Server's engine
  answered (script-runtime.md 13.20).
- Script sessions reload the schema after a Manage Database save.
- ExecuteSQL, finds and sorts stop when a script is aborted, and shutdown
  waits robustly.

Duplicate Record now takes its own reference to container data, so
clearing either copy no longer deletes the other's data.

The desktop client runs OnLayoutEnter's script before OnRecordLoad's, as
FileMaker's engine does. The client and server share one summary module
(`om_data::summary`) for every kind. ExecuteSQL summary columns summarize
the query's found set: the whole table without a WHERE, otherwise the
records the WHERE leaves, and in a join only the first table's conditions
(execute-sql.md 8.1). Finds on unstored calculations are evaluated record
by record over the candidates, on the client and in server scripts. Summary
fields are never criteria (error 201), and Set Field into a global in Find
mode sets its value (script-runtime.md 13.19). An unstored calculation whose
referenced fields are all empty reads empty, as in FileMaker.

The runtime replays 256 of 258 probe runs of FileMaker Server's script
engine exactly (`crates/om-scripts/tests/oracle.rs`; the other 2 need the
FileMaker Server sample's own file triggers). Collation matches FileMaker
for Han and Hangul in Chinese, Korean, Japanese and English files and for
every named sort locale but Catalog, Vietnamese and Japanese kana; lone
surrogates behave as FileMaker's UTF-16 units. The container analysis
functions (`GetHeight`, `GetWidth`, `GetThumbnail`,
`GetContainerAttribute`, `ReadQRCode`, `GetTextFromPDF`, ...) match the
engine. PDF data is read in a separate worker process (#566), on the
desktop and on OpenMaker Server alike, because the PDF decoder has no size
limit of its own: the worker may allocate 256 MiB beyond its input (its
allocator then refuses and it stops), it is killed after 15 s (a page
size) or 30 s (a page or the text), its answer is size-checked (a page's
pixels, at most 64 MiB of text), and at most two run at once. A worker
that stops, is killed or answers badly counts as a PDF that does not read
(`?`, or 0 for `GetWidth`). Without a worker (see development.md) these
functions fail with error 3; nothing reads the PDF in the program itself.
A legitimate PDF whose first page needs more than the budget does not
read either. Server scripts evaluate unstored calculations and every summary
kind, and FileMaker's ~1 GiB script stack guard (error 2) is in place.

File metadata (file-metadata.md) is decoded once (`om_data::meta`,
`om_calc::Locale::from_record`, `om_layout::triggers`) for every host: the
desktop client uses the file's locale, every relationship operator and
sort, every summary kind, file triggers and layout/object trigger records,
and answers the Design functions it can from the schema; script ids are
FileMaker's serial ids.

Open items:

- Value core: `Sin(1529)` follows a glibc near-tie (glibc's code is not
  reused: LGPL); Japanese kana sort order details, Catalog and Vietnamese
  residuals, Persian index keys; FileMaker's per-session font-name
  spelling; Live Text (Apple Vision) and HEIC in container analysis.
- Functions:
  - Desktop Get: the privilege selectors answer the file's privilege set,
    but LayoutAccess and RecordAccess ignore its layout entries and
    per-table rules ([#182](https://github.com/OpenMaker-Dev/openmaker/issues/182));
    while a report page is drawn, PageNumber, RecordNumber and RecordID
    answer that page and its record; GetLayoutObjectAttribute gives no
    live page's `source` or `content` (layout-model.md 9.6); the desktop
    area is read on X11 only. Still unmeasured: the hosted-only selectors,
    Shift for `ActiveModifierKeys`, and `isActive` with the window in
    front.
  - Server: per-account privilege metadata is decoded; a set's layout
    entries and per-table rules (re/session-identity.md 2) are not applied
    to LayoutAccess and RecordAccess, and FileMaker Server's answers under
    them are unmeasured.
  - `FieldAnnotation` and `FieldDisplayNames` answer empty text, as for a
    field without them: no file read so far stores either, so their
    storage is unknown.
  - OpenMaker Server answers `GetAddonInfo`, `GetSensor`,
    `GetAVPlayerAttribute`, `Furigana`, `Location`, `LocationValues` and
    `RangeBeacons` as FileMaker Server's script engine answered them
    (calc-functions.md, s45 and s48): the add-on interface's version for
    any add-on name (error 3 for empty text); empty for a sensor the
    engine lists, whatever its case and options, and error 1208 for any
    other name; empty for any player attribute; Furigana's text unchanged
    whatever its option, the engine's answer without its reading
    dictionary; error 1208 for the three location functions, which
    `Evaluate` text fails to compile, also where the call is in a branch
    never taken: every name table OpenMaker Server compiles text against
    withholds them (a step's, a sort's read, ExecuteSQL, value lists and
    privilege formulas; `om_data::host::device::withhold_server_lacks`).
    A file the desktop opens locally, served by the same engine
    (`LocalHost`), keeps them in its privilege formulas and stored
    recalculation, as FileMaker Pro has them.
    Arguments not run get the same answers [I].
  - The desktop answers `Location` and `LocationValues` empty, as Claris
    documents FileMaker Pro, and `GetSensor` as the server's engine does
    [I] (`om_data::host::device`). `GetAddonInfo`, `GetAVPlayerAttribute`,
    `Furigana` and `RangeBeacons` still answer error 1225 there: FileMaker
    Pro's answers are unmeasured ([#79](https://github.com/OpenMaker-Dev/openmaker/issues/79)).
  - On OpenMaker Server an unstored calculation read outside a step's own
    evaluation (a sort's, say) answers these functions as the step does.
  - Record rules (auto-entry, validation, stored recalculation and record
    privilege formulas) still refuse a formula that calls `Location`,
    `LocationValues` or `RangeBeacons` while they are planned, on either
    side, as they refuse plug-in functions; a write that needs one is
    refused. `GetSensor` answers there as above.
  - Also open: AI (`GetEmbedding`, `GetTokenCount`) and the TextEncode
    transliteration table.
- Import and export:
  - Shift-JIS, Big-5 and EUC-KR differ for some letters, and `.xlsx`
    times match to about 15 digits.
  - Import Records with "Split repeating values into separate records"
    or into a new table fails with error 3 and imports nothing: the split
    is not reproduced (script-steps.md, Implementation notes
    (import/export)), and OpenMaker creates no table. How FileMaker stores
    a new-table import's target table is not observed.
  - FileMaker 26.1's "Preserve target field data if source field is
    empty" (import flag `0x40`) keeps a field an update's source leaves
    empty. Unmeasured on FileMaker and OpenMaker's inference: a value
    counts as empty repetition by repetition, a missing column counts as
    empty, and a record the import adds is written in full. Neither import
    dialog offers the option (FileMaker Pro 26.1's), but both keep it.
    With "Perform auto-enter options" on too, an update fails with error 3
    and imports nothing: a field left unwritten would no longer be kept
    from auto-entry, and which FileMaker applies first is unmeasured. An
    import with a flag past `0x40` fails with error 3 and imports nothing.
  - A stored XML import did not run on FileMaker Server (717), so XML
    imports are checked by round trip only.
  - Not probed: the layout data-formatting option, non-English files and
    Save Records as Excel's stored options.
  - An export order saved by FileMaker Pro 26.1.1 (one more byte, "Use
    field names as column names in first row", script-steps.md 12.4) is
    read and kept. With it on, comma-separated and Excel output start with
    a row of the field names, as merge files name them (quoted in CSV);
    other formats ignore it. Both are inferred from Claris's description;
    no 26.1.1 order has been read or run in FileMaker. Neither export
    dialog shows the option, and the orders OpenMaker makes are 26.0's.
  - The desktop writes a modification count of 0 in XML.
  - `.xls` imports (`om_transfer::import`, the `xls` reader) read BIFF8
    workbooks only, as an `.xlsx` sheet is read: numbers as the shortest
    decimal of their value, formulas as their cached results, dates as
    their serial numbers. How FileMaker spells an `.xls` number, and
    whether FileMaker Server imports `.xls` at all, are unmeasured.
    Excel 5.0/95 and encrypted workbooks fail with 730. File > Import
    Records does not offer `.xls` (its type menu is FileMaker Pro's);
    stored steps do.
  - An import reads its whole source and stages every row in one
    transaction; only the import limits bound its memory.
  - No `Import.log` (FileMaker Server writes none; FileMaker Pro's format
    is unknown).
  - dBase exports with more than 2,046 fields or records over 65,535
    bytes fail with error 3 and write no file, OpenMaker's choice where
    the header's 16-bit lengths would wrap. `.xlsx` exports past column
    `XFD` write invalid references. FileMaker's behavior past either
    limit is unknown.
- Steps: the step catalog is FileMaker Pro 26.1.1's (valid ids below 251).
  Its new steps 249 Replace Field Contents by Name and 250 Set Variable by
  Name ([#349](https://github.com/OpenMaker-Dev/openmaker/issues/349)) are
  stored as its binaries write them (script-steps.md 12.4), edited in the
  Script Workspace, and run as Replace Field Contents and Set Variable do
  with the field found as Set Field By Name finds its target and the
  variable named by the calculation in slot 3. Their step text, the
  errors for a name that finds nothing (102) or is not a variable's (5),
  and the order the calculations are evaluated in are OpenMaker's
  inference: no step saved or run by FileMaker Pro 26.1.1 has been
  compared. On the desktop the unnamed id 248 fails with error 3: 26.1.1's
  step table gives it no client. OpenMaker Server's scripts keep FileMaker
  Server 26.0.3's table until FileMaker Server 26.1.1 ships: ids from 248,
  these two steps among them, fail with error 4, and Save a Copy as Add-on
  Package (96) with error 3 without evaluating its options.
- Replace Field Contents and Replace Field Contents by Name: serial numbers
  with "Use Entry Options" (`[00 5, 03]`, the field's own serial settings)
  fail with error 3 before any record is written; the field's settings are
  not read there. Other serial numbers count up the initial value's last
  run of digits, keeping its width, on every host; one past an `i128`
  fails with error 3.
- On the desktop Replace Field Contents and Replace Field Contents by Name
  refuse with error 3 a field reached through a relationship from the
  layout's occurrence, a self-join's too, whose related records Set Field
  would write; they replaced the found set's own field instead. Inside a
  transaction this refusal and Use Entry Options' are the transaction's
  error, as other writes OpenMaker cannot make are (script-runtime.md
  8.4), where FileMaker would have written. Names given to Set Field By
  Name and Replace Field Contents by Name match without regard to case,
  as on the server.
- In server scripts Print Setup with neither Restore nor its dialog sets
  nothing and gives 0, as on the desktop. A restored setup gives 3, since
  it would configure PDF output OpenMaker Server cannot make yet: Save
  Records as PDF and Print are 3 there, as the report builder is part of
  the desktop client
  ([#104](https://github.com/OpenMaker-Dev/openmaker/issues/104)). With
  its dialog on, Print Setup gives 3 as well, since a server shows no
  dialog. FileMaker Server's answer is unmeasured in every one of these
  cases, the 0 included, and so is how long a restored setup lasts there.
- Sort Records and Sort Records by Field in server scripts refuse with 3
  a sort by a self join's field: another occurrence of the layout's table
  that a relationship leads to. FileMaker sorts it by its first related
  record's value (inferred from Claris and calc-values.md 12.1), and the
  summaries a sorted found set feeds would have to follow
  ([#610](https://github.com/OpenMaker-Dev/openmaker/issues/610)). Before,
  it sorted by the record's own value. Sort Records [Restore] run again
  on another layout is checked against that layout's occurrence, so a
  stored sort can be refused there. A found set's sort carried to another
  layout is not run again: its order stays, and summaries read its fields
  from the record itself, as do relationship sorts, on both hosts (#610).
  A sort entry the layout cannot reach fails the whole sort with 103 once
  a record's key is read; on an empty found set no key is read and the
  order is kept, as before
  ([#628](https://github.com/OpenMaker-Dev/openmaker/issues/628)). Sorts
  nested more than 16 deep inside one another, through related sort
  fields whose relationships sort by related fields, a finite chain as
  well as a loop, stop with 3. That ends a looping relationship sort,
  which overflowed the server's stack before. The bound is OpenMaker's;
  FileMaker's handling of a loop is unmeasured.
- Scripts: desktop steps still missing include the account steps
  ([#83](https://github.com/OpenMaker-Dev/openmaker/issues/83)) and the PDF
  steps other than Save Records as PDF
  ([#96](https://github.com/OpenMaker-Dev/openmaker/issues/96)). Set
  Web Viewer and Perform JavaScript in Web Viewer need a native web viewer
  (the `web-viewer` feature on Linux, under X11 or, on a Wayland session,
  XWayland, and on macOS; Browse in Form or List view; layout-model.md
  9.6); elsewhere
  they fail with 116 (no live web
  viewer) rather than succeed. The error codes for a page still loading (5),
  a missing or throwing function (5) and a viewer that is not shown (116)
  are inferred, not measured on FileMaker Pro. On the desktop:
  - Set Field two or more relationships away creates the far record under
    the first available stored record at each hop. FileMaker's s49 runs
    measured two hops with at most one middle record. A missing middle
    record gives 415 when creation through its relationship is off. When
    creation is on, FileMaker creates it too. OpenMaker stops the script
    as unsupported. It also stops if the middle record exists only as a
    pending draft, even with an explicit key
    ([#41](https://github.com/OpenMaker-Dev/openmaker/issues/41)).
    OpenMaker skips records pending deletion by base table and record ID.
    Set Field returns 415 when no candidate survives that check. If the
    selected parent is marked for deletion after the draft was made,
    Commit Records returns 415 and keeps both changes pending.
    Inside a script transaction, the dependency survives a successful
    Commit Records. Commit Transaction returns 415 and rolls back if
    the transaction would delete the parent while keeping the new child.
    A rollback discards transaction drafts in each window of the file
    that joined the transaction, including related edits and deletions.
    Ordinary drafts still on their original record and record sessions
    held apart keep their edits.
  - At creation, OpenMaker reads an unstored equality key in the source
    record's occurrence with the relationship evaluator. It copies a
    single nonempty, single-line text value into an ordinary text match
    field. Unsupported key shapes and failed reads refuse creation.
    Browse shows already-staged rows in a multi-hop portal, as script
    navigation does. A blank creation row still requires a direct
    relationship. FileMaker parity for these cases is unmeasured.
  - A running summary or
  fraction on a record outside the found set reads empty (unconfirmed);
  related-field criteria in server scripts; sorts by a value list of
  related values only, by one whose second field is in another table, or
  by a calculated or external list (a list sorted by its second field
  orders by it); desktop
  transactions validate at Commit Transaction (FileMaker Pro's timing
  unknown); Insert from URL
  verifies certificates even when a script turns that off (deliberate),
  refuses with 3 cURL options that could split the request (a method
  or header with CR or LF, framing headers that disagree with the body),
  which cURL would send, and a cancelled transfer keeps its connection
  until the server answers or the transfer's time limit passes (the step
  itself ends at once).
- Script Workspace does not save to FileMaker Server. It saves step edits
  through OpenMaker's authoring extension on OpenMaker Server or a file
  opened from this computer. On those hosts, it saves script catalog
  changes (create, rename, move, duplicate, delete and options) with
  FileMaker Pro's lock and upload sequence (scripts.md, "Saving scripts
  to a host"), then confirms the disk save through the extension and
  reads the catalog back (desktop-client.md, Script Workspace).
- Server: auto-enter calculations are not recomputed after a schema
  change (by design, as FileMaker). Schedules run (#11).
  - Account steps: Change Password sets the password of the account the
    session logged in with (found by its record key, so a rename does not
    redirect it), as FileMaker's `DoChangePassword` and `SetPassword` do
    (re/local-files.md 2.4), with their errors. A step with full access
    skips the set's checks. With its dialog, for [Guest] or for an
    account of another kind it fails with 3. It holds the account
    catalog's lock while it writes (310 when another user holds it,
    inferred), raises the catalog's modification count and tells the
    other clients. Reset Account Password and Enable Account change an
    account found by name, for a session with full access (else 9), in
    the same way; a name no account has gives 213 (inferred). Reset
    Account Password with "Expire password" gives 3 until a hosted login
    can set a new password
    ([#248](https://github.com/OpenMaker-Dev/openmaker/issues/248)), and
    Enable Account gives 3 for OAuth and Claris ID accounts and rather
    than make a [Full Access] account inactive. Delete Account removes an
    account and its entries in the account catalog's index; it gives 3
    for [Guest] and for [Full Access], OAuth and Claris ID accounts. The
    steps that rewrite an account (all but Delete Account) leave alone one
    whose stored checksum OpenMaker does not reproduce (3). Add Account and Re-Login fail with error 3
    ([#83](https://github.com/OpenMaker-Dev/openmaker/issues/83)).
  - The recompute after a schema change reads container inputs from the
    table's library, computes repeating calculations in each repetition,
    and answers Design functions as FileMaker Server's file context does
    (empty, but `DatabaseNames`). With "do not evaluate if all referenced
    fields are empty", which inputs count in a repetition above 1 is
    OpenMaker's reading. It still skips these, leaving their values as
    they were and logging the field: calculations of more than 255
    repetitions; container results
    ([#384](https://github.com/OpenMaker-Dev/openmaker/issues/384));
    plug-in functions and `Get` selectors that need a window, whose
    FileMaker Server answers there are unmeasured
    ([#385](https://github.com/OpenMaker-Dev/openmaker/issues/385)).
  - FileMaker Pro clients: op77 GetFileList (the Hosts dialog's file list)
    and op89 CompareModCounts (reopening a cached file) are unsupported;
    an op13 with node state `00` (FileMaker Pro deleting a script) leaves
    an empty node (scripts.md, "Saving scripts to a host").
  - External container storage is not read (containers.md 8).
  - Unique and existing-value rules need a text field with a value index
    in English, System, Unicode standard, Unicode raw, the file's own
    language (0) when it is one of those and its locale record can be
    read, or a language FileMaker leaves untailored (record-writes.md).
    Field-backed value-list rules check their source in the same forms;
    a text source in a tailored language refuses the check with 3
    ([#571](https://github.com/OpenMaker-Dev/openmaker/issues/571)). Lists from another file, a calculation, related
    values or a second field still make the table read-only.
  - OpenMaker's own choices, not settled by the oracle: numeric forms
    beyond those tested, value-list dividers, ranges on text and date
    fields, and the year position for non-US date orders.
  - FileMaker enforced validation only in files it had generated itself,
  which points to bookkeeping of its own for validated fields. This
  matters if the server ever edits field definitions.
- SQL: summary columns ignore DISTINCT, and conditions with subqueries do
  not narrow their found set. FETCH and OFFSET cut a summary's found set
  to the result's records, and with a WHERE and an ORDER BY running
  summaries follow the ORDER BY. Both apply only to one ungrouped table
  whose columns and ORDER BY are plain reads, not to joins, CASE,
  subqueries or an ORDER BY on a summary. A running summary's order under
  ORDER BY with FETCH or OFFSET is inferred (execute-sql.md implementation
  notes). The memory budget makes a huge unfiltered product fail with
  error 2 where FileMaker would finish it.

## Script Debugger

`om-scripts`'s runtime has the Script Debugger's control of the run loop
(`runtime/debug.rs`; FileMaker's behavior is in script-runtime.md 6.4).
The desktop's Tools > Script Debugger, Debugging Controls and Data Viewer,
and the Script Workspace's Debug button, are still disabled
([#74](https://github.com/OpenMaker-Dev/openmaker/issues/74)). No window
uses it yet.

- `Runtime::start_debugging` opens the debugger, `stop_debugging` closes
  it. A stopped script then runs to its end, and script triggers are
  enabled again. A Pause/Resume Script pause stays, as without the
  debugger.
- While it is open, `Runtime::run` returns `RunState::Debugger` before a
  step at which it stops:
  - the first step of a script started while no script runs (scripts
    queued to run after the current ones do not count as running, and a
    trigger on top of a script waiting for its first step is no new
    script);
  - a step with a breakpoint (flag `0x100000`);
  - the next step after a finished Step Over, Step Into or Step Out;
  - with `set_pause_on_error`, the next step after a step whose error is
    stored or reported, a failed Commit Transaction included, or the
    script's end when that step was its last.
- `Runtime::debug` continues with Run, Step Over, Step Into or Step Out,
  from a stop or from a Pause/Resume Script pause in a script it shows.
  It does not stop again at the step it continued from, so Run from a
  breakpoint passes it.
- A stop belongs to its script and step. A trigger that runs on top of it
  follows the same rules, and its own stops and commands leave the lower
  stop, which holds again when its script is back on top there. A stop
  whose script ended (Halt, or another script started with Exit) is
  forgotten, so no other script takes it over.
- A failed step's error belongs to the script whose step it was, also
  when a trigger let the step run, or the debugger opened while it was
  waiting. It stops that script, not one that runs on top first.
- `Runtime::set_next_step` moves the current script's pointer while
  stopped. It is refused while a script transaction is open, since a jump
  could pass its Commit Transaction or reach another one. `Runtime::halt`
  is Halt Script. `Runtime::debug_stack` is the Call Stack: each running
  script's name and step, the innermost last.
- The debugger stops only in a script the host lets it show
  (`ScriptHost::script_debuggable`, none by default). Another script runs
  whole, its errors do not pause, and the Call Stack lists it by name
  without a step. This is how FileMaker treats an Executable Only script.
  A button's single step never stops or pauses on its error.

Choices not compared with FileMaker (script-runtime.md U17):
- the stop before a new script's first step;
- that disabled steps and comments are never stopped at;
- that Pause on error stops after a step's alert, with error capture on
  or off, and at the end after a failed last step;
- that a trigger script fired by a step counts as a sub-script for Step
  Over and Step Out;
- that the Call Stack lists an Executable Only script's name;
- what Set Next Step does across a block's boundaries (If, Loop), where
  OpenMaker follows the cached jump indexes as for any step; and that it
  is refused in a transaction;
- that closing the debugger leaves a Pause/Resume Script pause waiting.

The desktop host and OpenMaker Server close a debugger the runtime has
open: neither opens one yet.

## Calculation limits

What one evaluation may do, and whose limit each is. Past any of them the
evaluation ends with error 7, "Running out of memory", its result `?`,
which `EvaluationError` does not catch.

| Limit | Value | Whose |
|---|---|---|
| `While` iterations per loop; custom function calls per evaluation (`Evaluate` and `SetRecursion` count their own) | 50,000, or `SetRecursion`'s n | FileMaker's (calc-values.md 8.4, 8.5) |
| Bytes of one text value the evaluation builds (UTF-8 and style runs, `Text::size`) | 2 GiB on the desktop; 256 MiB in OpenMaker Server's scripts (`--calc-text-mib`) | FileMaker documents 2 GB for text, unmeasured in a calculation (calc-values.md U15); the server's is OpenMaker's |
| Text `Evaluate`, `GetField` and `IsValidExpression` compile | a 64th of the text limit (32 MiB; 4 MiB on the server) | OpenMaker's: a compiled tree takes up to about fifty times its text |
| Custom function calls and `Evaluate`s nested in each other | 100,000 | OpenMaker's: FileMaker's engine recursed 60,000 deep; its `Evaluate` nesting is unmeasured |

The limits are a host's (`om_calc::eval::CalcLimits`, `CalcHost::limits`)
and cover everything one top-level evaluation runs, `Evaluate` text and
custom functions included. Functions that can build text many times their
input refuse before they allocate it: `&`, `Substitute`, `Replace`,
`List`, `JSONFormatElements`, `GetAsCSS`, `GetAsSVG`, `GetAsURLEncoded`,
`Base64Encode`, `Base64EncodeRFC` and `HexEncode`. Any other result, at
most a few times its input (case mapping, `Quote`, `TextDecode`, a host
function's), is checked once built. FileMaker has no repetition or padding
function; `SetPrecision` (at most 400 digits) and `^` (error 14 past about
10^400) bound numbers.

Limits:

- Joining two values near the limit can take about three times the limit
  before it is refused.
- What one evaluation keeps in many values is not bounded: `$v[i]` set in a
  `While`, or a deep recursion passing large text, holds a copy each.
- The server's recalculation after a schema change, outside any script,
  uses the desktop's limits; the record rules a script's commit evaluates
  (auto-enter, stored calculations, validation) have the script's.
- Container data a function builds (`Base64Decode`, `TextEncode`) is not
  counted; it is at most a few times its input.

## Desktop Refresh Window

Refresh Window expires cached record bodies in both the worker and native
provider, including records read through imported owning-file routes. The
active window reloads its page and recalculates object visibility. It resumes
painting after Freeze Window and retains its record, layout, mode, found-set
order, focus, scroll and uncommitted edits. It does not commit or revert, reload
schema, reconnect, or resume other frozen windows.

Ordinary refresh can reuse cached relationship IDs. Flush cached join results
also expires relationship IDs, table lists and lookup candidates so a warm
relationship query is evaluated again. Cache generations make subsequent
provider answers replace earlier client data; page revisions reject superseded
page replies. Existing page-size, library-budget and volatile-key rules still
apply, so cache retention is an optimization rather than a lifetime guarantee.

Flush cached external data refers to **related ODBC data**, as specified in the
[Claris Refresh Window help](https://help.claris.com/archive/fm22/en/pro-help/content/refresh-window.html).
The native backend supports hosted FileMaker peer files and has no ODBC query
backend. Selecting this option therefore succeeds with no additional ODBC work;
it does not treat peer-file joins as ODBC queries. ODBC refresh remains
unsupported along with that backend. Targeted Refresh Object/Portal is a
separate feature. These semantics have synthetic regression coverage; native
FileMaker acceptance has not been measured for this slice.

## Desktop targeted refresh (C38)

Refresh Object (167) resolves the exact object name on the current layout using
Go to Object's lookup order. It expires that object's contents, conditional
formatting, hide condition, and other calculated display attributes. A target
inside a portal is invalidated in every displayed row, rather than only the
active row. Container targets include their decoded descendant objects.

The runtime retains the stored repetition expression: an explicit 1 selects
repetition 1, while zero or an absent expression selects all repetitions. The
native/editor parameter reader does not synthesize a missing slot. A different
selected repetition retains the displayed field content; hidden-condition
visibility is always recomputed. Nonfields ignore repetition selection.

Refresh Portal (180) rebuilds the target portal's relationship IDs and contents,
including its sort and filter evaluation on fresh globals. It uses the active
layout instance if no name is supplied; a nonportal target follows Refresh
Object. Provider requests use hydrated calculation-schema occurrence IDs and
the source record, including imported/peer-file routes. Both steps reuse the
Refresh Window cache generation and refusal boundaries, without committing,
reverting, thawing a frozen window, or broadcasting a record write. Cached
results for unrelated objects/portals and other windows' view/draft state remain
in place; an independent provider change notification can still invalidate data.

Targeted updates retain the record page. Refreshed stored field objects read
fresh calculation data until the next page/data replacement. Pending replies
from an older provider generation cannot reinstate pre-refresh inputs. A portal
row carries the record/draft identity established on entry: refresh remaps that
identity to its new position, or clears the selection when it disappears. The
existing refusal of script actions on sorted/filtered portal rows remains;
refresh clears their unknown row position instead of using an obsolete index.

Limits are explicit: targeted refresh uses Browse Form/List layout painting.
Table view and non-Browse screens report unsupported because their column,
request or authoring displays do not use the layout object evaluation path.
External/unknown object contents, undecodable target calculations, and
multi-repetition grids (the renderer currently displays one
FieldSpec repetition per object) report unsupported. Separate objects displaying
individual repetitions are supported. A missing named/active object returns 116;
an empty Refresh Object name returns 5, without changing focus. A repetition past
the field definition returns 102. Native FileMaker acceptance is unmeasured;
verification uses synthetic fixtures only. These changes do not establish full
C38/native UI parity.

References: [Refresh Object](https://help.claris.com/en/pro-help/content/refresh-object.html)
and [Refresh Portal](https://help.claris.com/en/pro-help/content/refresh-portal.html).
Synthetic regressions live in the existing GUI calculation, scripting, and
window-update unit tests, plus `om-provider/tests/lazy_related.rs`.

## Desktop button bars

A button bar's segments are layout buttons (layout-model.md 5.3): a click
on a segment runs its single step, Perform Script included, with its
parameter, through the same Browse action and script runtime as any
button (`om-gui` `app/portal_clicks.rs`). The window's file, layout and
record are the script's context; in List view the row's record becomes
current first. The focused field's exit triggers run first and a veto
cancels the click and keeps the typing; the user's typing is saved before
a segment's step runs. A segment's hide condition hides it alone (its
bar's hides the bar and every segment), an undecidable one shows the
disabled stand-in, and a segment without an action takes no clicks, as
for any button. Popover button segments open their popovers as other
popover buttons do.

The active segment paints in the theme's active state and is announced as
selected (layout-model.md 5.5.1): the segment Button Bar Setup chose, or
the segment whose object name the active segment calculation gives. The
calculation runs with the bar's other layout calculations
([conditional-visibility.md](conditional-visibility.md)): on the bar's
record (the row's record in List view), in its window, cached by layout,
object and record, evaluated again when the data, the variables, the
window context it read (`Get`) or a Refresh Object of the bar changes. Its
result names a segment as `GetLayoutObjectAttribute` names objects, so
both answer for the same object. Until its data arrives no segment is
active; when it reads what this window lacks, does not decode, or the
bar stores what the corpus does not explain, no segment is active and a
dashed outline with a note says why.

Limits:

- In a portal row, a calculated active segment is not evaluated (the bar
  says so); a chosen segment shows. Segment clicks in portal rows go
  through the portal row path below; no test covers a bar in a portal.
- Find mode does not show the active segment; Browse does.
- `GetLayoutObjectAttribute ( segment ; "isActive" )` keeps its measured
  answer (focus in the active window); whether FileMaker Pro answers 1 for
  the active segment is unmeasured.
- When FileMaker Pro re-evaluates the calculation (Claris advises Refresh
  Object after changing what it reads) is unmeasured: OpenMaker shows the
  result for the current context. Native acceptance is unmeasured.

## Desktop buttons in portal rows

A button in a portal row runs as a button outside portals does: its single
step, Perform Script included, with its parameter, in the same script runtime
(`om-gui` `app/portal_clicks.rs`, `scripting/host/portals.rs`). The click
carries the portal object, the record whose portal it is, and the row's
related record and number as Browse drew them, in the portal's own sort and
filter. The window's layout, record and file stay the script's context; the
related record never stands in for the parent record or for a record with
the same id in another table or file.

Before the parameter is evaluated, the worker makes that row the active
portal row, without a field. It finds the row again by its related record
among the portal's rows: in a portal of the relationship's order the row is
numbered where the record is now; in a portal that sorts or filters its rows
the number is the one Browse showed. `Get ( ActivePortalRowNumber )`, the
parameter, related field reads, Set Field into the portal's occurrence,
Delete Portal Row and Go to Related Record then use that record, through the
existing relationship routing, script write rules and commit. A row begun in
the creation row has no record yet. Related field reads in its context answer
what was typed or set into it (the window's draft): empty for a field it was
given nothing for, with no auto-entry or stored calculations, until an
ordinary commit previews them. The OnObjectSave script of the field the
user leaves by clicking the layout's background, the commit FileMaker Pro
26.0.3 was measured in, reads the row with what its own rules enter: its
match fields, and its auto-entered values and stored calculations as its
file's provider previews them (record-writes.md, "A new record's preview";
[#277](https://github.com/OpenMaker-Dev/openmaker/issues/277)). Browse shows them under what was typed from then on, and the commit
stores what the script read. A Tab or a click into another field previews
nothing [I: when FileMaker enters them then is unmeasured]. Its unstored
calculations evaluate over those values. Its own relationships find no
record, and its `Get` selectors answer as for no record. Nothing about it is
asked of the host (#1014). In a portal
that sorts or filters its rows, a clicked row and the row the user is in
are the record the window painted there: a stored record, a record begun in
the creation row, or the creation row itself. The triggers of the field the
user is in read it (#1027). Rows a script moves to (Go to Portal Row, Go to
Record) are numbered as Browse shows them, by the portal's sort and filter,
and keep their record from then on (#192). Go to Related Record with "Show
only related records" from a filtering portal's row finds the records the
portal shows. Only a sort or filter that does not decode leaves the rows'
order unknown, and steps reaching the active row's record are refused (3).

Hidden objects, undecidable stand-ins, objects whose hide condition is still
being calculated and the creation row's template take no clicks, as before.
A click that waits first (the focused field's exit triggers, the save of the
user's typing, or the move to another record's row in List view with its
triggers) is matched again to its row by the related record when its button
next paints shown. It never uses the old row number. It is dropped, without
running, when the trigger refuses, when the row, the button or the portal is
gone, hidden or replaced, when the layout, record, mode or window changes, or
when the user does anything else first. The worker then refuses, before
anything runs, a row whose record is no longer one of the portal's related
records, a portal no longer on the layout, and a click outside Browse mode.

Limits:

- A portal of the layout's own table occurrence (the found set) refuses
  its row buttons.
- Popover buttons in portal rows and nested portals still take no clicks.
  Segments of a button bar in a portal row take clicks as buttons there
  do ("Desktop button bars").
- In a portal that sorts or filters its rows, the worker checks that a
  clicked row's record is still related, not that the portal's filter
  still admits it, and keeps the number Browse showed for the rest of the
  script.
- What FileMaker reports in a portal button's script for `Get (
  ActiveLayoutObjectName )`, `Get ( RecordNumber )` in the parameter, and
  whether the row stays active after the script, has not been measured;
  the window keeps the row active until a commit, as script-entered rows
  are kept.
- Native FileMaker acceptance is unmeasured. The Claris pages of `Get (
  ActivePortalRowNumber )`, Delete Portal Row and Go to Related Record
  describe the active row but not a button click in a row. Verification
  uses synthetic fixtures (`om-gui/tests/client_portals.rs`,
  `browse_portals.rs`).
