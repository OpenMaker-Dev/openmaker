# Admin Console: FileMaker Server's web management interface

FileMaker Server is administered from a browser: the **Admin Console** shows
the hosted databases and connected clients, and changes the server's settings.
Scripts and other tools use the **Admin API**, a documented REST interface on the
same host. This document records how FileMaker Server 26 presents both.
OpenMaker Server's console, built from it, is described in
[../admin-console.md](../admin-console.md).

## Sources and provenance

- **Live console, black box.** FileMaker Server 26.0.3.309 on the Linux lab
  server, 2026-10-29, signed in as the server administrator. Pages were only
  viewed. Tabs, menus and dialogs were opened and then cancelled, and no setting
  was changed. The page measurements come from the browser's computed styles
  at a 1440x1000 viewport.
- **The console's string table.** The console loads its English UI text
  unauthenticated from `/admin-console/assets/i18n/generated/en.json`. It holds
  3044 strings and is used here for labels and messages on views that were not
  opened.
- **Admin API Reference.** This is the documentation shipped in the server
  package (`Documentation/Admin API Documentation/index.html`). It is a ReDoc
  page around an OpenAPI 3.0 document with 112 paths under `/fmi/admin/api/v2`.
- **Server package layout.** This includes `Admin/FAC/facdeploy.sh`,
  `Admin/FAC/config/config.js` and `Admin/conf/*.html`.
- **Live actions, 2026-11-11.** Send Message, Disconnect, Close, Pause,
  Resume, Open and Block New Users sent through the Admin API to the lab
  server while FileMaker Pro 26.0.3 had the lab test file open through the
  recording relay (section 9). Only that file was closed, paused or
  disconnected from, and every setting was put back.

- **The console's server code, read statically (from bin).** `facstart.sh`
  is a pkg-packed Node.js 22.23 executable. Its own modules
  (`dist_server/`) are stored as V8 bytecode only; their third-party modules
  (passport-http, express-jwt, swagger-tools) and the Admin API's Swagger
  2.0 document (`adminapi/v2/spec/adminapi.spec-2.0.json`, 114 paths) and
  error texts (`adminapi/v2/assets/error.texts.json`) are stored as source.
  The bytecode was disassembled, not run: on the development machine each
  module's code cache was loaded with V8's code logging on
  (`--log-code-disassemble`) in the package's own runtime started as plain
  Node. Native answers come from FileMaker Server's `libFMSLib.so`
  (`FMS::AdminAuthHelper`, `FMS::WebServicesStrings`), analysed in Ghidra
  with its DWARF. Section 8.1 uses this.

The console's browser code was not decompiled, and none of its assets (code,
style sheets, icons, logos, help text) is reproduced here.
The raw notes and screenshots are in `re/work/admin-console/`, which is
git-ignored because it shows real host data: `inventory.md`, `screens/`, `live/`
and `admin-api-v2.openapi.json`.

## 1. How FileMaker Server serves the console

- The console runs on Node.js. The server package ships it as a single
  executable (`Admin/FAC/facstart.sh`), with `FAC`, `SAC` and `AAC` variants for
  the server's deployment roles. It listens on `localhost:16001`. The server's
  web server proxies it on port 443 at `/admin-console`. FileMaker Server's own
  "Admin Console" desktop launcher is a page that redirects to
  `http://localhost:16001/admin-console`.
- The browser side is a single-page application. Bootstrap 5 handles styling,
  Angular Material the selects, ag-grid the data grids and Chart.js the charts.
- It uses three channels:
  - `POST /fmi/admin/internal/v1/user/login` signs in.
  - Other console-private REST calls also live under `/fmi/admin/internal/v1/`,
    such as `server/env`, `server/onboardingstatus` and
    `user/adminroleprivileges`.
  - A socket.io connection at `/admin-console/socket.io` carries every live view,
    such as the counters, the database and client lists, and the statistics.
- The documented Admin API is served alongside at `/fmi/admin/api/v2`.
- Limits come from `config.js`:

  | Setting | Value |
  |---|---|
  | Console sessions | at most 5 |
  | Admin API sessions | at most 5 |
  | Sockets per console session | 5 |
  | Admin API idle timeout | 900 seconds |
  | Socket disconnect timeout | 30 seconds |

## 2. Navigation

All console URLs are below `/admin-console/`. Sign-in is `signin`. The
signed-in application is under `app/`, with one tab per top-level route. A
tab's sub-views are routes beneath it, and the bare tab URL redirects to its
first sub-view.

| Tab | Route | Sub-views (route: title) |
|---|---|---|
| Dashboard | `app/dashboard` | - |
| Databases | `app/activities` | - |
| Backups | `app/backups` | `allbackups`: Auto-Backup and On-Demand; `preservedbackups`: Preserved; `backupsettings`: Backup Settings |
| Remote Backups | `app/cloudbackups` | `bindfilebox`: Remote Backup Drives (more after a drive is connected) |
| Configuration | `app/configuration` | `generalsettings`: General Settings; `filemakerclients`: FileMaker Clients; `folders`: Folders; `schedules`: Schedules; `emailnotifications`: Notifications; `sslcertificate`: SSL Certificate |
| Connectors | `app/connectors` | `webpublishing`: Web Publishing; `fmdapi`: FileMaker Data API; `plugins`: Plug-ins; `xdbc`: ODBC / JDBC; `fmodata`: OData; `clarismcp`: Claris MCP (`standbyserver` exists but is not listed) |
| AI Services | `app/llm` | `opensourcellm`: Model Server; `llmkeys`: Keys (more once the model server is set up) |
| Administration | `app/administration` | `licenses`: FileMaker Licenses; `administrators`: Administrator; `restrictaccess`: Restrict Access; `adminroles`: Administrator Roles; `externalauthentication`: External Authentication |
| Logs | `app/logging` | `logviewer`: Log Viewer; `logsettings`: Log Settings |

## 3. Application shell

**Sign-in page**

- A centered wordmark, with "ADMIN CONSOLE" beneath it.
- Two stacked 320x41 inputs: User Name, then Password.
- A 260x38 blue Sign In button, disabled until both inputs are filled.
- A "Password Help" link.
- The standard footer.

**Signed-in pages**, top to bottom:

