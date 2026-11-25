# New files

OpenMaker writes a new, blank `.fmp12` file itself (`om_data::new_file`),
from the format specs: no file FileMaker made and nothing of a Claris
theme is copied. What FileMaker Pro 26's own blank file holds, compared key
by key with OpenMaker's, is [file-format.md](re/file-format.md) 8; this
note says what OpenMaker writes and its limits.

## What a blank file holds

`new_file::blank(&NewFile, random)` returns the file's bytes for a name
(the file name without `.fmp12`), a locale, the user name and the time for
the stamps, and `mInternalCreation`; `random` fills UUIDs and salts. As
Pro's blank file, it holds:

- **Page 0** built for the name (`om_hbam::new_superblock`); the tree is
  written whole by the bulk rebuild (`om_hbam::writer::rebuild`).
- **The file status `[2]`**: "26.0", file version 1208, the locale record
  of the `Locale` given, the document preferences of Pro's blank (default
  fields on, layout 1 as "Switch to layout", unchecked), and
  `mInternalCreation`.
- **One table named as the file** with Pro's **five default fields**
  (PrimaryKey, CreationTimestamp, CreatedBy, ModificationTimestamp,
  ModifiedBy), made by Manage Database's own planner (`SchemaDraft`:
  New Table, five Create Field, five Options for Field), with FileMaker's
  default-field tags and comments of OpenMaker's own; its occurrence of
  the same name.
- **One record**, whose values OpenMaker's record rules enter for a new
  record made by Admin at the time given (`RecordPlan::compute`: a UUID,
  both timestamps, "Admin"), stored with the record list and PrimaryKey's
  index, as the host stores a record.
- **One layout named as the file**, a layout of the occurrence made by New
  Layout's planner (`layout_catalog::plan`) with Form, List and Table
  enabled (Form first), 1024 points wide with a 600-point body. Like Pro's,
  it has no field on it and stores a Table View layout
  (`layout_catalog::table_view`, layout-model.md 10.14): a header, a
  one-row body and an empty column, so FileMaker Pro shows the empty layout
  as a table (Create Field, no named column) instead of quitting (#806).
- **OpenMaker's own theme, "OpenMaker Plain"** (`new_file/theme.rs`, every
  value in one table): white layout and parts; fields white with a 1-point
  gray border, 12-point dark text and a blue border in focus; text and
  labels 12 points; buttons light gray with a gray border and 3-point
  corners, centered text, darker when hovered and pressed; the text of
  each a size inside its object (FileMaker Pro draws no text without one;
  file-format.md 8), a line height of one line and the file's font
  `00 00`. Every other object type and property takes FileMaker's
  built-in default. It is
  stored as FileMaker stores a theme made in a file
  (`com.filemaker.theme.custom.<UUID>`, group `custom`), with a fresh UUID.
- **Security**: Admin, [Full Access], no password; [Guest], [Read-Only
  Access], inactive; File Options log in as Admin with no password and
  show sign-in fields; the three built-in privilege sets; the eleven
  extended privileges of a FileMaker 26 file. Every checksum FileMaker
  checks at open is computed (`om_data::security_checksum`).
- **Empty script and value-list catalogs**, the font catalog with font
  `00 00` ("arial,sans-serif"), and catalogs `[64]` and `[65]` as every
  file has them.

## Making one

