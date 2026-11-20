# OpenMaker roadmap

**Stage: pre-alpha** · Specs: FileMaker Pro and Server 26.0.3, with Pro 26.1.1's differences [recorded](docs/re/README.md#filemaker-pro-2611) and not yet matched ([FileMaker 26.1](#filemaker-261)) · Catalogs: Pro 26.1.1 · Server target: 26.0.3 until FileMaker Server 26.1.1 ships · Last updated: 2026-11-19

Where OpenMaker stands against FileMaker, area by area, and what alpha
means. This page changes when an area's status changes, not with every PR.
Individual work items, bugs and gaps are
[GitHub issues](https://github.com/OpenMaker-Dev/openmaker/issues), labeled
by area ([AGENTS.md](AGENTS.md#6-issues)); the detail behind each row is in the
linked docs, whose "limits" sections list what is missing. The
[client progress map](docs/client-progress.md) is a finer-grained snapshot
of 68 capability groups; its header gives the commit it was checked at.

## Status by area

Status: **Working** (common cases work end to end) · **Partial** (usable,
with notable gaps) · **Early** (first slice, or read-only) · **Not started**.
Counts from FileMaker Pro 26's six starter solutions are the alpha
scorecard's [#202](https://github.com/OpenMaker-Dev/openmaker/issues/202); nothing in them is compared with FileMaker unless the
row says so.

| Area | Status | Works today | Main gaps | Docs |
|---|---|---|---|---|
| Hosted files | Working | Mutual TLS, Connect/Open/Login to FileMaker Server and OpenMaker Server, keepalive, reconnect after a lost connection from the window's bar; related files on another host or port open once the user signs in there; the Hosts dialog lists a host's files (OpenMaker Server's own listing, FileMaker's GetFileList for other hosts); a FileMaker client license is needed only for FileMaker Server | Listing and opening FileMaker Server's files from the Hosts dialog not verified live [#14](https://github.com/OpenMaker-Dev/openmaker/issues/14); reconnecting is manual and loses the session's locks, timers and script state [#16](https://github.com/OpenMaker-Dev/openmaker/issues/16); related files share the opened file's connection, checked against OpenMaker Server only [#229](https://github.com/OpenMaker-Dev/openmaker/issues/229), and open one request at a time [#237](https://github.com/OpenMaker-Dev/openmaker/issues/237); protocol `Reconnect` never observed | [network-protocol](docs/re/network-protocol.md), [hosted-file-list](docs/hosted-file-list.md), [recovery](docs/recovery.md) |
| Local `.fmp12` files | Partial | Open, edit and save through a private embedded host, with the file's own accounts and privilege sets; File > Create New; related and script-opened files beside it | No encrypted files [#58](https://github.com/OpenMaker-Dev/openmaker/issues/58), Save a Copy [#63](https://github.com/OpenMaker-Dev/openmaker/issues/63), Recover [#64](https://github.com/OpenMaker-Dev/openmaker/issues/64), conversion or the New File step [#86](https://github.com/OpenMaker-Dev/openmaker/issues/86); read-only on Windows [#30](https://github.com/OpenMaker-Dev/openmaker/issues/30) | [local-files](docs/re/local-files.md), [hbam-writer](docs/re/hbam-writer.md), [new-files](docs/new-files.md) |
| Layout rendering | Partial | Themes, styled text, fields, tab and slide panels, portals, popovers, button bars, conditional formatting, hide conditions, anchoring, container pictures and PDFs, web viewers (Linux and macOS), Form zoom; Browse draws 503 of the starters' 504 layout objects (Contact Details' web viewer shows a note in a build without web viewers) | Table view shows every field instead of the layout's columns [#270](https://github.com/OpenMaker-Dev/openmaker/issues/270), no sub-summary rows [#147](https://github.com/OpenMaker-Dev/openmaker/issues/147) and no header or footer parts [#306](https://github.com/OpenMaker-Dev/openmaker/issues/306); zoom only in Form view [#36](https://github.com/OpenMaker-Dev/openmaker/issues/36); the look has not been compared object by object with FileMaker Pro, and the starter walkthrough [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201) lists small differences [#231](https://github.com/OpenMaker-Dev/openmaker/issues/231) [#289](https://github.com/OpenMaker-Dev/openmaker/issues/289) | [layout-model](docs/re/layout-model.md), [desktop-client](docs/desktop-client.md), [conditional-visibility](docs/conditional-visibility.md) |
| Records and editing | Partial | Create, edit, duplicate, delete with auto-enter, stored calculations, validation (custom messages, calculated ones included) and exact-match lookups; related records in portal rows and creation rows; globals and later repetitions in Browse; save-conflict recovery; every table of the starters is writable (19 of 19) and Browse can edit every changeable field object (134 of 134) | Tables open read-only when a rule needs what the record rules cannot evaluate: related, global, summary or unstored inputs [#39](https://github.com/OpenMaker-Dev/openmaker/issues/39), lookups of times and timestamps, or of text or numbers into a date field [#235](https://github.com/OpenMaker-Dev/openmaker/issues/235) [#236](https://github.com/OpenMaker-Dev/openmaker/issues/236), entry from the last visited record, other lookup forms; container globals cannot be filled [#302](https://github.com/OpenMaker-Dev/openmaker/issues/302); layouts of another file's tables refuse imports, transactions, globals, containers and repetitions [#43](https://github.com/OpenMaker-Dev/openmaker/issues/43) | [record-writes](docs/record-writes.md), [recovery](docs/recovery.md), [write-path](docs/re/write-path.md) |
| Find, sort, found sets | Partial | Finds, sorts and relationship joins through query programs, on indexes and without them; index keys are found by binary search, and the work one program may do is bounded ([measured](docs/performance/query-index-lookups.md) against earlier OpenMaker only); Quick Find, Omit Multiple; the starters' find and sort tests pass (10 of 10) | Sorting by a related field [#47](https://github.com/OpenMaker-Dev/openmaker/issues/47); OpenMaker Server refuses LookupValue and Match finds from FileMaker Pro [#48](https://github.com/OpenMaker-Dev/openmaker/issues/48); text indexed in other languages than English [#46](https://github.com/OpenMaker-Dev/openmaker/issues/46) [#239](https://github.com/OpenMaker-Dev/openmaker/issues/239) [#299](https://github.com/OpenMaker-Dev/openmaker/issues/299) [#300](https://github.com/OpenMaker-Dev/openmaker/issues/300), and index keys that differ from FileMaker's [#175](https://github.com/OpenMaker-Dev/openmaker/issues/175) [#176](https://github.com/OpenMaker-Dev/openmaker/issues/176); a sorted local file reopens unsorted [#286](https://github.com/OpenMaker-Dev/openmaker/issues/286) | [query-program](docs/re/query-program.md), [desktop-client](docs/desktop-client.md) |
| Relationships and portals | Partial | Related fields and portals in each row's context, in sort and filter order; editing and creating related records in portal rows; related files, on another host too once the user signs in there; row buttons | Some portal row buttons take no clicks [#42](https://github.com/OpenMaker-Dev/openmaker/issues/42); a portal still filtered after its filter was turned off [#174](https://github.com/OpenMaker-Dev/openmaker/issues/174); calculations over more than one relationship keep their table read-only [#39](https://github.com/OpenMaker-Dev/openmaker/issues/39); related files sharing the opened file's connection are checked against OpenMaker Server only [#229](https://github.com/OpenMaker-Dev/openmaker/issues/229) | [record-writes](docs/record-writes.md), [desktop-client](docs/desktop-client.md) |
| Containers | Partial | Storage, upload, thumbnails on the server; container data in calculations and scripts; Insert Picture and File (including portal rows), Insert Audio/Video and PDF (interactive containers outside portal rows), and Export Field Contents from the menus; Browse draws pictures (all 8 starter container fields), a PDF's first page or an interactive PDF view, and plays audio and video in the web-viewer build; Preview and PDF print pictures | Insert asks for a typed path instead of a file dialog on Linux and Windows (the system's open panel on macOS), and "Store only a reference" is not done [#50](https://github.com/OpenMaker-Dev/openmaker/issues/50); Table view draws no pictures; externally stored container data on OpenMaker Server [#21](https://github.com/OpenMaker-Dev/openmaker/issues/21); HEIC images and Live Text in functions [#51](https://github.com/OpenMaker-Dev/openmaker/issues/51) | [containers](docs/re/containers.md), [server-thumbnails](docs/server-thumbnails.md), [desktop-client](docs/desktop-client.md) |
| Calculations | Working | FileMaker's decimal arithmetic and the function library, matching FileMaker Server's engine on its vectors; ExecuteSQL; every `Get` selector has an implementation; none of the functions the starters call is missing | 20 catalog functions have no implementation and answer error 1225: AI and model functions [#77](https://github.com/OpenMaker-Dev/openmaker/issues/77), persistent data [#78](https://github.com/OpenMaker-Dev/openmaker/issues/78), Furigana, GetAddonInfo, GetRecordIDsFromFoundSet and the FileMaker Go functions [#79](https://github.com/OpenMaker-Dev/openmaker/issues/79), GetFieldsOnLayout, GetTableDDL, the two FileMaker path converters and GetAVPlayerAttribute; times and timestamps lose their stored text [#236](https://github.com/OpenMaker-Dev/openmaker/issues/236); a few results differ from FileMaker's engine [#80](https://github.com/OpenMaker-Dev/openmaker/issues/80) [#81](https://github.com/OpenMaker-Dev/openmaker/issues/81) | [calc-functions](docs/re/calc-functions.md), [calc-values](docs/re/calc-values.md), [execute-sql](docs/re/execute-sql.md) |
| Scripts and triggers | Partial | Runtime with control flow, errors, variables, transactions and triggers; 129 of the 210 steps FileMaker Pro runs on macOS or Windows have an implementation on the desktop (a static count: each step run alone through the desktop's script runtime, against what the desktop host answers; an option a step does not handle still answers error 3); every step the starters' scripts and buttons use has an implementation (26 of 26) | The other 81 always fail with error 3: the Open Menu Item steps other than Open Manage Database and Open Script Workspace (14 of 16) [#85](https://github.com/OpenMaker-Dev/openmaker/issues/85), account management [#83](https://github.com/OpenMaker-Dev/openmaker/issues/83), spelling, AI [#77](https://github.com/OpenMaker-Dev/openmaker/issues/77), the PDF steps [#96](https://github.com/OpenMaker-Dev/openmaker/issues/96), Send Mail [#89](https://github.com/OpenMaker-Dev/openmaker/issues/89), plug-ins [#91](https://github.com/OpenMaker-Dev/openmaker/issues/91), file steps [#86](https://github.com/OpenMaker-Dev/openmaker/issues/86), Undo/Redo and Perform Find/Replace [#88](https://github.com/OpenMaker-Dev/openmaker/issues/88), Relookup [#87](https://github.com/OpenMaker-Dev/openmaker/issues/87), Execute SQL [#90](https://github.com/OpenMaker-Dev/openmaker/issues/90), the Data API [#94](https://github.com/OpenMaker-Dev/openmaker/issues/94), menu sets, the formatting bar and the text ruler [#93](https://github.com/OpenMaker-Dev/openmaker/issues/93), system integration [#92](https://github.com/OpenMaker-Dev/openmaker/issues/92), Enable Touch Keyboard on Windows [#337](https://github.com/OpenMaker-Dev/openmaker/issues/337); server scripts lack windows [#99](https://github.com/OpenMaker-Dev/openmaker/issues/99), portal rows [#100](https://github.com/OpenMaker-Dev/openmaker/issues/100) and Save Records as PDF [#104](https://github.com/OpenMaker-Dev/openmaker/issues/104) | [scripting](docs/scripting.md), [script-runtime](docs/re/script-runtime.md), [script-steps](docs/re/script-steps.md) |
| Native MBS capabilities | Early | A clean-room subset of 49 `MBS ( ... )` commands for idle scripts, dialogs, file/path operations and PDFs | The full MBS feature set is a longer-term goal [#287](https://github.com/OpenMaker-Dev/openmaker/issues/287); broad command coverage, developer tools, desktop/server availability and measured parity remain incomplete | [plug-ins](docs/plugins.md), [MBS behavior](docs/re/mbs-plugin.md) |
| Import and export | Partial | Import Records, Export Records, Save Records as Excel of the found set or the current record (also from File > Save/Send Records As and the toolbars), each row with its own running total and fraction of total in exports without groups; Insert from URL; the starters' CSV import, export and Excel round trip pass (6 of 6) | An Update or Replace import overwrites target data that FileMaker 26.1 keeps when the source is empty [#340](https://github.com/OpenMaker-Dev/openmaker/issues/340), and Export Records steps saved by FileMaker 26.1 fail [#347](https://github.com/OpenMaker-Dev/openmaker/issues/347); ODBC [#90](https://github.com/OpenMaker-Dev/openmaker/issues/90); FileMaker, folder and `.xls` sources and FileMaker exports [#114](https://github.com/OpenMaker-Dev/openmaker/issues/114); XSLT; FileMaker's import and export dialogs [#118](https://github.com/OpenMaker-Dev/openmaker/issues/118); Snapshot Link and JSONL [#95](https://github.com/OpenMaker-Dev/openmaker/issues/95); summary options other than a plain running total and fraction of total [#264](https://github.com/OpenMaker-Dev/openmaker/issues/264) [#265](https://github.com/OpenMaker-Dev/openmaker/issues/265) [#193](https://github.com/OpenMaker-Dev/openmaker/issues/193) [#194](https://github.com/OpenMaker-Dev/openmaker/issues/194) [#195](https://github.com/OpenMaker-Dev/openmaker/issues/195), and summary exports were not compared with FileMaker Pro's [#139](https://github.com/OpenMaker-Dev/openmaker/issues/139) | [scripting](docs/scripting.md), [script-steps](docs/re/script-steps.md#implementation-notes-importexport) |
| Preview, print, PDF | Partial | Preview, Print (CUPS, OpenMaker's own dialog), Page Setup and Save Records as PDF (from scripts, File > Save/Send Records As and the toolbars) for one-column reports with title, grand-summary and sub-summary parts, sliding, pictures and container fields; the report builder accepts all 21 starter layouts | Multi-column, summary-only and page-break layouts [#107](https://github.com/OpenMaker-Dev/openmaker/issues/107); overlapping sliders and sliding left beside sliding up [#108](https://github.com/OpenMaker-Dev/openmaker/issues/108); FileMaker's fonts and mixed-style text [#106](https://github.com/OpenMaker-Dev/openmaker/issues/106); print options [#110](https://github.com/OpenMaker-Dev/openmaker/issues/110) and Save Records as PDF options [#109](https://github.com/OpenMaker-Dev/openmaker/issues/109); Page Setup is not kept [#112](https://github.com/OpenMaker-Dev/openmaker/issues/112); printing on Windows [#111](https://github.com/OpenMaker-Dev/openmaker/issues/111); output never compared with FileMaker Pro's [#113](https://github.com/OpenMaker-Dev/openmaker/issues/113) | [preview](docs/preview.md), [printing](docs/printing.md), [portable-pdf](docs/portable-pdf.md) |
| Web viewers | Partial | Embedded viewer in every window, Form and List views (a view per row), with the JavaScript script bridge, Set Web Viewer and Perform JavaScript in Web Viewer: WebKitGTK on Linux, WKWebView on macOS | Through XWayland on a Linux Wayland session [#323](https://github.com/OpenMaker-Dev/openmaker/issues/323), not Windows [#305](https://github.com/OpenMaker-Dev/openmaker/issues/305); Table view parts; embedded viewports show a note [#319](https://github.com/OpenMaker-Dev/openmaker/issues/319) | [layout-model](docs/re/layout-model.md), [scripting](docs/scripting.md) |
| Application chrome | Partial | Menus, status toolbar, layout bar, the four modes, windows, cards and dialogs, Hosts dialog | macOS menu bar is drawn in the window [#55](https://github.com/OpenMaker-Dev/openmaker/issues/55); data-entry and Format commands in Browse are disabled [#52](https://github.com/OpenMaker-Dev/openmaker/issues/52) [#53](https://github.com/OpenMaker-Dev/openmaker/issues/53); window management [#54](https://github.com/OpenMaker-Dev/openmaker/issues/54); recent files and favorites [#56](https://github.com/OpenMaker-Dev/openmaker/issues/56); custom menu sets [#57](https://github.com/OpenMaker-Dev/openmaker/issues/57); a double-clicked `.fmp12` does not open [#262](https://github.com/OpenMaker-Dev/openmaker/issues/262) | [client-ui](docs/re/client-ui.md), [desktop-client](docs/desktop-client.md) |
| Developer tools | Partial | On OpenMaker Server and local files: Layout mode (canvas, Objects with a Fields list, Inspector; Field, Text, Button and Portal tools; the Selection tool moves, resizes and deletes stored objects; field control styles, layout width, Layout Setup, Part Setup and part boundaries), New Layout/Report for Form layouts, Duplicate and Delete Layout, Manage Layouts; Script Workspace (scripts created, renamed, duplicated, moved and deleted, Include in Menu and full access, steps saved); Manage Database (tables, fields of every kind and their options, calculated messages included, calculation and summary fields, occurrences, relationships and the graph's tools) saved with FileMaker Pro's schema transaction; Manage Value Lists and Manage Security read only, and a pop-up menu's Edit... saves a custom value list's values. The scorecard's build steps pass on every starter, and FileMaker Pro opened the 15 files of its earlier build steps | Other layout edits (restyling, objects inside groups, portals and panels, moving between parts) and List, Table and Report layouts; script folders and separators, and non-ASCII script names [#209](https://github.com/OpenMaker-Dev/openmaker/issues/209); Manage Database is read-only on FileMaker Server and lacks list features [#152](https://github.com/OpenMaker-Dev/openmaker/issues/152); Manage Value Lists cannot edit lists and security cannot be edited; Themes, Custom Functions, External Data Sources and Data Viewer are [not built](docs/desktop-client.md#not-built); not every kind of edit has been opened in FileMaker Pro [#318](https://github.com/OpenMaker-Dev/openmaker/issues/318), and none has been tried on a file that uses FileMaker 26.1's features [#341](https://github.com/OpenMaker-Dev/openmaker/issues/341) | [desktop-client](docs/desktop-client.md), [schema-manager](docs/schema-manager.md) |
| Accounts and privileges | Partial | Local files check the file's own accounts and passwords and enforce the built-in privilege sets and the custom sets whose members OpenMaker enforces (8 of 8 checks on Contacts copies made in FileMaker Pro); OpenMaker Server checks each hosted file's own accounts and applies the built-in sets' record access; Manage Security shows accounts and sets | OpenMaker Server refuses accounts in custom sets [#227](https://github.com/OpenMaker-Dev/openmaker/issues/227); a custom set whose default for all tables is limited by a calculation is refused at login [#220](https://github.com/OpenMaker-Dev/openmaker/issues/220); no [Guest], Claris ID, OAuth or external accounts [#19](https://github.com/OpenMaker-Dev/openmaker/issues/19); accounts and sets cannot be edited, by dialog or by script [#83](https://github.com/OpenMaker-Dev/openmaker/issues/83); FileMaker Pro's behavior under custom sets is largely unmeasured [#7](https://github.com/OpenMaker-Dev/openmaker/issues/7) | [local-files](docs/re/local-files.md), [session-identity](docs/re/session-identity.md), [desktop-client](docs/desktop-client.md) |
| OpenMaker Server | Partial | FileMaker Pro 26 browses, finds, writes, stores containers and edits schema on it; each file's own accounts; Perform Script on Server; schedules and backups; one TLS certificate kept across restarts; limits on connections, failed logins and the work one request may cause; with `--admin-user`, the Admin Console's Dashboard, Databases, Backups, Configuration (General Settings, Folders, SSL Certificate, Schedules), Administrator and Logs views, and the Admin API v2 | Rewrites the whole file per change [#20](https://github.com/OpenMaker-Dev/openmaker/issues/20); other users' changes reach a quiet client only with its next reply [#17](https://github.com/OpenMaker-Dev/openmaker/issues/17); custom privilege sets [#227](https://github.com/OpenMaker-Dev/openmaker/issues/227); the console and the Admin API show as unavailable, or refuse, what the server's console backend does not pass on yet, and its other views are not built ([limits](docs/admin-console.md#limits)) | [architecture](docs/architecture.md), [development](docs/development.md#the-servers-certificate), [admin-console](docs/admin-console.md) |
| Agent authoring | Early | `om-author`'s CLI and MCP server share one command catalog, which they list without signing in; they inspect hosted solutions and list the items a field control's value list offers, and on OpenMaker Server replace script steps, add stored fields, add, duplicate and delete Form layouts, place edit boxes, texts, buttons and portals, move, resize and delete layout objects, set field controls, Layout Setup, the layout width and parts, and run scripts (Perform Script on Server); 14 of the scorecard's 23 build steps on every starter | Edits only on OpenMaker Server: not on FileMaker Server [#23](https://github.com/OpenMaker-Dev/openmaker/issues/23) or local files [#208](https://github.com/OpenMaker-Dev/openmaker/issues/208); only replacing an existing script's steps [#24](https://github.com/OpenMaker-Dev/openmaker/issues/24); only stored fields of the plain types [#25](https://github.com/OpenMaker-Dev/openmaker/issues/25); edit boxes outside the tab order [#26](https://github.com/OpenMaker-Dev/openmaker/issues/26); no edit transactions [#27](https://github.com/OpenMaker-Dev/openmaker/issues/27); choosing a value-list item answers error 3, as an agent has no window; few of its edits have been opened in FileMaker Pro | [authoring](docs/authoring.md) |
| Platforms | Partial | Linux (Ubuntu) first; a `.deb` and a tarball for Ubuntu 24.04 on amd64 built by `tools/package-ubuntu.sh`; the crates are portable | Packages only for Ubuntu 24.04 on amd64, none installing OpenMaker Server as a service [#31](https://github.com/OpenMaker-Dev/openmaker/issues/31), and no published releases; Windows lacks printing [#111](https://github.com/OpenMaker-Dev/openmaker/issues/111), web viewers [#305](https://github.com/OpenMaker-Dev/openmaker/issues/305), writable local files [#30](https://github.com/OpenMaker-Dev/openmaker/issues/30) and server backups [#197](https://github.com/OpenMaker-Dev/openmaker/issues/197); macOS lacks its own menu bar [#55](https://github.com/OpenMaker-Dev/openmaker/issues/55) | [development](docs/development.md) |

## FileMaker 26.1

What FileMaker Pro 26.1.1 changes that OpenMaker would need to match
([ledger](docs/re/README.md#filemaker-pro-2611) F112-F114, [catalog rows](docs/re/catalogs/README.md#added-in-2611)).
Nothing has been run in it and no file it saved has been read, so its storage
facts are inferred from its binaries until one is. FileMaker Server 26.1.1 is
not released; the blocked row waits for it.

| Area | FileMaker 26.1 | OpenMaker today | Issues |
|---|---|---|---|
| Data safety | Update and Replace imports can keep a target field's value when the source field is empty | Ignores the option and writes the empty value over the target's data | [#340](https://github.com/OpenMaker-Dev/openmaker/issues/340) |
| | A file can hold 26.1 features that an edit and save must keep | Not checked on a 26.1.1 file. By the code, layout flag bits are kept and calculated value lists cannot be edited; unknown steps, the button icon formula and the agent file marker are open questions | [#341](https://github.com/OpenMaker-Dev/openmaker/issues/341) |
| Steps and functions | Steps 249 Replace Field Contents by Name and 250 Set Variable by Name | Stored, edited and run as inferred from the binaries and the steps they extend; not compared with a step FileMaker Pro 26.1.1 saved or ran. On OpenMaker Server they fail with error 4, as on FileMaker Server 26.0.3 | [#349](https://github.com/OpenMaker-Dev/openmaker/issues/349) |
| | GetToolDefinitions with custom function descriptions, Get ( SortOrder ), `pluginpath` in Get ( InstalledFMPluginsAsJSON ), error 120 | The names compile, and stored calls decode, print and re-encode; GetToolDefinitions and Get ( SortOrder ) fail with an error, descriptions are not read, `pluginpath` is absent | [#350](https://github.com/OpenMaker-Dev/openmaker/issues/350) [#351](https://github.com/OpenMaker-Dev/openmaker/issues/351) |
| Layouts and Browse | Calculated SVG button icons, a portal's new-record row at the top, an indicator for an edit box's clipped contents | Not drawn: no icon, the new-record row stays last, no indicator | [#342](https://github.com/OpenMaker-Dev/openmaker/issues/342) [#343](https://github.com/OpenMaker-Dev/openmaker/issues/343) [#344](https://github.com/OpenMaker-Dev/openmaker/issues/344) |
| | A checkbox set shown as one unlabelled checkbox | Drawn as a labelled checkbox set; with two or more list values, clearing the first checkbox removes its value instead of storing the second | [#345](https://github.com/OpenMaker-Dev/openmaker/issues/345) |
| | Calculated value lists, as plain values or JSON | Not supported where a list is used | [#44](https://github.com/OpenMaker-Dev/openmaker/issues/44) |
| | Layout Setup can turn off the "field isn't modifiable" and "no records" alerts | Always shown; where the option is stored is unknown | [#353](https://github.com/OpenMaker-Dev/openmaker/issues/353) |
| Script and transfer options | Export Records' field-names row for CSV and Excel, a new byte in the stored order | Steps saved by 26.1 fail, their order is read only in the Script Workspace, and CSV has no header row | [#347](https://github.com/OpenMaker-Dev/openmaker/issues/347) |
| | Show Custom Dialog's "Close after", Go to Related Record's "Table by Calculation", Configure Persistent Data deleting an entry whose value is U+2421 | Ignored: the dialog waits, a calculated table cannot be run; Configure Persistent Data answers error 3 | [#348](https://github.com/OpenMaker-Dev/openmaker/issues/348) [#352](https://github.com/OpenMaker-Dev/openmaker/issues/352) [#78](https://github.com/OpenMaker-Dev/openmaker/issues/78) |
| | Field display names for import, SQL and OData | Display names are not used yet | [#346](https://github.com/OpenMaker-Dev/openmaker/issues/346) |
| Developer tools | Go to Line, Paste in Place, the renamed developer privilege and its Data Viewer access, smaller dialog changes | Missing; the dialogs OpenMaker has behave as 26.0's | [#355](https://github.com/OpenMaker-Dev/openmaker/issues/355) [#356](https://github.com/OpenMaker-Dev/openmaker/issues/356) [#357](https://github.com/OpenMaker-Dev/openmaker/issues/357) [#358](https://github.com/OpenMaker-Dev/openmaker/issues/358) |
| Containers | AVIF images | Not inserted or drawn as pictures | [#359](https://github.com/OpenMaker-Dev/openmaker/issues/359) |
| macOS chrome | The toolbar in the Liquid Glass style | 26.0's look | [#354](https://github.com/OpenMaker-Dev/openmaker/issues/354) |
| Server and protocol (blocked) | Standby servers with automatic switchover, a WebDirect keyboard accessibility setting, hosts that may require client certificates | Missing; no client certificate is presented or asked for | [#360](https://github.com/OpenMaker-Dev/openmaker/issues/360) [#361](https://github.com/OpenMaker-Dev/openmaker/issues/361) [#362](https://github.com/OpenMaker-Dev/openmaker/issues/362) |
| File format, protocol | Binaries contain an unannounced agent interface, a file marker and a host refusal switch; whether the interface is enabled and when files are marked are unknown | Unknown to OpenMaker; what FileMaker Server 26.1.1 does with it is unknown too | [#363](https://github.com/OpenMaker-Dev/openmaker/issues/363) |

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
Where each criterion stands comes from the alpha scorecard [#202](https://github.com/OpenMaker-Dev/openmaker/issues/202), the
starter walkthrough [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201) and the open issues labeled `alpha`.

1. **Open** a hosted file and a local file with the file's own accounts,
   with privilege sets enforced.
   - Now: all six starters open locally and hosted by OpenMaker Server,
     and a local file's privilege sets pass 8 of 8 checks. The six starters have
     not been measured on FileMaker Server: one starter copy hosted there
     passed find and sort, Preview and PDF, and a commit read back on a
     second connection, and a large multi-file solution was browsed there
     read only. Privilege sets on a hosted file are not measured; open:
     [#14](https://github.com/OpenMaker-Dev/openmaker/issues/14), [#60](https://github.com/OpenMaker-Dev/openmaker/issues/60), [#229](https://github.com/OpenMaker-Dev/openmaker/issues/229), [#237](https://github.com/OpenMaker-Dev/openmaker/issues/237).
2. **Browse** every layout of the starter solutions so that it looks like
   FileMaker Pro, in Form, List and Table view, containers included.
   - Now: 503 of 504 layout objects are drawn (Contact Details' web viewer
     shows a note without the web-viewer build), and a record typed into
     every changeable field object of each base table's fullest layout is
     kept after reopening (92 of 92; the Edit counts below check the other
     objects through Browse's edit rules, not by typing). The
     look is compared with FileMaker Pro by the walkthrough's screenshots
     only, which list small differences; open: [#231](https://github.com/OpenMaker-Dev/openmaker/issues/231), [#288](https://github.com/OpenMaker-Dev/openmaker/issues/288), [#289](https://github.com/OpenMaker-Dev/openmaker/issues/289).
3. **Find and sort**: find, omit, constrain, extend, Quick Find, sort, Show
   All.
   - Now: the starters' find and sort tests pass (10 of 10); open: [#286](https://github.com/OpenMaker-Dev/openmaker/issues/286).
4. **Edit**: create, edit, duplicate and delete records in the starter
   solutions' tables, related records in portals included, with no
   layout falling back to read-only.
   - Now: 19 of 19 tables writable, 134 of 134 changeable field objects
     editable, and every field of the 9 portals' creation rows takes input
     (29 of 29). Outside the starters, rules the record rules cannot
     evaluate still make tables read-only; open: [#235](https://github.com/OpenMaker-Dev/openmaker/issues/235), [#236](https://github.com/OpenMaker-Dev/openmaker/issues/236).
5. **Script**: run the starter solutions' scripts, buttons and triggers
   without "command unavailable" (error 3).
   - Now: every step the starters' scripts and buttons use has an
     implementation (26 of 26; no script has an error-3 step). Running
     them end to end is not part of the scorecard.
6. **Output**: Preview, print and save as PDF the starter solutions'
   reports; import and export CSV and Excel.
   - Now: the report builder accepts all 21 starter layouts for Preview
     and Save Records as PDF, and the CSV and Excel steps pass on all six
     files. Printed output and FileMaker Pro's own output were not
     compared [#113](https://github.com/OpenMaker-Dev/openmaker/issues/113).
7. **Build**: create and change the tables, fields, layouts and scripts
   of an ordinary solution, in the client's developer tools or through
   `om-author`, and use the result in FileMaker Pro.
   - Now: the developer tools' build steps (24 of 24) and Manage
     Database's (27 of 27) pass on every starter, and FileMaker Pro opened
     the build steps' files (15 of 15, without the steps added since);
     `om-author` does 14 of 23 steps, on OpenMaker Server only; open:
     [#152](https://github.com/OpenMaker-Dev/openmaker/issues/152), [#208](https://github.com/OpenMaker-Dev/openmaker/issues/208).
8. **Trust**: no crash or data loss on any starter solution. Every file
   OpenMaker writes passes FileMaker's consistency check and opens in
   FileMaker Pro.
   - Now: records written as Browse writes them survive close and
     reopen in 19 of 19 tables, and the written files passed FileMaker's
     consistency check and opened in FileMaker Pro (6 of 6) at an earlier
     commit; later copies have not been checked yet.

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

The broader feature goal is tracked in [#287](https://github.com/OpenMaker-Dev/openmaker/issues/287), with individual
capabilities tracked in issues as work is scheduled. It is beyond the
current alpha gate; MBS work during the alpha push remains limited to
concrete blockers of that gate and its acceptance workflows.

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
| M5 Layout rendering | Partial | S5 Accounts from each file instead of the server configuration | Done for the built-in privilege sets ([accounts](docs/development.md#accounts-and-insecure-files)); accounts in custom sets are refused [#227](https://github.com/OpenMaker-Dev/openmaker/issues/227) |
| M6 Hosted writes | Done | S6 Admin Console ([admin-console](docs/admin-console.md)) | Early: the server serves the Dashboard, Databases, Backups, Configuration, Administrator and Logs views and the Admin API v2, mostly read-only: its console backend does not pass on the server's actions on clients and files, its backups, alerts or logs yet; other views are not built |
| M7 Local `.fmp12` open and edit | Partial | | |