1. **Top bar.** Height 60 px with a 1 px `#dddddd` bottom border, on `#f6f6f6`.
   Its content is padded 0 30 px.
   - The wordmark on the left links to the dashboard.
   - On the right, in order:
     - The server's date and time, for example "Thursday, October 29, 2026 at
       17:55". It is 13 px, `#222`.
     - A small "SERVER" pill: 8.45 px uppercase white text on `#aaaaaa`,
       radius 2 px. It switches the clock between server time and the
       browser's local time, and only changes the display.
     - A divider.
     - A bell button with a blue count badge (white on `#2f54e5`, radius 3 px).
     - A divider.
     - "Sign Out" as a blue text button.
2. **Tab row.** Height 56 px with a 1 px `#dddddd` bottom border, on `#f6f6f6`.
   - The tabs are 13.65 px, weight 500, color `#222`, with padding 20 px
     20 px 12 px and 10 px between tabs.
   - The active tab has a 2 px solid `#222` underline and no color change.
3. **Content.**
   - A 20 px gutter. The content starts 23 px below the tab row.
   - The Dashboard is full width.
   - Every other tab has two columns:
     - A 350 px sidebar with a 1 px `#dddddd` right border running the full
       height. It holds a title, an intro paragraph, then the sub-view list.
     - The view's content, padded 25 px on the left.
4. **Footer.** Centered, underlined vendor links separated by light "|"
   characters, then a gray copyright line. The Help link carries the page's
   help id.

The bell opens a 480 px panel right-aligned under it, with an up-pointer.
Each alert shows:

- a gray timestamp
- a severity icon and the message
- an "OK" link that acknowledges it

The panel ends with "Showing n of m". For example, it reported a database that
cannot be opened because it lacks the required password protection.

Sidebar sub-view items are 55 px tall, in Backups as in the other tabs: their
borders are 54.6 to 55.3 px apart in screenshots 13 (Backups), 17
(Configuration) and 70 (Logs). Each has a 1 px
`#dddddd` bottom border and a 20x20 line icon at 15 px from the left. The label
is 13 px `#222` at 46.8 px from the left. The selected item has an `#eceff3`
background.

## 4. Pages

### 4.1 Dashboard

1. **System Overview**
   - Two large counters with a divider between them:
     - **Total Connections**.
     - **Hosted Databases**, as "open / total", for example "118 / 118". The
       "/ total" part is smaller.
     - The numbers are 29.9 px, weight 300. The captions are 13 px `#888`.
   - Four client-type chips, each with a count and a small gray
     "Connections" caption:
     - **FileMaker Pro**, `#1d2e6b`, with a help icon: clients that count
       against the license's client limit.
     - **FileMaker Go**, `#2440a8`.
     - **FileMaker WebDirect**, `#2a4bc7`.
     - **Additional**, `#2f54e5`, with a help icon: clients that do not count
       against the limit.
     - The chips are 11.7 px white text, padded 4 px 10 px, with square
       corners.
   - Three columns of label/value pairs, with labels 13 px `#888` and values
     13 px `#222`:

     | Column 1 | Column 2 | Column 3 |
     |---|---|---|
     | Server Name | FileMaker Server License Expiration | FileMaker Data API Connections |
     | Server IP Addresses | SSL Certificate: "Expires on <UTC time>" and a "Details" link to Configuration > SSL Certificate | ODBC and JDBC Connections: "n / Unrestricted" |
     | Server Version | Last Backup: "<schedule> (<date>, <time>)" | FileMaker Script Engine Connections |
2. **Volume Status** (left third)
   - A doughnut chart with the percentage used in its center, captioned
     "Space Used".
   - Then the rows Total Space, Space Used (blue square) and Space Remaining
     (light gray square), with values right-aligned.
3. **System Statistics** (right two thirds)
   - One line chart: smooth, a thin blue line with black point dots and a faint
     fill. It has ten samples one minute apart, labeled `HH:MM:SS`, and the x
     axis is titled "Time".
   - Text tabs below it pick the chart. The active tab is gray, the others are
     blue links:

     | Tab | Chart title | Y axis |
     |---|---|---|
     | CPU | CPU Usage | Percentage |
     | Memory | Memory Usage | Percentage |
     | Network | Network Throughput | Bytes per Second |
     | Disk | Disk Throughput | MB per Second |

   - The y axis starts at zero. CPU's ran 0-7 by 1 for a largest value of
     about 6.7, and memory's 0-16 by 2 for about 14.3. The network and disk
     charts' axes (0-160,000 and 0-1,000) stood far above their values, for
     a reason not known.

Everything on the dashboard updates in place without a reload.

### 4.2 Databases (`app/activities`)

**Left sidebar: "Databases"**

- A search box (140x32) and "Sort by" with a blue dropdown: Filename, Status,
  Clients or Folder.
- A table of 60 px rows. The first row is the folder filter and is selected by
  default with an `#eceff3` background. It shows:
  - a folder icon
  - a "All Databases ▾" select with a count
  - a circled-chevron menu
- The folder select lists All Databases, then the default database folder and
  its subfolders, indented, then any additional database folders.
- The chevron menu holds Open All, Pause All, Resume All, Close All, Remove All,
  Verify All, Flush All, Clear All Encryption Passwords, and Show/Hide Extended
  Privileges. Items are enabled from the files' states. With every file open,
  Open All, Resume All, Remove All and Clear All are disabled.
- One row per hosted file, alphabetical by default:
  - a status icon, with a tooltip naming the status
  - an encryption icon (lock, or lock with an orange slash)
  - the file name without its extension
  - "n Clients" in gray
  - a circled-chevron menu
- The per-file menu holds Open, Pause, Close, Download, Remove, Verify, Flush
  and Clear Encryption Password. Items are enabled from the file's state:
  - An open file disables Open, Download and Remove.
  - An unencrypted file disables Clear Encryption Password.
- Show Extended Privileges adds a line of small blue tags under each name with
  the file's enabled extended privileges (`FMAPP`, `FMXML`, `FMPHP`, `FMXDBC`,
  `FMWEBDIRECT`, `FMREST`, `FMODATA`).
- Clicking a file row selects it and filters the Clients list to that file.

**Right panel: "Clients"**

- The toolbar holds outlined Disconnect and Send Message buttons, enabled once a
  client is checked. On the right are a search box and "Sort by" with Session
  Identifier, Client or Sign-in.
- The columns are:
  - a select-all checkbox
  - Session Identifier (the user name the client reports)
  - Databases (a count, with a tooltip listing the file names)
  - Host Name / IP Address
  - Sign-in (relative, such as "3 hours ago", with the absolute time in a
    tooltip)
  - Client (a 75x24 bordered tag such as "Pro", with a tooltip such as
    "Pro 26.0.3")
