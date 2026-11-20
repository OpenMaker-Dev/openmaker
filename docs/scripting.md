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

The desktop host answers the Get selectors, layout-object attributes
and Design functions for other files as FileMaker Pro 26.0.3 does
(calc-functions.md 6.3, measured on local files in the isolated capture
copy with `tools/calc-oracle/runner.py --scripts`): window and machine
selectors from the desktop's usable area, focus and selection, find
requests, sort and open state, `ModifiedFields`, and layout objects in
screen points at every zoom and toolbar state. Battery `s33` settled the
server's focus, `ModifiedFields`, `RequestCount` and extended privilege
answers.

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
Duplicate Record of a record holding a container reference is refused before
any effect. A script's transaction includes these layouts and commits all or
none: Commit Transaction prepares every file before any record data goes, and
a refusal in any file reverts every file's part; only a connection lost, or a
host refusing a later file, after an earlier file's commit leaves that earlier
file committed (record-writes.md, "A script's transaction across files").
Delete Record inside one is refused
([#312](https://github.com/OpenMaker-Dev/openmaker/issues/312)).
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
  rows, and auto-enter and validation through `om_data::rules`.
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
    `GetAVPlayerAttribute` and `Furigana` as FileMaker Server's script
    engine answered one call of each (calc-functions.md, s45): the add-on
    interface's version for `"x"` (error 3 for empty text), error 1208 for
    `"battery"`, empty for `"playbackState"`, and `"abc"` unchanged, the
    engine's answer without its reading dictionary. Other arguments get
    the same answers, OpenMaker's fallback [I]; Furigana's option is
    ignored. The desktop still answers them with error 1225: FileMaker
    Pro's answers are unmeasured ([#79](https://github.com/OpenMaker-Dev/openmaker/issues/79)).
  - Also open: AI (`GetEmbedding`, `GetTokenCount`), the mobile functions
    (`Location`, `LocationValues`, `RangeBeacons`) through each host, and
    the TextEncode transliteration table.
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
  - Set Field does not create records through more than one relationship
    ([#41](https://github.com/OpenMaker-Dev/openmaker/issues/41)).
  - A running summary or
  fraction on a record outside the found set reads empty (unconfirmed);
  related-field criteria in server scripts; sorts by
  a value list's second field or a calculated or external list; desktop
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
    than make a [Full Access] account inactive. None of
    them rewrites an account whose stored checksum OpenMaker does not
    reproduce (3). Add Account, Delete Account and Re-Login fail with
    error 3 ([#83](https://github.com/OpenMaker-Dev/openmaker/issues/83)).
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
    in English, System, Unicode standard or Unicode raw, value-list rules
    one in English. Lists from another file, a calculation, related values or
    a second field still make the table read-only.
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