The `create_file` operation (`new_file::create(path)`, label "Create
New...") makes the file at a path, adding `.fmp12` when the path does not
end in it, as FileMaker's save panel adds it. It takes the system's locale
(`LC_ALL`, `LC_TIME` or `LANG`), the user's full name for the stamps, and
the time now. It creates the file new and never replaces one: a file or
folder of that name is refused, and a failed write leaves nothing. It
needs no open file.

- **File > Create New...** follows FileMaker Pro 26's sequence
  (`crates/om-gui/src/create_new.rs`): the **Create an App** sheet with
  Blank, the only choice OpenMaker offers, and its description; Create
  shows the **save panel**, proposing "Untitled.fmp12" in the user's
  Documents folder: the system's own, limited to FileMaker files
  (`NSSavePanel` on macOS, the desktop's chooser through xdg-desktop-portal
  on Linux; [desktop-client.md](desktop-client.md#opening-files)), or
  where there is none, OpenMaker's (Save As and Where). Save makes the
  file, which opens in the window as Admin with no password, without the
  sign-in dialog; **Manage Database** then opens on its Fields tab, as in
  Pro, and closing it leaves the window in Browse on the file's record. A
  refusal, such as a name already taken (the system's panel asks whether
  to replace it, and OpenMaker still replaces nothing), shows in an alert
  after the system's panel and stays in OpenMaker's own. The file open in
  the window closes first, as File > Open does in OpenMaker.
- **`om-author create-file --path PATH`** and the MCP tool
  `openmaker_create_file` run the same operation without a connection
  ([authoring.md](authoring.md)).
- `cargo run -p om-data --example new_file -- <path>` does the same from
  a checkout.

## Checks

- Format layer (`new_file::tests`): the file reads back with its table,
  five fields, occurrence, layout and one record holding a UUID, the
  timestamps and "Admin"; rewritten from the tree it reads, it comes back
  byte for byte; every security checksum recomputes; Admin's password is
  the empty one; every stamp holds its time with its length.
- OpenMaker: the file opens through the local host as Admin with no
  password, on its layout with its record; a second record gets its
  values from the host's own rules and joins PrimaryKey's index. Opening
  it needs #723 (a file without an image library), as Pro's own blank
  file does.
- FileMaker 26.0.3 (#593, files made at commit `74e1c34` of the earlier, private history): a blank file, one
  named with a space, and one OpenMaker edited (a field placed with its
  label, a text object, a second record). `FMDeveloperTool
  --checkConsistency` finds no bad block in any, and `--saveAsXML` writes
  each with no error. FileMaker Pro opens each with no alert, draws the
  fields, labels and text, names the theme OpenMaker Plain in Layout mode,
  and keeps fields and records added in Pro through close and reopen.

## Limits

- **Differences from Pro's blank file** (file-format.md 8): the layout has
  one body part
  (Pro's has a 110-point top part), and its Table View layout's parts and
  column take the theme's styles where Pro's carry local styles (the
  column's data format record holds a new field's settings with a text
  height of 0 where Pro writes its theme's, #886); the fonts are not Apex
  Blue's;
  `fmplugin` has no description; `[65]` lists no standard menus; the file
  access catalog does not require authorization and lists no entry, since
  what Pro's entries for the file itself hash is not known; the
  calculation's context is stored as the occurrence; the history and page
  setup keys of `[2]` are left for FileMaker to write.
- **Scripts.** A new file takes new scripts, listed under FileMaker's
  name list key in whatever language it was made in
  ([#274](https://github.com/OpenMaker-Dev/openmaker/issues/274)).
  FileMaker Pro has not opened a new file holding a script OpenMaker added
  ([#407](https://github.com/OpenMaker-Dev/openmaker/issues/407)).
- **The theme** is plain and covers six object types; tabs, portals,
  popovers, button bars and the rest look as FileMaker's defaults make them
  ([#222](https://github.com/OpenMaker-Dev/openmaker/issues/222)).
- **Field heights**: FileMaker Pro sizes a field it places (a field added
  in Manage Database) from the border widths, the edit box's text margins
  and one line. It draws the text inside the text's insets too, without
  counting them, so a theme with insets cuts the text (#593; layout-model.md,
  "Text margins, insets and a placed field's height").
  - **Form view:** OpenMaker Plain gives the edit box 16 pt text on one
    line, with margins of 2 / 4 / 2 / 4 pt and no insets. A field Pro places
    is 24 pt high.
  - **Table view:** Pro sizes a row from the edit box's font but draws the
    cells at the field's 12 pt (layout-model.md 10.14). The stored table
    view has a 21 pt row and a 24 pt header, what Pro writes for this
    theme.
  - **Measured in Pro:** Pro drew `Agjpqy` whole in both views for a field
    it placed (#853, round 5).
  - **Files made before #853** keep their old values: 2 pt insets and a
    16 pt row, or a 12 pt edit box and a 17 pt row. OpenMaker does not
    migrate them, and Pro still cuts their text.
- **Hosting**: FileMaker Server refuses to host a file whose full access
  account has no password. A new file's Admin has none. File > Change
  Password changes the signed-in account's password in a file on this
  computer (re/local-files.md 4), but a change from an empty password has
  not been tested, and whether FileMaker Server then hosts the file has
  not been tried ([#12](https://github.com/OpenMaker-Dev/openmaker/issues/12)),
  so the file gets one in FileMaker Pro before FileMaker Server hosts it.
- `mInternalCreation` is the seconds since 1904; Pro's count from an epoch
  about 36.5 days earlier, which is not known.
- A name the table or layout planner refuses (one a calculation could not
  write as it is, such as one with `-` or a leading digit), or one the
  identity fence cannot hold (over 1004 UTF-8 bytes, `/` or a line break),
  makes no file; whether Pro accepts such names for a new file is
  unmeasured.
- A name already taken is refused in the save panel, not offered to
  replace as Pro's panel does.
- Only the 11 locales OpenMaker has records of (`Locale::for_tag`) are
  taken from the system; any other gets English (United States).
- FileMaker Pro's Create an App sheet also offers Convert and the starter
  solutions; those are Claris's files and OpenMaker does not ship them, so
  only the blank file is made.
