# OpenMaker's desktop client

What `om-gui` builds of FileMaker Pro's application chrome and workspaces,
the choices it makes where FileMaker's behavior is not known, and its limits.
FileMaker Pro's measured chrome is specified in
[re/client-ui.md](re/client-ui.md); the colors, type and shapes OpenMaker's
own chrome uses are in [design.md](design.md). Related notes:
[recovery.md](recovery.md) (lost connections, reconnecting, save conflicts),
[desktop-files.md](desktop-files.md) (Open File),
[record-writes.md](record-writes.md) (hosted record writes),
[schema-manager.md](schema-manager.md) (Manage Database),
[scripting.md](scripting.md) (scripts, triggers, portals, web viewers) and
[conditional-visibility.md](conditional-visibility.md).

## Chrome

- The content area is always the file's own layout, decoded and rendered
  from the file (layout-model.md). A layout without decodable geometry gets
  a generic Form, List or Table view built from its fields and records;
  nothing is ever specific to one file.
- On macOS the menus are in the system menu bar, as FileMaker Pro's are:
  the menus of the window AppKit has in front, built each frame through the
  same code as the window's bar (`menubar::MenuRows`, `system_menu.rs`),
  with key equivalents on enabled rows. Editing, text insertion and zoom
  shortcuts go through the window's keyboard handling, where Current Date
  takes precedence over Zoom Out in an entered Browse field. A choice
  reaches the window whose menus showed it in the order made. Native Edit
  clicks act on a still-focused text editor of the current record. Portal
  and related-field editors use keyboard editing. Field and record
  commands are dropped if their target changes before they can run.
  A modal dialog disables that window's native rows. The app menu adds macOS's Services, Hide,
  Hide Others and Show All, and the Help menu the system's search field;
  that FileMaker Pro's app menu has the same is inferred. The windows draw
  no menu bar of their own. The Scripts menu reads the file's scripts once
  its window is in front, since the system menu says nothing before it
  opens; after a read that failed it asks again when the window next comes
  to the front. Elsewhere, and in the GUI tests, which run without a native
  window, the menu bar is drawn inside each window. The window title is the
  platform's own.