- With one file selected, the Databases column becomes Account Name.
- The empty state is "No clients are connected."
- The list updates in place as clients come and go.

**Labels and dialogs from the string table.** The Send Message, Disconnect and
Close dialogs were not opened live; the string table gives their text:

- **Send Message**: titled "Send Message", with "Enter the message to send to
  FileMaker clients." and a message.
- **Disconnect**: titled "Disconnect Client", with the same instruction, a
  message, and "Delay Time (minutes)" with the placeholder "Minutes". The
  Admin API takes the delay in seconds (`graceTime`, 0-3600, section 8); that
  the dialog's minutes become it, so 0 to 60 minutes, is inferred.
- **Close**, from a file's menu or the folder row's: "Close Database" or "Close
  All Databases", with the same instruction and fields.
- The dialogs' buttons are OK and Cancel.
- A file's menu also has Resume, presumably in Pause's place on a paused file
  (every file was open).
- A file's status tooltip is one of Opening, Closing, Pausing, Resuming,
  Checking, Flushing, Open, Paused, Closed, Exclusive or Invalid.
- The Client column's tags by client type: Pro (also for Pro Advanced),
  Runtime, Server, CWP, Go, XDBC, FMSE, WebDirect, Data API and Other.
- A search adds "Showing n results" and "Clear search results". A client search
  that matches nothing reads "No clients are connected.", the empty list's
  text.

### 4.3 Backups

What a backup is on disk, how a run proceeds and what it logs is in
[backups.md](backups.md).

**Sidebar**

- An explanatory paragraph.
- **Backup Schedule** select, with that schedule's Repeat (for example "Daily
  at Midnight") and Keep (for example 7).
- **Next Backup**, with a "Back Up Now" link.
- **Backup Status**, for example "No Backup Running", with a "Cancel" link that
  is enabled only while a backup runs.
- Sub-views:
  - "Auto-Backup and On-Demand (n)"
  - "Preserved (n)"
  - "Backup Settings"
- Measured (screenshot 13): the select is 163x34 px, 13 px below the
  paragraph's last line, with its label beside it. Repeat and Keep, Next
  Backup and Backup Status follow it 20, 22 and 10 px apart, each a gray
  label over its value. Keep, Back Up Now and Cancel are right-aligned 11 px
  from the sidebar's border. The sub-views start 39 px below the status.

**Auto-Backup and On-Demand**

- Preserve Backup and Edit Label buttons.
- A "Showing" schedule filter (All Schedules or a schedule name) and a
  "Filter by Label" box.
- Columns: Folder Name (`<schedule>_YYYY-MM-DD_HHMM`), Time (relative), Label
  ("Auto-backup" or "On-demand", editable) and Preserved.
- Footer: "Showing n of m".
- Measured (screenshot 13): the buttons and the 290 px select are top-aligned,
  the label filter is 166 px wide, and the table starts 38 px below them. The
  header is 36 px, rows are 61 px apart, and the "Showing" row is 56 px with a
  bottom border. Taking the table's left edge as 0, the checkbox is at 13 px
  and the columns' text starts at 42, 282, 503 and 723 px.

**Preserved**: the same layout with Edit Label and Delete buttons. The empty
state is "No preserved backups were found."

**Backup Settings** has three rows, each showing a status beside its control:

- Progressive Backup: Enabled or Disabled, with a "Change" link to Folders.
- Parallel Backup: toggle.
- Keep Cancelled Backups: toggle.

### 4.4 Remote Backups

This is a Claris cloud service: drives in Claris Customer Console that the
server backs up to.

- Buttons: Assign Drive, Connect Drive, Unassign Drive, and Manage Storage in
  Claris Customer Console.
- Columns: Drive Name, Drive ID, Drive Space Used, Approval Status and
  Connection Status.

### 4.5 Configuration

Settings pages share one row pattern:

- **Section header.** 13.65 px, weight 500, with a 1 px `#dddddd` underline,
  5 px below the text and 20 px above the next element.
- **Rows.** 38 px apart, also with a link, in three columns:
  - the label, 13 px `#888`
  - the current value, 13 px `#222`
  - an action: a blue "Change" or "Copy" link, a "Change ⌄" preset menu, or a
    toggle switch
  - Each column is a third of the section's width plus a 19.5 px gutter,
    so the row reaches past the section's underline: at 1440x1000 the
    values start 345 px and the actions 690 px from the label. A link's
    text and a toggle start 13 px into the action column (measured from
    the screenshots, to about 1 px).
- **No page-level Save.** A toggle saves at once. "Change" opens either an
  inline editor (an input with small Save and Cancel buttons replacing the
  value and action columns) or a menu of presets plus "Custom".
- **Disabled rows.** The row is drawn at about 40% opacity with a gray label.

**General Settings**

- Server Information:

  | Row | Value | Action |
  |---|---|---|
  | Database Server | Running | "Stop Database Server" |
  | Start Time | long date and time | - |
  | Server Name | name | "Change", inline editor, at most 100 characters |
  | Server ID | 64 hex digits, ellipsized | "Copy" |
  | Server IP Address | address | - |
  | Server Version | version | - |
  | Display Language | language | "Change ⌄" with six languages |

- Startup Settings, one toggle each:
  - Automatically open databases that are in the database folders
  - Only open last opened databases
  - Automatically start Database Server
  - Automatically start Web Publishing Engine
  - Automatically start the AI model server
  - Persistent Cache, and Persistent Cache Sync to Disk, which is enabled only
    with Persistent Cache
  - Database Server Auto Restart
  - Block New Users: refuses new connections to hosted files and leaves current
    sessions alone

**FileMaker Clients**

- Session Timeout:

  | Row | Presets ("Change ⌄") |
  |---|---|
  | FileMaker Pro and FileMaker Go | No timeout, 10, 15, 20 minutes, Custom |
  | FileMaker WebDirect | 10, 15, 20 minutes, Custom |
  | Timeout dialog duration for FileMaker WebDirect | 10, 30, 60 seconds, Custom |

- Filter Databases: one toggle. When on, each client lists only the files its
  account can open.
- Networking: an HTTPS Tunneling toggle. When on, clients use port 443 instead
  of 5003.
- Script Engine: the number of Script Engine processes.

**Folders**

- The columns are a quarter, a half and a quarter (with the same gutter):
  the paths start 259 px and the actions 776 px from the label.
