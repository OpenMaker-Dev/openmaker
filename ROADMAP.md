# OpenMaker roadmap

**Stage: pre-alpha** · Target: FileMaker Pro 26.0.3 and FileMaker Server 26.0.3 · Last updated: 2026-11-18

Where OpenMaker stands against FileMaker, area by area, and what alpha
means. This page changes when an area's status changes, not with every PR.
Individual work items, bugs and gaps are
[GitHub issues](https://github.com/OpenMaker-Dev/openmaker/issues), labeled
by area ([AGENTS.md](AGENTS.md#6-issues)); the detail behind each row is in the
linked docs, whose "limits" sections list what is missing. The
[client progress map](docs/client-progress.md) is a finer-grained snapshot
(68 capability groups) taken at commit `44aa30e` on 2026-11-02; on
2026-11-10 and 2026-11-11 only the rows that later merges changed were checked again.

## Status by area

Status: **Working** (common cases work end to end) · **Partial** (usable,
with notable gaps) · **Early** (first slice, or read-only) · **Not started**.

| Area | Status | Works today | Main gaps | Docs |
|---|---|---|---|---|
| Hosted files | Working | Mutual TLS, Connect/Open/Login to FileMaker Server and OpenMaker Server, keepalive, reconnect after a lost connection; a FileMaker client license is needed only for FileMaker Server | FileMaker Server's file list (op77) not decoded; protocol `Reconnect` never observed | [network-protocol](docs/re/network-protocol.md), [hosted-file-list](docs/hosted-file-list.md), [recovery](docs/recovery.md) |
| Local `.fmp12` files | Partial | Open, edit and save through a private embedded host; related and script-opened files beside it | Only accounts in the three built-in privilege sets, whose record and design limits the desktop applies; custom sets refused (#156); no encrypted files, Save a Copy, Recover, conversion or New File; read-only on Windows | [local-files](docs/re/local-files.md), [hbam-writer](docs/re/hbam-writer.md) |
| Layout rendering | Partial | Themes, styled text, fields, tab and slide panels, portals, popovers, button bars, conditional formatting, hide conditions, anchoring, Form zoom | Container contents not drawn; Table view sub-summary rows and grouping by related sort keys; zoom only in Form view | [layout-model](docs/re/layout-model.md), [desktop-client](docs/desktop-client.md), [conditional-visibility](docs/conditional-visibility.md) |
| Records and editing | Partial | Create, edit, duplicate, delete with auto-enter, stored calculations, validation and exact-match text lookups; save-conflict recovery | Browse edits text, number, date, time and timestamp fields, dates and times as data entry reads them (FileMaker Pro's own entry unmeasured, #534), not non-text globals or later repetitions (#185); tables using entry from the last visited record, other lookup forms or other unsupported rules open read-only (#186); summaries not editable | [record-writes](docs/record-writes.md), [recovery](docs/recovery.md), [write-path](docs/re/write-path.md) |
| Find, sort, found sets | Partial | Finds, sorts and relationship joins through query programs, on indexes and without them; index keys are found by binary search, and the work one program may do is bounded ([measured](docs/performance/query-index-lookups.md) against earlier OpenMaker only) | Match finds, LookupValue; index keys for non-English languages | [query-program](docs/re/query-program.md) |
| Relationships and portals | Partial | Related fields and portals in each row's context, in sort and filter order; related files; row buttons | Editing related records in portals; Set Field through more than one relationship | [scripting](docs/scripting.md) |
| Containers | Partial | Storage, upload, thumbnails on the server; container data in calculations and scripts; Insert Picture/File and Export Field Contents from the menus | Desktop does not display images, PDFs or media | [containers](docs/re/containers.md), [server-thumbnails](docs/server-thumbnails.md) |
| Calculations | Working | FileMaker's decimal arithmetic and the function library, matching FileMaker Server's engine on its vectors; ExecuteSQL | 17 catalog functions have no implementation (AI, mobile and location, persistent data, Furigana) | [calc-functions](docs/re/calc-functions.md), [calc-values](docs/re/calc-values.md), [execute-sql](docs/re/execute-sql.md) |
| Scripts and triggers | Partial | Runtime with control flow, errors, variables, transactions and triggers; about 130 of FileMaker Pro's 210 desktop steps have an implementation (static count) | Accounts, spelling, AI and PDF steps, Send Mail, plug-ins, file creation and recovery | [scripting](docs/scripting.md), [script-runtime](docs/re/script-runtime.md), [script-steps](docs/re/script-steps.md) |
| Native MBS capabilities | Early | A clean-room subset of 49 `MBS ( ... )` commands for idle scripts, dialogs, file/path operations and PDFs | The full MBS feature set is a longer-term goal ([#1236](https://github.com/OpenMaker-Dev/openmaker/issues/287)); broad command coverage, developer tools, desktop/server availability and measured parity remain incomplete | [plug-ins](docs/plugins.md), [MBS behavior](docs/re/mbs-plugin.md) |
| Import and export | Partial | Import Records, Export Records, Save Records as Excel of the found set or the current record (also from File > Save/Send Records As and the toolbars), each row with its own running total and fraction of total in exports without groups; Insert from URL | ODBC, FileMaker and folder sources; XSLT; container export; summary options other than a plain running total and fraction of total, break-field rows of grouped exports, the current-record row and FileMaker Server's values are unmeasured in exports ([#372](https://github.com/OpenMaker-Dev/openmaker/issues/139)) | [scripting](docs/scripting.md) |
| Preview, print, PDF | Partial | Preview, Print (CUPS), Page Setup and Save Records as PDF (from scripts, File > Save/Send Records As and the toolbars) for one-column reports with summary parts and sliding | Multi-column and other report layouts; native print dialog; Windows printing; Save Records as PDF in server scripts | [preview](docs/preview.md), [printing](docs/printing.md), [portable-pdf](docs/portable-pdf.md) |
| Web viewers | Partial | Embedded WebKitGTK viewer with the JavaScript script bridge | Linux/X11 only, Browse Form and List views | [scripting](docs/scripting.md) |
| Application chrome | Partial | Menus, status toolbar, layout bar, the four modes, windows, cards and dialogs, Hosts dialog | macOS menu bar is drawn in the window | [client-ui](docs/re/client-ui.md), [desktop-client](docs/desktop-client.md) |
| Developer tools | Early | Layout mode (canvas, Objects, Inspector; the Field tool places edit boxes, the Button tool places buttons that perform a script or one script step, the Selection tool moves and resizes stored objects, New Layout/Report adds Form layouts), Manage Layouts, Script Workspace (scripts created, renamed, duplicated, moved and deleted, Include in Menu and full access set, steps saved, output and input files and windows by name in step options), on OpenMaker Server and local files; Manage Database saves supported table, normal-field, field-option, calculation-field, summary-field, occurrence and relationship edits with FileMaker Pro's schema transaction on OpenMaker Server and local files | No other layout changes from the client (deleting, restyling, arranging, objects inside groups, portals and panels, List, Table and Report layouts); no new script folders or separators, and step options keep import and export orders, print settings, data sources and Send Mail as stored; FileMaker Pro has not yet opened files with OpenMaker-made scripts, buttons or layouts (FileMaker's consistency check passed on new and moved layouts); Manage Database still lacks some relationship tools, lookups beyond exact text matches, option formulas over related fields, message calculations, weighted and subtotaled summaries and calculations over summary or global fields, and is read-only on FileMaker Server; Security, Value Lists, Themes, Custom Functions, External Data Sources and Data Viewer not built | [desktop-client](docs/desktop-client.md), [schema-manager](docs/schema-manager.md) |
| Accounts and privileges | Early | Local files check the file's own accounts and passwords | Privilege sets not enforced; hosted OpenMaker Server uses its configured account | [local-files](docs/re/local-files.md), [session-identity](docs/re/session-identity.md) |
| OpenMaker Server | Partial | FileMaker Pro 26 browses, finds, writes, stores containers and edits schema on it; Perform Script on Server; schedules and backups; one TLS certificate kept across restarts; limits on connections, failed logins and the work one request may cause; with `--admin-user`, the Admin Console's Dashboard, Databases, Backups, Configuration (General Settings, Folders, SSL Certificate, Schedules), Administrator and Logs views, and the Admin API v2 | Rewrites the whole file per change; other users' changes reach a quiet client only with its next reply ([#153](https://github.com/OpenMaker-Dev/openmaker/issues/17)); hosted files use the configured account, not the file's; the console and the Admin API show as unavailable, or refuse, what the server's console backend does not pass on yet (messages, notices, file states, backups, alerts and logs), and the console's other views are not built ([#162](https://github.com/OpenMaker-Dev/openmaker/issues/22)) | [architecture](docs/architecture.md), [development](docs/development.md#the-servers-certificate), [admin-console](docs/admin-console.md) |
| Agent authoring | Early | `om-author`'s CLI and MCP server share one command catalog, which they list without signing in; they inspect hosted solutions and list the items a field control's value list offers, and on OpenMaker Server replace script steps, add stored fields, add Form layouts, place edit boxes, move and resize layout objects and run scripts (Perform Script on Server). FileMaker Pro 26 checked, opened and edited a file with one added Text field and its edit box: consistency check, Browse entry and its own later saves passed (#166) | Other design changes are read-only, FileMaker Server takes no design change, and `run_script` and `list_choices` have run against OpenMaker Server only. Choosing a value-list item answers error 3, as an agent has no window. Other field types, labels, layouts, tab order and script edits have not been opened in FileMaker Pro | [authoring](docs/authoring.md) |
| Platforms | Partial | Linux (Ubuntu) first; the crates are portable | No packaging; Windows lacks print, web viewers and writable local files | [development](docs/development.md) |

## Alpha gate

The project's goal is 1:1 compatibility with FileMaker; alpha is a step
toward it, not that goal. Alpha is meant to let people who use FileMaker
open their local and hosted solutions in OpenMaker on Linux and do
everyday work with nothing lost, and let developers build most ordinary
solutions, by hand or through an agent with `om-author`.

FileMaker Pro 26's starter solutions (the templates shipped with the
installer) and files hosted by FileMaker Server are the first acceptance
corpus: each criterion below is checked on them. They are not the whole
scope, and passing on them does not show that every solution works.

1. **Open** a hosted file and a local file with the file's own accounts,
   with privilege sets enforced.
2. **Browse** every layout of the starter solutions so that it looks like
   FileMaker Pro, in Form, List and Table view, containers included.
3. **Find and sort**: find, omit, constrain, extend, Quick Find, sort, Show
   All.
4. **Edit**: create, edit, duplicate and delete records in the starter
   solutions' tables, related records in portals included, with no
   layout falling back to read-only.
5. **Script**: run the starter solutions' scripts, buttons and triggers
   without "command unavailable" (error 3).
6. **Output**: Preview, print and save as PDF the starter solutions'
   reports; import and export CSV and Excel.
7. **Build**: create and change the tables, fields, layouts and scripts
   of an ordinary solution, in the client's developer tools or through
   `om-author`, and use the result in FileMaker Pro.
8. **Trust**: no crash or data loss on any starter solution. Every file
   OpenMaker writes passes FileMaker's consistency check and opens in
   FileMaker Pro.

Issues that block this carry the `alpha` label.

## Native MBS capabilities beyond alpha

OpenMaker's goal includes the full feature set of the
[MBS FileMaker Plugin](https://www.monkeybreadsoftware.com/filemaker/)
as native functionality, available without installing the MBS plug-in.
The scope includes developer tools and editor improvements, PDF and
document processing, images, OCR and barcodes, spreadsheets, networking
and mail, database integrations, files and archives, XML/JSON, embedded
scripting, scheduling and dialogs, and OS, device, media and AI services.
The vendor's [function reference](https://www.mbsplugins.eu/) is the
coverage reference; the initial catalog baseline is MBS 16.5, consulted
2026-11-18.

Existing solutions should retain their documented `MBS ( ... )` calls,
while the underlying capabilities are shared native operations available
to scripts, the client, the server and agent authoring where applicable.
Implementation follows OpenMaker's clean-room rules, using public
specifications and observed behavior. Desktop/server applicability,
platform-specific services and portable equivalents must be documented;
unsupported operations fail explicitly. The existing bounded subset is
not full compatibility, and its native parity remains unmeasured
([current support and limits](docs/plugins.md)).

The broader feature goal is tracked in
[#1236](https://github.com/OpenMaker-Dev/openmaker/issues/287), with
individual capabilities tracked in issues as work is scheduled. It is
beyond the current alpha gate; MBS work during the alpha push remains
limited to concrete blockers of that gate and its acceptance workflows.

## Milestones so far

Early milestones from the original plan, kept so references to them
resolve. New work is planned in issues, not here.

| Client | | Server | |
|---|---|---|---|
| M0 Transport (TLS, GIOP) | Done | S0 Handshake | Done |
| M1 Connect | Done | S1 Open, Login, Download | Done |
| M2 Open and Login | Done | S2 Browsing (download lists, notifications, query programs) | Done |
| M3 Schema read | Done | S3 FileMaker Pro as a client | Done |
| M4 Records and finds | Done | S4 Writes (locks, create, commit, notifications) | Done |
| M5 Layout rendering | Partial | S5 Accounts from each file instead of the server configuration | Done for the built-in privilege sets ([accounts](docs/development.md#accounts-and-insecure-files)); accounts in custom sets are refused (#734), and server scripts of accounts outside [Full Access] fail closed rather than follow their set |
| M6 Hosted writes | Done | S6 Admin Console ([admin-console](docs/admin-console.md)) | Early: the server serves the Dashboard, Databases, Backups, Configuration, Administrator and Logs views and the Admin API v2, mostly read-only: its console backend does not pass on the server's actions on clients and files, its backups, alerts or logs yet; other views are not built |
| M7 Local `.fmp12` open and edit | Partial | | |

## Before the repository goes public

Tracked as issues labeled
[`pre-publication`](https://github.com/OpenMaker-Dev/openmaker/labels/pre-publication).