- Show/Hide Toolbars changes the status toolbar and layout bar together.
  Lock disables View > Status Toolbar and its shortcut in the current
  window until a script changes the lock. `Get ( StatusAreaState )` returns
  1 for shown, 0 for hidden, 2 for shown and locked, and 3 for hidden and
  locked. New windows inherit the current toolbar state, which is
  unmeasured in FileMaker. OpenMaker keeps the outer frame size when the
  bars change. The measured FileMaker Pro window changes its outer
  height ([#1064](https://github.com/OpenMaker-Dev/openmaker/issues/1064)).
- The Window menu's New Window runs the step. Show Window lists every open
  file's windows but cards, front first, a hidden one in parentheses, and
  scrolls when they do not fit; choosing one brings it to the front, shown
  with its cards if it was hidden (`OpenMakerApp::show_window`, which the
  list at the menu's end also runs), and tells the scripts' windows so,
  also while the file's connection is lost. FileMaker documents only that
  Show Window shows a hidden window by name; the rest is inferred, and
  reduced menus leave Show Window enabled (OpenMaker's choice, unmeasured:
  [#878](https://github.com/OpenMaker-Dev/openmaker/issues/878)).
  Hide Window, Tile, Cascade and Bring All To Front are disabled
  ([#54](https://github.com/OpenMaker-Dev/openmaker/issues/54)).
- The app menu is named OpenMaker. On macOS, `tools/package-macos.sh`
  puts the client in an app bundle, `OpenMaker.app`
  ([development.md](development.md#app-bundle-for-macos)), whose Info.plist
  names it OpenMaker in the menu bar, the Dock, Command-Tab and Finder. Run
  from the build with no bundle, its executable carries an Info.plist of
  its own (`build.rs`) whose `CFBundleName` the menu bar shows, but the Dock
  and Command-Tab show `om-gui`: LaunchServices names an unbundled process
  after its file
  ([#821](https://github.com/OpenMaker-Dev/openmaker/issues/821)). The
  Dock's icon is OpenMaker's logo ([assets/README.md](assets/README.md)):
  the bundle's, and once the app runs the one eframe sets, with or without
  a bundle. Every window carries the logo as its
  window icon: Windows shows it in title bars, and eframe also makes it
  the first window's large taskbar icon (other windows' taskbar icons are
  unchecked); X11 window managers that draw window icons show it. Wayland
  has no window icons: the shell shows the icon of the desktop entry
  matching the app id `openmaker`, which the Ubuntu package installs. Mode
  and other shortcuts use the platform's command key: ⌘ on macOS, Ctrl on
  Linux and Windows.
- The UI font is the bundled Inter (OFL), a cross-platform stand-in for the
  system font with similar metrics. Layout text uses the fonts the host
  sends. On macOS, a document's Arial or Helvetica Neue selection uses the
  installed family when available, with a bundled open font as the fallback.

## Opening files

The desktop starts on **Home**, laid out as FileMaker Pro 26.1.1's Home
([re/client-ui.md](re/client-ui.md#home-window)): **Create** and **Open**
in a sidebar, and **Favorites** and **Recent** tabs over a Name and
Location table. **Recent** lists the last 20 distinct files opened, most
recent first; **Favorites** the files pinned with a recent file's context
menu (Add to Favorites) or File > Favorites > Add Current File to
Favorites. A click selects a file, and a double-click or Return opens it.
Remove in a file's context menu, or Delete, takes it off the list; the file
itself stays. File > Favorites > Show Favorites and File > Recent > Show
Recent show the lists, and both submenus list their files. Home opens on
the list it showed last.

Home's Open and File > Open... show the system's open panel, asking it to
list only FileMaker files (`.fmp12`, in any case); Home's Create and
File > Create New... show the system's save panel after the Create an App
sheet ([new-files.md](new-files.md)). The panels are `file_panel`'s: on
macOS `NSOpenPanel` and `NSSavePanel`, beside the window; on Linux the
desktop's own file chooser (GTK's, GNOME's or KDE's), asked through the
xdg-desktop-portal FileChooser interface on the session bus, parented to
the window on X11 and unparented on Wayland. Windows shows none yet
([#826](https://github.com/OpenMaker-Dev/openmaker/issues/826)). Where no
panel can be shown (Windows, no portal on the session bus or one without a
file chooser, or no native window, as in the GUI tests) OpenMaker's own
chooser asks instead: a folder's subfolders and FileMaker files, and a
path that can be typed. A portal that does not take the request within
30 seconds, or leaves the bus before the user answers, counts as none.

What follows a choice is the same either way. Shift or Option (Alt) held
as Open is chosen, in Home or the File menu, asks for an account
([local-files](re/local-files.md) 3.1); so do those keys held as the
panel's own Open is clicked, which OpenMaker reads as the panel answers
(on Linux from the X server, so not on a Wayland session). While a panel is up, Open, Create and the other commands
that start from no file do nothing, Home takes no keys and, without
Home, Open Network File steps aside, as for OpenMaker's own choosers; an answer that comes
once a file is opening anyway is dropped. Both panels start in the user's
Documents folder (or home without one), as OpenMaker's own chooser does,
so macOS's open panel does not return to the folder last used. GTK's
chooser also offers "Open files read-only", which an unsandboxed app is
not told of: the file opens as it would from OpenMaker's chooser. Which
files FileMaker Pro's open panel lists, and where it starts, are
unmeasured.

A file on this computer opens at once. A hosted file asks for its sign-in
first: **Open “Name”**, with Account Name, Password and, when the provider
can send one, **Use saved FileMaker license**, then **Sign in**. Home keeps
paths, host names, ports, file names, the list shown and the saved hosts
(below) in the OS account's `settings.json`, never account names or
passwords. When that file cannot be read or written, the lists last until
OpenMaker quits: Home says so beside its tabs, and a pin, Remove or host
change that could not be saved shows an alert. A host Open Network File adds
is saved as its file opens; when it cannot be, its alert waits for any the
opening file shows, and stays until it is dismissed itself.

Under Home in the sidebar, **Hosts** lists the saved hosts, by name or
address; **+** adds one (Address, Port, and an optional Name the sidebar
shows instead of the address). Selecting a host shows its page, which lists
its files at once ("Hosts and their files", below): its name as the title,
its address and what it says it is beside a refresh button and **Search
Files**, and its files in Home's table. A click selects a file, and a
double-click or Return signs in to it. A host's context menu lists its
files again, opens **Open Network File...** with the host filled in, edits
the host (Edit Host...) or removes it (Remove Host, or Delete after
clicking it); removing a host forgets only the saved entry. File > Hosts >
Show Hosts shows the host shown last, or Hosts with **Add Host...** while
none is saved.

**Open Network File...** (File > Hosts, a host's context menu, and a host
page's empty or failed list) opens a file by its path,
`fmnet:/host[:port]/File`, read as a script's or related file's path is
(`om_data::file_refs`), with an IPv6 address in brackets
(`fmnet:/[::1]/File`, OpenMaker's own form): a line under the field says
which file on which host and port the path names, or what is wrong with it,
and **Open** waits until it names a host and a file. A host's address
cannot carry an account (`name@host`): the account goes in Account Name,
so none is saved with a host. It signs in as the sign-in does. A path
to a host Home does not have offers **Add host to Hosts**, off by default; the host is saved once the
file opens.
Searching a host page for a name it does not list offers **Open “Name”...**,
the same dialog with that file's path: a host can leave a file unlisted, and
it still opens by its name.

Where OpenMaker differs from FileMaker's Home, or chose where it is
unmeasured:

- **Hosts** takes the place of FileMaker Cloud and Resources in the
  sidebar, and its host pages take the place of FileMaker's Hosts window,
  which is a window of its own (capture 28) with a host field, the
  account, password and file along its bottom, and tiles. Selecting a host
  lists its files without a List Files step; a file's sign-in is a dialog
  of its own, as FileMaker asks for an account when it opens a file.
- Open Network File is OpenMaker's: how FileMaker Pro 26 opens a file a
  host does not list is unmeasured. It takes `fmnet:` paths only, not
  `fmp://` URLs, and has no shortcut (FileMaker gives Shift-Command-O to
  Show Hosts).
- The sign-in's default button reads **Sign in**, as FileMaker Pro's for a
  related file does; its wording for a hosted file is unmeasured.
- The header shows OpenMaker's name where FileMaker has the Claris logo, and
  files show OpenMaker's document glyph, or a server glyph for a hosted
  file, not the solution's icon.
- The colors measured in FileMaker's inactive window serve the active one
  too. A selected row is the accent with white text, as in a macOS table
  ([design.md](design.md)).
- The list is one Tab stop, as a macOS table is, and the arrow keys move
  its selection. Delete also removes from Recent, and an empty list says
  what fills it.
- Showing a list or opening a file from Home or the File menu closes the
  open file first, through its usual commit path and close triggers; a
  veto or a refused save stops it. With another file open, hidden ones
  included, a layout with unsaved changes, or a lost connection with
  unsaved record changes, nothing closes and an alert says why ("Other
  files are open. Close them first: Window > Show Window lists them,
  hidden ones too.", the Save Layout or Revert Layout alert, or Reconnect
  or File > Close). FileMaker shows Home in a window of its own and closes
  nothing, so it has no such alert: the wording is OpenMaker's. A file a
  script opened hidden (a close trigger's too) stays open behind Home once
  the visible file closes; Window > Show Window shows it in Home's place,
  and File > Close then closes it.
- A local file's location is its path, without FileMaker's `/Volumes/`
  prefix on macOS. A hosted file's location adds its port when that is not
  5003.
- The history keeps 20 files; FileMaker sets its count in its settings,
  which OpenMaker lacks. Add Current File to Favorites is unavailable for a
  file a script opened. Open Quickly remains disabled
  ([#56](https://github.com/OpenMaker-Dev/openmaker/issues/56)).

The GUI defaults to the native provider (`om-provider`). Opening a hosted
file performs verified TLS, Connect, Open and Login, then authenticated
downloads of the schema, layouts, themes, fonts and records. Each file's
node `[2]` (its locale and options) is read once, before Login, for the
login challenge, and kept for the calculation schema, as FileMaker Pro
reads it (network-protocol.md 5.6.9b). The provider
never substitutes sample records after a failure: sample data needs
`OPENMAKER_PROVIDER=mock` and is labeled in the window title and a banner.
`OPENMAKER_HOST`, `OPENMAKER_PORT`, `OPENMAKER_FILE` and `OPENMAKER_ACCOUNT`
prefill Open Network File, which a nonempty `OPENMAKER_HOST` opens over Home
at startup. The password is always entered separately. An app built without
Home (an embedding, most tests) shows Open Network File alone in its window
while no file is open, filled in with the last file opened. A local file opens through a
private embedded host (local-files.md).

A hosted file's related files open when the window first reads them, not
all at the open (#983; FileMaker Pro opened 19 of a 63-file solution's
files over a whole session, network-protocol.md 5.5). Files opened by a
script inherit the caller's demand policy, so preparing a destination's
first schema does not open its unrelated files before its window appears.
Its startup commands and transaction triggers use the schema grown by the
opening layout:
- The first calculation schema is the opened file's own. Each occurrence of
  another file's table waits for its file (`CalcSchema::is_pending`) and
  draws as loading, not as `<File Missing>`. A data source nothing reads
  gets no Open and no warning.
- Drawing a layout first opens the files of the occurrences it reads
  (`om_data::host::needs::layout_occurrences`). That covers its own
  occurrence, its fields and portals, its value lists' fields, the
  formulas of its objects that decide more than their look (hide
  condition, script parameter, web address, portal filter), the formulas
  of the calculation fields it shows (followed through the fields they
  name), and the relationship paths to them.
- An object's formulas that only change how it looks (conditional formats,
  tooltip, calculated label, placeholder, active segment) are evaluated
  only while the object is in the window's view, and a file only they
  read opens when one is drawn (#1120, network-protocol.md 5.5). An object
  with a hide condition is evaluated in or out of view.
- A calculation that reads a waiting occurrence anywhere else (a formula
  that builds a name, a custom function, a script, another layout) asks
  for its file and waits for it. So does one that reads only its table's
  or a field's name (`GetBaseTableName`, `GetFieldName`, #1123). A script
  waits as it runs, then reads again over the grown schema. A script step
  writing into a waiting occurrence's field (Set Field, and Set Field By
  Name with the occurrence's name) opens its file the same way and writes
  over the grown schema, rather than failing with 102 (#1149) [I:
  FileMaker Pro's opening for a write is unmeasured].
- The readers that read the schema directly wait the same way, rather
  than reading a waiting occurrence as a missing table or field (102,
  106) or as having no fields or records:
  - a value list of a related field's values;
  - `ExecuteSQL` over a related occurrence;
  - `FieldNames`, `FieldIDs` and the field Design functions;
  - a related set matched by scanning.

  Each asks for the occurrence's file, and its value is 1224 (data still
  to be fetched) until the evaluation runs again over the grown schema.
  `RelationInfo` asks for no file: it names every relationship's other
  side, and an unread one stays unknown.
  An occurrence also waits while its table's field catalog has not been
  read, even once its file is open (`CalcSchema::set_fields_pending`, for
  [#237](https://github.com/OpenMaker-Dev/openmaker/issues/237)'s lazy field catalogs). On demand, a related file reads only the
  requested tables' field catalogs at open. When a data source has
  alternative paths, its table requests follow the selected file, including
  an already-open file, so a first related find has its requested catalog.
  Hydration reuses an owning schema when all requested catalogs are already
  loaded; a pending catalog still rebuilds the file's copy even if its owner
  read it for a write first. A later read loads an unread catalog once and rebuilds both owner and
  imported schemas with stable IDs. Published import mappings, including
  unresolved formula references, are reserved across rebuilds so an earlier
  file's new references cannot consume a later file's IDs. Before planning
  a write, the provider reads the field catalogs its rules need, following
  dependencies exposed by each newly read catalog. Passive capability
  probes defer without those reads. A table-only request whose unread
  catalog has no occurrence through which to request it fails explicitly
  with error 3. Local files retain their eager loading.
- The calculations the provider evaluates itself over the opened file's
  schema do the same (#1125): a record page's unstored calculations and
  summaries, a find's criteria on unstored calculations, and value lists.
  Each opens the files it read, once per evaluation, and evaluates again.
  A find whose calculation still reads a file that did not open (its host
  not reached, another host's account not given) fails with 3 naming the
  file, rather than leaving records out as not matching [OpenMaker's
  choice; FileMaker Pro unmeasured]. A write to a table of the opened
  file opens the related files its rules read before it is planned
  (#1139, record-writes.md); the window's record capabilities, asked
  before a layout is drawn, do not, so a file only a write reads opens
  with the first write. A related file's own write planned for the
  opened file ([#39](https://github.com/OpenMaker-Dev/openmaker/issues/39)) opens, the same way, its own related files that its
  rules read, and reads them through the opened file's sessions to them
  (#1160).
- Each time, the schema grows from the opened file's own with no download.
  Its decoded root schema is kept before any related import and cloned for
  growth while root metadata is unchanged. Layout members (including absent
  ones), script catalog adoption and layout refresh discard that decoded
  copy; dropping the calculation schema, closing or reopening the file
  discards it with the root catalogs. Hydration still reads the raw root
  tree on each growth. This avoids repeated root decoding, not imports or
  record reads; startup savings have not been measured.
  The files already open import first, in the order they opened, so no id
  changes and what calculations read stays. The window takes the grown
  schema before the answer that needed it.
- Each file opens as before: on the opened file's connection with the
  account kept for its scripts' opens (#978); a file on another host with
  the account that host admitted, else the opened file's offered there
  once, else after a sign-in ([#272](https://github.com/OpenMaker-Dev/openmaker/issues/272), desktop-files.md). That host's account
  then stays, for that host and port only, until the file closes or
  reconnects, so its other files open when first read without asking
  again. A refused file is asked about when a layout needs it. A file
  whose host could not be reached waits, with its warning, and the next
  read that needs it tries again once 30 seconds have passed ([#229](https://github.com/OpenMaker-Dev/openmaker/issues/229)); a
  refused Login is not tried again.
- After authentication, a related file selects its tables `[3]`, custom
  functions `[16]`, data sources `[32]` and value lists `[33]` in one
  existing DownloadList request. Its login challenge `[2]` is reused; a
  joined file without that reply includes `[2]` in the selection. Field
  catalogs remain demand-driven and account/privilege paths stay narrow.
  If the root parent is denied with error 200, the same catalogs are read
  through their individual permitted paths, with no repeated Open/Login.
  This use of op92's sibling selection is inferred from its supported
  semantics; native root-catalog selection has not been compared with
  FileMaker (network-protocol.md 5.6.6b).
- A related file's open, whoever opens it, reads no font catalog `[25]`:
  only styled text of its records names its fonts (#1098). The first of
  its records or globals whose styled text names a font reads them, once
  for the file (through its owner provider). A failed read fails that
  table's read, rather than showing the text without its fonts. The file
  the user opened still reads its fonts with its layout catalogs.
- A file on this computer opens its related files all at once, as before,
  and other callers of the provider (`om-author`, OpenMaker Server's
  scripts) do too.

The opened file's own layouts are read the same way, a file on this
computer's as well (#1019; network-protocol.md 5.5, "The design catalogs
read at open"). The open
reads only the layout catalog's index `[4, 1]`, which is enough to list
every layout in the Layouts menu, by name and in order, and to answer
`LayoutNames`, `LayoutIDs` and `Get ( LayoutName )`. Each layout's member `[4, 5, L]` (its parts, objects and
object store) is read the first time something reads that layout, and the
calculation schema then grows with it:
- Showing a layout, or moving a window to it, reads its member first.
- A calculation that reads another layout's objects (`LayoutObjectNames`,
  `FieldNames`, `FieldIDs`, `FieldStyle`, `FieldRepetitions`,
  `FieldBounds`) asks for its member and waits for it (`CalcNeed::Layout`),
  as it does for a related file. One the provider computes itself (a
  record page's unstored calculation, a find's, a value list's, a lookup's
  relationship key) reads the members it asked for and is computed again.
  Record rules call no Design function, nor `Evaluate`.
- A layout the index lists whose member the host does not have is asked
  for once. It then reads as having no objects, and showing it fails as
  for a layout the host does not have.
- An edit of a layout (placing a button, the authoring operations) reads
  the members it changes fresh from the host, as before, so a catalog
  write never replaces a member that was not read.
- A catalog without an index, which FileMaker does not make, is read
  whole at the open, as before. `om-author` and OpenMaker Server's
  scripts read every layout whole, as before.

On a root with 300 layouts of 180 fields each, 1 ms each way to the host
(`startup_load`, cold, median of 3), the open received 169 KB instead of
37.8 MB, and the first layout drew in 448 ms instead of 2.7 s.

Its themes are read the same way (#1129):
- **At open:** the theme catalog's index `[6, 1]`.
- **When a layout is first decoded** to be shown, printed or measured: the member `[6, 5, T]` of the theme it names (its key `0b`). A member is read whole, once, and serves every layout that names that theme.
- **A theme the index does not list:** FileMaker's index lists every theme (layout-model.md 7.5), so this one is not asked for, and the layout decodes without a theme, as it did when the whole catalog was read.
- **An index with no member list,** which FileMaker does not make: the theme is asked for once. If the host has no such member, it is not asked for again until the layout catalogs are read again.
- **A catalog without an index:** read whole at the open, as before.

The calculation schema reads no theme. A layout decodes with the same styles as from the whole catalog. The font catalog `[25]`, which a theme's fonts name, is still read whole at open.

A hosted file's scripts are read the same way (#1052, #1053). Nothing at
open reads every script's steps; FileMaker Pro 26 reads the catalog's
index and the one script a file trigger runs (network-protocol.md 5.5).
- **The calculation schema** lists the scripts from the script catalog's
  index `[17, 1]`, the member cache of names and flags. `ScriptNames`,
  `ScriptIDs` and the Script Workspace's pickers read that list. Folders
  and folder ends are told by their flags. Every other entry is listed as
  a script, since the index does not show how a separator differs from a
  script [I].
- **Perform Script by name** (and a plug-in's script by name) takes its
  candidate from that list, confirmed by the script's own member: a
  script still of that name. A separator, a name changed or added since
  the index was read, or a member that cannot be read, is looked up in
  the whole catalog read then, as before.
- **A script that runs** (a file or object trigger's, a button's, Perform
  Script, nested or in another file) is read alone, its member
  `[17, 5, S]` (`DataProvider::script`), once per script session. A
  separator read so is still not run, and a script the file does not
  have, or that cannot be read, ends with error 104, as before.
- **The Scripts menu** reads the whole catalog the first time it is
  opened, showing "Loading Scripts…" until it is in, so its separators
  and "Include in Menu" flags are the catalog's own. So does a file a
  script opens. That read also serves the schema's later builds, which
  are built again when it holds a separator.
- **The Script Workspace and every catalog save** read the whole catalog,
  as before.
- **Without an index,** which FileMaker does not make, the schema reads
  the whole catalog, as do `om-author` and OpenMaker Server's scripts.

Limits: a value the provider computes itself (a record page's unstored
calculation) that reaches a waiting file only through text it builds reads
as missing until the window asks for that file. The data sources of a
related file that name nothing this client opens are still reported when
that file opens. After a reconnect the files open again in the order the
window reads them, so their ids may differ from before, and the window
reads everything again, as for any reconnect.

### Hosts and their files

A host's page lists the files of its host and port. A host
with OpenMaker's own file list extension, which OpenMaker Server has
([hosted-file-list.md](hosted-file-list.md)), lists through it. Any other
host, FileMaker Server above all, is asked with FileMaker's own GetFileList
(op77), anonymously, as FileMaker Pro's Hosts window asks it
(network-protocol.md 5.3). The request is the same 70 bytes, and each file is
named by its entry's `10`. A filename can always be typed directly. Each listing connects on its own, on a thread of its own, with the
certificate checks of opening a file and without the client license, and
disconnects when it is done, failed or canceled. It never uses or waits for
an open file's connection.

The page shows what came back, for that host and port only:

- the published files, alphabetically, without their `.fmp12`, with the
  host's name and version beside its address; **Search Files** filters
  them. Opening still goes through the sign-in: listing a file grants
  nothing;
- **This host lists no files** for a host that publishes none (OpenMaker
  Server's default), with **Open Network File...**;
- **OpenMaker cannot list this host’s files** for a host that answers
  neither the extension nor op77 with a list, or that does not answer as a
  FileMaker host;
- the host's reason when its op77 reply refuses the list:
  - sign-in required (18), which OpenMaker does not send yet;
  - not accepting new connections (1638, 812);
  - another FileMaker error, by its number;
- **Could not connect to this host** when it cannot be reached or trusted;
- **This host’s file list could not be read** for a malformed or
  incomplete list: an op77 tree that does not decode, names fewer files
  than its count, or runs past 64 MiB over its pages (how FileMaker Server
  pages a long list is unmeasured). No partial list is shown.

Each failure names the host and port and the cause, with **Open Network
File...** to open a file by name. A failure is not an alert, and neither it
nor a list changes an opening file or a window. The refresh button and the
host's List Files ask again. Leaving a host's page stops a listing under way,
and a reply to a listing no longer waited for is dropped unseen; a list that
came back shows again when its page does, until another host is listed. The
wording is OpenMaker's; FileMaker Pro's is not captured.

Opening a file the host refuses because it blocks new users (Open status
1638) shows FileMaker Pro's alert, "The host is not allowing new
connections. Try again later.", with OK, its error 1638
(network-protocol.md 5.6.4; `om_provider::HOST_BLOCKS_NEW_USERS`). Other
refused opens keep OpenMaker's wording.

### Messages from the host

A hosted file's administrator can send its sessions a message (FileMaker
Server's Send Message, OpenMaker Server's `ServerHandle::send_message`;
network-protocol.md 6.5). The window shows it as FileMaker Pro does: a
dialog "Message from “<server name>”" with the text, Close and "(This
dialog will be dismissed in N seconds.)", which closes itself after 30
seconds. Messages that come meanwhile follow, one at a time. A message
waits while another dialog, alert or confirmation is open (Settings, a
script's dialog, Delete Layout's question, a related file's sign-in, a
required password change), its countdown not started, so
its Close, Return and Escape are its own [I: FileMaker Pro shows it over
them]. The providers of every open file read their hosts' calls once a
second between requests (`DataProvider::take_host_messages`), so a message
shows within about a second, not at the session's next exchange. That read never
waits behind another exchange on the connection (a Tickle waiting for a
slow host), reads at most 32 messages, each for at most 5 milliseconds
however slowly it arrives, and writes its answers within 2 seconds or
loses the connection; the rest waits for the next second.

When the host disconnects a file (`0x0870`: an administrator's close or
disconnect whose time ran out), that open of the file is lost at once to
every handle holding it, related readers included, when the connection
reads the notice: the idle read, any request, a Tickle. A request for it
does not go out, whenever it was built. One op62 release follows from the
next idle caller (the worker between requests, or while a script waits in
a pause or a dialog), waiting at most 2 seconds for the host's answer and
sent again up to three times. The host drops a connection that still holds
such a file after its wait for the client to sign out, and with it the
connection's other files, which this keeps usable. A later open of the file
on the same connection is a new one, not lost.

The notice goes to the handles holding the file the host names, by its host
file id (one naming a file the connection no longer holds is dropped), so the
window it reaches shows that file. Its windows lose the connection at once:
nothing more is saved. An alert says, as FileMaker Pro's does, "File has been
forcefully disconnected by the host. All affected windows will be closed.",
then "[From Administrator: “<text>”]" when the administrator gave a text,
and OK. It waits for an alert or script dialog that is up, and holds the
others while it shows. Its OK closes every window of that file at once,
whichever window is in front; unsaved changes are lost with the file. A
related file the host disconnects fails only what reads it.

Limits ([#156](https://github.com/OpenMaker-Dev/openmaker/issues/156)):
- The host's requests to close the files (Close, Disconnect) are taken and
  not acted on yet: FileMaker Pro shows a Close File dialog with a
  countdown, and closes the files when it ends.
- On a forced disconnect FileMaker Pro signs out of the file (op81 Logout)
  and disconnects before its alert. OpenMaker sends the op62 release a
  user's close sends, as no Logout is built yet, and keeps the connection
  for its other files. The alert's brackets are as the capture's reading
  recorded them [I: whether FileMaker Pro shows them is unmeasured].
- A script that runs without asking anything of the host reads the notice
  at the next Tickle at the latest. The alert shows when the worker next
  reads the hosts' calls: between requests, or while a script waits.
- Disconnect Client: OpenMaker Server names one file in its forced notice
  and then ends the connection if a file is still open. Only that file is
  alerted about and closed; the connection's other files show the lost
  connection instead ([#638](https://github.com/OpenMaker-Dev/openmaker/issues/638)).
- Ordering and edge cases left open
  ([#640](https://github.com/OpenMaker-Dev/openmaker/issues/640)):
  - A loss the worker reports before the notice clears the app's close and
    script state the generic way.
  - A dialog wait of another file's script ends when the alert's OK closes
    the disconnected file.
  - The alert is missed when the window the notice went to closed first.
  - A commit refused before it was sent counts as ambiguous.
  - The release's callback replies keep the normal write timeout.
- The dialog's look is OpenMaker's; FileMaker Pro's was read from its
  text only (capture s07).
- An open Script Workspace holds the message too, although it is a
  window of its own: OpenMaker counts it among the dialogs that hold the
  document [I: FileMaker Pro is unmeasured there].

### Client license

**FileMaker License...** is in the OpenMaker menu. It is
an OpenMaker setting, not a file-defined layout object. The key is masked on
entry and is never read back into the dialog. Save replaces the current OS
user's stored key; Remove deletes it. **Use saved FileMaker license**, in the
sign-in and Open Network File, selects whether a connection sends that key,
and is kept for the next one. With no saved key, or with the
switch off, native connections send an empty install code. OpenMaker Server
connections therefore need no license; FileMaker Server decides whether it
accepts the client.

Persistence uses [keyring](https://docs.rs/keyring/3.6.3/keyring/)'s explicit
native backends: macOS Keychain, Windows Credential Manager and Linux Secret
Service with encrypted D-Bus transport. The service is
`org.openmaker.OpenMaker` and the account is `filemaker-client-license`.
Operations run in order on a thread of their own, for the provider worker.
Failures do not write plaintext or silently switch to temporary storage; a
failed save keeps the masked draft for retry. Opening or reconnecting to a
host waits for the saved key at most 3 seconds: the OS can hold a read
while it asks the user (macOS Keychain asks whether a newly built OpenMaker
may use the item, and the prompt can sit behind other windows). When the
store fails or does not answer in time, the connection goes ahead without
a key, which OpenMaker Server does not need; if the host refuses it, the
error says the saved license was not sent and why. The next attempt takes
the answer of a read that was still waiting rather than asking again, if it
came within the last minute ([#151](https://github.com/OpenMaker-Dev/openmaker/issues/151)).
While such a read waits, Save and Remove say so and change nothing;
otherwise they wait for the store without a limit. When the dialog cannot
tell whether a key is saved, **Check Again** reads the store again and keeps
the draft. The app-wide dialog takes only the reply to its current request,
even if the window that sent it closes or its session changes. License
requests reach the store even when the window's host is gone. If the
store's thread stops (its store failed), OpenMaker must be started again to
reach it. Turning off **Use saved license** bypasses the store. The desktop client does not
import license keys from `.env` or environment variables. License values and
machine-info license fields are redacted in diagnostics.

### Settings

**Settings...** (Cmd+, / Ctrl+,) in the OpenMaker menu opens Settings with
FileMaker Pro 26's **User Name** choice from its General tab
([calc-functions.md](re/calc-functions.md) 6.1.3): **System:
“<full name>”** or **Other** with a typed name. OK saves it and uses it at
once; Cancel and Esc change nothing. The name is what `Get ( UserName )`
answers, what user-name auto-entry writes, the account a related file's
sign-in suggests, and the session name sent with each login, so a solution
that lets only listed names open a file (#1234) behaves as it does in
FileMaker Pro set to the same name. A host keeps listing a connection under
the name it signed in with; files opened later on that connection are
listed under it too.

System is the account's full name: the first comma-separated part of the
account's GECOS field from the system's account database (`getpwuid`),
which macOS fills from the account's full name, else the login name. On
Windows it is `USERNAME`, which FileMaker Pro has not been compared with.
`OPENMAKER_USER_NAME`, read once at startup, overrides both choices for
tests and measures.

The choice is kept in a JSON file for the OS account, read at launch:
`settings.json` in `~/Library/Application Support/OpenMaker` on macOS,
`%APPDATA%\OpenMaker` on Windows and `$XDG_CONFIG_HOME/openmaker`
(`~/.config/openmaker`) elsewhere, or the file `OPENMAKER_SETTINGS` names.
It is written whole and moved into place, keeping the file's permissions
and the keys this version does not know. A read-only file, or one that is
not a settings object, is never written over: OK shows the reason and saves
nothing, and a launch that cannot read the file keeps System and says so on
standard error.

Limits: the other tabs and settings of FileMaker Pro's dialog are not
offered, and FileMaker Pro's own User Name setting is not read. What
FileMaker Pro starts with on a new installation, and whether changing the
setting while connected renames the client on the host, are unmeasured.

## Browse mode

Form view paints `RenderLayout`: parts, stacked objects, file colors, state
and named styles, padding, borders, fonts, styled text runs, static text,
literal placeholders, merge fields, tab and slide panels, portals, popovers,
button bars, conditional formatting and hide conditions, and field values
from the current record page. Stored anchors resize objects within their
containers. Record changes keep the user's selected panels. Browse shows no
authoring part tabs or object kind captions.

- Text objects in Browse draw an edge only when its style resolves to a
  line. A width and color alone leave the edge hidden.
- Styled layout labels keep the stored formatting of literal text around
  a merge symbol or field. The replacement takes the styling at its
  opening delimiter. OpenMaker also applies this rule to merge variables
  and paragraph runs (layout-model.md 8.2).
- Date fields, merge fields and date symbols in Browse can show the
  fixed named month and weekday presets in month-first English (see
  data-format.md). OpenMaker takes names from the file's locale for
  those presets. Short System Date is still displayed as entered.
- A text, number, date, time or timestamp value stored with character
  styles (a chunked value beside a style subtree `ff`, calc-values.md
  implementation notes) shows its text; a nested form the provider cannot
  read shows empty rather than raw structure.
- List view repeats the body, keeps navigation and header parts above the
  scroller, and virtualizes rows across the whole record count. Sorted by a
  sub-summary part's break field, a row also shows that part once per
  group, before the group's first record (leading) or after its last
  (trailing), in the order report pagination prints them
  (`om_layout::pagination::RecordGroups`, layout-model.md 4.2). Groups are
  read once per found set and data change, with the same
  `FileCalc::found_groups` and checks as Preview
  ([report sections](report-sections.md)). Unsorted by its break field, a
  part does not show. A summary field of the layout's occurrence in the
  part, as a field or merge field, shows its group's value
  (`GetSummary ( field ; break )` on the group's first or last record);
  while that is read it shows empty, never the found set's value. Other
  fields and calculations read that record. The part shows its group only:
  it takes no clicks, entry or focus (OpenMaker's choice; FileMaker's is
  unmeasured), and only the body selects its record. Visible uncached ranges load through the worker without
  moving the current record; navigation reuses cached pages, and refresh and
  layout changes invalidate them. Each layout's default and disabled views
  come from the member's private flags at `4/5/<id>/0xf1`, not the protobuf
  field, which these files leave out.
- Related sets (related fields, portal rows, calculations over related
  records) of a hosted file come from the host's value index when every
  hop of the relationship path can use it: `×` alone, or a hop with an `=`
  predicate on a stored target field with a value index, of the same
  type, and for text an English index. That predicate's index gives the
  candidates, and every predicate of the hop (the inequalities of a date
  range too) decides on them with the scan's comparison, so only the
  candidates are downloaded. They are put in creation order, the record
  ids' order (file-metadata.md 4.3), by sorting them: the related table's
  record list is not read for it (#1100). A list the provider already
  holds still filters them, so a record the open transaction deleted,
  which the page read leaves out of the window's table and the host's
  index still lists, is not related. Otherwise the window scans: it downloads every
  record of the related table and matches in memory. A hop of `×` beside
  other predicates, or of no usable `=`, scans. When one read asks for the
  related sets of many records through one relationship of one hop (a
  footer summary over the found set reads one per record, #982), their
  candidates come together: one JoinProjection per 256 records on that
  `=` predicate's value index (query-program.md 9.2; a whole number `n`
  joined as `n - 1 ... n + 1`, the neighborhood its own find uses), and
  the union downloaded in bulk. Each record keeps only the candidates its
  own keys select, so its set is the one its own read gives (OpenMaker's
  choice of batching; FileMaker Pro's own requests for such a summary
  are unmeasured). Text keys in an English index and whole-number keys
  are read so; dates, an index the host does not have yet and a refused
  join leave each record to its own read. A text key is looked up by its
  own index key (query-program.md 7.2), on its own and in a join, not read
  as find text: characters a find reads as operators (`#`, `.`, `=`,
  quotes, as in a paper's `50#` weight or a decimal width) are its own
  (#1068). A text key of several lines, with edge spaces or other control
  characters still scans. What a scan read of a table's
  match fields is kept for the window across records and List view rows,
  so a session reads that table about once rather than once per record
  (#804; OpenMaker's choice, FileMaker's own behavior is unmeasured). The
  kept values go with the rest of the window's calculation data: on a
  change notification of any file of the session (related files on the
  shared connection included), Refresh Window or Refresh Object when it
  flushes cached join results, the window's own writes, and a lost
  connection. A host sends notifications
  with its replies (OpenMaker Server in no other way), so before a
  calculation read reuses kept values it sends one HostTimeStamp (op82)
  on each connection of the session (a related file on this computer has
  its own) and takes those they bring; when one fails, the kept values are
  dropped and read again. A global match field's values are kept once per
  scan, not per row. Limits: the first scan still downloads whole
  records, though only the match fields are needed (an op92 key list
  naming fields has not been observed with FileMaker Server); at most
  2,097,152 scanned rows and about 256 MiB of match values are kept, and
  a scan past either is matched without being kept.
- While the window holds unsaved changes (typing in the record, in a
  related record, or in a portal's creation row), calculations read them
  over the stored data. A related set stays the one the host gave unless
  a change reaches a value its relationship compares: a match field of
  the source record, or past the first hop of any record of that table;
  a match field of a stored record of a target table; or, for a source
  match field that is an unstored calculation, another value over the
  changes than over the stored data (on the first hop; past it any
  change counts). Such a set is scanned from the records, as above, once
  per frame. A sorted relationship's set is sorted again when a changed
  record is in it, or when a sort field may read other records. A hop
  whose source match values are all empty relates nothing and reads no
  record. A new record the transaction holds ([#261](https://github.com/OpenMaker-Dev/openmaker/issues/261)) has no related set
  on the host yet, so its sets are scanned from its held values, also
  before anything is typed into it: right after New Record its
  auto-entered keys relate records ([#327](https://github.com/OpenMaker-Dev/openmaker/issues/327)). Results are kept from frame
  to frame while the changes stay the same, and are all evaluated again
  when they change; those that read the window's context (`Get`
  functions) are also evaluated again when that changes (#1308). Limits: a
  record begun in a creation row has no match values yet ([#277](https://github.com/OpenMaker-Dev/openmaker/issues/277)) and is
  in no related set until it is committed; FileMaker Pro's handling of
  uncommitted records in related sets, and when it recalculates during
  typing, are unmeasured.
- A record page read (opening, a page past those read, a layout or view
  whose page is not read yet) brings the page's records and the window's
  found set fresh. What else the window's calculations read stays, as long
  as the provider read the page under the calculation data generation it
  was read under (#1042). The page read takes the notifications its reply
  brings and sends one HostTimeStamp on each other connection of the
  session (none when the related files share it), as before reusing kept
  values. A notification, a failed check, or a page record whose
  modification count changed since this client last saw it starts a new
  generation, and the window then reads everything again; so do writes,
  Refresh Window and every other data change. A shown summary over the
  found set thus reads its records and related data once rather than on
  every page (on the two-file test fixture, the next page of orders took
  44 requests and 487 KB before, 4 requests and 328 B after). The provider
  keeps its own working set across pages up to its bounds (16,384
  records, 4,194,304 record ids, a list several occurrences share counted
  once), past which it keeps none of it; the window keeps what it has. The
  id bound holds the record lists calculations read (scans, `×`
  relationships, found sets; the index path read them for creation order
  until #1100): about 1.6 million ids for one record of a measured
  solution, where the earlier bound of 262,144 discarded the working set
  after every batch and a third of each Next Record's requests repeated
  (#1067). A calculation read that may reuse the working set first checks
  each connection of the session as it does before reusing a kept scan,
  so a change another client made shows at the window's next calculation
  request even when that request needs nothing new from the host; the
  check is one HostTimeStamp per connection and request (on the two-file
  test fixture, 2 of the 4 requests of the next page of orders).
  OpenMaker's choice: FileMaker Pro keeps cached records until a
  notification purges them (write-path.md 6.5), and its requests per page
  are unmeasured.
- While a save or another change has the window's calculations read
  their data again, each related field, merge field, summary, unstored
  calculation, hide condition, conditional format, tooltip and label keeps
  drawing the result it had before the change, until its fresh one can be
  computed; a portal already kept its rows ([#324](https://github.com/OpenMaker-Dev/openmaker/issues/324)). Before, a save drew
  them empty for the moment the data took to arrive, about 1 s on a hosted
  multi-file solution. A portal row's or related field's value is drawn
  read-only in that moment: its editor returns once its record is read
  again. The kept results go once a frame draws the window with nothing
  missing and nothing on its way. A read that fails draws its failure at
  once, not the last value. Preview, PDF and printing never draw kept
  results. OpenMaker's choice: whether FileMaker Pro draws anything in
  place of these values while it refreshes them after a save is
  unmeasured.
- A window asks its provider for what its calculations lack once per
  frame, all of it in one request. The request carries the window's token
  for its current layout, page revision, view and found set.
  - The found set counts as changed after every find, omit or sort that
    replaced it, also one that keeps its counts.
  - When any of them changes, or the window's file closes, the token is
    cancelled (#987; OpenMaker's choice, FileMaker Pro's own behavior when
    the user leaves a long summary is unmeasured).
  - A request the token carried that the provider has not started is
    answered as superseded. Nothing in it is applied or reported, and the
    window asks again for what it still needs.
  - A request the provider has started stops sending requests once the
    token is cancelled: each need, download batch and join batch checks
    it. It is answered superseded as a whole. What it read stays in the
    page's data, and nothing is marked missing or failed because of it.
  - A script's own reads, commits and writes are never stopped by it.
- Container fields show their picture (PNG, JPEG, GIF, BMP, TIFF, WebP,
  and the preview FileMaker stores for a PDF or HEIC picture) scaled by the
  object's graphic format: crop, reduce, enlarge, keep proportions and
  alignment, within the frame less each side's border width, drawn or not
  (data-format.md "Graphic format"), in Form and List view, in
  portals, on local and hosted files. A PDF shows its first page, placed
  the same way (a point of the page to a point of the layout), drawn by
  the PDF worker process (#566). Any other file shows a document icon
  above its name without its data being downloaded. Pictures decode on
  background threads, at most 16,384 pixels a side and 256 MiB each, into
  textures of the size they show at, kept by the data's MD5 and the
  frame's size (128 MiB in all), so scrolling and repainting do not decode
  again; the field shows empty while its data is read and when it does not
  decode (containers.md, desktop implementation notes).
- A container set to "Optimize for: Interactive content" (bit `0x0100`
  of its graphic format, containers.md 6.6) shows a PDF as the PDF view
  (`PdfViewer`, #200): its pages one under the other at the field's width
  on a gray ground, scrolled through, each drawn by the PDF worker
  (`pdf_worker::render_page`, every page's size from
  `pdf_worker::page_sizes`, at most 10,000 pages) when it comes into view,
  at the size it shows. A bar at its foot goes to the previous or next
  page and says which page shows ("2 / 3"). A click on a page selects
  the field, for Insert and Export, as a click on any container does. Its
  look is OpenMaker's own [I: FileMaker Pro's interactive container is
  unmeasured]. A container optimized for images shows a PDF's first page
  as above.
- An interactive container's audio and video play on the current record
  in Form or List view outside portal rows. Playback uses the native web
  viewer in builds with `web-viewer` (WebKitGTK on Linux/X11, including
  XWayland, or WKWebView on macOS). The audio or video is copied to a
  folder only its user may read and removed with the window. The player
  starts automatically when the container's "Start playback
  automatically" bit is set (`0x0200`, `GraphicFormat::autoplay`,
  #1338). A window without a player shows the filename and a note.
- The Insert menu offers PDF and Audio/Video for a selected, writable
  interactive container outside portal rows whose field object allows
  entry in Browse mode. The Insert commands ask for the file in the
  system's file panel (see Opening files), which shows every file (whether
  FileMaker Pro's panel filters by type is unmeasured); where there is
  none, an in-app dialog takes a typed path. Either way, a file of a different type (a picture Insert
  Picture cannot show, a PDF without its header, a file the player would
  not play) is refused with 734 or 700 before an edit is prepared or data
  uploaded. [Container menus and file dialogs](re/containers.md#desktop-menus-and-file-dialogs-2026-11-02)
- Button icons come from the file's library root `31` as the original SVG,
  colored through the icon component's style; no application icon is
  substituted for a file's library image.
- A button with FileMaker Pro 26.1's "Calculated icon" (`buttonObj.flags`
  `0x4`, layout-model.md 5.3.4) shows in Browse the SVG text its
  `KButtonIconSvgCalc` formula (`91`) gives on the record, in the library
  icon's place, drawn by the same SVG worker. "Image only" hides its label,
  as for a library icon. Text the worker cannot draw, or none, shows a
  warning sign, as FileMaker Pro shows a warning icon for SVG it does not
  support (Claris). Popover buttons take the option too. A formula stored
  without the flag is kept, neither evaluated nor drawn, and does not stop
  reports or Refresh Object when it cannot be read.
  - OpenMaker's choices: the icon is colored as the library icon is; text
    longer than 64 KiB is dropped as it is computed and shows the warning;
    the app keeps up to 256 distinct icons loaded across its windows.
    Only icons within the document's visible clip claim or renew a slot.
    The app warns on additional visible icons while all slots are in use. It keeps up to
    64 after they leave the screen, with a one-second grace period.
    Rasters are limited to 512 pixels per side and cached calculation
    results have a 16 MiB text budget. The SVG worker queues up to 128
    draws, reads their source bytes when drawing, and retries still-visible
    icons on repaint.
  - Inferred, not measured: the sign's look, which SVG FileMaker Pro
    refuses, and whether popover buttons offer the option. The bit and the
    key were read from FileMaker Pro 26.1.1's binaries; no file made by
    26.1.1 has been read.
  - Find mode, which evaluates no layout calculations, draws the library
    icon, if any, where FileMaker Pro draws the calculated one
    ([#512](https://github.com/OpenMaker-Dev/openmaker/issues/512)).
    Preview, printing and PDF still draw the library icon, and Layout
    mode, where FileMaker Pro shows a placeholder, too; whether FileMaker
    Pro prints the calculated icon is unmeasured
    ([#466](https://github.com/OpenMaker-Dev/openmaker/issues/466)).
- SVG pictures and icons are drawn outside om-gui's process, by the SVG
  worker (`om-gui-pdf-worker --svg`, the one reports use: its allocator
  capped 512 MiB past its input, killed after 30 seconds, one SVG at a
  time), on a background thread, at the pixel size they show at (at most
  8,192 pixels a side and 16 megapixels, or the GPU's largest texture,
  then stretched over the frame). A library SVG is hostile input that can
  ask usvg and resvg for unbounded memory or stack, which would abort
  om-gui ([#303](https://github.com/OpenMaker-Dev/openmaker/issues/303)).
  Nothing shows until the worker answers (a few milliseconds an icon).
  Drawn rasters are kept within 128 MiB beside those on screen, the sizes
  an SVG no longer shows at (a window being resized) dropped first. Text
  is drawn in the bundled fonts (Inter, else Liberation Sans); the images
  an SVG names are left out and never read from disk; an SVG that fails in
  the worker, or every SVG when no worker is installed beside om-gui,
  shows egui's error mark, as a picture that does not decode does
  [I, OpenMaker; FileMaker draws SVG with the platform's renderer, and
  what it shows for one it cannot draw is unmeasured]. Binary merge references resolve
  through the occurrence and field keys, to `Field` in the layout's
  occurrence and to `Occurrence::Field` in related ones (layout-model.md
  4.1).
- A field's text sits in the field's frame as FileMaker Pro draws it
  (layout-model.md 6.5): inset by the field's border widths and its text
  component's margins (properties 105-108, the same insets Preview and PDFs
  use), at the stored vertical alignment. A pop-up menu is inset by the
  widths of only the borders it draws, as in Preview; on Task Details'
  Category, Priority and Status that is 12 pt of margin and a 1 pt border,
  13 pt, where FileMaker Pro draws the text (#721). A pop-up's right inset
  stays its arrow's 20 points, not the icon component's 30. With none stored, an edit box,
  drop-down list or calendar puts its text at the top and a pop-up menu
  centers it, so a field its anchors make tall keeps its text at the top.
  The focused editor puts the text in the same place. Drop-down lists and
  calendars at the top is inferred. That Pro uses the file's stored margins,
  not its application copy of the theme, is inferred too: Meetings stores
  margins its theme's application copy does not have.
- **The focused field.** In Form and List view, a field of the layout's
  own table on the current record whose control has the keyboard focus
  draws its frame in its theme's **In Focus** state (state 4,
  layout-model.md 6.4). Each property falls back to the Normal state where
  the theme sets none.
  - The control is an edit box, pop-up menu, drop-down list, calendar, or
    checkbox or radio set.
  - A checkbox or radio set's items and markers take In Focus too, and
    checked+focus (16) when checked.
  - The six starters' themes give fields a blue border (0, 0.44, 0.81) In
    Focus. Their plain edit boxes draw only the bottom line, so that line
    turns blue.
  - The look comes from the file's theme only. A theme with no In Focus
    style draws the field as before.

  **Not covered**, drawn in the Normal state as before:
  - related fields, on the layout or in a portal row: their controls take
    the focus under another id;
  - container fields, which show their own selection (the starters' themes
    give them In Focus styles too, such as a white background, which are
    not drawn);
  - Table view, which another renderer draws;
  - Find mode, focus combined with hover (9), and buttons;
  - the field's text, which keeps the Normal state's font and color (no
    starter theme changes them In Focus).

  How FileMaker Pro draws a focused field has not been compared.
- The occurrence mapping accepts only the catalog encoding verified in real
  files; other encodings report an error.

**Table view's columns** scroll sideways, header and rows together, when
they are wider than the window (#1161). A sideways wheel, Shift with the
wheel and the horizontal scroll bar move them, and a cell that takes the
keyboard comes into view: by a click, by Tab, or by Go to Field, also on
the cell that has the keyboard already or in a row scrolled out of view,
once the row shows (Go to Field is not tested). FileMaker Pro's Table
view scrolls sideways too; its scroll bars and wheel have not been
compared. Limits:
- The rows' vertical scroll bar is at the table's right edge. With columns
  wider than the window it shows only once the last column is in view; the
  wheel scrolls the rows anywhere.
- The last column takes what is left of the window, at least 80 points, and
  dragging its edge does not resize it.
- The record-number column scrolls away with the others.

**Table view's parts.** On a layout OpenMaker draws, Table view shows the
top navigation, header, footer and bottom navigation parts that the
layout's Table View Properties include, with their objects, around the
grid ([#306](https://github.com/OpenMaker-Dev/openmaker/issues/306)).
The options' bits and FileMaker Pro 26.1.1's placement of the parts are
measured ([layout-model.md](re/layout-model.md) 4.2,
[#927](https://github.com/OpenMaker-Dev/openmaker/issues/927)), and
OpenMaker places them the same way: the top navigation and header parts
above the column headings, the footer and bottom navigation parts below
the rows, stacked at the window's bottom however few the rows. They do
not scroll sideways with the columns. Their fields show the current
record, and a web viewer in one of them shows one page that follows the
current record, as in a List's header and footer. Limits:
- **Zoom:** the parts zoom with the table. Pro keeps the navigation
  parts at 100% ([#981](https://github.com/OpenMaker-Dev/openmaker/issues/981)).
- **Tab:** Tab goes through the table's cells as it does without the
  parts; from a part's field that has the keyboard it goes to the first
  cell (Shift-Tab: the last). New Record's first field is the table's.
  Whether Pro's Tab reaches the parts' fields is unmeasured.
- **Unmeasured in Pro:** the pages of web viewers in the parts, and how
  the header's, footer's and bottom navigation part's objects behave when
  the table scrolls sideways.
- **Find mode:** Table view in Find mode still draws its criteria grid
  without the parts.

**Limits.** List view shows no sub-summary parts, with a note saying why,
when their groups cannot be read: a break field of another table, a summary
or container break field, a malformed key, a related sort key before the
break field, a sort whose order disagrees with the parts, groups that do
not nest, or more than 10,000 sorted records. A part showing a summary
field of another occurrence, a later repetition, or a summary field object
with layout calculations shows a note instead of its content. While groups
are read, rows show without sub-summary parts. Table view shows no
sub-summary rows, and Form view still draws sub-summary parts. Container
fields optimized for images show a movie or a sound as a file (an
interactive container plays it, above); a GIF shows its
first frame; Table view draws no pictures (Preview and PDF print them:
[portable-pdf.md](portable-pdf.md#container-fields)); a
picture is drawn one pixel to a point whatever resolution (`DPI_`) it
has; the file icon is OpenMaker's own, not the system's icon for the
file's type; and only the starters' two graphic formats (5 and 7, centered)
were compared with FileMaker Pro, the others are inferred from Claris's
flag names. See [Zoom](#zoom) for which views support scaling.

**Checks.** Regular tests use synthetic layouts and records. The ignored
`live` tests (`cargo test -p om-gui --test live -- --ignored`) host a
git-ignored, Contacts-based test file on an in-process `om-server`, open
Contact List and Contact Details, check records, panels and the row
button's navigation, and save screenshots (git-ignored).

### Reopening a local file

A local file opens where FileMaker Pro left it. The first time the window
reads a table through a layout, it starts on the found set FileMaker Pro
saved for that layout's occurrence when it last closed the file
(file-metadata.md 11). Records deleted since are left out, and a sorted set
is sorted again by its saved sort. The window goes to the saved current
record unless it has already moved: it asks for it
(`DataProvider::take_saved_position`) before the layout's other requests,
so it is there before the layout's triggers run, and a script going to a
table's layout first starts there too. Each window, and each window's
table, restores once. The provider reads `[TO, 07, 01]`
(`om_data::meta::SavedFoundSet`). The status toolbar says Found or Total,
Sorted or Unsorted, for the table the layout shows, from the found set
read with each page (`DataProvider::records_with_summary`).

When a window of a local file closes (Close Window, or the file closing,
which closes the front window last), and it changed the file, the file
keeps the window's found set of the table its layout shows and
its current record at the layout's occurrence, as FileMaker Pro writes an
unsorted found set (file-metadata.md 11.1): `03`, `05` or `ff`, the
table's data version in `fe`, `02` = 0 where a sort is kept, no `08`, every
other member as it was (a kept sort's `0a`, `f0` and `fd` included). A
window changed the file when it wrote a record of it, or when one of its
finds (Quick Find included) read an index the file lacked, which the host
then builds and stores, as FileMaker Pro's first find on a fresh starter
does ([#201](https://github.com/OpenMaker-Dev/openmaker/issues/201)). Which changes make FileMaker
Pro keep the found set is inferred (file-metadata.md 11, U9). A window
that changed nothing leaves what was there.
The write goes straight to the private local host
(`om_server::LocalHost::save_found_set`, `om_data::meta::saved_found_set_written`)
and is saved like a record's commit, taken back when the save fails. It
costs one save of the file, made by the provider's thread after the window
has gone, and is skipped when another request holds the file. A read-only
file, a table without a data version, or an entry with a member or shape
OpenMaker does not know writes nothing, and the window closes as before;
only the log says so. FileMaker writes some it does not know: `fc`, `09`,
`fe` naming several tables, and sorted orders `08` in other forms than
file-metadata.md 11 lists. Quit waits until the files have closed, at most
30 seconds, so the app never ends in the middle of that save.

**Limits.**
- A sorted found set is written unsorted (`02` = 0, the saved sort kept as
  FileMaker Pro last wrote it, not the window's), so it reopens unsorted.
- Only the table the closing window shows is written; other tables the
  window visited keep what the file had. A window a script closes keeps
  nothing.
- Hosted files are not affected. Whether FileMaker Pro applies the state a
  hosted file holds is unmeasured.
- The provider keeps one found set per base table, not per occurrence, so
  where two occurrences of a table both have a saved found set, the one
  the window reads the table through first decides.
- A sort the provider cannot apply leaves the found set unsorted: a related
  field, a summary order or a repetition.
- Only a window's first visit to a table gets the saved record; going to
  another table's layout and back returns to the record the window showed
  there (Layout menus, below).
- The last find's requests (`[TO, 07, 02]`) are not read: after reopening,
  Modify Last Find does not have the requests FileMaker Pro saved.

### Finds that match nothing

Perform Find, Constrain or Extend Found Set run from the window (the
toolbar, the Requests menu, a one-step button: the frame running the step
is not a script's, even over a paused script) use
`FoundSetChange::ManualFind`: a request set that matches nothing
leaves the found set, its sort, the mode and the requests as they were,
and FileMaker's dialog (client-ui.md, "Dialogs") offers Modify Find, which
stays in Find mode, and Cancel, which returns to Browse as Enter Browse
Mode does (OnModeExit can keep Find mode; OnModeEnter and OnRecordLoad
follow). A stored find run
from Browse by a one-step button goes to Find mode with its requests on
Modify Find. A script's find still ends with no records and reports 401
through the script error alert. **Limits.** The dialog's default button,
its title and the script case's three buttons are not compared with
FileMaker Pro.

### Quick Find

Return in the toolbar's search box runs Perform Quick Find (150) with the
box's text, as a one-step button does, so the toolbar and scripts share
one implementation (`om_data::quick_find`, query-program.md 6.6). It
searches the layout's field objects included for Quick Find (flag
`0x8000` clear, layout-model.md 5.4.1) and its merge fields, own and
related, never summary, container or global fields. Every word, or quoted
phrase, must be found in one of them; each is a Find mode criterion with
its operator characters escaped, and goes only to the fields whose type
reads it as a plain value. The provider runs one find per word and
intersects them (`FoundSetChange::QuickFind`), on local and hosted files
alike. The current record stays current when the new found set holds
it, else the first record is shown (`Window::change_found_set`, through
the provider's `record_position`). No match shows "No records match this
find criteria." and keeps the found set and its sort; a script gets 401. On a layout whose Layout
Setup turns Quick Find off, the box takes no text.

**Limits.** The rules for words in number, date and time fields, the
fields of hidden objects, merge fields in button labels, the found set's
sort and the no-match dialog's buttons are OpenMaker's reading of Claris's
help, not compared with FileMaker Pro (query-program.md 6.6, Unknowns).
`Get ( QuickFindText )` stays empty, and the search box has no recent
searches menu.

### Sort Records dialog

The toolbar's Sort, Records > Sort Records... (Command-S) and Sort Records
with "With dialog" show FileMaker's Sort Records dialog, the one the Script
Workspace draws for a step's order (`script_workspace/sort_dialog.rs`;
client-ui.md, "Dialogs"). It opens on "Current Layout" with the fields the
layout shows (a related one as `Table::Field`), alphabetically, as
FileMaker Pro lists them; the table pop-up also has "Current Table" and
every occurrence. The Sort Order starts with the
window's last sort: the stored order itself when it came from a Sort
Records order or this dialog (a value list's order, a summary field and a
language override shown again), else its fields and directions. Sort runs
the order; Unsort restores creation order as Records > Unsort does (both,
and a script's Unsort Records, commit the record first, as Show All and
Omit do); Cancel changes nothing (a script's step gets 1). The last sort
order is the table's within a window, as in FileMaker Pro: a window keeps
each table's own (`Window::set_layout`), so the dialog, and Sort Records
with no stored order, use the order last sorted by on the table the
layout shows. **Limits.** The table pop-up lists every occurrence, not
Pro's related and unrelated tables with `<none>` and Manage Database...;
the custom order's pop-up has no Manage Value Lists.... Related fields'
place in Current Layout's alphabetical list (sorted here by their
`Table::Field` text), the button order, Sort disabled for an empty
order, and a script's step succeeding after Unsort are OpenMaker's
readings, not compared with FileMaker Pro. A field of another occurrence
of this file's tables, a self join's included, sorts each record by its
first related record's value, as Claris documents ("the value in the
first matching record"), read as `Rel::field` reads it: without a
relationship sort, the related record first in creation order
(calc-values.md 12.1); a calculation reads with the parent record as
its context. Numbers, dates and times compare by value. Custom value
lists rank by the original text, including a stored date's padding and
separators. FileMaker's related sort and a sorted relationship's first
record are not measured. Fail with 3:
- a related field of an imported table, or of an unrelated occurrence;
- a related summary or container field, or a later repetition;
- a summary's reordering by a related field, and a summary of another
  occurrence;
- a path through a relationship that sorts by an unstored calculation or a
  summary, which OpenMaker's relationship sort reads as empty;
- any relationship path read by the sort, including by a calculation, that
  uses a Cartesian predicate or an unstored match field;
- a value that could not be computed (`?`, or a related file that did not
  open);
- more than 8,388,608 record IDs retained across related results and index
  lookup candidates, OpenMaker's limit per sort pass.

A local file reopened with such a sort reopens unsorted
([#47](https://github.com/OpenMaker-Dev/openmaker/issues/47)). A sort made from field names
(Sort Records by Field) comes back without a value list or summary
reordering, and a local file reopened sorted shows an empty order: the
order it saved (`0a`, file-metadata.md 11) is not read into the dialog. On
a layout without a decoded design (the sample provider's Contacts
layouts) the dialog lists no layout fields and a table's field chosen
from the pop-up does not sort, as steps naming fields fail there.

### Omit Multiple

Records > Omit Multiple... (Shift-Command-T) runs Omit Multiple Records
(26) with its dialog, as a one-step button does: the dialog asks how many
records to omit, starting with the current one, and the next record
becomes current; past the end of the found set, the last remaining one
does (Claris "Omit Multiple Records"). A script's step with "With
dialog" shows the same dialog, its number filled in. Cancel omits
nothing (1); a count that is not a whole number above 0 is 5. A count
past the end omits to the end. **Limits.** The dialog's wording and
layout are OpenMaker's, and FileMaker's message for a count past the end
is not shown; neither is compared with FileMaker Pro.

### Zoom

Loaded Browse Form and List layouts, Browse Table view, Find mode's layout
and the Layout mode canvas have document-local Actual Size, Zoom In and Zoom
Out commands, in steps of 25, 50, 75, 100, 150, 200, 300 and 400 percent.
The menu and keyboard controls scale the document's allocation, clipping,
pointer input and accessibility bounds together; menus and toolbars keep the
application scale. While the layout paints, egui treats the zoomed layer as
the one under the pointer wherever the document's own layer is, so portals
and other scroll areas in it get the wheel; a menu, popup or window over the
document keeps it. Then it hands the pointer back to the document's scroll
area, which scrolls by whatever wheel movement they leave (layout-model.md
5.3). The rendered percentage is the script window's zoom state
(`Get ( WindowZoomLevel )`). Set Zoom Level's Zoom In and Zoom Out move one
preset level, in the View menu's sequence
([Claris reference](https://help.claris.com/en/pro-help/content/set-zoom-level.html)).

A transformed layer alone would magnify glyph bitmaps rasterized at the
screen's ordinary density. For painted Browse text the renderer instead
rasterizes at the final size and converts geometry back to document
coordinates. Logical line breaks are kept, and row origins snap to final
physical pixels. Converted galleys are bounded and keyed by the font cache's
atlas identity, zoom and logical job. Different logical sizes may share a
raster job; they must not share converted geometry. An atlas or font
replacement invalidates the identity. Text whose row and source mapping
cannot be verified, justified text and elided text keep the ordinary
renderer.

List view draws its header, records and footer in the same zoomed layer,
in the layout's coordinates, so its rows scroll and take clicks as at 100%.
Find mode draws its request as Form view draws a record, so it was already
drawn at the window's zoom; its commands now change it there too.

Table view draws its grid, header and scroll areas in the zoomed layer too,
whatever the layout (the grid is the found set's), so column widths, rows
and clicks are in unzoomed points at every zoom. Its labels and its cells'
editors lay their text out at the zoom (`document::layer_galley`) to keep it
sharp; a value cut off at its column's edge keeps egui's renderer, as
elided text does above. The table's last column fills the width the previous
frame left, so a new zoom lays the table out once more before it shows. The
table keeps its columns' widths itself, by field, and draws their dividers in
the header: a divider dragged at any zoom follows the pointer from where it
was pressed, in the table's points (egui_extras' own resizing reads the
pointer in window points, so a zoomed drag jumped). While zoomed, Table view's scroll areas do not follow a
touch drag, which egui would move by the zoom's multiple of the finger's
way; the wheel and the scroll bars still scroll them.

A field being typed in lays its text out at the zoom as well, in Form, List
and Find (`field_galley`, and the single-line editor's layouter), so its text
stays as sharp as the field's text at rest; an empty editor's placeholder is
egui's. Text `scaled_text` cannot rebuild (justified, elided or with leading
space) keeps the unzoomed raster in the editor too, as do lines that a small
zoom and font would put on one pixel row. A new zoom brings a scrolling
field's insertion point back into view.

Set Zoom Level with Lock on disables View > Actual Size, Zoom In and
Zoom Out and their shortcuts for that window. A script can still set the
level, and Lock off restores those controls. A new window (New Window,
the step or Window > New Window) takes the lock with the zoom of the window
it opens from [I: inheritance has not been compared with FileMaker Pro].

Generic Form fallbacks keep their normal scale
([#36](https://github.com/OpenMaker-Dev/openmaker/issues/36)). Preview scales its paper
([preview.md](preview.md)). Scroll bars inside the zoomed layer (List and
Table view and Layout mode) scale with it. FileMaker Pro's scrollbar size
at different zoom levels is unmeasured. How FileMaker Pro scales Table view's
header and row heights is unmeasured. A Table view column is resized from
its divider in the header only, and a double click does not fit it to its
contents; FileMaker Pro's divider behavior is unmeasured.

### Layout menus

Catalog flags and display order are FileMaker facts
([file-metadata.md](re/file-metadata.md) 1.1): the Browse, Find and Preview
layout popups and View > Go to Layout omit ordinary layouts with `0x08`,
render `0x01` folders as nested submenus, consume `0x02` folder-end markers
and draw `0x10` separators. Folder flags may also contain `0x08`; folders
stay structural submenu headings. Layout mode and Manage Layouts keep the
complete navigable list. The built-in sets give every layout view-only
access, so no layout is hidden from them; a custom set's layouts without
access are left out (see Privilege sets).

The closed popup keeps the current layout's name even when that layout is
hidden from menus. The open menu contains only eligible entries, so a hidden
current layout has no checked row. This is an inference: the captures show
the popup and checkmark for listed layouts only.

A layout of the same base table keeps the current record. A layout of
another table returns to the record the window last showed on that table,
or starts on the first record (or the record a local file was saved on)
the first time. FileMaker Pro keeps a current record per table occurrence:
going from Task Details to Assignee List and back returns to the task it
showed [O: FileMaker Pro 26.0.3, the reopen fixture notes for #642].
OpenMaker keeps one per base table, as the provider keeps the found set,
and returns to the same position in the found set rather than finding the
record again by id.

**Views.** Each window keeps a view for each layout it has shown, as
FileMaker Pro does within a session (layout-model.md 4.2).
- **Going back to a layout** shows it in the view it last had in that
  window, at once, including a view the layout does not store. A layout
  shown for the first time takes the view its definition stores, as does
  a layout whose last view Layout Setup has since turned off.
- **What sets a layout's view:** the Layout menu, the layout pop-up,
  Manage Layouts' Open and a script's Go to Layout all show a layout this
  way. View As, the user's or a script's, sets the current layout's view.
- **Nothing is written to the file**, so a reopened file shows the views
  its layouts store.

**Limits.**
- **Across close and reopen:** whether Pro's last view persists is
  unmeasured. Pro showed it after reopening, but only in sessions that also
  wrote the file.
- **A new window** starts with a copy of its source window's views, on the
  source's layout and view. This is OpenMaker's choice; Pro's is
  unmeasured.
- **Find mode and Preview** keep the views without changing them; Pro is
  unmeasured there.
- **A script's Go to Layout** uses the window's views. This is covered by
  reading the code, not by a test.

Menu filtering does not change script targets by key, name or number
(script-steps.md 3.2: a calculated number is step value 4),
`Get ( LayoutNumber )` (position among listed layouts, calc-functions.md) or
the Design functions. `LayoutNames` omits folders and end markers and keeps
hidden layouts and separators (file-metadata.md 1.1).

### Find mode

Find mode draws the current layout as Form view does, with no record
(`document::show_find`, client-ui.md "Find mode content"). Each field
object takes the current request's criterion for its field: the
layout's own fields by name, related ones as `Occurrence::Field` (a
portal shows its first row for them), as Perform Find reads them.
Container, summary and global fields take none, and a field whose Find
mode entry is not Edit shows its criterion without taking typing.
Requests keep their criteria by field name, so they survive a layout
change. The magnifier badge sits at a field's right, left of a
drop-down arrow, and not on pop-up menus or checkbox and radio button
sets. Entering Find mode or making a new request focuses the first field
of the tab order that takes a criterion, and Return or Enter in a
criterion performs the find.
No record calculation applies: every object shows, in its own style
(hide conditions in Find mode are not evaluated, see [conditional
visibility](conditional-visibility.md)), and merge fields show nothing.
Table view, and a layout OpenMaker cannot draw, list the table's fields
instead.

Value-list controls are Browse's own (`document::popup`, `options`,
`drop_down`, `calendar`) over the criterion: a pop-up menu's item or a
radio button sets it, a checkbox adds or removes its line (two checked
boxes find records with both words), a drop-down list's item or a
calendar's day replaces it, and the field still takes typing where
Browse's does. Their items are read with no record, so a list of
related values offers none. A set's items are read to draw it; a pop-up
menu's or drop-down list's when it opens, and shown from the next frame.

A text criterion is keyed in the form of its field's index language
(query-program.md 7.2, `FindContext::with_text_keys`): English and English
international by primary weights, System and Unicode standard padded,
Unicode raw by its code units with case kept, and language 0 in the file's
own language. A field in another language is keyed in English, which keys
most text as those languages do.

Without the host's index, the client keys each record itself. That works
in the forms above and, for finds that read the value index (`==`, `=`,
`*` and `!`), in the languages FileMaker leaves untailored, whose value
keys are English's (`om_data::rules::value_text_keys`; the set is
inferred, [#46](https://github.com/OpenMaker-Dev/openmaker/issues/46)).
Finds that read the word index there (words, ranges of text) fail with 3
until the host has the index: those languages' word breaks are not
established, and French's differ from English's
([#300](https://github.com/OpenMaker-Dev/openmaker/issues/300)). So does a
find that reads an index in a tailored language, or in language 0 when
the file's locale record cannot be read. A criterion that gives no
finder (a leading wildcard, as in `==*pfel*`) is still tested against the
stored values in English in those languages
([#588](https://github.com/OpenMaker-Dev/openmaker/issues/588)).

A find on a related field walks each relationship back from the related
records (query-program.md 6.5): their match values, every stored
repetition and each nonempty line as stored, are projected for the near
match field's type and index language (9.1, 7.2). A date compares with a
timestamp as midnight, so times on the same date remain distinct. A
timestamp compared with a time uses its time part. A match field indexed
in Unicode or System uses that index's keys
([#573](https://github.com/OpenMaker-Dev/openmaker/issues/573)). A far
field compared with near fields of different types or forms is sent once
for each, under a source slot of its own (9.2). With a hosted near index,
a Japanese match field can use shared English weights, including `ー` and
`㐀` (7.2). German dictionary uses the untailored form. A related value
that needs an unsupported key, or a text field whose language 0 cannot be
resolved from the file locale, fails the find with error 3. A request
that finds no related records contributes no matches.

**Limits.** List view shows only the current request, as one body row.
In the padded form a word's start is matched by its primary weights alone
(`equ` finds `équipe`), and a Unicode raw find keeps case: FileMaker's keys
for those criteria are unmeasured. The mock provider's in-memory finds key
text in English.
Japanese match fields that need keys fail with error 3 without the host's
index. Date–Timestamp related finds remain unmeasured in FileMaker
([#606](https://github.com/OpenMaker-Dev/openmaker/issues/606)). A far text
value matched to a near number field must parse as a number. Extracting
digits from mixed text, as in the projection example in query-program.md
9.1, is unsupported.
The badge's place, the portal's single row, empty merge fields, Return
performing the find and how the value-list controls build criteria are
OpenMaker's reading, not compared with FileMaker Pro (client-ui.md, "Find
mode content").

## Editing records

Creating, editing and deleting records use per-layout operation capabilities
and an editable-field allowlist ([record-writes.md](record-writes.md) has
the rules and limits). Form, List and Table fields take clicks and text
input, into text, number, date, time and timestamp fields: the number as
typed, a date or time as data entry reads it in the file's locale, text
that is not one refused when the record is saved. Merge text, and
protected or calculated fields are display only.
In Form and List view, New Record focuses the first object of the
layout's stored tab order among the field objects the window draws for
the new record that the user may enter, as FileMaker does
(calc-functions.md 6.1, s33: Title on the Contacts starter's Contact
Details): a hidden object or one on another tab panel is passed over,
and of a field placed twice it is that object that takes the focus. On a
layout with none of them in its tab order, the first field drawn takes
it. Table view focuses its first column (FileMaker's choice there is
unmeasured).

**Tab.** In Form and List view, Tab and Shift-Tab move the focus to the
next or previous field object of the current record in the layout's
stored tab order, by place and then paint order, as New Record picks the
first field. That holds whatever the field's control is: an edit box,
drop-down list, pop-up menu, or checkbox or radio button set. In Find
mode they move the same way through the request's criteria, and wrap, as
FileMaker Pro does on the Tasks starter (client-ui.md, "Tab in Browse");
a criterion entered has its insertion point after its text. In Table view
they move across the current record's cells in column order and wrap
within its row, never to another record, and Find mode's criteria there
go in the same order and wrap, as Pro's Table view does on Task Details.

As FileMaker Pro 26.0.3 does on the Contacts starter (client-ui.md, "Tab
in Browse"):
- the field left keeps its typing in the record's draft, as when a click
  moves the focus, and the keys typed after the Tab go to the field
  entered;
- an edit box entered has its insertion point after its text, with
  nothing selected, while a drop-down calendar's text is selected, so
  typing replaces it (Pro on Meetings' Date, #964;
  `FieldObject::tab_selects_content`); that the control decides, not
  the date or an auto-entered value, is inferred;
- Tab from the last field of the order goes to the first, and Shift-Tab
  from the first to the last;
- in List view Tab stays on the current record;
- with a pop-up menu's list open, Tab does nothing;
- a portal's fields are taken row by row at the portal's place in the
  order, the creation row included, then the field after the portal, and
  Shift-Tab goes back the same way, to the last field of the row before
  and from the first row to the field before the portal (#859; Pro on
  the Tasks starter's attachment portal, which also stops on each row's
  buttons, [#253](https://github.com/OpenMaker-Dev/openmaker/issues/253)).

Inferred, unmeasured in Pro:
- a row's several fields go by their places;
- a row the portal shows only in part scrolls into view when Tab enters
  it;
- a drop-down list's open list keeps Tab as a pop-up menu's does;
- from a field outside the order, or with no widget focused, Tab goes to
  the first field and Shift-Tab to the last;
- Tab with Ctrl, Alt or Command is not taken.

**Limits of Tab:**
- **Stops Pro has that OpenMaker passes over:** the front tab panel
  itself, web viewers, buttons and container fields ([#253](https://github.com/OpenMaker-Dev/openmaker/issues/253)). On Contact
  Details OpenMaker's cycle is Title, First Name, Last Name, Job Title,
  Company and Website, where Pro's has 11 stops.
- **Hidden tab panels:** fields on them are passed over, as in Pro.
- **Table view's columns** are its stops, in their order. A calculation
  column, or one the account may not see, is passed over; a cell the
  account may see but not change is a stop (whether Pro stops there is
  unmeasured); whether a custom tab order changes Pro's Table view
  order is unmeasured. With the current row scrolled out of view, only
  its focused cell is drawn, so Tab stays there ([#255](https://github.com/OpenMaker-Dev/openmaker/issues/255)).
- **Portal rows a scrolling portal does not draw** (scrolled out of view)
  are no stops: Tab from the last row drawn goes on past the portal. Pro
  presumably scrolls to them; that is unmeasured.
- **A Tab into a portal row** aims at the row's field by its record and
  object, not by where the row was drawn. The field takes the focus when
  a frame draws it from a fresh read of its portal, and the keys typed
  meanwhile wait for it. If it does not come back within 120 frames
  (its record left the portal), or an alert comes up, those keys go to
  the field that still has the focus. That is OpenMaker's choice.
- **Not handled:** a field whose "Go to next object using" has Tab turned
  off; that is unmeasured in Pro too.

**The Insert menu.** With a text field of the record entered in Browse,
Insert > Current Date (⌘-), Current Time (⌘;), Current User Name (⇧⌘N),
From Index... (⌘I) and From Last Visited Record (⌘') run their script
steps (13, 14, 60, 11 and 12) on that field, at its insertion point and
over its selection. A click on the menu bar or on an open menu leaves the
field entered with its selection, as FileMaker Pro leaves it, and after the
step the insertion point is after what was inserted. Any other menu
command (View > Find Mode, Records > New Record, ...) ends the field's
editing first: its OnObjectValidate, OnObjectSave and OnObjectExit run,
and the command runs once they let it go, as a click on a button does;
one refusing keeps the field entered and the command undone. Scripts read
the user's selection in the field too (`Get ( ActiveSelectionStart )`,
`Get ( ActiveSelectionSize )`, the Insert steps); Select All, Set
Selection and the Insert steps leave theirs in the editor, and Set
Selection into another field shows when its editor takes the focus.
Limits:
- Find mode's criteria and Table view's cells take none of them yet; a
  container field selected takes Picture and File only.
- From Last Visited Record replaces the field's text, as its step does here
  (whether FileMaker Pro pastes it at the insertion point instead is
  unmeasured).
- That Editing only menus keep the five is inferred, and they run there,
  as Copy's record copy does; Minimum leaves them out, as it leaves out
  Export Field Contents.
- While edit preparation holds a menu command's field exit, later menu
  choices are ignored. File > Close can still cancel the pending edit.
- An Insert shortcut typed in the same frame as other keys runs its step
  after the editor took those keys. If field traversal is pending or its
  key shares the frame, the Insert shortcut is dropped. Command-minus
  still belongs to Insert while the field is entered.
- ⌘- inserts the date while a field is entered and zooms out otherwise;
  FileMaker Pro's own shortcut for Zoom Out is unmeasured.

**The Edit menu.** In the field the user is typing in, Edit > Undo and
Redo (labelled Can't Undo and Can't Redo when there is nothing to do), Cut,
Copy, Paste, Paste Text Only, Clear and Select All do what the field
editor's own keys do: Undo and Redo through its typing history, Cut, Copy
and Paste through the platform's clipboard, Clear deleting the selection
without the clipboard. Paste Text Only pastes as Paste does, since the
editors paste text only. With no field entered, Copy copies the current
record (the Copy Record/Request step, 101), from the menu or ⌘C. In a
field the account may not change, Select All and Copy work and Clear,
Undo and Redo are refused with the alert typing gets. In Layout mode,
Clear deletes the selected object, as before. Limits:
- Clear, Undo and Redo from the menu change the field's text without its
  object triggers (OnObjectModify), which typing runs.
- Find mode's criteria, Table view's cells and portal rows' fields take
  none of them from the menu but Cut, Copy and Paste; their editors' keys
  still work.
- Find/Replace and Spelling stay disabled.

**Committing and reverting.** A record being edited shows no bar, as in
FileMaker Pro 26.0.3 (#871, observed on the Tasks starter), so the layout
keeps its place. Switching fields keeps the record's draft. A click on
blank layout canvas, the numeric keypad's Enter, Command-Enter (Ctrl+Enter
on Linux and Windows) with either Enter key, going to another record, changing mode or layout, Refresh Window, closing
the window or file, quitting, and a script's Commit Records/Requests
commit it. Keyboard Commit leaves the field through the existing
OnObjectValidate, OnObjectSave (when changed) and OnObjectExit stages,
then OnRecordCommit (#1070); a field trigger that refuses restores its
editor and keeps the record uncommitted. A successful commit leaves no
field editor focused. Save/Exit/Commit and focus departure were observed
for one ordinary local Text field in FileMaker Pro 26.0.3 with macOS keypad
Enter (re/script-runtime.md 7.1). Hosted/portal behavior and the shortcut
mapping were not compared by that case. Return in an edit box types a line
break (below, Keypad Enter and Return).

Records > Revert Record... and, in Find mode, Requests > Revert
Request... are the `om_scripts::revert_record` command: each runs Revert
Record/Request (51) with its dialog, as a script does. Revert Record... is
enabled in Browse while the record has changes no commit has kept (typed
or left by scripts, its portal rows and related fields included, or a
new record not yet committed, untouched too) and no save of it is in
flight, and disabled while edits wait for Keep or Discard My Changes.
With a field active, Revert runs OnObjectExit and OnRecordRevert as
applicable, skipping the field's save and validation triggers.
Revert Request... is enabled while the current request has a criterion
typed. Neither has a shortcut. The alert has no title and reads "Revert
all changes to this record since it was last entered?" ("... to this
find request ..." for a request), with Cancel, then Revert, and neither
is the default: Return leaves it open, and Escape cancels and keeps the
draft. Revert restores the record, keeps it current, re-reads it from
the host and leaves no field focused; on a new record not yet committed
it removes the record (script-runtime.md 6.3 O3), and the former last
record is current; on a request it clears the
criteria. That is as Pro 26.0.3 does it (#871, observed on the Contacts
starter), except the re-read, which is OpenMaker's. Pro has no Save
Record menu item, and OpenMaker adds none.

Records > Relookup Field Contents is the `om_scripts::relookup` command.
It runs Relookup Field Contents (40) with its dialog on the field
entered, as a script does (re/script-runtime.md, Relookup Field
Contents), and the field stays entered while the step starts, as for
Insert's commands. The item is enabled in Browse mode when a field of the
record's own table is entered and the record has nothing to commit, and
only under All menu commands. Typing, or changes a script left, leave it
disabled: FileMaker's command keeps such a record uncommitted, while the
step commits it first, which would save it past the field's validation
and save triggers
([#624](https://github.com/OpenMaker-Dev/openmaker/issues/624)). So do a
running or paused script (the step's error, a Cancel included, would
fail a paused script's open transaction) and a hosted edit still being
prepared. A related field leaves it disabled, because the step cannot
relook up through a related field. The dialog is titled Relookup
Field Contents and asks "Look up the values again in all N records of
the current found set, through the field “Name”?". It has Cancel and OK,
and OK is the default. That text is OpenMaker's; FileMaker Pro's wording
is unmeasured (re/auto-entry.md, Lookups). Cancel relooks up no record.

**Limits of committing and reverting:**
- **Keypad Enter and Return.** egui reports both keys as Enter, so the
  app's event loop marks keypad Enter's events before egui reads them
  (`om_gui::keypad`). A program that runs `om_gui::run` with a logger of its
  own gets eframe's own loop instead, which keeps eframe's errors its
  return value, and there keypad Enter acts as Return.
  - Keypad Enter commits from a field of the record, or with no field
    active. In an open menu or list, and in the toolbar's boxes (Quick
    Find, the record number), it acts as Return.
  - From a field whose "Go to next object using" includes the Enter key
    (field flag `0x80`, layout-model.md 5.4.1), it goes to the next object
    as Tab does, and so does Return from one whose option includes the
    Return key (`0x40`). That FileMaker Pro goes on there, rather than
    committing or typing a line break, is inferred from the option, not
    measured. With Shift or Option either key still goes forward; what Pro
    does then is unmeasured. Return goes by the field that receives it,
    also after a Tab typed in the same frame; the keypad's Enter typed so
    goes by the field the Tab left.
  - From any other edit box, Return types a line break, stored as
    FileMaker stores one, a carriage return; a pasted line break is stored
    so too, and text already stored is left as it is, also by Undo and
    Redo. While it is edited, the field's text wraps, is aligned and is
    spaced as at rest, and it scrolls to keep
    the insertion point in view, with a scroll bar only where the field
    has one. Up and Down move between lines. Drop-down lists, pop-up
    menus, calendars and Table view's cells keep one line, and Return ends
    their editing: what FileMaker Pro's Return does in them is unmeasured.
    In Find mode, Return still performs the find.
- **A new record and reconnecting.** Records > New Record and Duplicate
  Record run the New Record/Request and Duplicate Record/Request steps:
  the record is held in the window's record session until it is
  committed, and Revert removes it, as Pro 26.0.3 does ([#261](https://github.com/OpenMaker-Dev/openmaker/issues/261);
  record-writes.md, "A window's record session"). A held record that sent
  nothing to the host is carried to the new connection on Reconnect, its
  typing included (recovery.md, "A new record not saved yet";
  [#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)). Whether Pro holds a duplicate uncommitted is
  unmeasured; OpenMaker holds it as a new record.
- **Revert Request** clears the request's criteria. For a request that
  held criteria before it was entered (Modify Last Find), Pro may bring
  those back instead; that is unmeasured.
- **Revert Record/Request with dialog** in a script asks the menu's
  alert. That is inferred; Pro's step dialog is unmeasured.

A failed save keeps the draft and blocks the deferred action.
A successful one shows at once the values the host kept for the fields
edited, and the record's other changes (calculations, auto-entered
values) when its page is read again.
Every layout saves record changes automatically: OpenMaker does not yet
read Layout Setup's "Save record changes automatically" (layout flag
`0x10`, layout-model.md 4.2), so leaving a changed record never asks "Save
changes to this record?", and neither does Commit Records/Requests with
dialog On, which in FileMaker asks only on a layout with that option
cleared (re/script-runtime.md, Errors and dialogs;
[#153](https://github.com/OpenMaker-Dev/openmaker/issues/153)).

**Related records.** A field of a portal row's own occurrence, and a related
field outside portals (it shows the first related record, in the
relationship's order), take typing when the occurrence is one relationship
from the layout's and the related table's own rules let a user edit the
field (`RecordCapabilities::related`, from `RecordPlan::editable_fields`).
A table of another file is that file's: its rules decide, its ordinary
stored text, number, date, time and timestamp fields take typing, and the
file writes the record (record-writes.md, "Related records of another
file"). The edit belongs to the record the layout
shows: the window keeps it with what scripts left on the record
(`Uncommitted`), calculations see it, and saving commits the record and its
related records together through the worker, as Commit Records does
(script-runtime.md 13.24): a related record's validation failing refuses
the whole save, nothing written and every edit kept; Revert Record drops
them. The typed values are checked by a user's entry rules
(`ScriptWrite::typed`), and a related record another client changed since
the window showed it is refused (306) rather than overwritten.

The user's focus and typing in a related field's edit box on the current
record run its object's triggers (#1005), as on the layout's own fields:
OnObjectEnter as the field is entered, OnObjectModify as it is typed into,
and OnObjectValidate and OnObjectSave (when it was changed) and
OnObjectExit as it is left. Their scripts see the field and its portal row
active: `Get ( ActiveFieldName )`, `Get ( ActivePortalRowNumber )`,
`Get ( ActiveFieldContents )` with what was typed, and the row's related
fields. The field stays active through its enter and modify triggers'
scripts, which read the portal's rows again (a script's variables do), and
its editor takes the focus back after them. An exit trigger that refuses
the move (OnObjectValidate, OnObjectSave or OnObjectExit returning False)
gives the row's editor the focus back, with what was typed (#1050) [I:
FileMaker Pro keeping a portal row's field active then is unmeasured]. A
click on the layout's background that leaves the field commits the record
once the field's exit triggers let the click go on (#1066), as FileMaker
Pro 26.0.3 ran a portal row's OnObjectSave and then committed ([#277](https://github.com/OpenMaker-Dev/openmaker/issues/277)'s
native cases); a trigger that refuses the exit leaves the record
uncommitted. Keyboard Commit uses the same field-exit path (#1070).
The held commit or click, like a button clicked out of a field, goes on
only on the record it was made on. That record is the one the worker
reports its window left current when the trigger ended (none, after an
empty found set), not the window's page, which a trigger's Show All
Records can leave stale until it is read again (#1073). When the worker
can't say (the trigger ended in another window, or the read failed), the
page decides. Otherwise the order of these triggers against the commit
of the related record is unmeasured on FileMaker Pro.

Typing into a portal's creation row (shown when the relationship allows
creating records from the layout's side, file-metadata.md 4.1), or
choosing in its pop-up menu, checkbox or radio button set, drop-down list
or drop-down calendar, begins a related record: the row becomes the
record's, a new creation row follows it, and the save creates the record
with its match fields from the record as it is then
(`match_begun_records`, as Set Field creates one, script-runtime.md
13.24); a value the user typed into a match field stays. The portal's
records are read again as the record begins; meanwhile it shows the rows
it showed last, so the row's editor keeps the keyboard, the caret and
every key typed in between. Clicking a pop-up menu in a stale row opens
the list once the same related record is ready. Using another ready
related control cancels the waiting click
([#288](https://github.com/OpenMaker-Dev/openmaker/issues/288)). Until the save,
the begun record's row shows only what was typed: calculations and other
objects of the row see no record, as a script's record to create is not
seen. A value-list control there offers its list as in a saved row, except
that a list of the values related to the row's record offers none until
the record is saved (FileMaker Pro creates the record at once;
unmeasured). The creation row and begun rows show the row's other objects
too (buttons, icons, text), over no record (`NO_RECORD`): their fields
read empty and hide conditions are decided on that, an editable field's
too, so a hidden one takes nothing. A button there runs in that row, which
the worker finds by its number (`enter_clicked_row` without a record
identity): Set Field into the creation row creates a related record, and
Delete Portal Row there deletes nothing (script-runtime.md 13.24).

A portal saved with FileMaker Pro 26.1's "Place row for new record at top"
(portal option `0x400`, layout-model.md 5.3.2) draws the creation row,
and the rows begun in it, above its records, when it does not scroll
(option `0x1` clear); a portal that scrolls keeps them last, as FileMaker
Pro 26.1.1's `DataEntryModel::IsVisibleTopNewPortalRow` decides
([#343](https://github.com/OpenMaker-Dev/openmaker/issues/343)). Rows are numbered where they are drawn, in Browse, for Tab and
for scripts alike: the creation row is row 1, the records follow, and
scripts reach the creation row there (Go to Portal Row, a button in the
row) as Browse shows it [I: how FileMaker numbers a row drawn at the top,
and whether a script reaches it, are unmeasured]. A portal whose initial
row is past 1, scrolling or not, draws the records from that row on and
numbers its rows from there, whether or not records fill the rows before
it, also for scripts. Go to Portal Row [First] and Go to Field go to the
initial row, Previous stops there, and a number before it is 101. Go to
Related Record from a filtering portal's row includes earlier filter
matches in the found set [I: the effect of Initial Row is unmeasured].
A Go to Portal Row whose OnObjectExit begins or deletes a row moves to
the row it chose, found again by what it shows (a record being created
by its draft, whatever other drafts go), or fails with 101 when that row
left the portal. The option's storage is read from FileMaker Pro 26.1.1's
binaries, not from a file it saved, and Portal Setup does not offer it.

A step that puts the focus in a field of a portal row (Go to Field, Go to
Object, Go to Portal Row with a field active), as the starter solutions'
add buttons do with Go to Portal Row [Last] and Go to Field, gives that
row's editor the keyboard, the creation row's too, so typing begins the
related record; Go to Field [Select/perform] selects the field's text.
The row must be showing: a portal is not scrolled to it.

A click on a button or into a field of a row that a portal shows while
it reads its records again (as typing in the record makes it, when the
portal's relationship compares what was typed, or its filter or sort
reads data not read yet) waits for the rows as read. The button then
runs, or the field takes the keyboard, in the row of the record it was
clicked on. When that record is no
longer the portal's, or its row is no longer shown (a portal is not
scrolled to it), the click goes; it never acts in another row (#781,
[#281](https://github.com/OpenMaker-Dev/openmaker/issues/281)). A field's held click also goes when another field takes the
keyboard meanwhile. It does not put the insertion point where the user
clicked, and keys typed before the field has the keyboard go to no
field (OpenMaker's choice; FileMaker Pro is not measured).

A container field of a portal row's occurrence takes Insert Picture and
Insert File: a click in the row selects it, as a container of the
record's own table is selected, and the file goes into that row's record
(in the creation row, a new related record's), committed with the record
(re/containers.md, Implementation notes); Export Field Contents writes
the selected row's file. What a portal row's record keeps uncommitted
shows before the save, a step's changes too: the inserted picture, and a
record a step created through the creation row, as a row of its own,
which keeps the match fields it was created with (a script's later Set
Field of one too) rather than taking them again at the save.

A click in a portal row outside its fields selects the row (the portal's
active row style); a field's focus, a click on the layout, or another
record or layout ends it. With a row selected, Records > Delete Record and
the toolbar's Delete Record delete the row's record when the portal allows
deleting its records (`kAllowDelete`, layout-model.md 5.3.2), and are
unavailable when it does not, as FileMaker Pro disables them on the Tasks
starter's attachments portal. The dialog asks "Delete this related
record?" (FileMaker Pro's text for a portal that allows deleting is not
known; its Delete Portal Row alert reads "Permanently delete this one
related record?", client-ui.md), and the record leaves the
portal at once but is deleted with the record's next save, as Delete
Portal Row deletes it (script-runtime.md 13.24): Revert Record brings it
back. Whether FileMaker Pro's Browse deletion also waits for the commit is
unmeasured. Not yet:
- scripts do not see the selected row as the active portal row
  (`Get ( ActivePortalRowNumber )`);
- occurrences further than one relationship, a field
  of the layout's own occurrence or another occurrence in a portal row,
  and the record the layout shows reached
  through a self-join (a portal row of the record itself);
- of related field objects' triggers: OnObjectKeystroke; OnObjectModify for
  a choice in a related pop-up menu, set, list or calendar; and the active
  field in the scripts of a related field outside portals (they run, but see
  no active field);
- editing only on the current record: a related field of another List view
  row is read-only until its record is current.

The related record is locked when the record is saved, as OpenMaker locks
the record itself; FileMaker Pro locks a record as editing begins
(write-path.md 4.1), and whether it also locks the parent when a portal row
is edited is unmeasured.

Revert reloads the host's values. Delete Record asks "Permanently delete
this ENTIRE record?" with Cancel and Delete, local or hosted, as FileMaker
Pro does on a local file (client-ui.md, "Delete confirmations"; its button
order and default are unmeasured, and OpenMaker keeps Cancel left and
Delete right), then locks, deletes and verifies the record (write-path.md
7). A server-side
success is never reported for a local-only change. Save conflicts and lost
connections are handled as in [recovery.md](recovery.md). The ignored
`live_edit` test creates, edits, navigates and deletes a record through the
GUI on an in-process `om-server`, checking the host's records after each
step; `om-provider`'s `om_server` tests check the auto-entered values
(creation and modification fields, UUID keys) and recomputed calculations
of each write (record-writes.md, Tests).

**Value-list controls.** Checkbox and radio button sets use their labels'
resolved line height, including saved leading, as the minimum row pitch.
Their checked icon's border and fill override the option item's styling;
mark ink remains independent. An explicit text-component left offset plus
its left margin positions each label when the lengths are absolute or em;
em follows the font used to shape that label. Its click region and column
width include the positioned label. Missing or unsupported component
geometry, including a combined position before the item origin, keeps the
fallback gap. Marker dimensions and that fallback gap
are inferred; the private owner comparison shows a mismatch, and this
bounded correction has only a neutral Linux fixture comparison, without a
new macOS/FileMaker measurement.

**A single checkbox without value.** A checkbox set with FileMaker Pro
26.1's "Display single checkbox without value" (field flag `0x400000`,
layout-model.md 5.4.1) draws one box without a label, checked when the
field holds the list's first value. A click or Space checks it, storing
that value, or clears it, storing the list's second value, or emptying
the field when the list has one value (`om_data::choice::single_checkbox`,
from Claris's release notes). A field holding anything else, another
value or other lines with the first, is not written over: the click is
refused with an alert (error 3), the data kept, until what FileMaker Pro
does then is measured. A letter a keystroke trigger lets through changes
nothing. In Find mode it gives the first value as the criterion, or none
(`single_checkbox_criterion`) [I]. Inferred, not measured: the box's place
in the field (where the set's first item would be), its look, and that a
value holding the first value among other lines checks it. The flag's storage
was read from FileMaker Pro 26.1.1's binaries; no file made by 26.1.1 has
been read.

Checkbox and radio button sets, pop-up menus and
drop-down lists (layout-model.md 5.4) read their items through
`om_data::choice`, the operation `om-author`'s `list_choices` also runs.
- **The list.** The items are complete or the control shows why it cannot
  offer them:
  - another file's or a calculated list;
  - more than 100,000 items;
  - a read that failed.

  While the data is still being read, the control waits.
- **An unstored calculation's values.** A list of a field's values whose
  field is an unstored calculation evaluates that calculation on each
  record, as a calculation reading the field does
  (`FileCalc::read_field_computed`, #1058). That covers the choices, the
  provider's `list_choices` (with the connection's session) and
  `ValueListItems`. Its text is split into one item per line, as a stored
  text field's is [inferred].
  - While the calculation reads data still being read, the list waits, as
    for a stored value not yet read.
  - A record whose value OpenMaker cannot compute fails the list: a `?`,
    data the provider could not supply, or a field or table the schema
    does not have. Such a value is never an item, and the list is never
    shorter for it.
  - The calculation sees no global variables, as when OpenMaker computes
    a summary or sort key itself.
  - Such a list of all of a field's values is built once on the worker
    for every object and record, from committed values ([#290](https://github.com/OpenMaker-Dev/openmaker/issues/290); "One
    build per list, never in paint", below). Its calculation may read
    other tables and the session: it is built again after any table's
    data changes, and when the window's context (its size, for example)
    or the date changes. A build asked before such a change is let finish
    and its answer dropped, then one build for the new context follows,
    so a resize keeps one build at a time on the worker. A list of an
    unstored calculation's related values is built on the worker per
    record, as a list of related values is (below).
  - Validation against a related-values list of such a field computes it
    on each related record, and refuses with 3 only a record whose value
    cannot be computed (`om-provider`'s `related_lists.rs`;
    record-writes.md, "A list of an unstored calculation").
- **When the items are read.** A checkbox or radio button set reads its
  items to draw them. A drop-down list or pop-up menu reads them only once
  it is entered, opened or clicked, or the pointer rests on its arrow; a
  pop-up menu whose list shows only its second field's values also reads
  them to show the label of the stored value. So a closed list builds
  nothing and asks the host for no records. A click on a list whose items
  are still being read opens it once they are.
- **One build per list, never in paint.** A list that does not depend on
  the record (custom values, or all of a field's values, an unstored
  calculation's among them) is built once for every object, record and
  portal row, from committed values.
  - A field's list is built on the provider's thread (`list_build`, #745),
    which reads its table's records in two requests: their ids, then their
    values. Meanwhile the control waits as for any data being read, and the
    window keeps drawing and taking input.
  - It is built with the window's calculation session and a summary
    cache, as the window's paint built it.
  - A build whose reads straddle a change of the provider's data is done
    again from fresh data, at most three times, and then fails rather
    than answering ids and values of two moments.
  - A build that lacked data held by a related file's unreachable host
    gives no list. It is asked for again once the host may be reached
    ([#229](https://github.com/OpenMaker-Dev/openmaker/issues/229)), whatever else grows the schema meanwhile. A build that got
    everything it read is shown, even while another host is unreachable.
  - A record's unsaved values are not items until it is saved, as FileMaker
    builds such a list from the field's index [inferred, [#148](https://github.com/OpenMaker-Dev/openmaker/issues/148)].
  - The list is built again after the data of a table it reads changes.
    One the window stopped reading before it was built (#987) is asked for
    again when a control shows it.
  - An unstored calculation's values are computed on each record of its
    table, in that record's context, so no record of the window decides
    them ([#290](https://github.com/OpenMaker-Dev/openmaker/issues/290)). The calculation may read any table and the session: the
    list is built again after the data of any table changes, and when the
    window's calculation session changes (its layout, record, mode or
    account). A record's unsaved values are not read, as for a field's
    list [inferred: how FileMaker lists an unstored calculation's values
    is unmeasured]. On a debug build, with 30,000 records, the closed
    pop-up menu that shows a label from such a list no longer blocks a
    frame for 1.1 s; its longest frame while the worker builds the list is
    44 ms.
  - A list of related values is built on the worker too, once per record
    and portal row it is shown on, over the window's unsaved changes as
    the window's paint read them, starting from that record
    ([#290](https://github.com/OpenMaker-Dev/openmaker/issues/290)). Its relationships can lead through any table, so it is
    built again after any data change (a page read under the same data
    generation is none), and after the window's unsaved changes, or the
    records they are laid over, change; one whose list a calculation
    computes also after the window's calculation session or the date
    changes. A build asked before such a change is let finish and its
    answer dropped, so typing, scrolling or a resize keeps one build per
    list and record on the worker. While the stored values of a record
    with unsaved changes are not in the window (a refresh dropped them),
    the list waits for them rather than be built over the stored values
    (one the provider cannot read, or the host has no record of, makes the
    list unknown, error 3). The
    build reads the record it starts from (and the portal row) first, so a
    new record a transaction holds relates records by its own values, as
    paint did. A build whose reads took a change the window had not seen (a
    newer data generation) has the window read its data and lists again;
    one read under a generation older than the window has seen is not
    taken. A reconnected file counts generations again, so the window
    forgets the newest it saw then. A page of the generation the window has
    seen keeps the built lists and their waits. A related host it could not reach holds that record's list only,
    until the data, the unsaved changes or a refresh change its inputs.
    Built lists are kept within 64 MiB and 4,096 lists beyond those the last
    frame showed, those shown longest ago going first. Like a list of all of a field's values it sees no global
    variables. On a debug build, with 30,000 related records,
    the closed pop-up menu that shows a label from such a list no longer
    blocks a frame for 1.05 s; its longest frame while the worker builds
    the list is 43 ms.
- **A hosted file's field lists.** A list of all of a stored text field's
  values, with no second field and not only related values, comes from
  the host. One CreateValueListEx is sent when a control first needs the
  list, as FileMaker Pro sends it (#464; never from paint). The items are
  in the index's order, with values whose index keys are equal listed
  once, each with the text the host sends; which text FileMaker Pro shows
  among values differing only by case, accent or width is unmeasured. The
  list is kept while a control shows it (the field entered or its list
  open), and asked for again when it is next opened. When the host cannot
  give the whole list (no index, a cut list, a key that may stand for
  several values, an unreadable reply or a failed request), the records
  give it, as above. Number, date and time fields, other lists and local
  files read the records.
- **A pop-up menu** takes no typing. It shows the stored value, including
  a value that matches no item, with its arrow only when the object
  includes it (`HasIcon`). A click enters the field, after the left
  field's exit triggers, then opens a menu of the items: dividers as
  lines, the current item checked; Escape closes it unchanged. Choosing an
  item replaces the field's value with the item's stored value through
  `om_data::choice::choose`. The change then goes the way a typed edit
  goes: OnObjectModify, the record's draft, validation, the lock and
  OnRecordCommit when the record is committed.
- **"Other…" and "Edit…"** (#869) end a pop-up menu whose object asks for
  them (flags `0x1` and `0x8`, `om_data::choice::menu_commands`), after
  its items and stored dividers, as FileMaker Pro 26.0.3 shows them
  (layout-model.md 5.4). They are commands, never items: a choice,
  completion or find criterion never meets them.
  - Other… opens Pro's dialog: a multi-line box, Cancel and OK. It holds
    the field's value when no item stores it, selected, and is empty
    otherwise. OK enters the text as typing would
    (`om_data::choice::other_value`): the record's draft, OnObjectModify,
    the commit. The text is not added to the list, and the menu then
    checks Other…. Cancel or Escape changes nothing. In a portal row the
    text is the related record's edit, as a chosen item is.
  - Edit… commits the record, as Pro does, then opens Pro's Edit Value
    List on the list as the host stores it now
    (`DataProvider::custom_value_list`): titled with the list's name in
    curly quotes, one multi-line editor with an item per line and `-` for
    a divider, Cancel and OK. OK saves the lines as the list's values
    (`DataProvider::set_value_list_values`, authoring operation 24), if
    the list is still at the revision the dialog opened on; then the menus
    read the lists again. A final empty line is dropped (inferred). Cancel
    and Escape change nothing. A list of a field's values is refused with
    error 3, and so is a file hosted by FileMaker Server, which has no
    authoring extension.
  - A drop-down list offers neither command: FileMaker Pro 26.0.3 showed
    no Edit… in one whose object stores the Edit flag (#869).
- **A drop-down list** is the field's edit box: it takes typing, also a
  value that is no item, with the edit box's focus, draft, triggers and
  commit. When the object includes its arrow (`HasIcon`), a click on the
  arrow enters the field, after the left field's exit triggers, then
  opens the items in a list laid out as the pop-up's menu; opening it
  changes nothing. A second click on the arrow closes it. Typing with the
  list open edits the text, through the field's keystroke triggers, and
  leaves the list open. Choosing an item
  replaces the whole value, typed text included, through
  `om_data::choice::choose`, then goes the way a typed edit goes. Escape
  closes the list and leaves the field, its focus and the record's draft
  as they were. Without the arrow, the list opens when the field is
  entered, by a click or Tab, as Claris's help describes ("Setting up a
  field control"); Escape closes it the same way, and it opens again the
  next time the field is entered. Not measured on FileMaker Pro: whether
  it opens the list on entry with the arrow too, and its Escape, geometry
  and keyboard. With the arrow, opening only from the arrow is
  OpenMaker's choice.
- **Auto-complete.** A drop-down list with "Auto-complete using value
  list" (flag `0x800`, `FieldObject::auto_completes`) completes what is
  typed from its items, as FileMaker Pro 26.0.3 does on the Contacts
  starter's Company (layout-model.md 5.4):
  - typing at the end of the text, when the text then begins an item,
    shows the rest of the first such item after it, selected
    (`om_data::choice::completion`, `drop_down::complete`); a list open
    then closes, so Tab moves on;
  - the typed letters keep their case: "a" shows "acme";
  - the next letter replaces the selected part and completes again,
    Backspace removes it, and an entry no item begins stays as typed;
  - leaving the field (its editor losing the focus to another field or
    the canvas; an alert holding it entered does not count) keeps the
    item's own text while the field still holds exactly the completed
    text: "acme" is stored as "Acme", also when the field the focus moved
    to commits in the same frame. Text typed on, a Backspace, a Revert or
    a script's own draft is kept as it is.

  The items are those the field reads once entered, as its list reads
  them (on a hosted file one request, which typing does not repeat). While
  they are being read nothing completes. A portal row's or related
  field's drop-down list completes the same way, and so does a criterion
  in Find mode. In Table view, a cell whose field the layout shows as such
  a drop-down list completes from that list's items, read while the
  current record's cell is entered (`DocumentState::text_edit`), also
  while the row is scrolled away, as FileMaker Pro does in Table view; of
  several such objects of the field, the first in the layout's order
  lends its list (inferred).
- **A drop-down calendar** of a date field is the field's edit box: it
  takes a typed date as any date field does (record-writes.md), and offers
  a calendar the way a drop-down list offers its list. With its icon
  (`HasIcon`, "Include icon to show and hide calendar"), a click on the
  icon enters the field and opens the calendar, and a second click closes
  it; without the icon, the calendar opens when the field is entered.
  Escape closes it and keeps the field entered. The calendar opens on the
  month of the field's date, or of today; its arrows go back and forward
  a month, and Today picks today. A picked day replaces the value with
  that date in the file's date format (`Date::format`: `11/20/2026` in
  en-US) and then goes the way a typed date goes. The icon is an
  OpenMaker glyph; the theme's `-fm-icon` is not read. A calendar of a
  field that is not a date stays an edit box. Not measured on FileMaker Pro: the calendar's look,
  its first day of the week (OpenMaker's is Sunday), its keyboard, where
  it opens, and what it does in a timestamp field.
- **In a portal row, or of a related field outside portals** (#537), a
  pop-up menu, checkbox or radio button set, drop-down list or calendar
  edits the related record as the related field's edit box does (above):
  the item or day chosen is the related record's edit, shown in the row
  and committed with the record the layout shows. A pop-up menu or set
  there changes at once, with no object triggers (a related edit box runs
  its own, "Related records" above); a drop-down list or calendar opens from its arrow or
  icon, or as the field is entered without one. Whether FileMaker Pro
  runs a related control's triggers, and in what order, is unmeasured.
- **Refused, with the reason on hover** (over a drop-down list's arrow,
  whose text still takes typing where the edit box does):
  - a control showing several repetitions, or a related one the related
    table's own rules do not let the user change (one further than one
    relationship is shown only);
  - a field entry other than Edit;
  - a field the connection may not change.
- **Not offered yet:**
  - "Other" and "Edit" items in checkbox and radio button sets, unmeasured
    in Pro;
  - either command in Find mode;
  - the keyboard in a menu, list or calendar beyond Escape.
- **Auto-complete's limits.** Ignoring case is observed; which item wins
  when several begin with the typed text (the list's first here) is
  inferred, and so is a value typed in full in another case keeping its
  own case (no completion was left to accept). Find mode's criteria in
  Table view (a list of the table's fields) do not complete; Pro's
  reading there is unmeasured.
  The keypad's Enter as Return is the tool's input, not a physical
  keypad.

## Scripts

In Browse, an actionable layout button whose stored "Show hand cursor"
option is on requests the operating system's pointing hand on hover. The
same response covers grouped buttons, button-bar segments and popover
buttons. Ordinary text, actionless segments, Find/Layout mode, masked or
disabled content, and buttons while input is held or saving do not request
it; covering popovers use the normal hit-test ordering. Leaving the hit
region lets the platform cursor reset. Linux GUI tests check the cursor
request; the macOS white-glove shape remains unmeasured.

Buttons, the Scripts menu and script triggers run the file's decoded scripts
in `om-scripts`' runtime ([scripting.md](scripting.md)). A step or
calculation OpenMaker does not support stops the script with an error
(error 3 for a step) rather than being skipped.

### Export Records and Save/Send Records As

Three commands write the window's records to a file. Each runs its script
step with the script runtime, in the originating file and window:

| Command | Where | Step |
|---|---|---|
| Export Records... | File menu | Export Records (36) |
| Excel... / Save as Excel... | File > Save/Send Records As, the Browse toolbar's Share menu, the Preview toolbar | Save Records as Excel (143) |
| PDF... / Save as PDF... | the same three places | Save Records as PDF (144) |

Save Records as Excel and as PDF are commands in `om_scripts::save_records`
(AGENTS.md, "Everything is a command"): a stable id, label and menu path,
typed parameters (format, records being browsed or the current record, an
output path, with dialog), an availability check (a layout in Browse or
Preview mode) and the step that performs them, whose error code is the
result. The records choice is stored as FileMaker stores it: in
`mStepValue` for PDF and in Excel's option slot 1 with Restore
([script-steps.md](re/script-steps.md) 11.7); `Records::of` reads it back
for every host. The menus and toolbars run the command with the records
being browsed and the step's dialog; the window adds only its own state:
connected, not saving, and not waiting for a script or dialog. Export
Records has the same window rule. Save Records as Excel of the current
record writes the field-name row and that record alone; summary values
are based on the window's found set. Local FileMaker Pro 26.0.3 checks
confirm a total, running total and count (including restart), List,
weighted average, standard deviation and subtotaled fraction in Current
record Excel exports
([script-steps.md](re/script-steps.md), Exports). OpenMaker's export still
misses a restarted value at a break ([#264](https://github.com/OpenMaker-Dev/openmaker/issues/264)) and a sorted subtotaled
fraction ([#265](https://github.com/OpenMaker-Dev/openmaker/issues/265)), though Browse shows those values. With dialog off,
Current record with Restore wrote only the current record and Restore off
wrote the whole found set whatever choice the step kept, as OpenMaker does
([script-steps.md](re/script-steps.md) 11.7); runs with the dialog, and
FileMaker Pro's dialog itself, were not compared.

Pending record edits pass the commit and OnRecordCommit veto flow first; a
refused save keeps the edits and stops the command. The step then shows its
own dialog (the output file, and the records being browsed or the current
record; for PDF, a save chooser like File > Open's, with Save As and
Where) and writes the file with the step's implementation, as a
script with dialog on would: the same output, paths, errors and alerts
([portable-pdf.md](portable-pdf.md) for PDF, the export engine below for
Excel). The Share menu's Print... runs Print (43) as File > Print does.
Save/Send Records As > Snapshot Link... and the Share menu's Save as
Snapshot Link... stay disabled: Save Records as Snapshot Link (152) has no
implementation. No agent tool runs these commands yet: the PDF report
builder lives in `om-gui` and server scripts do not run either step. Whether
FileMaker Pro fires OnRecordCommit for these menu commands, and the
submenu's exact item text ("Excel...", "PDF...", "Snapshot Link..."; Claris
documents "Save/Send Records As > Excel"), were not measured. Providers
without the required calculation access report the unsupported-step error
rather than using a separate fallback.

Export Records' dialog takes an output path (relative paths use
Documents), format, character set and field order. Formats are tab-separated, CSV, Merge, HTML
table, FMPXMLRESULT XML, Excel `.xlsx` and dBase. Export uses the current
window's found set in its current order. Cancel or Escape returns before
output is created or replaced. Output folders must already exist.

This reuses the combined script dialog, not FileMaker's sequence of dialogs
([Exporting data](https://help.claris.com/en/pro-help/content/exporting-data.html),
[Export Records](https://help.claris.com/en/pro-help/content/export-records.html)).
There is no grouping editor, saved last-export settings, XSLT,
FMPDSORESULT, or automatic open or email option. Container fields are left
out of every format, as FileMaker Server's engine leaves them out
(script-steps.md, "Implementation notes (import/export)", Exports);
FileMaker Pro's dialog is not measured.
Scripted grouped exports keep their engine. Layout data formatting is not
supported and its checkbox is disabled with an explanation. The engine's
limits in [script-steps.md](re/script-steps.md#implementation-notes-importexport)
apply. The dialog has not been compared with FileMaker Pro's.

## Developer tools

### Layout mode and Manage Layouts

Layout mode uses the provider's `LayoutGeometry` at the window zoom.
When the decoded layout is available, the canvas uses its stored
background, part colors, text, shapes, fields, button labels, button
library icons and standalone library images. These use Browse's static
paint routines in their normal style at the current draft frames. Button
icons and labels use the saved placement. Merge text shows its stored
markers and fields show their design names. The editor draws this artwork
without evaluating record calculations or running document actions. If an icon
cannot be resolved, a labeled button keeps its full-width label and an
image-only button uses the canvas's outline and caption.
Pasteboard objects, unsupported objects and geometry-only providers still
use outlines and captions. Design-time web viewer placeholders are tracked
in [#1019](https://github.com/OpenMaker-Dev/openmaker/issues/1019).
The scrollable canvas, part tabs, objects and selection handles scale
together. Placement, movement, resizing and the Inspector use the original
layout points at every zoom. The toolbar, Objects pane and Inspector keep
their size. The wheel and scroll bars scroll the canvas at every zoom.
Touch drag to scroll is available at Actual Size.

Objects that cross the layout edge retain their full outline, with artwork
clipped to the layout. Borderless fields and text have a faint editing
boundary. FileMaker's boundary display options have not been compared.
Contents of closed popovers still appear on the canvas
([#1019](https://github.com/OpenMaker-Dev/openmaker/issues/1019)).

The Objects pane is 220 pt wide, lists the layout's objects front to back,
filters by kind or known field name, and selects and reveals the matching
canvas object. Inspector tabs are interactive; Position shows real bounds
with pt, in and cm conversion, and Data shows a known field binding and
type and the field's control style and value list. A field object shows
its field's name on the canvas, and a field another occurrence shows (a
portal's, say) `::Field`, FileMaker Pro's Layout mode default with View >
Show > Sample Data off (not compared with a capture). The Objects pane
and the Inspector name that field `Occurrence::Field`. A field of a
related file that is not open shows its object kind. Selection resets when the layout geometry or the file changes. These
dimensions and interactions are OpenMaker's choices: FileMaker's runtime
behavior was not observed (re/client-ui.md, Unknowns).

**Placing a field.** The Field tool places an edit box with the operation
`om-author`'s `place_field` uses (`DataProvider::place_layout_field`,
`om_data::layout_placement`; [authoring](authoring.md), "Fields on
layouts"). Saving a placed edit box adds it at the end of the layout's
stored tab order.
- **Fields sidebar.** Open Objects, then Fields to list the selected
  layout's own table's placeable fields. Search filters definitions, not
  already-placed objects. Drag a field onto the design surface to add a
  bound draft (120 by 44 pt, top-left at the rounded drop point, with Create
  label on). OpenMaker uses 44 pt on every theme so the default Universal
  Touch edit box can show its text in FileMaker Pro. Pro's own sidebar
  drop dimensions are unmeasured. Additional drops
  keep earlier drafts. Select one on the canvas or Objects to edit its
  binding and frame in Inspector. Save and Revert have the same deferred
  semantics as fields drawn with the Field tool.
  The drag carries document, layout, revision, table and field identity;
  a changed layout, another document or an outdated field choice cannot
  substitute a field of the same numeric ID. Manage Database commits
  refresh both sidebar and Inspector choices without reopening.
  The table name comes from the layout's occurrence and schema, also in
  the layout bar; an unavailable binding stays unavailable. Other tables,
  related fields, containers, drag preferences and defining/editing field
  definitions in this pane are not implemented. Use Manage Database for
  definitions and the existing tools for other objects.
- **Selection.** Plain click selects one saved object or field draft;
  Shift-click adds/removes it from the selection, on the canvas or in
  Objects. Blank canvas clicks clear selection. With the Selection tool,
  Edit > Select All (Cmd/Ctrl+A) selects every shown canvas object and
  field draft. A box starting on blank layout space replaces the selection
  with the objects it crosses. Hold Cmd on macOS or Ctrl elsewhere to
  select only objects entirely inside. Both use the canvas's existing
  flat object list, including nested entries it already shows. Draft
  text, buttons and portals keep their existing editors. Tool-specific
  selection and popover-scoped boxes are not implemented. Dragging any
  selected movable member moves the whole selection, including a mix of saved
  objects and unsaved field drafts. Origins snap to whole points, keeping
  each object's size and the spacing between whole-point members; fractional
  stored origins are rounded before saving. A Shift-drag of an unselected
  member adds it rather than replacing the selection (OpenMaker's choice;
  native Shift-drag axis constraints are not measured). If any member
  cannot move, the selection stays put and Inspector states why.
  Unmodified arrow keys nudge the selection by one layout point, through
  the same whole-point frame and draft checks as a drag. A focused text
  editor keeps its keys. Delete and Edit > Clear validate the whole
  selection before staging deletions;
  an unsupported member refuses the whole action, without deleting peers.
  These changes stay unsaved until Save; Revert/Discard clears additions,
  moves and deletions. Selection belongs to the exact shown layout revision.
  Multiple-object Inspector frame/binding/control edits and handle resizing
  are not supported: Inspector shows the selected count and asks for a
  single selection. It never edits an arbitrary member. Native mixed
  locked-object and bulk Inspector behavior has not been measured.
- **Drawing and choosing.** Each drag on the canvas adds an independent
  field draft, in whole points, as a dashed box over the layout. The
  ordinary Field tool returns to Selection after the drag; double-clicking
  Field Tools locks it for repeated use. The Inspector shows the selected
  field draft: a field of the layout's own table (no
  containers), kept with the table it was chosen from, its left, top,
  width and height, and **Create label**, on at first: the field's name
  as a text label, shown dashed left of the field (90 points wide, 4
  points away, as high as the field, narrower near the layout's left edge,
  and off with no room for 20 points; OpenMaker's choice, as where
  FileMaker Pro puts a new field's label is not measured). Nothing is
  stored yet. Another field drag adds a draft instead of replacing it.
  Each draft can be selected on the canvas or in Objects, moved by dragging,
  resized and rebound in Inspector, or removed with Delete. A draft label
  stays attached to its field until Save; independent draft-label editing
  is not implemented.
- **Save Layout** (Layouts menu, Ctrl/Cmd-S while fields are pending)
  sends the drafts through the existing checked operation sequence, each
  with the revision of the layout the window showed
  (`LayoutGeometry::revision`) and the field's table. The provider plans it
  as the host does, and the host checks the account, the layout's lock, the
  revision and the occurrence's table, and saves it whole or not at all.
  Until the host answers, the field is what was sent: the canvas takes no
  drag or click, the Inspector's controls take no input, and Revert Layout
  waits.
  - Placed: every window of the file reads the layout again, and the edit
    box shows in Layout mode and in Browse.
  - The layout changed since it was shown (306): nothing is written, the
    remaining drafts are retained and the layout is read again. Their
    original revisions are kept, so Save cannot silently overwrite another
    client's changes; Revert explicitly discards them.
  - The layout's occurrence shows another table than the one the field was
    chosen from (306, also with the layout itself unchanged): nothing is
    written, rather than placing that table's field of the same id; the
    draft is retained with its original table identity. Manage Database's
    successful commit refreshes the shared field list without reopening.
  - Refused for another reason (a frame outside the body or the layout's
    width, a fractional top or left, an overlapped container, a layout
    this version does not edit): the alert says why and the field stays
    to be changed.
  - Without a known revision, it is not sent.
- **Revert Layout** discards every field addition and the other unsaved
  moves, deletions and edits. Completing a drawing or choosing a binding
  never writes the file.

Save Layout remains a sequence of checked operations, not an atomic
whole-layout transaction. After a partially successful Save, acknowledged
objects are stored and unfinished work remains staged; Revert discards only
that remaining work. An unknown outcome keeps the draft for the existing
idempotent/revision checks rather than assuming it was saved. The checked
revision/table identity is rebased only after a known successful reply.

**Undo and Redo.** Edit > Undo and Redo (Ctrl/Cmd-Z and Shift-Ctrl/Cmd-Z)
step through this window's unsaved changes. Each drag or arrow-key nudge
is one step. History covers moves, resizes, Arrange, deletions, layout
width, field controls,
text edits and staged placements and their settings. A focused text
editor keeps its own keyboard undo and redo. On leaving the editor, the
completed edit becomes part of the draft history. A new draft edit drops
the redo branch. OpenMaker keeps up to 32 steps and clears history if the
draft exceeds its snapshot budget. Applying a step clears selection.
Undo and Redo remain available for a local draft after disconnect.
History ends when Save Layout queues a write, when the user reverts the
layout, when the layout revision changes, or when the user leaves the
layout or mode. A Save Layout refusal before queuing a write keeps the
history. Part boundaries, Part Setup and Layout Setup are immediate
writes outside the draft. FileMaker Pro's precise
menu labels, supported operations and history across Save are unmeasured
([#902](https://github.com/OpenMaker-Dev/openmaker/issues/902)).

OpenMaker chooses bindings and label/control options in Inspector rather
than FileMaker's Specify Field dialog. The deferred layout-save workflow
is owner-reported (#1240); single-use and locked tools are documented in
Claris Help. Native save timing, locked-tool preferences and macOS UI have
not been measured for this change.

**Placing a text.** The Text tool places a text object holding one line,
with the operation `om-author`'s `place_text` uses
(`DataProvider::place_layout_text`; [authoring](authoring.md), "Texts on
layouts"). A drag on the canvas draws its frame, in whole points; a click
starts one 120 by 20 points where it is (OpenMaker's choice: FileMaker
Pro's Text tool grows the text as it is typed). The text is typed in the
dashed box on the canvas, which takes the keyboard when drawn; the
Inspector shows its frame. Save Layout places it, after any moves and
deletions, as it places a field, and an empty text is not sent. Revert
Layout drops it. One new object (field, button or text) waits at a time.
A stored text's words are retyped as the next paragraph describes.

**Retyping a text.** In Layout mode the canvas shows a text object's
words, a field's label's included. A double click on one whose words are
one line of plain text (not styled, no merge field) types into it in
place; Return or a click elsewhere keeps the new words as an unsaved
change, outlined as moved objects are, and Escape drops them (empty words,
or the stored ones, are no change). Save Layout sends each retyped text
after any moves, deletions and control changes, with the operation
`om-author`'s `set_text` uses (`DataProvider::set_layout_text`;
[authoring](authoring.md), "Texts on layouts"), and the layout changed
since it was shown is refused (306) as for the other changes. Revert
Layout drops them. Limits: one line only; styled words, merge fields and
texts kept as calculations are not retyped; the frame does not grow with
the words; the editor's look is OpenMaker's.

**Placing a button.** The Button tool places a text button that performs
a script or a single step, with `DataProvider::place_layout_button`
([authoring](authoring.md), "Buttons on layouts").
- **Drawing.** A drag on the canvas draws the button's frame, in whole
  points, as a rounded box with its label. When the drag ends, **Button
  Setup** opens, as FileMaker Pro opens it: the label, Action and the step
  as the Script Workspace shows it, with Specify… opening the step's own
  options dialog, as the Script Workspace edits it.
  - **Perform Script** (the default): "Perform Script" Options, with the
    script chosen in Specify Script and the parameter typed or built in
    Specify Calculation.
  - **Single Step**: choosing it opens Specify Step, the Script
    Workspace's steps pane (search, categories, the description) listing
    only the steps FileMaker Pro allows on a button; a double click,
    Return or OK chooses. A new step has its defaults, as when the Script
    Workspace inserts one. Choosing Perform Script there switches to the
    Perform Script action.

  OK keeps the label and the step on the new button; Cancel drops a button
  that has no action yet. Button Setup… in the Inspector,
  or a double click on the new button, opens it again; the Inspector also
  shows the frame.
- **Save Layout** sends it as it sends a new field, with the layout's
  revision, and the answers are handled the same way. Refused before
  anything is sent: a Perform Script whose script is specified by name or
  belongs to another file, and a single step with something stored that
  its schema does not explain (what the Script Workspace cannot edit) or
  a calculation that does not decode. In Browse the button runs its
  script with its parameter, or its step as a script step runs (a step
  OpenMaker does not run fails as in a script), and it stays after the
  file is closed and opened again.

**Placing a portal.** The Portal tool places a portal of related records
with fields in its first row, with `DataProvider::place_layout_portal`
([authoring](authoring.md), "Portals on layouts").
- **Drawing.** A drag on the canvas draws the portal's frame, in whole
  points. When the drag ends, **Portal Setup** opens, as FileMaker Pro
  opens it, with its controls where Pro's `IDD_PORTAL_SETUP.nib` puts them
  (re/client-ui.md, "Portal Setup"): Show records from (the occurrences
  the relationship graph joins to the layout's, the first chosen) and
  Number of rows (as many 39-point rows as the frame holds, OpenMaker's
  choice). Pro's other options are shown at the defaults it gives a new
  portal and are not offered. OK opens **Add Fields to Portal**: the
  occurrence's fields, Move (Clear while an included field is chosen),
  Move All and Clear All, up to 20 fields; container fields are listed
  and cannot be moved. OK needs a field, and fields that fit the rows and
  the width; Cancel in either dialog drops a portal not set up yet.
- **The new portal** waits for Save Layout as a dashed frame with its rows
  divided, its fields where operation 21 places them, captioned
  `::Name`, and its occurrence's name below the first row. A double click
  opens Portal Setup again, with the fields kept.
- **Save Layout** sends it as it sends a new field, with the layout's
  revision, and the answers are handled the same way.

One new object at a time: while a new field waits for Save Layout the
Button and Portal tools draw nothing, and so on for each of them.

Limits of portals: only the related records, rows and fields are set;
sorting, filtering, deletion, scrolling, the initial row and row states
stay as Pro sets them for a new portal. The Inspector does not show the
new portal's frame. Whether FileMaker Pro's dialogs list the same
occurrences and rows, and lay several fields out the same way, is not
measured; the dialogs' look follows Pro's nibs, not a capture.

Limits of buttons: FileMaker Pro's Button Setup is a panel that also sets
icons and popover buttons and offers Do Nothing, and the label is typed in
the button itself; here the label is a field of the dialog, and the
dialogs' look (Button Setup, Specify Step, the Action pop-up's order) is
OpenMaker's (no capture of FileMaker's). The button is placed on the body
only, outside the tab order, with the theme's button style, and no icon,
tooltip, name or hand-cursor setting. Whether FileMaker Pro opens and runs
a button placed this way has not been checked yet ([#70](https://github.com/OpenMaker-Dev/openmaker/issues/70)).

**A new layout.** Layouts > New Layout / Report... (Ctrl/Cmd-N in Layout
mode), the Layout mode toolbar's New Layout / Report and Manage Layouts'
New open New Layout/Report: Show records from (the file's table
occurrences, starting on the current layout's), Layout Name (the
occurrence's name until the user types one), and the device and view,
where only Computer and Form are offered. Finish adds the layout with the
operation `om-author`'s `create_layout` uses (`DataProvider::create_layout`,
`om_data::layout_catalog`; [authoring](authoring.md), "New layouts"): a
Form layout with one empty 600-point body, in the theme and width of the
layout the window showed. The host checks the account, the name and the
occurrence and saves it whole or not at all, on a local file (through its
own host) as on OpenMaker Server. Every window of the file then reads its
layout list and the calculation schema again, and the window opens the
new layout in Layout mode, as FileMaker Pro opens a new Form layout; the
Field tool places fields on it. A refusal shows its reason in an alert.
While a field or button drawn with the Field or Button tool is not saved,
New Layout/Report is refused with an alert asking for Save Layout or Revert Layout (FileMaker
Pro asks whether to save the layout instead), and the dialog closes when
a window of another file comes to the front. From Browse with a record
being edited, the record is saved first, then the new layout opens.
Limits: no List, Table or Report layouts, labels or envelopes, no other
devices, and no fields placed when the layout is made; the layout goes
last in the order, outside folders. The dialog's look, the default name,
the part set, the body height, the width and the theme are OpenMaker's;
FileMaker Pro's dialog and the layout it makes have not been compared
(re/layout-model.md 10.6).

**Moving and resizing an object.** With the Selection tool, dragging a
movable object moves it and dragging one of its eight handles resizes it,
in whole points; the Inspector's Left, Top, Width and Height set it too (in
points). Movable objects are the top-level ones without contents (fields
showing one repetition, text, shapes, lines, buttons, pictures); a line
keeps its empty side. Changed objects show outlined until Save Layout,
which sends every change in one request with the operation `om-author`'s
`set_object_frames` uses (`DataProvider::set_layout_object_frames`,
`om_data::layout_objects`; [authoring](authoring.md), "Moving objects"),
at the revision of the layout the window showed when the first object was
changed. The host refuses a frame outside the part that lists the object
or the layout's width, or over a container, and a layout changed since
(306, the changes dropped and the layout read again); another refusal keeps
the changes. A new field drawn meanwhile is placed after them, on the
layout they made. Revert Layout drops them. While a layout has unsaved
changes (moved or deleted objects or a new field), another layout, another mode and
New Layout/Report are refused with an alert asking for Save Layout or
Revert Layout. The desktop close controls check the affected drafts. A
document window's close button checks that window, File > Close checks the
file's windows, and Quit checks every open file. A layout save awaiting
the host also keeps the affected windows open. After a connection drops,
Revert Layout can still discard the local draft so the window can close.
A script's Close Window step targeting another window can still discard
its draft ([#978](https://github.com/OpenMaker-Dev/openmaker/issues/978)).
Explicit saving, the handle sizes and the outline are OpenMaker's choices.
FileMaker Pro saves layout changes when the user leaves or confirms them.

**Arranging objects.** Arrange > Align (Left Edges, Vertical Centers,
Right Edges, Top Edges, Horizontal Centers, Bottom Edges), Distribute
(Horizontally, Vertically) and Resize To (Smallest or Largest Width,
Height, or Width and Height) apply to the selected objects when every one
of them can be moved. They need two objects, and three to distribute. The
new frames are changes like a drag's: outlined until Save Layout, which
sends them in the same request, or dropped by Revert Layout. The frames
come from `om_data::layout_arrange`, which `om-author`'s `arrange_objects`
runs too ([authoring](authoring.md), "Arranging objects").
- Edges align to the object farthest out, as Claris documents it. Centers
  align to the middle of the objects' extent, and Distribute keeps the two
  outermost objects and spaces the others evenly; both are OpenMaker's
  reading of the help (re/client-ui.md, "Layout mode: Align, Distribute
  and Resize To").
- Changed left and top edges land on whole points. Objects too wide (tall)
  together to distribute with equal gaps get evenly spaced leading edges
  instead. Arrange shows an error and leaves the draft intact if rounding
  would change the distribution order or a new frame would collapse to
  a point.
- A pending move keeps the revision it was made on through Arrange, so
  frames derived from it conflict at Save Layout if the layout changed
  since.
- The submenus' item names follow the help's wording; FileMaker Pro's are
  unmeasured.
- Arrange does nothing for a selection holding a new field or an object
  that cannot be moved. Group, Lock, the stacking commands, and Arrange's
  other commands are still disabled ([#70](https://github.com/OpenMaker-Dev/openmaker/issues/70)).

**Deleting an object.** Delete or Backspace (unless a text field has the
keyboard), or Edit > Clear, deletes the selected object: the canvas and
the Objects pane stop showing it, as an unsaved change. Objects that can be
deleted are those that can be moved, show no image from the file's
library (a button with an icon, a picture), and are not named by another
object (a field's label) or a popover button. Save Layout sends the deletions after
any moves, on the layout the moves made, with the operation `om-author`'s
`delete_objects` uses (`DataProvider::delete_layout_objects`; [authoring](authoring.md),
"Deleting objects"), and a new field after them. Revert Layout brings the
objects back. A selected object that cannot be deleted yet (a group,
portal, tab or slide control, popover or button bar, or an object inside
one) answers with an alert saying which objects can. A layout changed since
it was shown is refused (306): the deletions are dropped and the layout
read again.

**A field's control style.** With a field object selected, the
Inspector's Data tab offers Control style, FileMaker Pro's seven items in
its order, and, for a drop-down list, pop-up menu, checkbox set or radio
button set, Values from: the file's value lists (re/client-ui.md,
"Inspector: a field's Data settings"). A new field drawn with the Field
tool offers the same two in its section. Drop-down Calendar is offered for
a date field only, and Concealed Edit Box not at all. A container, a field
showing several repetitions or a control OpenMaker does not change shows
its control with the pop-up disabled. A changed object shows outlined
until Save Layout, which sends each changed object's control with the
operation `om-author`'s `set_field_control` uses
(`DataProvider::set_field_control`; [authoring](authoring.md), "Field
controls") after any deletions, one object at a time on the layout the one
before made, then places a new field and sends its control. A list control
without a value list is not sent: an alert asks for one. A layout changed
since it was shown is refused (306): the control changes are dropped and
the layout read again; another refusal keeps them. Revert Layout drops
them, and so does reading the layout again without the object (another
window deleted it). A related field's control is offered too. Limits: the canvas outlines the changed object but does not draw the
control until the layout is saved and read again; the options under Values
from (the arrow, editing, other values, auto-complete, the checkbox icon)
and the button that edits value lists are not offered, and the Data tab's
look is OpenMaker's.

Limits: a stored object cannot be moved between parts or restyled;
groups, portals, tab and slide controls, popovers and button bars and the
objects inside them do not move and cannot be deleted, nor can objects
showing a library image, a field's label or a popover button; only a stored text of one line of plain text can be retyped; and the Text tool places one line of plain text in the theme's text style, with no character styles. The Field, Text, Button and Portal tools place objects; the other tool buttons only select a tool. For a stored object, the Inspector shows its
named style and calls out local styling beyond the object's stored
rotation. Appearance shows known
normal-state fill, border and text colors and the base font from Browse's
style cascade. These properties are read-only. Appearance can show swatches
for sides that draw no border, and Styles can list names the cascade skips
([#886](https://github.com/OpenMaker-Dev/openmaker/issues/886)). Autosizing,
names and tooltips are unavailable. Data Formatting shows OpenMaker's
read-only summary of the decoded number, date, time or graphic format for
a stored field whose type is known to the window. Its wording has not been
compared with FileMaker Pro's Inspector
([#891](https://github.com/OpenMaker-Dev/openmaker/issues/891)). Layouts
without geometry keep the generic field view, which is not inspectable as
document objects; they cannot be edited.
The revision is the one the window read when it opened the file or after
its last placement: a layout another client changed since is refused at
Save Layout, not shown changed before.

On a decoded layout, the canvas draws and picks objects on the front tab
or slide of each control. It starts with the stored current tab, the
stored default tab, or the first panel, in that order. The Objects list
still includes inactive panels and their contents. Selecting one of those
rows reveals its containing panels in this window. This is
OpenMaker's editor navigation and makes no layout changes. Tab and slide
navigation directly on the design surface, handoff of the last Browse
panel, and FileMaker's box selection of an entire control across its panels
are not implemented.

Manage Layouts opens from File > Manage > Layouts, Layouts > Manage Layouts
and the Layout mode Manage button. Search, row selection, double-click,
Open, Close, Enter and Escape work; Open goes to the chosen layout through
the worker, and New opens New Layout/Report (above). Associated Table
shows the table occurrence each layout shows records of, from the
calculation schema (a dash when it has none). Menu Set shows a dash until
the provider exposes it, and Edit is disabled. Duplicate copies the
selected layout (below), closes the dialog and shows the copy; Delete
deletes it after FileMaker Pro's alert (below). The dialog blocks document
shortcuts while open. Its single-window modal, Close button and search,
and Duplicate and Delete closing it, are OpenMaker's choices.

**Duplicating a layout.** Layouts > Duplicate Layout (Layout mode, design
access) copies the layout the window shows with the operation
`om-author`'s `duplicate_layout` uses (`DataProvider::duplicate_layout`;
[authoring](authoring.md), "Duplicating layouts"): the copy, named after
the layout with " Copy", goes last in the layout list, outside any folder,
and opens in Layout mode, as FileMaker Pro shows its copy. While the layout
has unsaved changes it is refused, as New Layout/Report is. The host
refuses a folder or separator and a layout whose styles show a library
image; a layout changed since the window read its layout list is a
conflict, and the list is read again.

**Deleting a layout.** Layouts > Delete Layout... (Cmd/Ctrl-E in Layout
mode, design access) asks "Permanently delete this layout?" with Cancel
and Delete, as FileMaker Pro asks; Delete deletes the layout the window
shows with the operation `om-author`'s `delete_layout` uses
(`DataProvider::delete_layout`; [authoring](authoring.md), "Deleting
layouts"). The window then shows the layout after it in the list (the one
before, for the last) in Layout mode, as FileMaker Pro shows Contact
Details after deleting Contact List; other windows of the file that showed
it go to the first layout. A record's edits are kept by the layout they
were made on, so before it asks, Delete Layout saves the window's record
as leaving the record does, and the deletion is refused while the layout
has unsaved changes in Layout mode or another window of the file holds
unsaved edits made on it (OpenMaker's choices; FileMaker Pro's are not
measured). The host refuses a folder or separator, the last layout, a
layout whose styles show a library image and one whose stored table view
shows a library item. No lock protects another client's unsaved changes
on the layout: when a layout is deleted elsewhere, a window that held
unsaved changes on it says they could not be saved and goes to the first
layout ([#217](https://github.com/OpenMaker-Dev/openmaker/issues/217)).

**Layout width.** With the Selection tool, Layout mode's layout right edge
drags to set the layout's width in whole points; Save Layout saves it
first, before the moved, deleted and new objects, with the operation
`om-author`'s `set_layout_width` uses (`DataProvider::set_layout_width`;
[authoring](authoring.md), "Layout width"), and Revert Layout drops it.
Neither FileMaker Pro's drag nor OpenMaker's stops at the rightmost
object (re/layout-model.md 10.12).

**Layout Setup.** Layouts > Layout Setup... and the layout bar's Layout
Setup button (Layout mode, design access) open the dialog over the current
layout. Its window and tabs follow FileMaker Pro 26.0.3
(re/client-ui.md, "Layout Setup"). The General tab uses estimated widths
and row spacing to fit the 26.1.1 field-alert checkbox. OK sets the
Layout Name, "Include in layout menus", the General tab's checkboxes,
the views available and the default view, and the Printing tab's "Use
fixed page margins" and its Top, Bottom, Left and Right margins, with the
operation `om-author`'s `layout_setup` uses (`DataProvider::set_layout_setup`;
[authoring](authoring.md), "Layout Setup"); every window of the file then
reads its layouts again. The margins are edited in points (OpenMaker's
choice: Pro's fields show the ruler's units), and are sent only when
they or the box changed, with the setup in one commit. With the box off
they are not offered and show the printer's, 18 / 40 / 18 / 18, as Pro's
do (re/layout-model.md 4.2). On a layout printing in columns neither the
box nor the margins are offered: Pro works the columns' width out from
them, which OpenMaker does not. The General
tab's Show records from and Menu Set, and the Printing tab's "Print in N
columns", show the layout's values and are not offered. The column order
and Facing Pages are not offered and show no state, since where FileMaker
stores them is unknown
([#948](https://github.com/OpenMaker-Dev/openmaker/issues/948)). Nor are
Reset Quick Find, the Script Triggers tab or Manage Layouts' Edit offered. It is refused while the layout has unsaved
changes, as New and Duplicate Layout are.

In a drawn Browse Form with a loaded empty found set, clicking a bound
edit box directly on the canvas shows the New Record alert when the
General tab's "Show alerts when field isn't modifiable or no records are
present" option is on. FileMaker Pro was measured with an empty table.
OpenMaker applies the option to the empty found set shown in the window.
Clicks on checkbox sets, radio button sets, pop-up menus and drop-down
lists leave the New Record alert closed in OpenMaker. FileMaker Pro's
behavior for these controls remains unmeasured. Field clicks inside
popovers, nonmodifiable field clicks and defaults for new files remain open
([#353](https://github.com/OpenMaker-Dev/openmaker/issues/353)).

**Part Setup.** Layouts > Part Setup... (Layout mode, design access) opens
FileMaker Pro's Part Setup over the layout the window shows, its controls
where Pro's nib places them (re/client-ui.md, "Part Setup"): the layout's
parts, top to bottom, named as Save as XML names them ("Sub-summary by
Initial (Leading)"). Change, or a double click, opens Part Definition on
the selected part, which offers what Pro's offers for the part's type
(re/layout-model.md 10.15):
- the page break before each occurrence or after every N occurrences (the
  two clear each other, and N is 1 to 32,767);
- restarting page numbers;
- allowing the part to break across pages, and then, on a body,
  discarding the remainder;
- a leading sub-summary's sort field, among the fields of the layout's own
  table.

OK sets them with the operation `om-author`'s `set_part_options` uses
(`DataProvider::set_part_options`; [authoring](authoring.md), "Part
options"). **Create** opens Part Definition with the kinds of part the
layout lacks offered (not sub-summaries), and OK adds the one chosen at its
place in the order of parts: a Header 110 points high and a Footer, Title
Header or Leading Grand Summary 22, as Pro's Create made them
(re/layout-model.md 10.18), and the other kinds 22, OpenMaker's choice.
**Delete** removes the selected part when it is empty, not a sub-summary
and not the only part.
Both go with the operation `om-author`'s `change_parts` uses
(`DataProvider::set_layout_parts`; [authoring](authoring.md), "Parts"): the
parts are keyed again and the objects below move with their parts. Part
Setup shows the parts as saved, takes later changes at the layout's new
revision, and every window of the file reads the layout again. Until the
host has answered a change and the layout is read again, the window's
canvas takes no edit and Part Setup cannot be opened anew; the dialog
itself waits without closing, unless another window comes forward, which
closes it as it closes Layout Setup. A change refused because the layout
changed meanwhile closes it; opened again, it shows the layout as it is
now.

**A part's boundary.** In Layout mode the Selection tool drags a part's
lower boundary (a band across the layout and the part strip) to set its
height in whole points, no higher than the lowest object in it reaches;
the height shows as it is dragged, and the release sends it with the same
operation at once, as Part Setup's changes go, the canvas taking no other
edit until the host answers and the layout is read again. The parts below
and their objects move with it.

Limits: deleting a part that holds objects (Pro deletes them with it after
asking) and sub-summaries are not offered; a part's type and row states
and the table pop-up are shown, not offered; a trailing sub-summary's and
the navigation parts' options are not offered (Pro's were not measured).
Pro keeps Part Setup's changes and a dragged boundary with the layout's
other unsaved changes until Save Layout; OpenMaker sends them at once, so
Part Setup is refused, and the boundaries are not offered, while the layout
has unsaved changes. Where Pro stops a dragged boundary above an object
(135 points for a field ending 82 points into the part, [#71](https://github.com/OpenMaker-Dev/openmaker/issues/71)) is not
matched: OpenMaker stops at the object. The list's place and the dialogs'
look are OpenMaker's.

### Manage Value Lists

File > Manage > Value Lists (and the Layout mode Manage button) shows the
open file's value-list catalog, hosted or local, read through the
provider's `value_list_catalog` (`om_data::value_lists`, file-metadata.md
6), the operation the authoring service's `get_value_lists` runs too. Each
list shows its name, its source (custom values, a field, another file's
list, a calculation) and its values or fields in the catalog's display
order; the selected list's related-values occurrence, second-field
options, re-sort language and unknown option bits are listed below it. A
list OpenMaker cannot decode is shown as unreadable with the reason,
never left out; a provider without a catalog (the sample provider) shows
its error instead of a list.

The dialog belongs to the file that opened it and to its own catalog
request. Another file's window stays behind while the dialog is open.
Closing the last window of its file closes it. Replies for another file
or a closed dialog are discarded.

Limits: this dialog changes no value list; a custom list's values change
through a pop-up menu's Edit… ([Editing records](#editing-records)). No
writer for the definition
`DBValueList::Definition` is specified and the host drops uploads to the
protected index `[21, 1]` that lack `KModCount` and `KSerNum`
(schema-path.md 2), so New, Edit, Duplicate and Delete are disabled with
that reason and nothing is saved. Another file's list (type 2) is shown
with its stored file reference unresolved (file-metadata.md 10, U6), and a
calculated list without its formula. Field names come from this file's
catalogs, so a field of another file's table shows its id. The dialog's
columns, Close button and option texts are OpenMaker's; FileMaker Pro's
dialog has not been compared, and the Open Manage Value Lists script step
does not open it.

### Script Workspace

Script Workspace loads the file's scripts, with searchable script selection,
step drafts, Run and Revert. Run runs the selected script in its file's
window; a dialog the script shows (Show Custom Dialog, a "With dialog"
confirmation, "Save changes to this record?", an error alert) brings that
window in front of the workspace so it can be answered, as it does in front
of any other window, and restores it when it is minimized. Where FileMaker
Pro shows it is unmeasured. Another file's window stays behind, by a
script or Window > Select Window, while the front file's
workspace waits for a save or a read, or while its Manage Database, Manage
Security or Manage Value Lists is open (FileMaker Pro's are modal); the
front window keeps the focus, and another file's script's dialog waits in
its own window ([#497](https://github.com/OpenMaker-Dev/openmaker/issues/497)).
Manage Layouts, which holds nothing unsaved, closes. Save stores the active
script's steps:
- **On a host that offers OpenMaker's authoring extension to the account**
  (OpenMaker Server, and the private host of a file opened from this
  computer), the native provider saves through the step save
  `om-author`'s apply makes (`NativeProvider::save_script_steps`,
  [authoring.md](authoring.md)).
  - Only the steps are written. The name, options, UUIDs and keys OpenMaker
    does not know stay as the host has them.
  - It is saved only when the script is still at the revision the scripts
    were last read at, and still has the modification count (`fc`,
    re/schema-path.md 8.1) the draft's copy was read with. Every save of
    the script advances that count, so a draft kept from an older read is
    not saved over a newer one, even after a reconnect read the scripts
    again. Then the script is read back on the connection, and the
    read-back copy becomes the saved one.
  - A script changed on the host since it was read is refused as a
    conflict. So are a lock, a denial or an unavailable host. All of them
    leave the draft unsaved, with the reason in an alert. A lost answer
    says the script may or may not have been saved.
- **The scripts pane's changes are saved at once** on the same hosts, as
  FileMaker commits them: new, duplicated, renamed, moved and deleted
  scripts (a deleted folder with what it holds), and Include in Menu and
  Grant Full Access Privileges. New Script makes "New Script" (numbered
  when taken) after the selection, saves it and opens it, with its name
  ready to type; the name typed is a rename, saved the same way. The
  native provider sends FileMaker Pro's own script save (re/scripts.md,
  "Saving scripts to a host"): the index `[17, 1]` and each changed or
  deleted script locked, uploaded whole (a deleted one emptied) and
  released with attention 2, so other clients read them again. op13's
  reply says nothing about the disk, so the host is then asked whether
  the file on disk holds the uploads (the authoring extension's Save,
  [authoring.md](authoring.md)), and the catalog is read back. Each script's and the index's entries are planned by
  `om_data::script_catalog` on the catalog as stored (re/scripts.md, "The
  catalog index"), with the catalog's digest `08`, which FileMaker checks
  when it opens the file and refuses the file without (re/scripts.md,
  "The script catalog's digest"; [#66](https://github.com/OpenMaker-Dev/openmaker/issues/66)).
  - The host gives a new script (or a copy) the id after the last one its
    index issued, never a deleted script's; the workspace follows that id.
  - A catalog another user changed since the workspace read it is not
    saved over (306); nor is a script or the index another user holds
    (301). Either alert says nothing was saved.
  - A save that fails after part of it was uploaded, or that the host
    could not write to disk, says what the host may hold; the workspace
    must be opened again.
  - Steps, buttons and privilege sets that name a deleted script keep its
    id, which no script takes again.
- **Refused with an alert (error 3), nothing saved:** new folders and
  separators (and duplicated folders), a name another script or folder
  has, names with characters outside ASCII
  ([#209](https://github.com/OpenMaker-Dev/openmaker/issues/209)), and
  deleting or renaming one of two members of one name. New and renamed scripts are listed under
  FileMaker's name list key, which is the same in every file, so a Create
  New file and the Japanese, Korean and Chinese starters take them too
  ([#274](https://github.com/OpenMaker-Dev/openmaker/issues/274)). A
  file whose script index lacks a part every file FileMaker made has is
  refused too, and so is one whose stored digest is not the one OpenMaker
  computes for its scripts.
- **Elsewhere the catalog is read-only, with the host's reason:** FileMaker
  Server, a host without the extension, an account that may not author,
  a file opened read-only. Nothing is written there.
- The sample provider saves both. A failed save keeps the draft.
- **The workspace belongs to its file.** While another file's window is
  in front, the workspace and its drafts wait with their file. They return
  when any window of that file comes forward, including after another
  file closes. Closing an individual background window keeps the draft
  while its file stays open. Closing the last window of a background
  file discards its parked draft.
  - It takes only the answers to its own requests (its catalog, a script
    save, a catalog save), each named by a token of its own.
  - An answer for a workspace that was closed, reopened or replaced since
    goes to the file's windows only. They take the saved scripts, and a
    lost connection is still the file's. Such an answer never touches the
    current workspace's drafts, waits, alerts or follow-ups, even for a
    script with the same id.
  - The file's own reads of its scripts (on opening, on reconnecting)
    update its windows, not the workspace.

Manage Database has Tables, Fields and Relationships tabs
([schema-manager.md](schema-manager.md)).

### Specify Calculation

Code: `crates/om-gui/src/script_workspace/calculation.rs` and `calculation/`
(`fields.rs`, `functions.rs`, `syntax.rs`), over `om_data::formula_editor`
(the scanner the colors and completion share, the function list, insertions
and name completion). The Script Workspace opens it for
a double click on a step's calculation option. It follows FileMaker Pro's
dialog as measured in re/client-ui.md (Specify Calculation); the
catalogs `calc-functions.csv`, `get-selectors.csv`, `enumerations.csv`
(resource 135) and `error-codes.csv` supply its lists and messages.
OpenMaker's render is `re/captures/om-ui/42-specify-calculation.png`
(git-ignored).

**Window.** A modal window over the workspace at capture 42's size
(smaller screens: the screen less 12 pt a side), movable by its title bar,
resizable from its right and bottom edges and corner (minimum 780 x 440),
zoomed to the screen by its green button; Escape is Cancel. Its geometry was
matched by overlaying the render on capture 42. Without the field options
the bottom bar is 60 pt. Field type icons are OpenMaker's own drawings in
42's style.

**Steps and field calculations.** For a script step, and a button's
script parameter in Button Setup, the bottom bar holds Cancel and OK only: the result type, repetitions, "Do
not evaluate if all referenced fields are empty" and Storage Options apply
to calculation fields. The stored context stays occurrence 0
(`Calculation::NO_CONTEXT`). The context popup lists every occurrence and
chooses the occurrence unqualified names compile against; it opens on the
occurrence of the formula's unqualified fields, else the file's first
occurrence. Fields inserted from the list are qualified (`Contacts::First
Name`); `Self` is error 1225. The field mode (`for_field`) is what Manage
Database opens for a calculation field and for a field's auto-enter,
validation and message formulas (schema-manager.md): it shows 42's options,
lists only the occurrences of the field's table, stores the chosen
occurrence as the context (child 4), inserts fields of the current table
unqualified and allows `Self`. Its Storage Options button opens the
calculation's storage options.

**Fields pane.** The table popup lists the current table (the context
occurrence), then Related Tables and Unrelated Tables (whether the
relationship graph joins them to the current table). The list shows the
table's fields in creation order (the bars menu: creation order, field name,
field type), filtered by the search field. A double click inserts the
reference at the cursor as `om-formula`'s printer writes it, `${...}` around
a name that needs quoting (calc-encoding.md 4.2).

**Functions pane.** Every `kind=function` row of `calc-functions.csv` with
its signature, the generic `Get ( flag )` replaced by the 138 documented
selectors of `get-selectors.csv`, and the file's custom functions
(`Name ( p1 ; p2 )`), sorted by name case-insensitively. The bars menu shows
All Functions by Name (the default), All Functions by Type (headed by
resource 135's category names) or one category; the search field filters
names. A click shows the function's catalog description (custom functions:
"No description"); the panel's "?" opens its Claris page (the `doc_url` of
`calc-function-spec.json`, checked by a unit test). The "?" at the top opens
the Specify Calculation help page; the pane toggle hides the functions pane.

**Inserting.** Insertions replace the editor's selection, the one it had
when a list or button took the focus, then give the focus back. A function
inserts its signature as listed, braces included, with the first parameter
selected (`Abs ( number )` selects `number`, `Let ( {[} var1 = ...` selects
`var1`); a Get function or one without parameters leaves the cursor after
it. In the operator strip, quotes and parentheses enclose the selection
(`( 1 + 2 )`; an empty pair puts the cursor inside), `not ` goes before its
operand, and the other operators get a space before them (unless one, or
`(`, is there) and one after.

**Completion.** Typing a name opens a list under its first character
(above it near the window's bottom) of the names that complete it, from
`om_data::formula_editor::complete`, the query `om-author`'s
`complete_calculation` answers (docs/authoring.md): fields of the context
occurrence, or of `Occurrence::` once typed, written as the fields pane
inserts them; the file's custom functions; FileMaker's functions and Get
functions; and Get selectors after `Get (`. Each row shows the name as it
is written (a function's signature) and, dimmed at the right, its category
or `Field`. Up and Down move the highlight, Return, Tab or a click write the
highlighted name over the typed prefix as an insertion from the panes does
(a function's first parameter selected), and Escape closes the list; a
second Escape cancels the dialog as before. Typing refines the list, a
space or any other character that ends the name closes it, and so do
moving the cursor and clicking elsewhere. Text after the cursor is never
replaced, and nothing is completed inside text literals, comments or a
word. A name runs over the spaces between its words, so `First Na` lists
`First Name`; when the whole run matches nothing, the run from its next
word is tried. All of this is OpenMaker's choice: FileMaker's trigger,
list, matching, order and keys are unmeasured (re/client-ui.md, Unknowns).
Matching is from the start of the name, ignoring case; an exact name comes
first, then names alphabetically, then fields, custom functions and
functions. The list shows ten rows and scrolls.

**Editor.** Monospaced, wrapped to the pane, Tab typed as a tab (egui draws
it at a fixed width, not to tab stops). Colors follow 42; text literals
`#C2410C` and comments `#5E864D` in italics are OpenMaker's choice. The
coloring reads names as the scanner does (a name runs over the spaces
between its words and stops before `and`, `or`, `xor`, `not`; a function only
before `(` unless it takes no arguments), so `Left ( Last Name ; 1 )` colors
`Left` but not `Last`. FileMaker's line breaks are CR; the editor shows each
as LF, character for character, and OK turns them back. `L: n C: m` follows
the cursor (the selection's start), lines and columns from 1.

**OK and errors.** OK compiles with `om_formula::compile` (the file's
decimal separator, the context popup's occurrence) and stores the formatted
and Polish streams. Unchanged text compiles the source exactly as it was
printed, so a formula opened and okayed unchanged keeps its streams (unit
test with comments, tabs, returns, escaped quotes, `¶`, a variable and
unqualified fields). A formula that does not compile shows an alert with
the error code's text from `error-codes.csv` (`Abs ( 1 ; 2 )`: "Too many
parameters in the function") and keeps the dialog; dismissing it selects
the error's range (the engine's UTF-16 span as characters), so typing
replaces it.

Limits: completion does not offer plug-in functions, occurrence names,
variables or `Let` names, and it does not open on its own after `(`, `;` or
an operator; the open questions in re/client-ui.md (Unknowns) decide
qualification, alert texts, colors and completion's behavior.

### Manage Security

The dialog belongs to the file that opened it. Switching to another file closes
it; replies from another file or an older dialog request are discarded.

File > Manage > Security shows the file's accounts, privilege sets and
extended privileges, read only. It shows only what the host decodes for
the `get_security` command (authoring.md, "Reading security"), with no
password, hash or other credential:
- each account's name, whether it is active, its type, its privilege set,
  and whether its password must be changed at the next login;
- each set's name, record access (named only for the observed flags,
  otherwise "Not decoded" with the flags in hex), password lifetime, its
  accounts and its extended privileges;
- each extended privilege's keyword and the sets that have it.

Anything the host could not read says "Not readable". Refresh reads the
catalog again.

Limits:
- **Hosts.** It needs OpenMaker Server, for a hosted file or a local file
  opened through the client's private host, and an account of the file in
  its built-in [Full Access] set. Any other account, and any other host
  (FileMaker Server included), gets the host's refusal in the dialog.
- **No editing.** Accounts and privilege sets cannot be created, changed or
  deleted ([#12](https://github.com/OpenMaker-Dev/openmaker/issues/12), [#7](https://github.com/OpenMaker-Dev/openmaker/issues/7)), so the dialog has no editing controls.
- **Not decoded.** The File Access tab, set descriptions, and layout,
  field, script, value-list and menu access are not shown ([#7](https://github.com/OpenMaker-Dev/openmaker/issues/7)).
- **Not compared.** The layout and labels are OpenMaker's; FileMaker Pro's
  dialog was not compared.

### Privilege sets

A local file opens with an account in a privilege set OpenMaker enforces
(re/local-files.md 2.3, re/session-identity.md 2.5), and the desktop and
its host apply the set's limits:
- [Full Access]: everything.
- [Data Entry Only] and [Read-Only Access]: no design change (File >
  Manage's dialogs, Layout mode, Script Workspace disabled); [Read-Only
  Access] creates, edits and deletes no record (New, Duplicate and Delete
  Record and Import Records disabled; a script's write gets 200). A
  refused change shows FileMaker Pro's "Your access privileges do not
  allow you to perform this action.", and a write to a view-only field
  its "This action cannot be performed because this field is not
  modifiable." ([#7](https://github.com/OpenMaker-Dev/openmaker/issues/7)'s reading).
- **Typing a refused change.** Browse lets the user enter a field the set
  lets them see but not change, as FileMaker Pro does: they can move the
  cursor, select and copy, and typing, pasting, cutting or deleting shows
  the refusal's alert and leaves the value as it was. Under [Read-Only
  Access], on a record the set may not view or edit, or through a layout
  whose records it makes view only, that is the privileges alert; in a
  view-only field, the field's. OK dismisses it and the field stays
  entered, with no exit or enter trigger (#690; [#7](https://github.com/OpenMaker-Dev/openmaker/issues/7)'s and #156's
  readings).
- A custom set whose every member OpenMaker enforces: each table's create,
  edit and delete yes, no or limited by a calculation (a limited action is
  refused with 200 where its formula is not true on the record), the
  table's records viewable, viewable where a calculation is true, or
  never, every field modifiable, view only or without access, layouts,
  scripts and value lists with
  any access, the records through every layout it may see modifiable, any
  menu commands, printing and exporting allowed or not. Each table's
  actions are applied as above, per table; a
  view-only field refuses typing (above) and a write to it gets 201; no
  design change is allowed.
- Under such a set a layout without access is left out of the layout menu.
  A file whose opening layout ("Switch to layout", else the first layout)
  is hidden opens on it, as FileMaker Pro does: a blank gray page, an empty
  toolbar layout name and no alert (#696); Go to Layout offers the layouts
  the account may see. Go to Layout to it shows
  FileMaker Pro's gray `<No Access>` page with a blank layout name. A
  script's New Window to it opens a window on the blank page, and Go to
  Related Record to it moves the window to `<No Access>` with the related
  records found and the layout named in the toolbar, each with 0, as
  FileMaker Pro does (#696). A script without access is left out of the Scripts menu, and
  calling it (from a button, a trigger or another script) fails with 9.
  A value list without access offers no items in a pop-up menu, drop-down
  list, checkbox or radio button set, `ValueListItems` answers empty, and
  a sort by it fails with 9. A set that denies printing disables Print,
  Page Setup and Save as PDF (menu, shortcut and toolbar); one that denies
  exporting disables Export Records and Save as Excel. In a script, as in
  FileMaker Pro 26.0.3: Print, Save Records as PDF, Export Records and
  Save Records as Excel fail with 3 and no alert; Copy All Records shows
  "Your access privileges do not allow you to perform this action." and
  fails with 200; Print Setup and Export Field Contents run. A script
  that runs with full access in that file is allowed them all.

Limits:
- **Records a view formula rejects** show in lists and found sets with
  `<No Access>` in their fields (the provider judges this on the values it
  holds, so a record withheld whose formula is true on empty values shows
  empty); their values never reach the desktop, and they cannot be
  edited (200), whatever the set's edit access, since writing back what
  the desktop holds would erase them. An entered field of one still
  reads `<No Access>`; what FileMaker Pro shows there is unmeasured. A new
  record the formula rejects is created and counted, as FileMaker Pro
  26.0.3's New Record does ([#7](https://github.com/OpenMaker-Dev/openmaker/issues/7)'s reading), and shows `<No Access>` [I];
  only its id and modification count are read back to check the write.
  Under a formula true on an empty record, such a create is still reported
  as not verified, since an empty read-back could then be a lost write
  [I].
  An edit that makes a record one the formula rejects is reported as not
  kept, though it is ([#268](https://github.com/OpenMaker-Dev/openmaker/issues/268)). A table with a view
  limit has no indexes and no container data on the desktop, so value
  lists built from its fields' indexes are empty and its containers show
  nothing; while any table has one, no container data is shown, and none
  can be inserted or removed.
- **Layouts without access** still count in OpenMaker's `Get ( LayoutNumber )`,
  `Get ( LayoutCount )`, `LayoutNames` and a layout number in a script;
  `FieldNames`, `LayoutObjectNames` and the other design functions read
  nothing in them. On Pro's hidden opening and `<No Access>` pages,
  `Get ( LayoutName )` returns the hidden layout's name, while
  `Get ( LayoutNumber )` and `Get ( LayoutAccess )` return 0 (#696).
  In a local Pro copy, Go to Related Record to a hidden layout succeeded
  with error 0, showed `<No Access>`, and found one matching record out
  of two. New Window to it succeeded with error 0 and opened a second,
  blank gray window; the original stayed open (#696). OpenMaker follows
  these two paths (session-identity.md 2.5); hosted behavior is unmeasured.
  A script set to run with full access was not measured in this native check.
  Pro omits the hidden layout from its layout menu; its script error for
  Go to Layout and its design functions' answers are unmeasured. With no
  layout it may see, OpenMaker's window shows none.
- **Value lists without access** keep their names and ids
  (`ValueListNames`, `ValueListIDs`) and still validate their fields.
  What FileMaker Pro shows in such a control and answers for its items is
  unmeasured.
- **Printing and exporting** follow FileMaker Pro 26.0.3's readings on
  local files (session-identity.md 2.3, [#214](https://github.com/OpenMaker-Dev/openmaker/issues/214)). With errors captured, Copy
  All Records shows no alert, which is unmeasured [I]. Save a Copy as
  returns 800 under a denying set, with no chooser (it has none yet).
  Snapshot Link is not implemented, and Import Records reads no FileMaker
  file, so their outcomes under a denying set (0, 714) are not reproduced.
- **Editing only and Minimum menus** keep layout and record navigation,
  the status toolbar and formatting bar, zoom and Minimize Window, and
  disable modes, views, the record, omit, find and sort commands,
  printing, exporting, New Window and Insert's Picture and File, as
  FileMaker Pro 26.0.3 does ([#7](https://github.com/OpenMaker-Dev/openmaker/issues/7)'s and [#214](https://github.com/OpenMaker-Dev/openmaker/issues/214)'s readings). Editing only
  also keeps Show All, Show Omitted Only, Refresh and Export Field
  Contents, which Minimum leaves out. OpenMaker's own choices [I]
  (session-identity.md 2.3): Minimum leaves out Show All too; Change
  Password and Revert Record need all menus; Minimum disables the status
  toolbar and the layout bar; and the shortcuts follow the menus.
  Disabled commands' shortcuts do nothing. Scripts, buttons and fields
  work as before under both. The field editors' own cut, copy and paste
  work under both; shortcuts under either level are unmeasured in
  FileMaker ([#214](https://github.com/OpenMaker-Dev/openmaker/issues/214)).
- **Tables and fields without access.** A table whose records may never
  be viewed lists its records, as a view formula no record meets; a
  field without access is left out of Browse and keeps
  its value when the record is edited. Both draw `<No Access>`, as
  FileMaker Pro does for a record a view formula rejects; their values
  are empty. A portal leaves out a related record the set may not view,
  as FileMaker Pro 26.0.3 does (#695's reading), and its creation row
  follows the rows listed: the provider marks such records in the
  calculation data (`RecordValues::no_access`), and the window and a
  script's portal rows skip them (row numbers follow the rows listed
  [I]). A related field without access draws `<No Access>` in each row,
  in the creation row and placed on the layout, and cannot be entered or
  reached with Tab. A related file's table is judged by that file's set,
  its view formula not at all (such rows stay, blank).
  Hosted Root and related-field edit capabilities also use each owning
  account's decoded record and field rights. Synthetic OpenMaker Server
  provider tests cover Read-Only Root refusing an edit with 200 and
  preserving its value, and editable Root plus Read-Only Related on one
  endpoint. FileMaker Server, native FileMaker and unavailable hosted-set
  metadata are not covered by those tests.
- **Records through a layout.** Through a layout whose records the set
  makes view only, Browse refuses typing with the privileges alert and
  every write through it fails with 200, a deleted portal row's record
  included. The desktop
  checks this alone, as a commit does not name its layout. FileMaker
  Pro's behavior here is unmeasured.
- **Typing a refused change** covers edit boxes, drop-down lists and
  calendars of the layout's own record, and Table view's cells. Checkbox
  and radio button sets, pop-up menus, containers, portal rows and
  related fields are not covered: most are not entered, and a set or
  pop-up menu says why where the pointer rests on it. A field without
  access is not entered. A
  record the set may not edit refuses before a view-only field does, and
  a text global under a set that edits no record is refused like any
  field; both are OpenMaker's choices, as FileMaker Pro is unmeasured
  there. Cut puts the selection on the clipboard before it is refused.
- **Records without access through a layout** the set shows (#707):
  the account opens, and on that layout every record-bound field object,
  related field, portal cell and merge field shows `<No Access>`, with no
  editor and no Tab stop, while the layout's labels and shapes draw and
  the found set's count shows, as FileMaker Pro 26.0.3 draws it. No hide
  condition, conditional format, calculated label or tooltip is evaluated
  there and no tooltip shows [I: unmeasured, conservative]. New,
  Duplicate and Delete Record through it are refused [I]. A script still
  reads and writes the record as the table's rules allow (Set Field is
  saved), and `Get ( LayoutAccess )` is 0, `Get ( RecordAccess )` the
  table's answer, as Pro measured.
- **Refused at login (9).** A custom set with a limit by calculation on
  its default for all tables: not enforced yet (#156, [#220](https://github.com/OpenMaker-Dev/openmaker/issues/220)), so the
  account is refused rather than given more than its set allows.
- **Duplicate Record** of a record whose copied values include a view-only
  field is refused (201), as the copy's values are the write's own.
  Whether FileMaker copies them is unmeasured.
- **Not compared with FileMaker Pro.** What FileMaker Pro disables under
  these sets, its alerts, and its codes (OpenMaker answers 200 for a
  refused record action) were not measured.
- **Hosted files.** A local file's host tells the desktop the verified
  set; a hosted file applies the record and field rights decoded from its
  own account's privilege-set reply (above), and where that reply is
  unavailable the host's own refusals stand.

### Not built

The Themes, Custom Functions, External Data Sources and Data Viewer
dialogs, and Manage Value Lists' New, Edit, Duplicate and Delete (a custom
list's values are edited from a pop-up menu's Edit…, under
[Editing records](#editing-records)). Of layouts, only what
[Layout mode and Manage Layouts](#layout-mode-and-manage-layouts) lists is
saved, and New Layout/Report makes only Form layouts; of scripts, Script Workspace saves
steps and catalog changes but no folders or separators
([Script Workspace](#script-workspace)).