- Paths are shown in FileMaker path syntax, such as `filelinux:/...`,
  `filemac:/...` or `filewin:/...`, and end with `/`.
- Database Folders: Default and Secure database folder paths, each with "Copy".
- Additional Database Folders 1 and 2: toggles. Turning one on reveals a path
  editor.
- Backup Folders: the Default Backup Folder Path with "Change", and a
  Progressive Backup toggle.

**Schedules**

- An ag-grid with checkbox selection. Its toolbar:
  - Create Schedule (always enabled).
  - Duplicate, Delete, Run Now and Edit, enabled once a schedule is selected.
  - Reset All Filters, plus Save and Load, which store the grid's filter
    layout.
  - A gear for Column Settings.
- Columns: Schedule Type, Schedule Name, Database, Last Run, Last Status,
  Scheduled Time, User Name, and Enabled (a toggle in each row). Token Expires
  is hidden by default.
- The grid (1440x1000, measured from the screenshot): a 26 px header on
  `#fafafa` without a top border, 48 px rows, `#e2e2e2` row borders, 18 px
  checkboxes with a light gray border. After the checkbox column the
  columns are of equal width, about 120 px, and a cell too long for its
  column is cut short with an ellipsis (dates included). The toolbar's
  buttons are at least 90 px wide.
- Values observed:
  - Database: `file:<file name>` for a FileMaker Script schedule (the Admin
    API's `resource` has the same form), `N/A` for a System Script.
  - Last Run and Scheduled Time: `MM/DD/YYYY HH:MM`; Scheduled Time reads
    "Disabled" for a disabled schedule.
  - Last Status: "Succeeded" or "Failed".
  - User Name: the account, or "Default" for a System Script on the
    default account.
  - The string table also has "Running..." and the empty grid's "There are
    no schedules." (not observed).
- The server's default backup schedule, which the Backups tab shows, is
  not listed.
- Create Schedule is a modal with these fields:
  - **Every schedule:** type (FileMaker Script, System Script, Script Sequence,
    Verify or Backup), name (at most 100 characters), and Frequency (Once,
    Daily, Weekly or Every N Days). Also a start date, a start time
    (HH:MM selects), "Repeat every n minutes or hours", and an end time.
  - **FileMaker Script:** Use Guest Account (or an account name and password),
    Database, Script Name (from the chosen file), an optional parameter, and an
    optional Timeout (minutes) with "End at timeout".
  - **Verify:** which databases (All, In Folder or one), a delay, and a message
    to clients.
  - **Backup:** target folder, which databases, number to keep, and Verify,
    Clone and Clone Only.

**Notifications**

- A Local Notifications toggle.
- A Send Email Notifications toggle. Turning it on reveals SMTP settings: From,
  Reply-to and To addresses, server, port, authentication (password or OAuth
  2.0), SSL/TLS, and "Do not verify the server certificate". It also offers Test
  Email Settings.

**SSL Certificate**

- An intro paragraph, then outlined buttons: Create CSR, Request Let's Encrypt
  Certificate, Import Custom Certificate and Delete Files.
- Certificate Information: Status ("Certificate installed"), Issued To,
  Issued By and Expires, the time in UTC: `2027-01-15 08:30:00 UTC`. The
  string table also has "Expired On", "Expires In", "day", "days",
  "Expires today" and "Warning:" (where they appear is not observed).
- Certificate Details: Country, Organization and Name (the subject's
  common name); the string table also has State/Province and Locality.
- A Let's Encrypt section, for certificates it issued: renewal status and
  Renew Certificate.
- Create CSR asks for:
  - Domain Name, Organization, Organizational Unit, City, State or Province,
    and a two-letter Country
  - a password for the private key it writes
  - the curve name: None means RSA, otherwise prime256v1, secp384r1 or secp521r1
- Import asks for the signed certificate, the private key, an optional
  intermediate certificate, and the private key password.

### 4.6 Connectors

Each connector page is a section of toggles:

| Page | Controls |
|---|---|
| Web Publishing | WebDirect toggle, Launch Center URL, Nginx Load Balancer URL, Custom Homepage, Custom Home URL, Accessible Keyboard Navigation; Web Publishing Engine totals and one block per machine |
| FileMaker Data API | toggle and a documentation link |
| Plug-ins | Script Engine, Web Publishing and Data API plug-in toggles, each with "Install Plug-in File Script Step" |
| ODBC / JDBC | toggle; "Connections n / Unrestricted" |
| OData | toggle |
| Claris MCP | a pointer to Claris's cloud |

### 4.7 AI Services

- **Model Server**: status and an enable toggle.
- **Keys**: a list of API keys (Name, Created, Expires) with Create Key and
  Delete.

### 4.8 Administration

**FileMaker Licenses**

- License Key: masked, with "Show License Key".
- Product Name, with "Import License Certificate".
- License Type.
- Users: the client limit, with "Buy More".
- ODBC/JDBC Client Connections.
- "Sync License" and "Go to Claris Studio".

**Administrator**

- User Name and Password, each with "Change".
- Contact Information, used in alert emails.
- A link to the Admin API documentation.
- FileMaker Admin API Public Keys: Add Public Key and Delete, a list of Name,
  Date Added and Last Accessed, and the empty state "No Public Keys".

**Restrict Access**

- One toggle that limits console and Admin API access to listed IP addresses.
  The primary machine is always allowed.

**Administrator Roles**

- Create Role, Delete, and "Save or Load ⌄".
- A list of Role Name and Role Folder.
- Create Role adds an inline editor row, not a modal. It asks for:
  - a name (at most 31 characters)
  - a database folder
  - a password
  - privileges: databases, schedules or logs
  - an external group

**External Authentication**

- Identity provider settings: Amazon, Google, Microsoft, Apple, AD FS, a custom
  OAuth provider, and a directory service.
- Toggles that allow external sign-in to the console and to databases.

### 4.9 Logs

**Log Viewer**

- Download, Download All Logs, Reset All Filters and Collapse Side Bar.
- An Interactive View toggle, which merges several logs by timestamp.
- A log file picker:
  - `Access.log`, `Event.log`, `Stats.log`, `TopCallStats.log`
  - `fac.log`, `fmdapi.log`, `fmodata.log`, `fmshelper.log`
  - `nginx-access.log`, `scriptEvent.log`
- An ag-grid of the chosen file. `Event.log` has the columns Timestamp, Level,
  Error, Host and Message. Rows are loaded in blocks of "Block Size" rows.
  The files themselves are described in [server-logs.md](server-logs.md).
- Before a file is chosen, the grid shows "No data to display".
- Measured (screenshots 70 and 72): a rule 5 px below the buttons. Interactive
  View and Log Files are 41 px apart, and the grid's 26 px white header starts
  10 px below Log Files. A one-line row is 49 px; a long message wraps onto
  lines 47 px apart, while no other column wrapped. Taking the grid's left
  edge as 0, the columns' text starts at 25, 216, 351, 465 and 576 px. Before
  a file is chosen, the grid's empty body is 749 px tall, with its message in
  the middle.
- The file menu ("Select") is 219 px wide, centered under its link, with
  32 px items in the accent color.

**Log Settings**

- Logging Options: Top Call Statistics, Server Statistics and Script Log to
  Event Log.
- FileMaker Script Log Options: Script Engine, Data API and Web Publishing
  script logs.
- Log Viewer Settings: Block Size, for example 200 rows.

## 5. Interaction patterns

- **Immediate saves.** Toggles take effect when clicked. Some changes restart a
  component, such as WebDirect or the Data API, and their help text says so.
- **Modals** hold complex or destructive actions:
  - The content is 660 px wide, white, with a 1 px `#ccc` border, radius 5 px
    and padding 25 px 30 px. The title is 16.9 px and an X closes it.
  - Forms have two columns. Required labels end in a red asterisk.
  - Footer buttons are left-aligned: the primary action first, disabled until
    the form is valid, then Cancel.
  - The backdrop is white at 0.7 opacity, not dark.
- **Lists.**
  - Checkbox selection enables the toolbar buttons that act on the selection.
  - An empty list shows a centered gray message.
  - Long lists end with "Showing n of m".
- **Help.** A blue circled "?" opens a popover of about 360 px with the text
  and a help link.
- **Times.** Recent times are relative, with the absolute time in a tooltip.

## 6. Visual system

| Element | Value |
|---|---|
| Font | `Helvetica, Arial, sans-serif` family; 13 px, weight 500, line height 22 px, letter spacing 0.195 px; controls weight 400 |
| Text | primary `#222222`, secondary and labels `#888888` |
| Accent | `#2f54e5`: links, link buttons, enabled outlined buttons, toggles, badges, tags |
| Chrome | `#f6f6f6` bars; borders `#dddddd`; input borders `#cccccc`; table header `#fafafa`; selection `#eceff3`; grid borders `#e2e2e2`; required `#ff0000` |
| Titles | page title 16.25 px/500 (42 px box); section header 13.65 px/500; modal title 16.9 px/500; table header 12.35 px/400 `#888` |
| Outlined button | 13 px/400, padding 3.9 px 13 px, about 29 px tall, radius 3.25 px, transparent; enabled `#2f54e5` text and border; disabled `#888` text, `#ccc` border; 6.5 px apart; modal buttons 120x32 |
| Link button | `#2f54e5` 13 px/400, padding 5.2 px 13 px, no border; a dropdown variant adds a small chevron |
| Input | 32-37 px tall, 1 px `#ccc`, radius 3.25 px, padding 7.8 px 11.7 px; search boxes 140x32 |
| Toggle | 35x15 pill with a 1 px `#2f54e5` border, radius 10 px; off: transparent track, 9 px `#2f54e5` knob on the left; on: `#2f54e5` track, white knob on the right; 0.3 s transition |
| Table | header 35 px; rows 54-61 px with a 1 px `#dddddd` bottom border |
| Dropdown menu | white, 1 px border, up-pointer; items 32 px tall, padded 5.2 px 19.5 px; enabled items `#2f54e5`, disabled light gray |
| Icons | 20 px line icons in the accent color (sidebar); 18 px status icons; 16 px help icon |

## 7. Database and client states

The console and the Admin API use these values:

- **Server:** `RUNNING` or `STOPPED`.
- **Database:** `NORMAL`, `CLOSED` and `PAUSED`, plus transitions such as
  `OPENING` and `CLOSING`. The console shows them as status icons: open,
  closed, paused, in progress, or out of sync for a standby server.
- **Database operations:**
  - `OPENED` (with an encryption key and whether to save it)
  - `PAUSED`, `RESUMED`, `CLOSED`, `FLUSHED`, `PRESNAPSHOT`
  - Closing takes an optional message and a delay for connected clients.
- **Client type** (Admin API `appType`): `FMPRO`, `PROADVANCED`, `FMGO`,
  `FMWEBDIRECT`, `FMSE`, `FMDAPI`, `XDBC`, `CWP`, `RUNTIME`, `SERVER`, or
  empty (section 8.1). The counts are per family: Pro, Go, WebDirect,
  concurrent, Script Engine, ODBC/JDBC, and other. The version string, for
  example `Pro 26.0.3`, is what the client sends in `Connect`
  (network-protocol.md 5.2).
- **Client status:** `NORMAL`, `IDLE`, or `INVALID` for an unknown state
  (section 8.1).

## 8. The documented Admin API (v2)

Everything in this section before 8.1 is from the Admin API Reference, its
schemas and its examples; 8.1 is read in the console's server code. No
reply was compared with a live server's except for the actions of
section 9.

**Envelope.** Every reply is `{"response": {...}, "messages": [{"code": "0",
"text": "OK"}]}`. Errors use the same shape with an empty `response` and a
FileMaker code. The codes the reference lists:

| Code | HTTP | Text |
|---|---|---|
| `1700` | 404 | Resource doesn't exist |
| `1703` | 401 | Invalid username or password, or JSON Web Token |
| `1707` | 400 | Required parameter wasn't specified in the request; also `<name>: required parameter wasn't specified for this request` |
| `1708` | 400 | Parameter value is invalid.; also `<name>: parameter value is invalid.` |
| `1709` | 409 | Operation is invalid for the resource's current status |
| `1710` | 400 | JSON input isn't syntactically valid |
| `-1` | 500, 400 | Unknown error |

For the paths in the table below, the reference gives each `GET` 200 and
500 only; `PATCH` adds 400 or 409, `DELETE /clients/{id}` and
`POST /clients/{id}/message` 404. Other paths differ (`GET
/remotebackup/list` has 400).

**Authentication.** `POST /user/auth` with HTTP Basic credentials (or a
`PKI <token>` made by FileMaker's PKI scripts) returns `{"token": ...}`.
Later calls send `Authorization: Bearer <token>`. Basic or PKI credentials
on any other call also work: each opens a new API session, whose token the
reply carries in the `X-FM-Access-Token` header. `DELETE /user/auth/{token}`
signs out; it takes no `Authorization` header. A token expires after 15 idle
minutes, and at most 5 API sessions are open (section 1).

**Formats** (from the examples). Ids are strings (`"389"`). Times are
written `2020-04-19T07:09:11`, without a zone; that they are the server's
local time is inferred, as no example says. A schedule that has not run has
`lastRun` `0000-00-00T00:00:00`. `connectDuration` is `00:00:05`. Folders
are in FileMaker path syntax with a closing slash, and the optional ones are
arrays: `["disabled"]`, or `["enabled", "<path>"]`. `/server/metadata` adds
`timezone` (IANA) and `offset` (minutes between UTC and the server's time
zone). `/server/certificate/info`
answers `{"certificate": {"issuerName", "subjectName"}}`. A client's
`appType` is `FMPRO` with `appVersion` `ProAdvanced 18.0.1` and `extpriv`
`fmapp`; no other `appType` is shown. Schedule id 1 is reserved by the
server, so schedule paths take ids from 2 (the schema's minimum; a smaller
one is a parameter value the validator refuses).

**The subset the console's own features need:**

| Method and path | Purpose | Response fields |
|---|---|---|
| `GET /server/metadata` | identity | `serverName`, `serverID`, `serverIP`, `serverVersion`, `serverHostTime` |
| `GET /server/status`, `PATCH /server/status` | run state | `status` (`RUNNING`/`STOPPED`); PATCH also takes `messageText` (at most 200) and `graceTime` (0-3600 s, default 90) |
| `GET /server/config/general` | startup settings | `openDatabasesOnStart`, `onlyOpenLastOpenedDatabases`, `autostartDBS`, `cacheSize`, `maxFiles`, `maxProConnections`, `maxPSOS`, ... |
| `GET /databases` | hosted files | `databases[]` of `id`, `filename`, `folder` (FileMaker path), `status`, `clients`, `size`, `isEncrypted`, `enabledExtPrivileges[]`, `decryptHint`, `hasSavedDecryptKey`; plus `totalDBCount`, `openDBCount` |
| `PATCH /databases/{id}`, `PATCH /databases` | file operations | `status` per section 7; `key` and `saveKey` "allowed only if" `OPENED`; `messageText` (0-200 characters) "required and allowed only if" `CLOSED`, and `force` (close at once, disconnecting clients) allowed only with `CLOSED`; no `graceTime`. `PATCH /databases` takes only `status` and `force`. Replies `{"database": {"id", "filename", "status"}}`, or `{"databases": [...]}` for all; 409/1709 when the state forbids it |
| `GET /clients` | sessions | `clients[]` of `id`, `userName`, `computerName`, `ipaddress`, `macaddress`, `appType`, `appVersion`, `appLanguage`, `operatingSystem`, `connectTime`, `connectDuration`, `status`, `concurrent`, `teamLicensed`, `extpriv`, `guestFiles[]` (`id`, `filename`, `accountName`, `privsetName`, `groupName`); plus `fmproCount`, `fmgoCount`, `fmwebdCount`, `fmconcurrentCount`, `fmsaseCount`, `fmxdbcCount`, `fmmiscCount` |
| `DELETE /clients/{id}` | disconnect | query `messageText` (at most 200) and `graceTime` (0-3600 s, default 90); replies `{"client": {"id"}}`; 404/1700 for an unknown client |
| `POST /clients/{id}/message` | send a message | body `{"messageText"}`: 1-200 characters, not only whitespace; code 0 also for a FileMaker Data API client, which gets nothing |
| `GET /server/certificate/info` | SSL certificate | |
| `GET`/`PATCH /server/config/blocknewusers` | Block New Users | `blockNewUsers` |
| `GET`/`PATCH /fmclients/sessiontimeout/progo` | client timeout | |
| `GET /server/currentfoldersettings` | folders | `defaultDatabaseFolder`, `secureDatabaseFolder`, `additionalDatabaseFolder`, `additionalDatabaseFolder2`, `additionalRemoteContainerFolder`, `additionalRemoteContainerFolder2`, `backupPath`, `progressiveBackupFolder` |
| `GET /schedules`, `GET /schedules/{id}`, `PATCH /schedules/{id}` | schedules | `id`, `name`, `enabled`, `status` (`IDLE`/`RUNNING`), `lastError`, `lastRun`, `nextRun`, `sendEmail`, and one member naming the type and its task (`filemakerScriptType`, `backupType`, `verifyType`, `messageType`, `systemScriptType`, `scriptSequenceType`) and one its timing (`onceType`, `weeklyType`, `everyndaysType`). PATCH edits any of them or, with `status` `RUNNING`, runs it; at least one field |

The rest covers the other settings pages, such as `/license/*`,
`/server/config/*`, `/plugins/*`, `/extauth/*`, `/standby/*`, the
`/schedules/*` creators and `/modelserver/*`.

### 8.1 Answers the reference does not give (from bin)

Read in the console's server code (sources): `adminapi/v2/` (`index.js`,
`authenticators/`, `errors/`, `validators/generic.validator.js`,
`controllers/`, `converters/`, `shared/`), `middlewares/adminapi.js`,
`routes.js`, `routes/v1/databases/databases.controller.js`, the session
store and its expiry job (`store/states/session.state.js`,
`lib/scheduler/session-status.job.js`), and libFMSLib.

**Live response controls.** FileMaker Server 26.0.3.309 on Ubuntu,
2026-11-26. Database requests used one disposable database, observed
in `NORMAL` before the validation requests. Known endpoints used an
authenticated session except where noted. The table records the HTTP
status and FileMaker API message code [O].

| Request | HTTP | Code | Additional observation |
|---|---:|---|---|
| GET `/server/metadata` with no Authorization header | 400 | `1705` | No WWW-Authenticate header |
| GET an unknown v2 path with no Authorization header | 404 | `1700` | |
| PATCH `/server/metadata` | 405 | `1704` | |
| GET `/fmclients/fmseSessionsBeforeQueuing` | 500 | `1808` | |
| PATCH `/databases/{id}` with status PAUSED and an unknown `extra` member | 400 | `1706` | Database stayed NORMAL |
| PATCH `/databases/{id}` with status null | 400 | `1708` | Database stayed NORMAL |
| PATCH `/databases/{id}` with status CLOSED and no messageText | 200 | `0` | Database became CLOSED |

See [#163](https://github.com/OpenMaker-Dev/openmaker/issues/163) and
[#180](https://github.com/OpenMaker-Dev/openmaker/issues/180). The rest of
this section describes the server modules and binary unless marked as a
live observation.

**The pipeline.** A request under `/fmi/admin/api/v2` passes, in order:
swagger-tools' metadata step (the path and method looked up in the Swagger
document), the authenticator, swagger-tools' validator (types, enums,
required members, `Content-Type`), a check for parameters the document
does not define, the controller, and the error handler, which writes the
envelope. A path the document does not have skips all of them and reaches
`resourceNotFound`.

**Status per code.** Each code has one HTTP status (`ERROR_TO_STATUSCODE`
in `errors/error.definition.js`; texts from `error.texts.json`):

| Code | HTTP | Text |
|---|---|---|
| `0` | 200 | OK |
| `-1` | 500 | Unknown error |
| `214` | 403 | Too many login attempts |
| `956` | 401 | Maximum number of Admin API sessions exceeded |
| `1638` | 500 | Block new users is enabled |
| `1700` | 404 | Resource doesn't exist |
| `1701` | 503 | Host is currently unable to receive requests |
| `1702` | 400 | Authentication information wasn't provided in the correct format; verify the value of the Authorization header |
| `1703` | 401 | Invalid username or password, or JSON Web Token |
| `1704` | 405 | Resource doesn't support the specified HTTP verb |
| `1705` | 400 | Required HTTP header wasn't specified |
| `1706` | 400 | Parameter isn't supported |
| `1707` | 400 | Required parameter wasn't specified in the request |
| `1708` | 400 | Parameter value is invalid |
| `1709` | 409 | Operation is invalid for the resource's current status |
| `1710` | 400 | JSON input isn't syntactically valid |
| `1711` | 403 | Host's license has expired |
| `1713` | 500 | The API request is not supported for Cloud |
| `1808` | 500 | The API request is not supported on this platform |

The other codes (`826`, `1639`, `1712`, `1714`-`1717`, `10028`, `10029`,
`10603`, the administrator role and standby codes `1800`-`1853`, `8557`,
`20630`) are 400 or 500, and `1840` "Switchover started" is 200. A reply
whose messages map to different statuses is sent as 500. Errors from the
database server are translated (`FMS_ERROR_TO_API_ERROR`): 3 to `1701`; 9,
212 and 10026 to `1703`; 10 and 10800 to `1700`; 214 and 10025 to `214`;
956 to `956`; 958 to `1707`; 960 to `1708`; 1638 and 1639 to themselves;
10006, 10013 and 10904 to `1709`. An error number without a translation is
logged and the reply is a 500.

**Sessions and tokens.**

- A Basic or PKI sign-in (`POST /user/auth`, or Basic or PKI credentials on
  any other call) is refused while 5 Admin API sessions are open
  (`MAXIMUM_API_SESSIONS`): 401, code `956`. No session is ended to make
  room; calls with an existing token are not limited.
- Each call with a token records the session's last call time. A job that
  runs every minute (`00 */1 * * * *`) removes the Admin API sessions idle
  for more than 900 s, so an expired token still counts toward the limit for
  up to a minute.
- `/user/auth` takes only `POST` and `/user/auth/{access_token}` only
  `DELETE`; another method is 405, `1704`. A `Bearer` header on
  `POST /user/auth` is 400, `1702`.
- An invalid or expired token, or one whose session is gone, is 401, `1703`.

**The `Authorization` header.**

- Missing: 400, `1705`, text `Authorization: required header wasn't
  specified for this request`.
- The scheme is matched without regard to case: `Basic` and `PKI` (on
  FileMaker Server; Claris FileMaker Cloud takes `SSO` and `FMID` instead)
  and `Bearer`. Any other scheme is 400, `1702`.
- Wrong Basic credentials, as the database server answers them, are 401,
  `1703`, without a `WWW-Authenticate` header. Basic credentials that decode
  to an empty account name or password, or have no `:`, are 401, `1703`; the
  empty ones also carry `WWW-Authenticate: Basic realm="Users"`
  (passport-http's challenge, realm left at its default).

**Lockout.** The sign-in goes to the database server's administrator
authentication (`FMS::AdminAuthHelper::LoginWithLockoutCheck`, libFMSLib
`nm` 0x42e150). Bad attempts are counted per account name, lowercased
(`MakeBadAttemptID`), not per address. When the account has 5 or more bad
attempts and the latest is less than 5 minutes old (300,000 ms on a
monotonic clock, `FlushBadAttemptList`), the sign-in is refused before the
password is checked with error 10025: 403, code `214`, "Too many login
attempts". Attempts older than 5 minutes are dropped. The first five bad
attempts are 401, `1703`. That the console's own sign-in shares the count is
inference (it uses the same helper).

**Bodies and parameters.**

- A body member the Swagger document does not define is 400, `1706`,
  `<name>: parameter isn't supported`, one message per member, checked into
  nested objects unless the schema allows other members. Query parameters
  are checked the same way on operations that define any.
- `null` is a type error, not an absent member: 400, `1708`, `<path>:
  parameter value is invalid`. A missing required member is 400, `1707`,
  `<path>: required parameter wasn't specified for this request`; JSON that
  does not parse is 400, `1710`; a wrong `Content-Type` is 400, `1708` for
  `Content-Type`.
- `PATCH /databases/{id}`: `key` or `saveKey` with a status other than
  `OPENED`, or `messageText` or `force` with a status other than `CLOSED`,
  is 400, `1706`. `CLOSED` without `messageText` is accepted and closes with
  an empty message; only `status` is required.

**Paths and methods.**

- A path under `/fmi/admin/api/v2` that the document does not have: 404,
  `1700`, before any authentication. Under `/fmi/admin/api/v1`: 404 with a
  text pointing to version 2.
- A known path with a method it does not have: 405, `1704`.
- A feature the platform lacks: 500, `1808`. On FileMaker Server for Linux
  and macOS this is `GET` and `PATCH /fmclients/fmseSessionsBeforeQueuing`
  (Windows only, tested on `process.platform`). Features Claris FileMaker
  Cloud lacks (`/php/config`, `/xml/config`) answer 500, `1713` there.

**Clients.** `appType` is the database server's client type
(`WebServicesStrings::AppTypeToJsonString` over
`Draco::ApplicationInfo::AppType`) renamed by the console
(`customizeClientInfo`):

| Client | Server's name | `appType` |
|---|---|---|
| FileMaker Pro (`kPro`) | `PRO` | `FMPRO` |
| `kDeveloper` | `PROADVANCED` | `PROADVANCED` |
| FileMaker Go (`kMobile`) | `GO` | `FMGO` |
| WebDirect (`kIWP`) | `WEB` | `FMWEBDIRECT` |
| script engine (`kSASE`) | `SASE` | `FMSE` |
| Data API (`kWebIntgrPlat`) | `WIP` | `FMDAPI` |
| ODBC/JDBC (`kXDBC`) | `XDBC` | `XDBC` |
| Custom Web Publishing (`kCWP`) | `CWP` | `CWP` |
| runtime | `RUNTIME` | `RUNTIME` |
| server | `SERVER` | `SERVER` |
| OData, the admin server, others | `ODATA`, `OTHER` | empty string |

A client's `status` is `NORMAL`, `IDLE`, or `INVALID`
(`GuestStatusToJsonString` over `FMSTypes::eGuestStatus`). There is no
disconnecting state: a client being disconnected keeps its status until it
is gone (inference).

**Times.**

- `connectTime`: the database server sends UTC (`MM/DD/YYYY hh:mm:ss A`);
  the console converts it to its own process's time zone (the machine's,
  unless `TZ` is set) and writes `YYYY-MM-DDTHH:mm:ss` without a zone.
- `serverHostTime`: the current time in the operating system's zone as the
  console's system report gives it, `YYYY-MM-DDTHH:mm:ss`.
- `timezone`: the console process's IANA zone
  (`Intl.DateTimeFormat().resolvedOptions().timeZone`). `offset`:
  JavaScript's `getTimezoneOffset()`, in minutes, positive west of UTC (360
  for UTC-6).
- `lastRun` and `nextRun`: passed through from the database server's
  schedule records, which `ScheduleDateTimeToJsonString` prints with no zone
  conversion. They are in the server's local time (inference: schedules run
  on local time, backups.md 4). A date of `0000-00-00` becomes
  `0000-00-00T00:00:00`.

## 9. What connected clients see

FileMaker Pro 26.0.3 connected to the lab server while each action was sent
through the Admin API, 2026-11-11 (capture `2026-11-11-s07-admin-actions`;
the wire format of every message, with vectors, is in
[network-protocol.md](network-protocol.md) 6.5). The console offers the
same actions (sections 4.2 and 4.5) through its internal API; that it sends
the same messages is inferred, not measured.

| Action | What FileMaker Pro shows | What happens next |
|---|---|---|
| Send Message | "Message from “<server name>”", the text, Close, "(This dialog will be dismissed in N seconds.)" | the dialog closes itself after about 30 s; nothing else |
| Disconnect, with a message and `graceTime` | "Close File": "Files to close" (each file's name with its extension), "Messages" (the text), "(Will attempt automatic close in 30 seconds.)", Cancel, Close Now | at zero the client closes its files and the server lists it no more. If it does not, the server disconnects it when `graceTime` ends |
| Close a file, with a message | the same "Close File" dialog, for that file | the file is `CLOSING` until its clients have closed it. A client that does not is asked again after 45 s and disconnected after 90 s; the file is then `CLOSED` |
| either, when its time ends | "File has been forcefully disconnected by the host. All affected windows will be closed. [From Administrator: “<text>”]", OK | the client has signed out of the file and disconnected |
| Pause | nothing | the client's requests on the file wait, unanswered, until Resume; FileMaker Pro's window stops responding while one waits |
| Block New Users | when a client then opens a file: "The host is not allowing new connections. Try again later." (error 1638), OK | the file does not open |

The client's countdown is 30 s whatever the grace time (40 s, 60 s and
Close's default of 90 s were tried). The Admin API's Close takes no grace
time; the console's Close dialog offers one (section 7), sent another way.

## 10. Unknowns

- FileMaker Server 26.1.1 (announced in Claris's 26.1 launch webinar, not
  released on 2026-11-19): the Admin Console's settings for automatic switchover to a standby server
  (backups.md 7) and a WebDirect keyboard accessibility setting (WCAG 2.2
  AA); their screens, Admin API paths and defaults are unknown until it
  ships.

- What the Dashboard's Additional chip counts; its string's key calls it
  non-FLT Pro connections. Which client types count against the license.
- What the CPU, memory, network and disk figures measure, and how a chart
  picks its y axis when the axis stands far above the values.
- What an empty Delay Time in the Disconnect and Close dialogs means: no delay,
  or the Admin API's 90 s.
- Whether the folder row's All items act on every file or on those of the
  selected folder, and whether a folder shows its subfolders' files.
- What the Databases and Clients searches match, and the sort orders of
  Status, Clients, Folder and Sign-in.

- Whether a Disconnect with a long grace time asks the client again, as
  Close does after 45 s.
- When FileMaker Server ends the session of a client that closed its files
  when asked: 90 s after the action with a 60 s grace time, by a
  `Disconnect` call (network-protocol.md 6.5); the grace time plus 30 s, or
  60 s after the client closed its file.
- A grace time below the client's 30 s countdown, and `graceTime` 0.
- Disconnect for a client with no file open.
- The console's own Close delay: which call carries it.
- Send Message and Disconnect for a client with several files open: one
  message per file or one in all.
- Whether the Schedules grid lists backup schedules other than the
  server's default one, and what it shows for a schedule that has not run.
- When the SSL Certificate view shows "Expired On" and "Expires In".
- What Block New Users does to a client that is already connected and opens
  another file.
- What FileMaker Server answers an Open of a file that is closing, closed or
  paused, and when Close repeats its request for a grace time other than
  its default 90 s.
- Pause's effect on requests other than creating a record, and whether
  FileMaker Pro gives up on one that waits long enough.
- The Admin API's sixth-session response (401, `956` in the modules)
  and live client types with FileMaker Go and WebDirect connected.
  The lockout must not be tried on a shared administrator account:
  it refuses that account for 5 minutes.
- Whether a sign-in refused during a lockout counts as another bad attempt
  and so extends it.
- The zone of the database server's schedule times (`lastRun`, `nextRun`),
  and where the console's `serverHostTime` zone comes from when the
  machine's zone and the console process's `TZ` differ.
