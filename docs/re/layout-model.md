# Layout, style/theme, and rendering model

Workstream: layouts, objects, styles, themes, rendering. Target: FileMaker Pro
26.0.3 (Mac x86_64 slices are the primary reference, Windows x64 install for
platform comparison).

Legend used throughout:

- **[O]** observation: directly seen in a binary, shipped file, or decoded data.
- **[I]** inference: follows from observations but was not directly confirmed.
- **[U]** unknown: open question.

Evidence cites demangled symbols with their x86_64 address in
`re/work/thin/FMEngine` (or the named binary), string values, file offsets, and
the REA Evidence ID where REA was used. Scratch output lives in `re/work/layout/`.

## 1. Key findings

1. **A layout is protobuf all the way down.** [O] Layout definitions
   (`FMLayoutPB.LayoutDef`), part definitions (`PartDef`), the object list
   (`FMLayoutPB.LayoutObjectStore`), theme styles (`ThemeStyles`), and theme
   metadata (`ThemeMetadata`) are protobuf messages stored as long values in the
   file's HBAM key tree. Schemas: [protobuf/](protobuf/) and
   [protobuf-schemas.md](protobuf-schemas.md) (maintained by the protobuf
   worker; this document adds semantics). Confirmed by decoding real records in
   the shipped `DynamicTemplates/*.fmp12` files (section 10.4).
2. **Styles are a CSS model compiled to numbers.** [O] Every style value is a
   (component, state, property) triple with numeric IDs for object type
   (`FMStyle::ObjectType`), component (`FMStyle::Component`), state
   (`FMStyle::State`), and property (`FMStyle::Property`). The complete ID
   tables were recovered from FMEngine's static initializers (sections 6.2-6.5).
   Theme CSS files are parsed (flex/bison CSS2-style grammar) and converted into
   these IDs; shorthands such as `border-color` are expanded to longhands.
3. **Themes travel inside the database file.** [O] A file stores a full copy of
   each theme it uses (styles, metadata, base theme name and version). The
   installed `Themes/` directories are only the source used when a developer
   applies or upgrades a theme. OpenMaker does not need the Claris theme files
   to render a hosted layout.
4. **Cascade.** [O] Object local style, then the object's custom (named) styles
   (layout-level definitions before theme definitions), then the object type's
   style (layout level, then theme), then each parent object type, then built-in
   defaults. Compound states fall back through their simple parts to `normal`
   (section 6.7).
5. **Coordinates** [O] are doubles (points, [I]) rounded to 1e-6, stored as
   top/left/bottom/right. Child objects of portals and button bars are stored
   relative to their parent; parts are stacked by `offset`/`size`.
6. **Anchoring** [O] lives in the top four bits of `LayoutObject.flags`:
   `0x10000000` left, `0x20000000` top, `0x40000000` right, `0x80000000` bottom.
7. **Rendering stack.** [O] Mac: `libViewSystem` over CoreGraphics/CoreText.
   Windows: `ViewSystem.dll` over Skia (raster `SkBitmap` canvas, DirectWrite
   font manager) plus GDI/GDI+ and Uniscribe. Both sit behind the same
   `IGraphics` abstraction. The app-level `CSSBoxRenderer` paints the CSS box
   model. Charts: ChartDirector 7 to bitmap. Web viewer: WKWebView / WebView2.
   PDF: PDFium (Windows), CGPDF plus the PDFHummus writer with FreeType (Mac).
8. The installer's **`FileMaker Pro 26.pdf` is not a PDF**; it is a 574-byte
   INI-style deployment "Package Definition File" (section 11).

## 2. Sources and method

- Binaries: `re/work/thin/{FMEngine,FileMakerPro,libViewSystem,Support,DBEngine}`,
  Windows DLL import tables (`llvm-objdump -p`).
- Theme packages: `re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro/Themes`
  and `re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/Themes`.
- Shipped database templates: `.../DynamicTemplates/<name>/<lang>.fmp12`.
- Decompilation: Ghidra 12.1.4 against the pre-analyzed project
  `re/work/ghidra/FMEngine`, opened **read-only** by headless scripts
  `tools/ghidra/DecompileList.java` and `tools/ghidra/XrefsList.java`
  (4 GB heap, `-readOnly -noanalysis`, about 10 s per batch). One REA run was
  made for `Draco::FMLayout::LayoutDef::Read` (REA Evidence
  `ev_27e8f820a04654acb990a93b7a469680a290ee3f6de7c68e64a40089782d4126`);
  REA 6.8.0 ignores `REA_GHIDRA_PROJECT_PATH` and re-imports the binary each
  call (about 7 minutes), so the read-only headless route was used instead.
- Scripts written for this workstream (all read-only, no execution of inputs):
  `tools/layout_theme_inventory.py` (theme CSS/manifest inventory),
  `tools/pb_raw_decode.py` (schema-less protobuf decoder),
  `tools/fmp12_xor_strings.py` (raw and XOR-0x5A string scan),
  `tools/fmp12_find_layout_objects.py` (locates `LayoutObject` records),
  `tools/extract_style_ids.py` (ID tables from decompiler text).
- Cross-references: [catalogs/layout-object-types.csv](catalogs/layout-object-types.csv),
  [catalogs/script-triggers.csv](catalogs/script-triggers.csv),
  and DBEngine string resource 129 (the UI names quoted below; the raw
  dump `re/work/arch/dbengine-strres.tsv` is regenerated as in
  [catalogs/README.md](catalogs/README.md)).

## 3. Coordinates and units

| Topic | Finding | Type | Evidence |
|---|---|---|---|
| Storage | `Rect {top, left, bottom, right}` as doubles | O | `LayoutObject.proto`; decoded records (10.4) |
| In memory | converted to x, y, width, height; each edge rounded to 1e-6 | O | `Draco::LayoutObject::Deserialize` @0x394dae; constant 1e-6 at 0x89f7e0 |
| Unit | points (1/72 in). Theme CSS lengths are `pt`; template layouts are 1024 wide, parts 30/44/60 high | I | theme CSS; `LayoutDef.baseWidth = 1024.0` in Tasks template |
| Child coordinates | children of portals and button bars are relative to the parent's top-left | O | Tasks template: button bar at left 330, its segments at left 0 and 200; portal at (163, 700), its children at (8, 95) etc. |
| Other containers | groups, tab panels, slide panels, popovers: relative coordinates likely as well | I | `LayoutObject::GetGlobalBounds`, `GetDescendentOffset`, `ConvertBounds` exist |
| Parts | each part has absolute `offset` (top) and `size`; parts stack top to bottom | O | decoded `PartDef`: top nav 0/44, header 44/30, sub-summary 74/30, body 104/60 |
| Hairline | `Draco::LayoutObject::kHairlineWidth` = 0.25 (float, points) | O | FMEngine 0x89f810; table-view grid borders stored as 0.25 pt |
| Pixel snapping | `IGraphics::SnapRectToPhysicalPixels_{MaintainSize,Outwards,ToEnsureAbuttment}`, `SnapPointToPhysicalPixelBoundary_Nearest`, `kForceSingleDeviceUnitLine` | O | libViewSystem exports |
| Rotation | `LayoutObject.rotation` and style property 613 (`kRotation`, int); on load the style value is moved into the object and removed from the style | O | `LayoutObject::Deserialize` |

## 4. Layout structure

### 4.1 `LayoutDef` semantics

Field names and numbers are from `FMLayout.proto`. Semantics from
`Draco::FMLayout::LayoutDef::Deserialize` @0x2c9904 and `FMLayout` accessors.

| Field | Meaning | Type | Notes |
|---|---|---|---|
| `flags` | raw protobuf field; not consumed by modern `LayoutDef::Deserialize` | O | effective view flags come from catalog member key `0xf1`, via `FMLayout` vtable+0x150 (4.2) |
| `tableAliasKey` | table occurrence the layout shows records from | O | `FMLayout::GetTableAliasKey` |
| `menuSetKey` | custom menu set | O | `FMLayout::GetMenuSetKey` |
| `localStyles` | layout-level `ThemeStyles` (overrides theme styles for this layout) | O | `FMLayout::GetLocalStyles`, `ObjectStyle::TraverseInCascadingOrder` |
| `backgroundStyle` | style of the layout background (object type 20 `layout_background`) | O | decoded: custom style `secondary-background`, objectTypeId 20 |
| `boxStyle` | style of the layout box (object type 21 `layout_box`) | O | decoded objectTypeId 21 |
| `tableGridStyle` | table view grid lines | O | decoded: 0.25 pt top/right borders, 50% gray |
| `baseWidth`, `maxWidth` | layout width; maxWidth semantics tied to responsive layouts | O/U | `FMLayout::SetBaseWidth`, `SetMaxWidth`; `GetConstrainedAutoresizingFlags` compares objects against baseWidth |
| `printColumnWidth`, `printColumnCount` | multi-column printing parameters | O | `FMLayout::SetColumnWidth`, `SetColumnCount`. Every layout of the six English starter solutions (FileMaker Pro 26.0.3) stores count 2 and width 0 [O]. After Pro saved Task List's Layout Setup, the count was 2 and width 270 pt with "Print in 2 columns" either off or on [O, #925]; bit `0x400` of the effective flags enables the option (4.2). The parameter values alone do not say whether it is enabled |
| `customMargins` | Layout Setup's fixed page margins (Rect) | O | `SetFixedMargins`; the starter layouts store 36 pt each side with "Use fixed page margins" unchecked, and FileMaker Pro 26 then prints them at the printer's margins, 18 / 40 / 18 / 18 pt (top, bottom, left, right) on US Letter with Any Printer [O, #263, [#107](https://github.com/OpenMaker-Dev/openmaker/issues/107)]. The rectangle is stored even when the checkbox is off; bit `0x40` of the effective layout flags enables it [O, #609, 4.2]. All 202 layouts in 35 local starter-derived files store 36 pt each side with no other `LayoutDef` field [O] |
| `tblCustomRowHeight`, `tblHeaderSize` | table view row height and header size | O | `SetCustomRowHeight`, `SetTableHeaderSize` |
| `layoutType` | layout kind; legacy values 1 becomes 7, 2 or 3 become 8 on load | O/U | Deserialize remap; meaning of values [U] |
| `orientation` | page orientation | I | `FMLayout::GetOrientation`. Every starter solution layout stores 1 [O]; read as portrait and 2 as landscape, as macOS (`kPMPortrait` 1, `kPMLandscape` 2) and Windows (`DMORIENT_*`) number them [I] |
| `layoutClientType` | 0 or 3: non-responsive, 2: responsive, 1: not set | O | `FMLayout::IsResponsiveLayout` @0x2d23d6, `IsNonResponsiveLayout`, `IsLayoutClientTypeNotSet` |

The layout's theme is a catalog reference (`FMLayout::GetThemeKey` reads member
data key 11), not part of `LayoutDef` [O].

`tableAliasKey` is the **serial-key value** (query-program.md 2.6) of the
occurrence's catalog key [O]: Contacts layouts in test project and the Contacts
template store 1065090 = `0x104082`, the value of serial key `d0 00 02`, which is
occurrence `[3, 17, 5, d0 00 02]`; the portal on Contact Details stores 1065091
(`d0 00 03`, Addresses). A field object's `fieldSpec` names its occurrence the
same way. The catalog identifies an occurrence by its key bytes read as a
big-endian number (`0xd00002`), and so do merge-field references in object
text.

Merge-field control tokens in label text use `<<`, `U+0001`, an observed
variant `U+0001` or `U+0002`, a character holding the hex payload's character
count, the hex payload, then `>>` [O, 2026-10-29]. The payload is a BE u16
table-key byte length, the raw table key, a u8 field-key byte length, and
the raw field key. Thus `0003D00002028123` has count `U+0010` (16 hex
characters) and identifies occurrence `0xd00002`, field `0x8123`; a
one-byte field key instead needs count `U+000E`. Both observed variants
carry this field-reference structure. Neither the count nor field IDs
should be hard-coded to the short-key form. Unknown variants or malformed
lengths remain unresolved.

The table key names any occurrence, not only the layout's own, and the field
key is the field's catalog id in that occurrence's base table [O, 2026-10-30,
hosted layouts with related merge fields]. FileMaker Pro writes a merge field
as `<<Field>>` in the layout's own occurrence and as `<<Occurrence::Field>>`
for a related field (FileMaker Pro Help, placing merge fields). A second
occurrence of the layout's base table is a related occurrence too, since the
token names the occurrence [I]. Whether a token can name a repetition is [U]:
none observed so far does, and OpenMaker shows the first repetition. OpenMaker
leaves an object's text as stored when any of its references names an
occurrence or field the catalog lacks.

The string literals an object stores in its attribute 17 next to the text
are not always the merged fields' names [O, six English starter solutions
of FileMaker Pro 26.0.3]: on Assets' Asset Details a button's `<<Used By>>`
token references Assignments field 9, named `Name`, and no field of the file
is called "Used By"; on Meetings' Meeting Details the literal `Notes` sits
beside a reference to the layout's own field `Note`. The token's reference
names the field; OpenMaker names it from the catalog.

### 4.2 Views (form, list, table)

[O] `FMLayout::IsViewStyleEnabled(view)` tests `flags & mask[view]` with the
mask table at FMEngine 0x89ace0 = `{form: 0x2, list: 0x4, table: 0x8}`; a set
bit **disables** that view. `FMLayout::GetDefaultViewStyle` returns table (2)
if flag `0x4000` is set, else list (1) if flag `0x200` is set, else form (0).

**The view a window shows a layout in** [O: FileMaker Pro 26.0.3, #805, a
local Tasks copy, Task Details (default Form) and Task List (default List)
on one table]:
- **Within a session**, a window shows each layout in the view it last had
  there. This holds when that view is not the layout's default: Table
  chosen on Task Details came back after a visit to Task List, as did List
  on Task List.
- **After closing and reopening**, the last view came back too. Both
  sessions wrote the file for another reason (a Layout Setup save, and one
  that changed the file with no record edited), so what makes a view
  persist is not isolated.
- **Not measured:** what a new window inherits, and Find mode and Preview.

Rendering consequences [I, from public FileMaker behavior and the part model]:
form view draws one record (body once); list view repeats the body part per
record inside a scrolling area, with header and footer fixed; table view is
generated from the layout's fields (`Draco::TableLayoutBuilder`, which builds
columns, grid lines from `tableGridStyle`, and summary rows), with the top
navigation, header, footer and bottom navigation parts its Table View
Properties include around it (below).

Sub-summary parts in Browse [documented]: they "appear in Table View and List
View when sorted by break fields, and update dynamically when you change data
in the file"
([Claris: viewing records](https://help.claris.com/en/pro-help/content/viewing-records.html)).
A summary field in the part summarizes its group of records sorted by the
break field; other fields show the group's first (leading) or last
(trailing) record
([Claris: layout part types](https://help.claris.com/en/pro-help/content/layout-part-types.html)).
[I] List view places them as printing does: a leading part before each
group's first record, outermost break first, and a trailing part after its
last, innermost first; a part whose break field the found set is not sorted
by does not show. [I] Form view does not show sub-summary parts: the help
names only Table and List view. Unmeasured: whether a click, field entry or
button acts in a sub-summary part in Browse, the part's current-row and
alternate-row styling there, and how scrolling to a record places it.

The current native Contacts file stores the effective flags as an eight-byte
big-endian member value at `4/5/<layout>/0xf1`, rather than in the protobuf
`LayoutDef.flags` field. Contact List has `0x10f30a0b` (default List), while
Contact Details has `0x10f3080d` (default Form). Both protobuf blobs omit
field 1 and therefore decode it as zero. The installed arm64 FMEngine's
`LayoutDef::Deserialize` at `0x2bbcb8` does not consume that protobuf field;
`FMLayout::GetDefaultViewStyle` at `0x2c23f0` reads the member flag accessor
at virtual offset `0x150`. OpenMaker hydrates `LayoutDef.flags` from member
key `0xf1` when decoding a catalog tree, retaining the raw protobuf fallback
for standalone or older definitions. This prevents the real Contact List
layout from opening as Form because its protobuf flag field is absent.

[O, 2026-11-11, FileMaker Pro 26.0.3] Bit `0x10` set means Layout Setup's
**Save record changes automatically** is cleared. Clearing it on Contact
Details of a copy of the Contacts template changed that one bit,
`0x10f3080d` to `0x10f3081d`, in two places: the member at
`4/5/<layout>/0xf1` and the layout's entry at `4/1/7/<layout>/0xf1`, which
holds the same flags beside the layout's name (`10`). The rest of the save
was the layout's modification stamps (`0xfc`, `0xfc01`, `0xfc03`), its
object store `07` written again at the same length, and the file's history
`[2, 8]`, `[2, 9]`. Save as XML shows these flags as the layout's
`<Options>`, and a file FMUpgradeTool generated with the bit set there
opened in FileMaker Pro with the option cleared in Layout Setup. No layout
of the starter templates in the FileMaker Pro installers has the bit set
(`DynamicTemplates/*/en.fmp12`, macOS and Windows). What a cleared option
does in Browse is in script-runtime.md, Implementation notes (desktop
client), Errors and dialogs.

[O, 2026-11-26, FileMaker Pro 26.1.1 for macOS,
[#353](https://github.com/OpenMaker-Dev/openmaker/issues/353)]
Bit `0x2000` in the effective layout flags enables Layout Setup >
General's **Show alerts when field isn't modifiable or no records are
present**. On an English Tasks copy, Task Details' member and catalog
entry flag words both moved from `0x10f3080d` to `0x10f3280d` after
enabling the option and saving the layout. Reopen showed the checkbox
still enabled. Clearing and saving it restored both original flag words.

In Browse with zero records, clicking the same Task field with the
option off showed no alert. With the option on, clicking showed
"No records are present. To create a new record choose the New Record
menu command." Typing without focus after the click with the option off
still showed the regular focus alert.

[O, 2026-11-14, FileMaker Pro 26.0.3, local disposable Tasks copies, #609]
Bit `0x40` set means Layout Setup > Printing's **Use fixed page margins** is
checked. On Task List, the effective flag word at both
`[4,5,<layout>,0xf1]` and `[4,1,7,<layout>,0xf1]` changed from `0x10f30a0b`
to `0x10f30a4b` when the box was checked, with the stored margins still
36 pt on every side. Setting top/bottom/left/right to 50/60/70/80 pt kept
that flag word; unchecking restored `0x10f30a0b`. Protobuf
`LayoutDef.customMargins` (field 12 of `[4,5,<layout>,02]`) stores the
rectangle in top/left/bottom/right order even with the option off. Pro also
rewrote `printColumnWidth` and save metadata, so their changes do not
identify the checkbox. [O, 2026-11-25: the same copies' Task List
`LayoutDef` re-read with `tools/fmp12_keytree.py`] Pro rewrote the stored
`Rect` in place: top/bottom/left/right 50/60/70/80 is stored as its fields
1-4 top 50, left 70, bottom 60, right 80. `printColumnWidth` (field 10)
became the US Letter width less the left and right margins over
`printColumnCount` (2): 270 with fixed 36 pt margins, 231 with 70/80, and
288 with the box off (the printer's 18/18). Every save also wrote
`layoutClientType` (field 17) 0 explicitly, 3 bytes the original lacked.
With the box off and Format For set to **Any
Printer**, Page Setup's paper help and Task List's disabled Printing fields
read as follows [O, local disposable copies, #609]:

| Paper | Page Setup top/bottom/left/right | Task List top/bottom/left/right |
|---|---|---|
| US Letter | 0.25 / 0.56 / 0.25 / 0.25 in | 18 / 40 / 18 / 18 pt |
| A4 | 0.25 / 0.57 / 0.25 / 0.25 in | 18 / 41 / 18 / 18 pt |
| US Legal | 0.25 / 0.56 / 0.25 / 0.25 in | 18 / 40 / 18 / 18 pt |

On US Letter, Pro's Save/Send Records As > PDF output placed the Task List
heading's text bounding box at x/y 33/27.235 pt with the option off, then
51/45.235 pt with fixed 36 pt margins, and again 33/27.235 pt after the
option was turned on and back off [O, local, #609]. The first record name
moved by the same 18 pt in both directions; the Category heading was partly
visible with the option off and absent with fixed 36 pt margins. Each output
was one page with the same three records. PDF output on A4/Legal, other
printers and Windows remains unmeasured [U].

[O, 2026-11-15, FileMaker Pro 26.0.3, local disposable Tasks copies, #925]
Bit `0x400` set means Layout Setup > Printing's **Print in 2 columns** is
checked. On Task List, both effective flag words (`[4,5,<layout>,0xf1]` and
`[4,1,7,<layout>,0xf1]`) changed from `0x10f30a4b` to `0x10f30e4b` when the
box was checked. Protobuf `LayoutDef` kept `printColumnCount=2` and
`printColumnWidth=270` pt in both copies. The unchecked copy exported as a
one-page PDF in FileMaker Pro. All 38 layouts of the six English starter
solutions as shipped have the bit clear [O]. Whether Pro uses this bit for
column rendering on every layout kind remains unmeasured [U].

Bit `0x8000` set means Layout Setup's **Enable Quick Find** is cleared
(query-program.md 6.6). Evidence: Claris's Database Design Report grammar
gives each layout a `quickFind` attribute [Claris: FileMaker Pro 13
Advanced, "Database Design Report XML Output Grammar", Layout section];
a published measurement of FileMaker Pro 26.0.2's Save as XML, made by
setting one option at a time and reading which bit of the layout's
`<Options>` moved, puts it at bit 15, inverted
([andykear/FileMaker-XML-bit-flags](https://github.com/andykear/FileMaker-XML-bit-flags),
`data/layout-flags.json`) [O, third party]. The same table reads the
starter templates' other bits consistently: `0x10f30a0b` (the List
layouts) is "delineate current record only" (`0x1`), Form and Table view
disabled (`0x2`, `0x8`), the default List view (`0x200`, above), vertical
part labels (`0x800`), the table view's grid, column headers and column
options (`0xf30000`) and the record indicator (`0x10000000`). No layout of
the six English starter solutions sets `0x8000` [O]. FileMaker Pro
26.0.3 sets this bit and the other four General options' bits, the record
indicator's inverted, one checkbox at a time (10.11) [O, 2026-11-14].

Two of these bits are Layout Setup's field delineation options:

| Bit | Option | Evidence |
|---|---|---|
| `0x1` | Delineate fields on current record only | [O] FileMaker Pro 26.0.3's Layout Setup sets it (10.11); the third-party table above agrees, and Pro reads the option on for Task List, whose flags `0x10f30a0b` have it set (#263) |
| `0x20` | Show field frames when record is active | [O] FileMaker Pro 26.0.3's Layout Setup sets it (10.11); Task List reads it off, and the bit is clear |

With `0x1` set, a field's borders and fill show on the current record only. Claris documents that they then do not show "when you print, print to PDF, or preview" [Claris: "Showing field borders and fill for the current record"]. FileMaker Pro 26.0.3 prints none of the starter lists' field underlines [O, #263, 2026-11-13]. Its Task Details PDF draws the `fill-secondary-popup` pop-ups and the `fill-container` photo field with no fill or border operations at all, only their clip paths and text [O, [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201), 2026-11-13]. Fields in header, footer, summary and navigation parts never show borders and fill [Claris, same page].

**Table View Properties** (Layout Setup > Views > Table > Properties)
keep their options in the same flag word. The four part options are
measured [O, FileMaker Pro 26.1.1 for macOS, [#927](https://github.com/OpenMaker-Dev/openmaker/issues/927)]:
four separately saved copies of a one-record Contacts fixture, each with
one option checked, changed the one bit in the table, against a copy with
all four off, in both flag words (`[4,5,L,0xf1]` and `[4,1,7,L,0xf1]`).
No leaf under the layout's `0d` changed. The saves also changed other
save metadata; no save without a change was made to compare it with. The
other
options' bits come from the third-party table above, FileMaker Pro
26.0.2's Save as XML read one option at a time [O, third party]; they
have not been read back from a file FileMaker saved [U].

| Bit | Option | Evidence |
|---|---|---|
| `0x10000`, `0x20000` | Grid: Horizontal, Vertical | third party |
| `0x40000` | Include header part | 26.1.1 |
| `0x80000` | Include footer part | 26.1.1 |
| `0x100000` | Include column headers | third party |
| `0x200000`, `0x400000` | Resizable columns, Reorderable columns | third party |
| `0x800000` | Sort data when selecting column | third party |
| `0x4000000` | named `customColumnOrder` there; no option of the dialog has this name [U] | third party |
| `0x20000000` | Include top navigation part | 26.1.1 |
| `0x40000000` | Include bottom navigation part | 26.1.1 |
| `0x1_0000_0000` to `0x8_0000_0000` | macOS: System appearance, Comfortable formatting, Alternating row colors, Row numbers | third party |

- **Claris's help** says to "Select the parts to display: top
  navigation, header, footer, and bottom navigation", and that "These
  settings matter only if you have defined the parts for the layout, and
  they're only in effect in Table View"
  ([Setting up form, list, and table views for a layout](https://help.claris.com/en/pro-help/content/form-list-table-views.html)).
  [Layout part types](https://help.claris.com/en/pro-help/content/layout-part-types.html)
  says each of the four shows in Table view only when its option is on,
  and that navigation parts are not zoomed. The System appearance
  options apply "only to the table, not to any of the selected layout
  parts" [Claris, the first page].
- **The starter layouts:** their `0xf30000` is the two grids and the four
  column options on, and the four parts off [O].
- **Where Pro's Table view puts the parts** [O, FileMaker Pro 26.1.1,
  #927]:
  - The top navigation and header parts are above the column headings, in
    that order.
  - The footer and bottom navigation parts stack at the window's bottom,
    also with one record and a large gap above them; a footer alone is at
    the window's bottom too.
  - At 150% zoom the table, header and footer scale; the navigation parts
    keep their 100% height.
  - Scrolling sideways moves the columns, and the top navigation part's
    text keeps its place in the window.
- **Not measured** [U]: how the header, footer and bottom navigation
  parts' objects behave when the table scrolls sideways (the measured
  parts held no objects), whether Tab reaches the parts' fields, and the
  pages of web viewers in them.

### 4.3 Parts

`PartDef.type` is copied unchanged into the internal `LayoutPart::PartType`
[O, `LayoutPart::Definition::Deserialize` @0x2da44e]. The part-to-style-type
table at FMEngine 0x8e4a10 [O, used by `LayoutPart::CalculateObjectType` @0x2dad14]:

| PartType | Part | Style object type (CSS element) |
|---|---|---|
| 0 | Title header | `title_header` (31) |
| 1 | Header | `header` (32) |
| 2 | Leading grand summary | `leading_grand_summary` (33) |
| 3 | Leading sub-summary | `leading_sub_summary` (100); level n uses `leading_sub_summary_n` (101-116) |
| 4 | Body | `body` (34); alternate rows `body_alt` (35) |
| 5 | Trailing sub-summary | `trailing_sub_summary` (200); levels 201-216 |
| 6 | Trailing grand summary | `trailing_grand_summary` (37) |
| 7 | Footer | `footer` (38) |
| 8 | Title footer | `title_footer` (39) |
| 9-11 | (generic `part`) | `part` (30) [U: unused or legacy] |
| 12 | Top navigation | `top_nav_part` (40) |
| 13 | Bottom navigation | `bottom_nav_part` (41) |

`PartDef` fields [O]: `type`; `flags` (the pagination options; the binary
also has `LayoutPart::Definition::UseAltStyling` and `UseCurrentRowStyling`,
whose storage is [U]; `0x400` is not either of them as a plain bit, below); `pageBreakAfter`; `offset` and `size`
(doubles); `breakFieldKey` (sub-summary "when sorted by" field [I]); `style`
(the part's `ObjectStyle`); `altStyle` (style for alternate body rows, object
type `body_alt`).

`breakFieldKey` [I] is a counted key holding a counted list of length-prefixed
segments: `[n][n - 1][file][occurrence][field]`. Every shipped template seen has
an empty file segment, then the occurrence and field key segments, read
big-endian as the catalogs number them: Tasks `08 07 00 03 d0 00 02 01 04` is
field 4 of occurrence `d0 00 02`; Content Management has `... 01 02` and another
Tasks layout `03 d0 00 04 01 0a`. A non-empty file segment (presumably another
file's field) has not been seen; `Part::break_field` refuses it and every other
shape.

[O] The six English starter solutions' Details layouts (9 of them) set
body `flags` `0x400`; no other starter part has nonzero `flags`. What the
bit means is [U]:
- A published Save as XML decoding of part `<Options>` (andykear/FileMaker-XML-
  bit-flags, `part-flags.json`, FileMaker 26.0.2) lists only pagination bits
  (1 break before, 4 restart page numbers, 64 allow break across pages, 256
  discard remainder) and says the two row-state options are stored elsewhere.
- [O] FileMaker Pro 26.0.3 for macOS shows the same Part Setup options for
  Task Details' body (`0x400`) and Task List's body (`0`): "Use alternate
  row state" off and "Use active row state" on ([#107](https://github.com/OpenMaker-Dev/openmaker/issues/107), 2026-11-13). The two
  bodies differ in the bit and agree on both checkboxes, so `0x400` does not
  store either checkbox as a plain bit, set or inverted [I]. Whether it takes
  part in their storage some other way is [U]; settling it needs a toggle
  experiment. Portals keep these two options in style properties 610
  and 620 (5.3.2); whether bodies do too is [U].
- [O] Preview of Task Details with two records shows the same body fill on
  both pages ([#107](https://github.com/OpenMaker-Dev/openmaker/issues/107); compared by eye, not sampled). OpenMaker prints these
  bodies as it prints bodies without the bit.

**Group headings stay with their first record.** [O] FileMaker Pro 26.0.3's PDF
of the Tasks starter's Task List (#263, 2026-11-13) shows this. The 60 records
are sorted by Status into a group of 22 (R00-R21) and one of 38. Page 2 holds
the header and R11-R21, ending 708 points down. Group B's 30-point leading
sub-summary would fit below (738 of the 752-point printable height), but it
starts page 3 instead, directly above R22. Six pages; the space below each
page's last row takes the body's white fill. What happens with several nested
leading sub-summaries, or a leading grand summary, is [U]. OpenMaker moves
consecutive leading sub-summaries together with the record they head [I].

**The printed page behind the parts.** [O] FileMaker Pro 26.0.3's PDFs of
Task Details, Content Details and Content List show the order in which a page
is painted ([#201](https://github.com/OpenMaker-Dev/openmaker/issues/201), 2026-11-13, read from the content streams):
1. The whole printable area is filled with the layout background's fill
   (object type 20, `layout_background`; the starters' `secondary-background`
   gray, 0.914).
2. The parts are drawn.
3. The area from the bottom of the last part to the bottom of the printable
   area is filled with the body part's fill, without its border: gray (0.914)
   below a Details layout's body, white below a list's last row. The list's
   layout background is gray too, so this step is what makes the list white
   there.

On these pages the last part is always a body. When a summary part or a
footer ends the page, it is [U] whether that area takes the body's fill or
the last part's, and where it stops above a footer.

**Part borders.** [O] The starter lists' header and sub-summary parts
store 0.25-point borders on all four sides, with a solid style (5) on the top
and bottom and no style on the left and right. The Details body stores the
same. Pro's PDFs (Content List, Task Details; [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201)) stroke only the top and
bottom edges: every part stroke in their content streams is horizontal. An
unset border style on a part edge therefore draws no line. That Pro's Browse
does the same is [I]. For layout objects an unset style is not measured.

Theme metadata supplies minimum part sizes and padding for new layouts
(`kTitleHeaderMinSize`, `kBodyMinSize`, ... in 7.4); they are layout-builder
hints, not render rules [I].

### 4.4 Layout-level data stored next to `LayoutDef`

See section 10.1 for the key map. Layout script triggers (`KScriptTriggers`),
layout metadata (`KMetadataDefs`), and layout-level styles (`KLayoutStyleDefs`)
are separate records [O].

## 5. Layout objects

### 5.1 Object store, tree, and z-order

[O] All objects of a layout are one `FMLayoutPB.LayoutObjectStore` message
(`Draco::LayoutObjectStore::Load` @0x3ae2c0):

1. For each `objectDef`, create the object from `LayoutObject.type` via
   `Draco::LayoutObjectFactory::Create` @0x3dceee, then call its `Deserialize`.
2. Objects with `childkey` entries are containers; after all objects exist,
   children are attached in `childkey` order.
3. `rootChildKey` lists the top-level objects; they are attached to the store
   root in that order.

Z-order: list order is stacking order [I]; first listed is back-most [U, needs a
live check]. Object keys are per-layout serial numbers (the Tasks template
reuses keys 1..30 in every layout) [O].

Forward compatibility [O]: if `LayoutObject.version` exceeds the version this
client supports for that type, the object is created as `UnknownLayoutObject`.
Unknown protobuf fields are kept (`LayoutObject::SaveUnknowns`,
`RestoreUnknowns`, `CloneUnknowns`).

### 5.2 Common `LayoutObject` fields

| Field | Meaning | Type |
|---|---|---|
| `type` | object kind (5.3) | O |
| `key` | object key, unique within the layout | O |
| `flags` | bit field, named in 5.2.1; anchors in bits 28-31 (5.7); bit `0x4000` gates the tooltip lookup (`LayoutObject::GetTooltipCalc` @0x39619c); sliding and Hide when printing are `0x10`, `0x20`, `0x40`, `0x200` and `0x8000` (5.8); the hide condition and its "Apply in Find mode" are `0x4` and `0x100` (5.5) | O |
| `bounds` | Rect (section 3) | O |
| `style` | `ObjectStyle`: local style overrides, custom style names (internal and display), and `objectTypeId` (the style object type, 6.2) | O |
| `childkey` | children of container objects, in order | O |
| `textObj` ... `segmentedBarObj` | one type-specific submessage (5.3) | O |
| `rotation` | rotation (also carried as style property 613) | O |
| `labelKey` | key of a label object associated with this object [I: field label] | O/I |
| `version` | per-type object version for forward compatibility | O |

### 5.2.1 `LayoutObject.flags` bits

`LayoutObject.flags` is the object's `mFlags` (`LayoutObject` + 0x58):
`LayoutObject::Deserialize` (FMEngine 0x394dae) copies protobuf field 3
there and `LayoutObject::Serialize` (0x39519c) writes it back [O, from
bin]. The bit names are the enumerators of `Draco::LayoutObject::FlagValues`
in FileMaker Server's `libFMEngine.so` DWARF (`LayoutObject.h`). The same
enum also serves `mVolatileFlags` (+0x5c), which is not stored, so a name
alone does not say that FileMaker stores the bit; the last column says
what else supports it. Corpus counts are over the 24,788 objects of the
MBS 16.3 examples and the harness fixtures (703 files FileMaker wrote,
2026-11-11).

| Bit | DWARF name | Meaning and evidence |
|---|---|---|
| `0x1` | `kHasConditionalFormatting` (also `kCollapsed`) | the object has conditional formatting: 534 of the 547 objects with the bit have `KCFConditions` (`8a`), 47 objects with `8a` lack it [I] |
| `0x2` | `kLocked` | locked in Layout mode [I: name; `LayoutObject::IsLocked`] |
| `0x4` | `kHasHideCondition` | "Hide object when" is on (5.5) [O] |
| `0x8` | `kTabOrderOmit` (also `kTategaki_OBSOLETE`) | read by `LayoutObject::GetTabOrderOmitted` [I: meaning] |
| `0x10`, `0x20`, `0x40` | `kSlideUp`, `kSlideLeft`, `kShrinkPart` | 5.8 [O] |
| `0x80` | `kTableView` | set on 6,327 of the 6,743 objects of layouts' table views (`0d`, 10.1), on no other object [O] |
| `0x100` | `kEvaluateHideConditionInFindMode` | "Apply in Find mode" (5.5) [O] |
| `0x200` | `kNonPrinting` | 5.8 [O] |
| `0x400`, `0x800` | `kUNUSED3`, `kUNUSED4` | not seen |
| `0x1000`, `0x2000` | `kLineStartLeft`, `kLineStartTop` (also `kTableViewHide`, `kTableViewKeep`) | a line's first endpoint (5.3.1) [O]; the table-view meanings [U] |
| `0x4000` | `kHasTooltip` | the tooltip gate (5.5); 940 of the 958 objects with the bit have a tooltip `03` [O] |
| `0x8000` | `kSlideDirect` | 5.8 [O] |
| `0x10000` | `kShowHandCursor` | the button's hand cursor option: FileMaker's Database Design Report stylesheet (`en.lproj/DDR/Detail.xsl`) reads it from button and grouped button objects; in the corpus only on buttons (251) and groups (943) [O] |
| `0x20000` | `kFixedPart` | seen on 3,221 objects of most kinds [U: meaning] |
| `0x40000` ... `0x800000` | `kChanged`, `kChangedRep`, `kChangedAll`, `kHasUndo` (also `kSummaryLabel`), `kSelected`, `kDeleted` | editing state [I: names]; a FileMaker Pro layout save left `0x400000` on the object that was selected (10.5), and 86 corpus objects have it |
| `0x1000000`, `0x2000000` | `kVertCenter`, `kVertBottom` | [U] (1,038 and 3 objects) |
| `0x4000000`, `0x8000000` | `kEmptyColumn`, `kOnlyOnTableLayout` | [U] (996 and 0 objects) |
| `0x10000000` ... `0x80000000` | `kAnchorLeft`, `kAnchorTop`, `kAnchorRight`, `kAnchorBottom` | 5.7 [O] |

[I, OpenMaker] Browse uses the actionable object's `kShowHandCursor` bit to
request the platform pointing-hand cursor from its hover response, including
grouped buttons, button-bar segments and popover buttons. The owner's hover
report supplies behavioral evidence; native cursor shape and detailed mode
parity have no new FileMaker measurement. Other modes and unavailable
controls retain their usual cursors.

### 5.3 Object type catalog (wire `LayoutObject.type`)

Wire values are from `LayoutObjectFactory::Create` @0x3dceee [O]. These are
**not** the `GetLayoutObjectAttribute` objectType indexes listed in
[catalogs/layout-object-types.csv](catalogs/layout-object-types.csv).
Style object type resolution is `ObjectStyle::GetObjectType(LayoutObject const&)`
@0x3235cc [O]. Components are the ones themes actually style for that element.

| Wire | Object | FMEngine class | Style type (CSS element) | Submessage fields (LayoutObject.proto) | Components used by themes |
|---|---|---|---|---|---|
| 1 | Field (all control styles, container fields) | `FieldLayoutObject` | by display type, 5.4 | `fieldObj`: flags, fieldSpec {tableKey, fieldKey, repetition}, numOfReps, inputMode, displayType, valueListKey, keyboardType, scrollbarStyle | self, text, inner_border, repeat_border, icon, baseline, scrollbar_* |
| 2 | Text | `TextLayoutObject` | `text_box` (70) | `textObj`: flags only; the text itself is stored elsewhere (5.5) | self, text, icon, inner_border |
| 3 | Graphic (static image) | `GraphicLayoutObject` | `graphics` (84) | `graphicObj`: libraryKey (image in file library), pictFormat, mixColor | self |
| 4 | Line | `ShapeLayoutObject` | `line` (50) | `shapeObj`: type, renderType | self |
| 5 | Rectangle | `ShapeLayoutObject` | `rectangle` (51) | same | self, inner_border |
| 6 | Rounded rectangle | `ShapeLayoutObject` | `rounded` (52) | same | self, inner_border |
| 7 | Oval | `ShapeLayoutObject` | `oval` (53) | same | self, inner_border |
| 8 | Group (and grouped button) | `GroupLayoutObject` | `group` (87) | `groupObj`: scriptStep (a nonzero step ID gives the group an action) | - |
| 9 | Portal | `PortalLayoutObject` | `portal` (83) | `portalObj`: tableContextKey, tableAliasKey, portalFlags (5.3.2), oldNumRows, initialRow, numRows (double), scrollbarStyle | self, contents, row, row_alt, row_active, scrollbar_track, inner_border |
| 10 | Button, also each button bar segment | `ScriptStepButtonLayoutObject` | `button` (80); `button_bar_segment` (79) inside a button bar | `buttonObj`: flags, textFlags, scriptStep {id, value, flags, reserved}, libraryKey (icon), iconSize, displayType | self, text, icon, inner_border |
| 11 | Tab control | `TabLayoutObject` | `tab_control` (81) | `tabObj`: flags, tabWidth, tabHeight, tabStyle, defaultPanelKey, currentPanelKey | self, tab_header |
| 12 | Tab panel | `TabLayoutObject::Panel` | `tab_panel` (82) | `tabPanelObj`: flags, tabLeftEdge, tabWidth, textFlags | tab, tab_inner_border, text, panel, panel_inner_border, self |
| 13 | External: web viewer, chart, plug-in object | `ExternalLayoutObject` | `web_viewer` (86) or `chart` (85) by external type | `externalObj`: type (four-char code), subType, flags | self, inner_border (web viewer also status_bar, progress_bar, completed components) |
| 14 | Popover button | `PopoverButtonLayoutObject` | `button` (80) | `popoverButtonObj`: flags, textFlags, popoverKey, libraryKey, iconSize, displayType | as button |
| 15 | Popover | `PopoverLayoutObject` | `popover` (89) | `popoverObj`: flags, textFlags, position, buttonKey | self, contents, text, inner_border |
| 16 | Slide control | `DotLayoutObject` | `dot_control` (91) | `dotObj`: flags | self, contents, dot_container, dot, inner_border |
| 17 | Slide panel | `DotLayoutObject::Panel` | `dot_panel` (92) | `dotPanelObj`: flags | - |
| 18 | Button bar | `SegmentedBarLayoutObject` | `button_bar` (78) | `segmentedBarObj`: flags (`0x2` calculated active segment), activeSegmentKey (5.5.1) | self, button_bar_divider |
| other | Unknown | `UnknownLayoutObject` | `unknown` (0) | preserved unknown fields | - |

Observed in the Tasks template [O]: a button bar (type 18) whose children are
type 10 objects with button-bar-segment styles; a portal (type 9) with four
children (fields and buttons); field displayType 6 with style type 66 (calendar).
Its segments are Go to Layout buttons (step 6, value 5, the layout's key in
`reserved`), each a single-step button like any other (section 5.6); the
active segment is section 5.5.1.

Slide navigation [O, FileMaker Pro 26.1.1]. In a synthetic three-panel control
with swipe enabled, `dotObj` (field 19) stores `08 00` when Show navigation
dots is on and `08 01` when it is off. Clicking a dot switches the active
panel. The dots use component 26, normal state 1 and active state 7. In
Enlightened the default dot is 9 pt. Setting 11 pt saves the dot's width
and height (properties 109 and 110) as 11 pt, the dot container height
(27/110) as 17 pt, and the contents bottom (8/119) as 23 pt. The default
heights are 15 and 21 pt. The dot row is centered, with a gap of about
4 pt and a row bottom inset of 3 pt in both cases. The theme stores a dot
left margin of 0.33 em and a dot container bottom of 0.25 em. Relating
these lengths to 12 pt for the measured theme is inferred. Other theme
unit bases, missing flags, swipe and panel-switch triggers were not measured.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1031#issuecomment-7192568479).

The contents component paints the panel's background and border and clips
its child objects [O, FileMaker Pro 26.1.1]. A two-line label placed at the
lower edge shows the portion inside the panel, and the remainder stays
clipped even when the label is still present in accessibility. The same
control with dots disabled has a taller contents frame: near 3 pt of its
height remains below the border in Enlightened. Using the dot container's
bottom inset for that remaining space is inferred from the measured
geometry. [Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1121#issuecomment-7192729802).

Popovers [I: the field names]: a popover button's `popoverKey` is the object
key of its popover, and the popover's `buttonKey` the key of its button. In
Browse, OpenMaker keeps a popover hidden until its button is clicked; a second
click closes it. Each layout has at most one open popover. It stays open while
records change and shows on the current record, painted above the layout's
other objects. Objects under an open popover take no clicks, and scroll areas
under it no wheel; the popover's own objects do. It shows only while its
containers do: a panel that is not selected, a hide condition or a portal row
out of view hides it with them, and it shows again when they do.

Close Popover (script step 169) dismisses the current layout's popover in
the script's document window. It leaves other layouts' and windows' popovers
and unsaved record drafts alone. Field exit stages and the popover's
OnObjectExit use the existing cancellable trigger path; on success the
current record's associated button receives focus and OnObjectEnter before
the script goes on. A later field/navigation step takes precedence over
that pending focus.
No open popover is success without an event. These window, no-op and button
entry rules are documented by Claris ([Close Popover](https://help.claris.com/en/pro-help/content/close-popover.html),
[OnObjectEnter](https://help.claris.com/en/pro-help/content/onobjectenter.html));
the generic field stages reuse the client's existing exit lifecycle, without
a native desktop probe. Other closing rules (a click elsewhere, Esc, a layout
change) remain [U] and are not modelled.

The wheel in Browse Form view: how FileMaker routes the mouse wheel and
trackpad scrolling between a portal and its window is [U]; no capture records
it. OpenMaker's rule is a design choice. A scrolling portal under the pointer
takes the wheel, including one in a selected tab panel or an open popover.
Anywhere else on the layout, the document scrolls. The document also gets
what a portal at its first or last row cannot use, as egui passes it from a
nested scroll area to the one around it. An open popover keeps the wheel from
the objects it covers (above), and a menu, popup or window over the layout
keeps it from the whole layout. The rule holds at every document zoom level
(desktop-client.md, Zoom).

### 5.3.1 Line direction and stroke geometry

[O-bin, O-code; macOS arm64 FileMaker 26] A line uses the top border's
width/color and one stroke; it does not draw a filled rectangle or its four
border sides. Common flags `0x1000` and `0x2000` select the first endpoint's
left and top edges, respectively; the second endpoint uses the opposite edges.
`CalcLineGeometryPoints` and `CalcLineStrokePoints` are at FMEngine
`0x388fd8` and `0x38903c` in this build. Direct calls on invented coordinates
confirmed all four directions for horizontal, vertical and sloped lines at
0.25, 0.5, 1 and 2pt. Stroke endpoints extend beyond geometry endpoints:
half the width is distributed over x/y in proportion to the absolute slope.

[O-code] `LayoutObjectView::DrawView` (`0x101a621c4`) offsets sub-point
strokes by 0.5 layout points along x when height exceeds width, otherwise y.
Widths of at least 1pt whose rounded integer is odd offset both axes by 0.5pt;
`Coord::AsInt` rounds positive halves upward (1.5 to 2, 2.5 to 3, confirmed by
direct calls). Zero width is skipped. These are layout-point offsets before
zoom, not a minimum visible-width rule.

[O-code, O-GUI] `LayoutObjectView::CalculateFrame` (`0x101a66e64`) expands
the unshifted line bounds by `width / sqrt(2)` on every side. The graphics
clip snaps that frame outwards to physical pixels, separately from the
stroke's half-point placement. This can clip a narrow stroke partly or
entirely out of its frame. At 150% on a 2x display, the scratch probe's
whole-point 0.25pt horizontal line disappears, while the half-point one
retains 0.375 physical pixels of coverage. At 100% both retain 0.25px; at
400% the whole-point one disappears. Native captures at 100/150/400% also
confirm the 0.5/1/2pt coverage used by the GUI regression. Thin borders
on fields have their own rendering path; this clip applies to line objects.

### 5.3.2 Portal options (`portalFlags`)

Portal Setup's options are bits of `PortalLayoutObject.portalFlags`, which
FileMaker keeps as 16 bits (`PortalLayoutObject::Serialize`, FMEngine
0x3a8e84, writes the `u16` at +0x1d4) [O, from bin]. The names are the
enumerators of `Draco::PortalLayoutObject::PortalFlagValues` in FileMaker
Server's `libFMEngine.so` DWARF (`PortalLayoutObject.h`). The dialog's
controls are tied to the bits by `CPortalSetupDlog::InstallItems` and
`GetItems` (FileMaker Pro x86_64 0x1005b0e54, 0x1005b119a), each control
named by its tag in `Base.lproj/IDD_PORTAL_SETUP.nib`:

| Bit | DWARF name | Portal Setup | Evidence and use |
|---|---|---|---|
| `0x1` | `kShowScrollBar` | "Allow vertical scrolling" | the creation row and rows past the visible ones (script-runtime.md 13.24; `DataEntryModel::GetPortalRowCount` 0x5aa392) |
| `0x2` | (none) | none | 3 corpus portals; Portal Setup does not keep it (below) |
| `0x4` | `kAllowDelete` | "Allow deletion of portal records" | `FMWindowModel::IsDataEntryCommandEnabled` (0x61c914) enables the commands that delete a record, while a portal row is active, only with this bit (and a valid row); 11 corpus portals |
| `0x8` | `kSortRows` | "Sort portal records" | `PortalLayoutObject::GetSortSpec` (0x3a8312) answers only with it and `SetSortSpec` (0x3a8560) can clear it; all 33 corpus portals with the bit have a sort spec `0a`, one more keeps a spec without it |
| `0x10` | `kRetainScroll` | "Reset scroll bar when exiting record", **cleared** when checked | `FMLayout::ResetScrollSettings(bool)` (0x2cf6c0) resets a portal's scroll position when forced or when the bit is clear; the dialog shows the checkbox's state only with `0x1` |
| `0x20` | `kAutoresizeRows` | none | `PortalLayoutObject::GetRowCount` (0x3a75e8) and `FMLayout::SetDisplayBounds` test it [I: rows follow the portal's height]; not seen |
| `0x40` | `kPortalUseRightToLeft` | none | [I: name]; not seen |
| `0x80` | `kFilterRows` | "Filter portal records" | `PortalLayoutObject::GetFilterCalc` (0x3a8658) answers only with it; all 9 corpus portals with the bit have a filter `25`, two more keep a filter without it |
| `0x100` | `kInlineScroll` | "Show scroll bar": "When scrolling" (item 2 of the pop-up; "Always" clears it) | 68 corpus portals, every template portal |
| `0x200` | `kMasterDetail` | none | [U]; not seen |
| `0x400` | (26.1.1; no DWARF yet) | "Place row for new record at top" | FileMaker Pro 26.1.1 (x86_64): `CPortalSetupDlog::GetItems` (0x1005e10e4) sets it from the new checkbox, `PortalLayoutObjectView::HasNewRecordRowTop` (0x101d2f3b6) tests it; FMEngine `DataEntryModel::IsVisibleTopNewPortalRow` (0x5d2076) takes the top row only with `0x400` set and `0x1` clear. Claris: the option is offered when the relationship allows creating related records. Not seen in 26.0 files |

Portal Setup's OK writes the whole word from its controls, so it clears
`0x2`, `0x20`, `0x40` and `0x200` [from bin]; 26.1.1's also writes `0x400`. "Use alternate row state"
and "Use active row state" are not flags: they set style properties 610
(`-fm-portal-alt-background`, `FMStyle::kPortalAltBackground`) and 620
(`-fm-use-portal-current-row-style`, `kUsePortalCurrentRowStyle`) in the
portal's local style, component `self`, state `normal` (6.5) [O: bin;
7 corpus portals store 610 = 1 there and 5 store 620 = 0].

Corroboration [O]: FileMaker's Database Design Report stylesheet
(`en.lproj/DDR/Detail.xsl`) lists "Show vertical scroll bar" for bit 1,
"Allow deletion of portal records" for bit 4 and "Reset scroll bar when
exiting record" when bit 16 is clear. Counts are over the 161 portals of
the MBS 16.3 examples and harness fixtures and the 110 of the 66
templates (2026-11-11): the templates store 273 (`0x111`) and 257
(`0x101`), the corpus most often 273, 16, 25 and 17.

Deleting: FileMaker Server's script engine deleted the active row with
Delete Portal Row under option words 0, 1, 256, 257 and 273, none with
`0x4` (script-runtime.md 13.24), and the step's
`DataEntryModel::DeletePortalRowInternal` (0x5b3444) does not test the
bit: the option limits deleting from Browse, not the script step. How
FileMaker Pro tells the user that a row cannot be deleted (a disabled
command or an alert) has not been seen [U].

### 5.3.3 Buttons that perform a script or a single step

[O, 2026-11-13, the six English starter solutions
`DynamicTemplates/*/en.fmp12`, read-only through `om_layout`] The six
files hold 141 objects of type 10; 35 are text buttons (displayType 0, no
icon) whose step is Perform Script (step 1). Each is `LayoutObject`
fields 1 (type 10), 2 (key), 3 (flags), 4 (bounds), 5 (style: type
`button`, 80, a rotation only) and 16 (`buttonObj`):

| `buttonObj` | The 35 text Perform Script buttons |
|---|---|
| 1 flags | 0 |
| 2 textFlags | `0x2` on the 3 with a plain label, `0xa` on the 31 whose label is a merge field (`<<Name>>`), 0 on one without a label |
| 3 scriptStep | id 1; value 4 on 27 and 0 on 8 (unmapped, as on the Perform Script steps of scripts, script-steps.md); flags `0x40` (kScriptKey), with `0x4000` (kCalculation) on the 33 that have a parameter; reserved: the script's key as an `HBKey<5>`, `02 01 <id>` with the script's serial id (the bytes after it vary, script-steps.md 2.1) |
| 4 libraryKey | absent |
| 5 iconSize | 16 on 34, 12 on one |
| 6 displayType | 0 (absent) |

Object flags are anchors only: left and top (`0x30000000`) on 13, top and
right (`0x60000000`) on 12, three sides on 9, and one with another bit.
No object has a field the decoder does not know.

The attribute record `[4, 5, L, 05, O]` holds the label and the step's
data:

- `01` the label: a plain label is a plain SCSU value, as a text object's
  (5.5); a merge label is a subtree with its formatting;
- `11` (17) the step's data, the subtree a script step keeps at `[17, 5,
  S, 5, key]` (script-steps.md 4.2): the parameter is slot 0's
  calculation, `11, 00 00, 05` with `04` (context `01 00`, no occurrence,
  as step calculations store it), `05` (formatted) and `06` (Polish).
  The 33 buttons with a parameter have it; the 2 without have no `11`;
- `0d` the text format (`00 00` on the plain labels), `10` an empty name,
  `d8` and `d9` UUIDs, on every one;
- `07` the tab order on 16 (`00 00` on most, a position on the rest);
  the other 19 have none. `03` a tooltip on some.

Every one has an `0a` entry (10.5) whose value's second `u16` is 0.

[I] FileMaker Pro runs the step with the parameter from `11`: it is the
only place the 33 buttons store one. OpenMaker's Browse reads it there,
as the button test in `om-gui` shows. What value 4 and the `0a` value
select is [U].

OpenMaker's placement (`om_data::layout_placement::plan_button`,
authoring.md "Buttons on layouts") writes the plain-label shape: textFlags
`0x2`, iconSize 16, value 4, flags `0x40` or `0x4040`, anchored left and
top; the attribute record with `01`, `0d` `00 00`, `10`, `d8`, `d9` and,
with a parameter, `11`; no `07` (outside the tab order, as 19 starter
buttons are) and no `03`; the `0a` entry, the body part list entry and
`05, 00` as for a field (10.5). Whether FileMaker Pro opens, shows and
runs a button placed this way has not been checked [U] (issue [#70](https://github.com/OpenMaker-Dev/openmaker/issues/70)).

**Single steps.** [O, same files] Of the 141 type 10 objects, 57 have
step id 0 (no action), 42 Perform Script and 42 another single step: Go to
Layout (6) on 31 (value 5, flags `0x8` kMemberKey on 25 and `0xa` on 6,
the layout's key in `reserved`), Delete Portal Row (104) on 10 (flags 0)
and Close Window (121) on one (flags `0x80000000`). Each keeps its options
where a script step keeps them (script-steps.md 2, 4): the descriptor's
value, flags and `mReserved` on the object, its data subtree in the
attribute record's `11`. All 42 have `11`: Go to Layout's holds slot 6's
list value 0, the others' is empty. FileMaker Pro allows every step on a
button but 15 (the step catalog's `not_button_compatible`: If, Else If,
Else, End If, Loop, Exit Loop If, End Loop, Allow User Abort, Set Error
Capture, the comment, Set Layout Object Animation and the four transaction
steps) [O, from bin].

OpenMaker places a single step the Script Workspace can write
(`check_button_step`: a step of the catalog not in those 15, enabled, with
nothing stored its schema does not explain, its calculations decodable):
the same text button with the step's descriptor and its data in `11`,
an empty `11` when it has none, as the 42 have. Steps whose options are
kept in other places (step-level keys such as find requests or a sort
order) go with their data unchanged. Whether FileMaker Pro opens and runs
such a button has not been checked [U] (issue [#70](https://github.com/OpenMaker-Dev/openmaker/issues/70)).

#### 5.3.4 `buttonObj.flags` and the calculated icon (26.1.1)

[O, FileMaker Pro 26.1.1 FMEngine x86_64] `buttonObj.flags` is the
button's `u32` at `ButtonLayoutObject` + 0x188 (`ScriptStepButtonLayoutObject::
Serialize` 0x4790b0 writes it as field 1, `textFlags` at + 0x18c as field
2). Save as XML (`ObjectTransferLayouts::XMLSerialize_Object_Button_INTERNAL`
0xb246a) reads two of its bits:

| Bit | Meaning |
|---|---|
| `0x2` | the label is a calculation (`KLabelCalc`, 5.5) rather than stored text |
| `0x4` | "Calculated icon" (26.1.1): the icon is the SVG text a calculation returns, `KButtonIconSvgCalc` (`91`) in the object's attribute record `[4, 5, L, 5, O]`; without it the icon is the library image `libraryKey` |

`SetButtonIconSvgCalcText` (0x45858c) stores the formula with
`DBCalcFormula::Set` under the object key and `91`, with no context
occurrence (`KUnknown`), as the other object calculations of 5.5. The
functions are on `ButtonLayoutObject`, the class of buttons, button bar
segments and popover buttons; Claris documents the option in Button Setup
and Button Bar Setup. Layout mode shows a placeholder icon (FMEngine holds a
blank 24 x 24 SVG); Browse, Find and Preview show the calculated SVG, and a
warning icon when the text is not SVG FileMaker supports (Claris).
FileMaker Pro caches the rendered images under keys made by
`FMFileUser::GenerateRuntimeLibraryKey` (`runtime_calc_` names, FileMaker
Pro strings); nothing shows that they are stored in the file [I].
OpenMaker reads the bit (`ButtonObject::calculated_icon`, also on popover
buttons) and the formula (`ObjectCalcs::button_icon`), keeping every other
byte of the object and its record as it is.

### 5.4 Field control styles (`FieldLayoutObject.displayType`)

From `FieldLayoutObject::GetDisplayType` @0x3d22de, `GetTypeName` @0x3d5a66,
and `ObjectStyle::GetObjectType` [O]. UI names are resource table 129 strings.

| displayType | Control (UI name) | Style type | Conditions |
|---|---|---|---|
| 0 | Edit Box | `edit_box` (61) | becomes `text_area` (68) when a field flag (bit 8, `0x100`, the vertical scroll bar [O: `FieldStyle` answers `Scrolling` for exactly these objects]) is set on a single-repetition field; `container` (67) when the field's type is container |
| 1 | Drop-down List | `drop_down` (62) | only when the control shows its icon (`HasIcon`), otherwise styled as edit box |
| 2 | Pop-up Menu | `pop_up` (63) | |
| 3 | Checkbox Set | `checkbox_set` (64) | |
| 4 | Radio Button Set | `radio_set` (65) | |
| 5 | (falls back to Edit Box) | `edit_box` | [U] legacy value |
| 6 | Drop-down Calendar | `calendar` (66) | only with icon, otherwise edit box |
| 7 | Concealed Edit Box | `edit_box` / `secure_text` (94) | also implied by a field flag (`0x20` at object byte 0x1a2) |

A field whose style sets `-fm-texty-field` (property 617) is styled as `text_box`
[O]. Value lists come from `valueListKey`; their contents are database data,
not layout data. OpenMaker reads `valueListKey` as a serial catalog key, like
a field spec's field key (file-metadata.md 1) [I: no value list id in the
corpus reaches 128, where the two readings differ].

Checkbox and radio button sets [I] draw one item per value-list item, in
`ValueListItems` order (file-metadata.md 6.4): a marker, then the item's
label. A checkbox set's value holds one checked item per line; an item is
checked when it equals a line of the value, and a radio set's value is its
one checked item. Markers take the `option_item` (7) and `icon` (4)
components (6.3); a checked item adds their `checked` state (7, 6.4) to the
normal one. [O, retained styles] Checked icon components can override their
own border independently of the mark ink (6.4). [I] OpenMaker spaces rows
by at least their shaped label line height, including saved leading; it
centers the marker in that line box. [O, retained theme CSS] Checkbox text
components declare `left` (property 120) and `margin-left` (108), including
em-relative and absolute lengths. [I] OpenMaker positions each item's label
by their sum when an explicit left and supported margin are available;
em uses the label shaper's effective font size. Column widths and hit
regions include the positioned label. Missing, percentage or automatic
geometry, and a combined label position before its item origin, retain
the inferred fallback gap so a label cannot enter the previous column's
click region. Marker size, glyph and the
owner comparison's overlapping outlines remain unmeasured. How FileMaker shows a line that is not an item of the list is
[U]; OpenMaker draws only the list's items. It does not read the icon
component's `-fm-icon` glyph (6.5) yet: a checked box shows a cross. A list
set to "Show values only from second field" shows each item's second-field
value and stores its first-field value [Claris: FileMaker Pro Help,
"Setting up a field control"]; without that option an item shows its
stored value (how FileMaker labels a set whose list also displays the
second field is [U]).

Editing a set in Browse (OpenMaker's `document/options.rs`), as the help
pages "Entering data into checkbox sets and radio button sets" (FileMaker
Pro 13) and "Setting up a field control" describe it: clicking a checkbox
selects or clears its value, clicking a radio button selects its value,
Shift-clicking a selected radio button clears it; tabbing into the field,
an arrow key or the first letter of an item picks an item and Space enters
it. A checkbox that is turned on adds its value at the end of the field and
one turned off removes that value, so the field keeps its values in the
order they were checked and keeps lines that are no item [Claris knowledge
base, "How do fields formatted with checkboxes and radio buttons work in
FileMaker Pro"]. A radio button replaces the value; Shift-click removes the
item's line only, so a value outside the list stays, as the help warns
[I]. Shift-clicking a radio button that is not selected selects it [U].
What a set's "Other" and "Edit" items look like and do is unmeasured
(their flags are `0x1` and `0x8`, as for a pop-up menu below). Not measured
against FileMaker Pro: no file OpenMaker edited this way has been opened in
it.

Pop-up menus (display type 2) in Browse [Claris: FileMaker Pro Help,
"Setting up a field control"; native baseline #178, FileMaker Pro 26.0.3,
one local file: a stored text field, one record, a custom list of three
items, "Allow entry of other values", "Allow editing of value list" and
"Include arrow" off; I and U where marked]:

- **Observed (Claris help).** A pop-up menu takes no typing.
- **Observed (native baseline).**
  - The closed menu shows the stored value, also a value that is no item
    of the list, after closing and reopening the file too.
  - A click opens a menu of exactly the list's items, in list order. The
    stored value is not added as an item, and with those options off
    there are no "Other..." or "Edit..." items.
  - Choosing an item shows it; the record commits it and it stays after
    reopening. The menu checks the current item.
  - Escape with the menu open closes it without a change.
  - With "Include arrow" off, no arrow.
- **Second field.** A list set to show values only from the second field
  shows each item's second-field value and stores its first-field value
  [Claris help, as for sets above; not observed for a pop-up].
- **Observed: "Other..." and "Edit..."** (FileMaker Pro 26.0.3 on copies
  of the English starters, #869: Contacts' Title and Content Management's
  Type, a menu offering Other... and one offering both; the flags `0x1`
  and `0x8` in the field object flags table below).
  - Other... is the menu's last item, after the list's items and its
    stored dividers; Edit... follows it directly, with no divider between
    them. A divider before Other... is the list's own last `-` line.
  - Other... opens a dialog titled "Other…": one multi-line text box and
    no message, Cancel on the left and OK, the default, on the right. It
    is empty at first. OK enters the text as the field's value: the menu
    shows it, the text is not added to the list, and the reopened menu
    checks Other.... Reopening Other... shows the value, selected. Cancel
    changes nothing.
  - Edit... commits the record first, an uncommitted new record included,
    then opens a dialog titled "Edit Value List “Type”" (the list's name
    in curly quotes): one multi-line editor with an item per line and `-`
    for a divider, no message, Cancel on the left and OK, the default, on
    the right. Cancel leaves the list as it was; the commit stays. After
    OK the menu shows the new item after the list's preceding divider,
    then a divider, then Other... and Edit...; the field keeps its value.
    A `-` line is a divider, never an item to choose. What the file
    stores after that edit: file-metadata.md 6.5.
- **Inferred.**
  - Choosing an item replaces the whole value with the item's stored
    value, as a radio button does.
  - A value `-` alone in custom values is a divider (file-metadata.md 6.1),
    drawn as a line and not offered.
  - Whether the closed menu draws its arrow follows `HasIcon` as om-layout
    reads it (`FieldObject::has_icon`: flag `0x80000` hides a pop-up's
    arrow, read from the engine's code). The flag's value in the baseline
    file was not read.
- **Unknown.**
  - The keyboard beyond Escape (arrows, Enter, typing to select).
    OpenMaker opens the menu with a click only.
  - How a list that also displays its second field labels its items.
  - What "Other..." and "Edit..." do in Find mode and in a checkbox or
    radio button set. A drop-down list with the Edit flag `0x8` (Assets'
    Type, a list of a field's values) offered no Edit... in FileMaker Pro
    26.0.3, empty or holding values (#869).
  - The order of script triggers when an item is chosen. OpenMaker runs a
    typed edit's order, the same as for sets: the left field's exit
    triggers before the menu opens, OnObjectModify after the choice,
    OnObjectValidate and OnObjectSave when the field is left, and
    OnRecordCommit when the record is committed.
  - FileMaker Server's part, and drop-down calendars.

Drop-down lists (display type 1) in Browse [native partial case #178,
FileMaker Pro 26.0.3, one local file: an own-table stored text field, a
custom list of three items, "Include arrow to show and hide list" on,
"Allow editing of value list" and "Auto-complete using value list" off;
I and U where marked]:

- **Observed (native).**
  - The control's inspector offers those three options, and no "allow
    entry of other values"; they read back the same after saving and
    reopening.
  - A click in the text enters the field for typing. A value that is no
    item can be typed: committed by a click outside the field, it stays,
    also after closing and reopening the file, with no validation
    refusal.
- **Observed (native, "Auto-complete using value list", flag `0x800`;
  FileMaker Pro 26.0.3, a local Contacts starter copy, its Contact
  Details > Company with the committed value `Acme`; #870's readings).**
  - Typing `A` into the empty field shows `Acme` with `cme` selected, and
    no list; then `c` shows `Acme` with `me` selected.
  - Typing a lowercase `a` shows `acme` with `cme` selected: the typed
    letter keeps its case. After Tab and commit the stored value is
    `Acme`, the item's own text.
  - Tab and Return accept the completion and move to the next field
    (Website); so does the tool's keypad Enter. Escape leaves the text
    and selection and the field entered. Backspace removes the selected
    `me`, leaving `Ac`. Down clears the selection and leaves `Acme`.
    `Ax` stays as typed.
  - The same completion in Find mode (the empty Company criterion) and in
    Table view (turned on for that layout in Layout Setup).
  - Not measured: which item wins when several begin with the typed text,
    a physical keypad's Enter, and whether a list shows for a person (the
    tooling exposed none).
- **Observed (native, field-backed case [#148](https://github.com/OpenMaker-Dev/openmaker/issues/148)).** Clicking the arrow opened
  the list. With two objects of the same field and automatic record saving
  off, a new record's uncommitted value appeared in both fields but not in
  the list; after saving, it appeared in the list. Pop-up, checkbox and
  radio controls made the same non-related omission. A separate related
  pop-up used the current record's draft relationship key and draft value
  when that record was also a related source (file-metadata.md 6.4). Escape
  closed the drop-down list without choosing an item.
- **Earlier custom-list case #178.** The desktop tooling exposed no list
  after a click in the text or on the arrow; its accessibility Expand
  action reported expanded and exposed no items. This does not establish
  how that custom-list control behaved.
- **Inferred.**
  - Choosing an item replaces the whole value, typed text included, with
    the item's stored value, as in a pop-up menu.
  - The arrow follows `HasIcon` (`FieldObject::has_icon`: flag `0x400`
    for display type 1, read from the engine's code; the `drop_down`
    style type applies only with it, table above). The flag's value in
    the native file was not read.
- **Unknown.**
  - Whether the list opens when the field is entered (by a click in the
    text or by Tab), and how a list without the arrow opens. OpenMaker
    opens the list only from the arrow, and offers none without it.
  - Choosing an item, and the list's position, width and timing. OpenMaker
    lays the list out as a pop-up's menu; Escape closes it and leaves the
    field entered with its text. The field's focus after native Escape was
    not checked.
  - The keyboard in the list, and typing while it is open (OpenMaker:
    the text changes, the list stays open).
  - The flag of "Allow editing of value list". OpenMaker offers no
    "Edit..." item.
  - How the field and the list label a list that shows its second field
    (OpenMaker: the field shows the stored value, the list the items'
    labels).
  - The order of script triggers when an item is chosen (OpenMaker: a
    typed edit's order, as for pop-up menus).

Field entry behavior [O: FMEngine 26.0.3 arm64, static disassembly only;
Claris: FileMaker Pro Help, "Allowing or preventing entry into fields"].
`FieldLayoutObject::GetFieldEntryMode(bool)` reads one of two bit pairs of
`FieldLayoutObject.flags` (the object's `flags` member at `+0x1a0`): with
`true`, `0x4` and `0x100_0000`; with `false`, `0x10` and `0x200_0000`.
`DataEntryModel::CanEnterObject`, `ActiveObjectBase::LoadField` and
`EnterObject` pass `true` when the window model's mode (`+0x40`) is 0 and
`false` when it is 1, the numbering of `Get ( WindowMode )` (Browse 0, Find
1); `HasFieldEntryMode` uses no portal row in mode 1 [I: Browse is the
`true` pair]. Each pair holds a mode: neither bit 0, the high bit alone 1,
the low bit alone 2, both 3 (`SetFieldEntryMode`'s tables at `0x866a48`
and `0x866a54` hold the bits of modes 1, 2 and 3). `HasFieldEntryMode`
compares a mode 3 object's field entry calculation with 1 for mode 0 and
2 for mode 1, the help's "Set by Calculation" values (1 Edit, 2 Select
Only, any other View Only); `CanEnterObject` refuses mode 2. So 0 is Edit,
1 Select Only, 2 View Only and 3 Set by Calculation; the help's note that
clients before 26.0 take Select Only as Edit and a calculation as View
Only matches the low bit being the old "prevent entry" bit [I]. The shipped
templates' fields set none of these bits. OpenMaker changes a checkbox or
radio button set only in Edit; it does not evaluate the calculation.

Repetitions [O: `FieldRepetitions`, 2026-10-29]: `numOfReps` is the number
of repetitions shown, stored negative when they run across (`-9` is `9
horizontal`, `-1` is `1 horizontal`). A portal's field objects are stored
relative to the portal; FileMaker places them in the first row inside the
portal's left and top borders where the portal's style draws them
(`FieldBounds`: `+0, +1` for the templates' portals, whose left and right
borders are `none`; `+1, +1` for the FMS sample's, all four solid).

#### 5.4.1 `FieldLayoutObject.flags` bits

Claris documents the bits of a field object's flags in the Database
Design Report grammar [Claris: FileMaker Pro 13 Advanced, "Database Design
Report XML Output Grammar", `<FieldObj flags>`]; a published measurement
of FileMaker Pro 26.0.2's Save as XML (one option changed at a time, the
`<Options>` of the layout object's `Field` element read back;
andykear/FileMaker-XML-bit-flags, `data/layout-object-flags.json`) [O,
third party] gives the same bits, plus the ones added since. The bits
agree with what this spec found on its own (the entry modes above, the
vertical scroll bar of 5.4's table) and with the six English starter
solutions' 179 field objects [O]: `0x40` and `0x80` are set together
(123) or not at all; `0x1`, `0x8` and `0x400` occur only on drop-down
lists, pop-up menus and calendars; `0x8000` is set on exactly two
objects, Assignees::Photo (a container) on the Tasks template's Assignee
Chooser and Add Assignee layouts, which are also the only two without
`0x20`.

| Bit | Meaning | Source |
|---|---|---|
| `0x1` | value list: add "Other..." (allow entry of other values) | DDR, 26.0.2 |
| `0x2` | select entire contents on entry | DDR, 26.0.2 |
| `0x4`, `0x1000000` | Browse mode field entry (above) | DDR (`0x4` "do not allow entry"), 26.0.2 |
| `0x8` | value list: add "Edit..." | DDR, 26.0.2 |
| `0x10`, `0x2000000` | Find mode field entry (above) | DDR (`0x10`), 26.0.2 |
| `0x20`, `0x40`, `0x80` | go to next object using Tab, Return, Enter | DDR, 26.0.2 |
| `0x100` | vertical scroll bar | DDR, 26.0.2, 5.4 |
| `0x200` | horizontal scroll bar | DDR only |
| `0x400` | include the drop-down or calendar icon | DDR, 26.0.2 |
| `0x800` | auto-complete | DDR, 26.0.2 |
| `0x2000` | visual spell checking off | DDR, 26.0.2 |
| `0x8000` | **not** included for Quick Find (the Inspector's "Include field for Quick Find" cleared; query-program.md 6.6) | DDR ("Disable for quick find"), 26.0.2 |
| `0x10000` | placeholder text **not** shown in Find mode | 26.0.2 only |
| `0x100000` | data formatting **not** overridden by the value list | 26.0.2 only |
| `0x400000` | "Display single checkbox without value" (26.1.1): one checkbox without a label; checked stores the value list's first value, cleared its second, or clears the field when the list has one value (Claris) | 26.1.1 bin |
| `0x800000` | "Show indicator for clipped contents" (26.1.1): three dots at the bottom of an edit box in Browse mode when its text does not fit (Claris) | 26.1.1 bin |

`0x20000` (with placeholder text, above 5.5) and `0x80000` (on every
value-list control and calendar of the templates) are in neither source
[U]. OpenMaker reads `0x8000` (`FieldObject::quick_find`), `0x40` and
`0x80` (`FieldObject::next_object_on_return`, `next_object_on_enter`,
which Return and the keypad's Enter follow in Browse), and `0x400000`
(`FieldObject::single_unlabeled_checkbox`); the other bits of the table
it reads only where this section's other paragraphs say.

[O, FileMaker Pro 26.1.1 x86_64] The Inspector writes the two 26.1.1 bits
into this word: `PropFieldFormatter::ApplyFormatImpl` (0x101b35f90 and
0x101b3630a) reads it with `FieldLayoutObject::GetFieldFlags`, sets or
clears `0x800000` for the property `FieldClippedIndicator` and `0x400000`
for `SingleUnlabeledCheckBox`, and writes it back with `SetFieldFlags`.
Neither is set for a container field (data type 6, schema-path.md); it
sets `0x800000` only on an edit box (display type 0), and `0x400000` only
when two more tests of the object pass (unnamed virtual calls; Claris: the
option needs a value list).
`FieldLayoutObject::Serialize` (FMEngine 0x3eb416) stores the word as
`fieldObj.flags`. Browse draws the dots with `FieldClippedIndicatorView`
(`FieldTextView::ShouldShowClippedIndicator` 0x101d086da), in a color
chosen for contrast with the field's background (`GetFieldClippedIndicatorColor`).
FileMaker Go 26.1.1 shows the indicator too (Claris release notes,
FileMaker Go 26.1.1).

### 5.5 Per-object data stored outside the protobuf

[O] These are separate records addressed from the object key. Function names
and the HBAM key constant each uses (DBEngine exports `Draco::K*`, identified
through FMEngine's GOT bindings and Ghidra cross-references):

| Attribute | Key constant | Reader / writer |
|---|---|---|
| Hide object when (calc) | `KHideCondition` | `LayoutObject::GetHideConditionCalc` @0x395dc0; applies only with flags `0x4`, "Apply in Find mode" is flags `0x100` (below) |
| Tooltip (calc) | gated by flags bit `0x4000`; formula looked up from the object key | `LayoutObject::GetTooltipCalc` @0x39619c |
| Conditional formatting | `KCFConditions`, `KFormat` | `LayoutObject::GetCFConditions` @0x397444; each condition is a `CFCondition {flags, style, predefinedType}` protobuf (`CFCondition::Load`, see [protobuf-schemas.md](protobuf-schemas.md)) carrying the style applied when its formula is true; the formula itself is stored as a calculation [I] |
| Extended attributes incl. data formatting (number/date/time/graphic) | `KFormat` | `LayoutObject::GetBaseExtendedAttrs`; `TNumericStyle`, `TDateStyle`, `TTimeStyle`; layout and the graphic format in [data-format.md](data-format.md) |
| Script triggers | `KScriptTriggers` | `LayoutObject::LoadScriptTriggerCache` @0x397d5a; trigger list in [catalogs/script-triggers.csv](catalogs/script-triggers.csv) |
| Tab order | `KTabOrder` | `TabOrderMgr`, `LayoutObject::LoadTabOrderList` |
| Placeholder text (calc) | `KPlaceholderText` | `FieldLayoutObject::GetPlaceholderTextCalc` |
| Field entry calc | `KFieldEntryCalc` | `FieldLayoutObject::GetFieldEntryCalc` |
| Button label, popover title, tab panel label (calcs) | `KLabelCalc` | `ButtonLayoutObject::GetLabelCalc`, `PopoverLayoutObject::GetTitleCalc`, `TabLayoutObject::Panel::GetPanelCalc` |
| Active button bar segment (calc) | `KActiveObjectCalc` | `SegmentedBarLayoutObject::GetActiveSegmentCalc` |
| Calculated button icon (SVG calc, 26.1.1) | `KButtonIconSvgCalc` (`91`) | `ButtonLayoutObject::GetButtonIconSvgCalc` @0x458326, `SetButtonIconSvgCalcText` @0x45858c (26.1.1 FMEngine); applies with `buttonObj.flags` `0x4` (5.3.4) |
| Text content, merge fields, layout calculations | `KLayoutCalcs`, `KTempTextLayoutCalc` | `LayoutObject::LayCalc_StoreTextAsDBCalc`, `GetLayoutCalculationResult` |
| Web viewer and chart settings | `KExternalOptions` | `ExternalLayoutObject::GetExternalProperties` @0x3d961e |

[O] The Tasks template's label texts ("Due Date", "Status", ...) are absent as
plain strings and present only XOR-0x5A encoded, consistent with text being
stored as styled text in calculation storage rather than in `textObj` [I].
[O, 2026-10-29] The object attribute records `4/5/<layout>/5/<object>` are
keyed by the object's `key` as a serial key (`80 09` for object 137, as
every catalog member, file-metadata.md 1), not by its big-endian bytes.
An object's name (Position inspector; `LayoutObjectNames`,
`GetLayoutObjectAttribute`) is `.../10`, empty for an unnamed object, and
its UUID (`GetLayoutObjectAttribute ( ... ; "UUID" )`) is `.../d8`, 16
bytes shown as `8-4-4-4-12` upper-case hex [O: FileMaker Server's answers
over the Contacts and Tasks templates, calc-functions.md 5.8].

[O, 2026-10-28] The Contacts object attribute subtree identifies two distinct
calculation keys: `4/5/<layout>/5/<object>/3` is the tooltip, while sibling key
`6` is placeholder text. A single literal can be read from calculation child
`6` (cached form) or child `5`. Company field 10 and Website field 8 each have
an attribute-3 label and common object flag `0x4000`, but no attribute 6. Title
40, Job Title 37, Photo 41 and First Name 39 have both calculations. Treating
attribute 3 as placeholder incorrectly repeats Company and Website inside
otherwise blank fields. `om-layout` now exposes these separately as `tooltip`
and `placeholder`.

[I, paired file comparison] Field flags differ by `0x20000` between Website 8
(`0x1000e0`) and First Name 39 (`0x1200e0`), and between Company 10 (`0x80ce0`)
and Job Title 37 (`0xa0ce0`). The objects with the extra bit have placeholder
attribute 6. This strongly suggests a field-level placeholder flag, but the
reader's bit test has not been independently decompiled; rendering currently
uses the actual placeholder calculation's presence rather than assigning an
unverified public flag meaning.

#### 5.5.1 Active button bar segment

Button Bar Setup offers None, one segment, or "Specify", a formula that
returns the object name of the segment (Claris, [Defining or changing a
button bar](https://help.claris.com/en/pro-help/content/defining-changing-button-bar.html)).
The active segment shows the Active display state (style state 7,
`checked`, 6.4) in Browse and Find modes.

[O] How the bar stores the choice, from every button bar of the 66
FileMaker Pro templates and the MBS 16.3 examples (the only corpus file
with `KActiveObjectCalc`):

| Choice | `segmentedBarObj` | `142` | Seen in |
|---|---|---|---|
| None | `flags 0`, no `activeSegmentKey` | absent | Tasks, Contacts, Assets secondary bars; MBS Phidget Voltage Input object 69 |
| A segment | `flags 0`, `activeSegmentKey` = that segment's object key | absent | Tasks en: the same navigation bar on two layouts stores keys 2 and 3, its own layout's segment on each; Assets en: 2, 3 and 4 |
| Specify | `flags 2`, no `activeSegmentKey` | the formula | MBS Phidget Voltage Input, layout 1, object 62: the number literal `0` (`10 02 0001 0010 00000000 ...`), the documented way to make no segment active; its six segments are unnamed and carry `KLabelCalc` labels |

OpenMaker (`om_data::host::layout_objects::segment_choice`,
`named_segment`) takes flag `0x2` as "Specify": the formula's result, as
text, is looked up as `GetLayoutObjectAttribute` looks up names (any case,
depth first) and is active when that object is one of the bar's own
segments; empty text, `0` or the name of any other object makes none
active. Whether FileMaker matches the name in any case is inferred from
the other object-name functions [I]. What the corpus does not settle is
refused rather than guessed: a formula that does not decode, flag `0x2`
without a formula, a formula on a bar without the flag, or a key that is
not one of the bar's segments.

#### 5.5.2 Hide object when and "Apply in Find mode"

The option is bit `0x100` of `LayoutObject.flags` (`kEvaluateHideConditionInFindMode`,
5.2.1); the condition itself is on only with bit `0x4` (`kHasHideCondition`)
[O, from bin: FileMaker Pro 26.0.3 FMEngine x86_64 and FileMaker Server's
`libFMEngine.so`]:

- `LayoutObject::HasHideCondition` (`libFMEngine.so` 0xf64940, vtable slot
  `0x100`) tests `mFlags & 0x4`; `HasEvaluateHideConditionInFindMode`
  (0xf64950, slot `0x108`) tests `mFlags & 0x100`, and
  `SetEvaluateHideConditionInFindMode` (0xf64960) sets or clears exactly
  that bit.
- FileMaker's own writers name the bit: `XMLOStreamLayout::SaveHideCondition`
  (FMEngine 0x411e0) and `ObjectTransferLayouts::XMLSerialize_Object_HideCondition`
  (0xaa604) write slot `0x108`'s answer as `HideCondition`'s `findMode`
  attribute, and the Database Design Report stylesheet
  (`en.lproj/DDR/Detail.xsl`) prints "Apply in Find mode" under an
  object's hide condition when its `flags` have bit 256.
- `Draco::EvaluateHideCondition` (FMEngine 0x5d4972), called from
  `LayoutObject::IsObjectVisibleBasedOnHideCondition` (0x398d64), reads
  the window's `FMWindowModel::mUserMode` (+0x40; DWARF `UserMode`: 0
  Browse, 1 Find, 2 Layout, 3 Preview, 4 Print). It evaluates the formula
  only when the object has `0x4` and the mode is not Layout, and in Find
  mode only when the object also has `0x100`. Otherwise, and when the
  object has `0x4` but no formula, the object shows: in Find mode an
  object without `0x100` shows whatever its condition says.
- `IsObjectVisibleBasedOnHideCondition` then evaluates each enclosing
  object's condition the same way, each under its own flags, and hides
  the object when one of them holds; above a portal the parents are
  evaluated without the portal row's record.
- A window-level switch (`FMWindowModel` vtable slot `0x1b0`, with a file
  option) makes `EvaluateHideCondition` hide objects other than fields,
  portals, groups and tab and slide controls and panels without
  evaluating, outside Preview and Print; what sets it is not decoded [U].

Files FileMaker wrote agree [O, 2026-11-11, 703 files: the MBS 16.3
examples and the harness fixtures; the 66 templates have no hide
condition]: 268 objects in 41 files have `KHideCondition` (`141`), all 268
with `0x4`; two of them (a field and a button) also have `0x100`. Four
objects have `0x4` without the formula.

[O, FileMaker Pro 26.0.3, local disposable Contacts copies, [#35](https://github.com/OpenMaker-Dev/openmaker/issues/35)] On a
text object with `Hide object when = 1`, Pro hid the object in Browse
with "Apply in Find Mode" checked or unchecked. In Find, it hid the
object when the option was checked and showed it when unchecked;
neighboring text objects remained visible. The copies had no records;
other formulas and object kinds were not tested [U]. This check agrees
with the mode test above.

[O, FileMaker Pro 26.0.3, local disposable Contacts copies, [#38](https://github.com/OpenMaker-Dev/openmaker/issues/38)] A
standalone text object with `Hide object when = 1/0` disappeared in
Browse. Data Viewer evaluated `1/0` to `?`. With "Apply in Find Mode"
off, the object was visible in Find. A separate object's condition
`Get ( FoundCount ) = 1` was false and the object visible with three
records; a Find that left one of three records hid it immediately, and
Show All Records made it visible again without Refresh Object.

[O, same local check] An object with condition `$$om184 = 1` was visible
while the global was unset. A one-step script set the global to `1`
without Refresh Object. The object remained visible immediately after
the script ran, both when launched from Script Workspace and on a fresh
copy when launched from the Scripts menu. Data Viewer later confirmed
the global's value was `1`. After that visit, the object was hidden.
On fresh copies, clicks on the window title and an empty layout header,
and switching a separate tab panel away and back, left the standalone
text visible. Switching to another layout and back, or Browse to Find
and back, hid it [O, [#38](https://github.com/OpenMaker-Dev/openmaker/issues/38)].
Clicking a field with no records raised an alert; the text was hidden
after the alert was dismissed, without isolating the click from the
alert as the trigger [O]. Which action in the earlier Data Viewer visit
caused its re-evaluation is also unknown [U].
These cases do not establish a general cache invalidation rule for every
condition or window event [U].

### 5.6 Property table by object kind

What OpenMaker must read and honor per object (O unless noted):

| Property | Applies to | Source |
|---|---|---|
| Bounds, anchoring, z-order, rotation | all | `LayoutObject` common fields |
| Style (local + named + theme) | all | `ObjectStyle` + cascade (6.7) |
| Conditional formatting | most objects [I] | `KCFConditions` |
| Hide object when | all | `KHideCondition` |
| Tooltip | all | flags `0x4000` + calc |
| Script triggers | fields, buttons, tab/slide controls, portals, web viewers, other active objects | `KScriptTriggers` |
| Button action | button, button bar segment, grouped button; popover button opens its popover | `scriptStep`; its data, the parameter included, in the attribute record's `11` (5.3.3) |
| Icon | button, segment, popover button | `libraryKey`, `iconSize`, `displayType` (Button Setup's label type, 5.6.1); default glyphs 9.7 |
| Placeholder text | fields | `KPlaceholderText`, drawn in `placeholder` state |
| Data formatting | fields, merge text | `KFormat` extended attributes |
| Control style, value list, repetitions, scroll bar | fields | `fieldObj` |
| Portal rows, initial row, scroll bar, alternate and active row styles | portals | `portalObj` and its options (5.3.2); components row, row_alt, row_active; whether rows use them is style properties 610 and 620 (5.3.2) |
| Default and current panel, tab widths and justification | tab controls | `tabObj`, `tabPanelObj` |
| Popover position and title | popovers | `popoverObj.position`, `KLabelCalc` |
| Active segment | button bars | `activeSegmentKey`, `KActiveObjectCalc` gated by `segmentedBarObj.flags` `0x2` (5.5.1) |
| URL, interaction, status and progress bar | web viewers | `externalObj.flags`, address formula at attribute `00 00` (9.6) |
| Chart definition | charts | `KExternalOptions` [I] |
| Image | graphics | `libraryKey` into the file library |

#### 5.6.1 Button label type (`displayType`)

Button Setup offers six label types, in this order: Label text, Image only,
then four icon-and-text positions, "Icon at top, text at bottom", "Text at
top, icon at bottom", "Icon at left, text at right" and "Text at left, icon
at right" [Claris: FileMaker Pro Help, "Defining or changing a button"].
Numbered in that order from 0, they give the table below. The counts are
over every layout of the six English starter solutions [O]:

| `displayType` | Label type | Starter buttons |
|---|---|---|
| 0 | Label text | 67, all with `libraryKey` 0: 66 labelled, and Assignee Chooser's unlabelled row-wide button |
| 1 | Image only | 64 with an icon: 62 without a label, and 2 with a stored label that does not show, Task Details' "Add Assignee" and Assignee Chooser's "Add " |
| 2 | Icon at top, text at bottom | none |
| 3 | Text at top, icon at bottom | none |
| 4 | Icon at left, text at right | 10, all labelled and left-aligned: the 9 Details layouts' back buttons (a chevron left of "Assets", "Contacts" and so on, in the top navigation part) and Add Assignee's "Add Assignee" |
| 5 | Text at left, icon at right | none |

Claris's documentation lists the choices in order. The starter
measurements cover selectors 1 and 4:
- **1 is Image only [O].** FileMaker Pro 26.0.3's PDF of Task Details ([#201](https://github.com/OpenMaker-Dev/openmaker/issues/201),
  2026-11-13) draws the "Add Assignee" button, type 1 with that label stored,
  as its 13-point "+" icon centered in the 291-point frame (icon at 523-536,
  frame 384-675). None of the page's 11 text runs is "Add Assignee". The
  stored label is kept but not shown.
- **4 is Icon at left, text at right [O].** Pro's Preview and PDF of the Add
  Assignee layout show the "+" left of "Add Assignee" ([#201](https://github.com/OpenMaker-Dev/openmaker/issues/201), 14:27Z).

A local FileMaker Pro 26.1.1 Browse check with a 100 × 90 pt frame,
a 48 pt icon and a short label confirmed all four placement selectors
and that an image-only button hides its label [O]. The vertical
arrangements center the icon and label together, with the label across
the frame's width. The gap between the icon and the text line box is
about 8 pt [I from visible glyph bounds]. The left and right arrangements
wrap the label beside the icon. A narrower 70 pt frame and longer label
clip the text while the icon keeps its size. A popover button with the
icon above matched the ordinary button.
[Native Browse check](https://github.com/OpenMaker-Dev/openmaker/issues/1016#issuecomment-7185360328).

OpenMaker's Browse and Layout mode share these placement directions for
buttons and popover buttons. Browse supplies evaluated calculated icons.
Layout mode supplies stored library icons. An image-only button keeps its
stored label as its accessible name in Browse. Reports support types 1
and 4 (portable-pdf.md, Buttons and groups).

### 5.7 Anchoring (autosizing)

[O] Anchor bits in `LayoutObject.flags`: `0x10000000` left, `0x20000000` top,
`0x40000000` right, `0x80000000` bottom. Default is left+top (`0x30000000`, the
most common value in the templates). Observed combinations: `0x70000000`,
`0xf0000000`, `0x60000000`, `0x90000000`, `0xb0000000`.

[O, FileMaker Pro 26.1.1 on macOS] Autosizing edits saved on four copies of
one starter layout changed these bits in field 3 (`flags`) of the target
`LayoutObject` in the layout member's `07` object store. A no-edit
open/close copy matched the baseline byte for byte. Each edit was saved
and checked by reopening the file:

| Individually selected target | Stored anchor bits before | Stored anchor bits after |
|---|---:|---:|
| Ungrouped edit box | `0xd0000000` (Left, Right, Bottom) | `0x70000000` (Left, Top, Right) |
| Ungrouped text | `0x30000000` (Left, Top) | `0xc0000000` (Right, Bottom) |
| Text inside a Left+Top group | `0x30000000` | `0x70000000` |
| Pop-up field inside an all-edges portal | `0x30000000` | `0x70000000` |

Right was enabled for both nested targets, and the enclosing objects kept
their own anchor bits. Comparing raw messages by object key found no other
field changes. In each save FileMaker moved the `0x00400000` selection bit
(5.2.1) from the previously selected object to the edited top-level object
or, for the nested cases, to its group or portal. It also serialized the
same object definitions in reverse order. Root keys, child keys and all
other bytes within each object definition stayed as stored, apart from the
flag changes. Within `[4, 5, L]`, only `07` and stamps `fc` and `fc03`
changed. File history and modification counts elsewhere also changed.
[Native run and method](https://github.com/OpenMaker-Dev/openmaker/issues/888#issuecomment-7214357808).

**Unknowns:** Whether the definition order reverses on every subsequent
save, how the Inspector handles multiple selected objects or all four
anchors unchecked, and effective Browse resizing of the edited nested
objects.

[O] `LayoutObject::GetConstrainedAutoresizingFlags` @0x3939a4 and
`Draco::GetConstrainedFlagsForObject` @0x2ce93d: effective anchors are
constrained by ancestors. Walking up from the object, if an enclosing container
is not stretchable on an axis (does not have both anchors on that axis), the
child's anchors on that axis are forced to the leading edge (left or top). A
further rule forces right-only anchoring for some objects positioned beyond
`LayoutDef.baseWidth` that are not in groups, panels, or popovers
[O, exact intent U].

Resize behavior [I, matches public FileMaker documentation]: with both left and
right anchors the object stretches with the window width; with only right it
moves; vertical anchors work the same way and affect the body height in form
view.

### 5.8 Printing and sliding

`Draco::SlideMgr` with `HorzSlideRecord` / `VertSlideRecord`, view methods
`ApplySlideMgrAdjustments` / `InformSlideMgrOfShrinkability`, and resource
strings "Object slides left/up when printing", "Object hidden when printing"
[O].

The Inspector's Sliding & Visibility options are bits of `LayoutObject.flags`
[O, FileMaker Pro 26.0.3, arm64 slices, 2026-11-02]:

| Inspector property (`PropBase::k...`) | Bit |
|---|---|
| `kSlideLeft` | `0x20` set when checked |
| `kSlideUp` | `0x10` set when checked |
| `kSlideUpCheckAllAbove` ("All objects above") | `0x8000` **cleared** when checked, set for "Only objects directly above" |
| `kSlideShrinkEnclosing` ("Also resize enclosing part") | `0x40` set when checked |
| `kNonPrinting` ("Hide when printing") | `0x200` set when checked |

Evidence:

- **The Inspector writes the bits.** `PropSlidingFormatter::ApplyFormatImpl`
  (`FileMaker Pro`, 0x10194a8b0) compares the property key with each constant
  above, then ORs or clears exactly that mask in the word at
  `LayoutObject + 0x58`.
- **That word is the stored flags.** `LayoutObject::Deserialize` (FMEngine
  0x38885c) stores protobuf field 3 `flags` (has-bit 18, message offset
  0xb8) there.
- **Which bits slide.** `SlideMgr::ApplySliding` (FMEngine 0x6c2bd4) tests
  bit 5 (`0x20`) before `ApplyHorzSliding` and bit 4 (`0x10`) before
  `ApplyVertSliding`. `SlideMgr::ShrinkUpdate` (0x6c1700) uses the
  horizontal shrink only with `0x20` and the vertical shrink only with
  `0x10`.
- **The modifiers depend on Slide up.** `SlideMgr::SlideRecord::SlideRecord`
  (0x6c3158) builds its own record bits from the flags: `0x1` for `0x20`;
  for `0x10`, `0x2`, plus `0x4` with `0x40` and `0x8` unless `0x8000`. So
  the resize and based-on options are read only with Slide up.
- **The resize option accumulates.** `SlideDeltaCalculator::GetSlideDV`
  (0x6c349c) adds to a separate running total the shrink of items carrying
  the `0x4` (resize) bit.
- **Unknown tolerances [U].** `GetSlideDV` compares edges with ±4-point
  margins whose role is not decoded.

Buttons and groups [I]: no flag but Hide when printing (`0x200`) keeps any
object from printing, so a button prints its appearance and a group its
members. A group's members are positioned relative to the group's top-left
corner, as portal and button bar children are (section 3; `om_layout`'s
`LayoutObject::bounds` records it observed for groups in the shipped files
too). How the slide manager treats a group and its members (whether they
slide as one) is [U].

Claris documents the behavior: fields shrink to their data, objects slide by
the shrink of the objects above (all of them, or only those directly above),
the part shrinks with "Also resize enclosing part", and headers and footers
never shrink ([Removing blank spaces in printouts](https://help.claris.com/en/pro-help/content/removing-blank-spaces.html)).
OpenMaker's bounded report model is in [report sliding](../report-sliding.md).

## 6. Style model

### 6.1 Data model

[O] `StyleSet` is a list of `ComponentStatePropSet {componentId, stateId,
propset}`; `PropertySet` is a list of `Property {propertyId, inherited, value}`
where the value is one of `intVal`, `stringVal`, `colorVal`, `lengthVal`,
`imageVal`, `boxShadowVal`, `borderImageInfoVal`, background lists, and so on.
`ObjectStyle {localStyles, customStyleName[], objectTypeId,
customStyleDisplayName[]}` is what every object, part, and layout box carries.
Only properties that differ are stored.

Decoded example (Tasks template, theme custom style `minimal_edit_box`,
objType 61) [O]: state normal sets background-image none, background-color
transparent white, four border colors `rgba(222,222,222,0)`, border-image-source
none; state hover sets the border colors opaque; state focus sets an opaque
white background. This is exactly `edit_box.minimal_edit_box:{normal,hover,focus}
.self` in `enlightened.css`, with `border-color` expanded to properties 20-23
and `rgba(222, ...)` stored as floats 0.870588.

### 6.2 Object types (style "elements")

[O] From `__GLOBAL__sub_I_ObjectType.cpp` @0x34fa1a. "Parent" is the next type
consulted in the cascade.

| ID | CSS element | Parent | ID | CSS element | Parent |
|---|---|---|---|---|---|
| 0 | unknown | - | 66 | calendar | field |
| 1 | layout_object | - | 67 | container | field |
| 10 | current_record | - | 68 | text_area | field (second base: edit_box) |
| 11 | active_record | - | 70 | text_box | layout_object |
| 20 | layout_background | - | 71 | label | text_box |
| 21 | layout_box | - | 72 | text_object | text_box |
| 30 | part | - | 78 | button_bar | layout_object |
| 31 | title_header | part | 79 | button_bar_segment | layout_object |
| 32 | header | part | 80 | button | layout_object |
| 33 | leading_grand_summary | part | 81 | tab_control | layout_object |
| 34 | body | part | 82 | tab_panel | layout_object |
| 35 | body_alt | body | 83 | portal | layout_object |
| 37 | trailing_grand_summary | part | 84 | graphics | layout_object |
| 38 | footer | part | 85 | chart | layout_object |
| 39 | title_footer | part | 86 | web_viewer | layout_object |
| 40 | top_nav_part | part | 87 | group | - |
| 41 | bottom_nav_part | part | 89 | popover | layout_object |
| 50 | line | shape | 90 | scrollbar | - |
| 51 | rectangle | shape | 91 | dot_control | layout_object |
| 52 | rounded | shape | 92 | dot_panel | layout_object |
| 53 | oval | shape | 93 | fm-list-view-filler | - |
| 54 | shape | layout_object | 94 | secure_text | field |
| 60 | field | layout_object | 100 | leading_sub_summary | part |
| 61 | edit_box | field | 101-116 | leading_sub_summary_1..16 | leading_sub_summary |
| 62 | drop_down | field | 200 | trailing_sub_summary | part |
| 63 | pop_up | field | 201-216 | trailing_sub_summary_1..16 | trailing_sub_summary |
| 64 | checkbox_set | field | | | |
| 65 | radio_set | field | | | |

### 6.3 Components (sub-elements)

[O] `__GLOBAL__sub_I_Component.cpp` @0x358f80; the name in parentheses is the
WebDirect (IWP) class where one exists.

| ID | Component | ID | Component |
|---|---|---|---|
| 1 | self | 17 | completed (progress bar completed) |
| 2 | inner_border | 18 | panel |
| 3 | text | 19 | panel_inner_border |
| 4 | icon | 20 | scrollbar_top_button |
| 5 | baseline | 21 | scrollbar_bottom_button |
| 6 | repeat_border | 22 | scrollbar_track |
| 7 | option_item | 23 | scrollbar_thumb |
| 8 | contents | 24 | tab_header (`v-tabsheet-tabcontainer`) |
| 9 | tab | 25 | unscrollable_contents |
| 10 | tab_inner_border | 26 | dot |
| 12 | row (`v-table-cell-content`) | 27 | dot_container |
| 13 | row_alt (`v-table-row-odd`) | 28 | row_active (`iwp-portal-selected-row`) |
| 15 | status_bar | 29 | button_bar_divider |
| 16 | progress_bar | | |

### 6.4 States

[O] `__GLOBAL__sub_I_State.cpp` @0x35d64a (IDs) and
`FMStyle::State::Transition::GetState` @0x35d31a (runtime bits). IDs 1-10 are
inferred from declaration order anchored by the explicit IDs 11-19 and by
decoded data (normal 1, hover 2, focus 4) [O/I].

| ID | State | WebDirect name | Runtime bit(s) | Inspector label (res 129) |
|---|---|---|---|---|
| 1 | normal | | none | Normal |
| 2 | hover | | 0x1 | Hover |
| 3 | pressed | active | 0x2 | Pressed |
| 4 | focus | | 0x4 | In Focus |
| 5 | enabled | | 0x8 | |
| 6 | disabled | | 0x10 | |
| 7 | checked | active | 0x20 | Active |
| 8 | indeterminate | | 0x40 | |
| 9 | focushover | focus:hover | 0x5 | |
| 10 | checkedhover | active:hover | 0x21 | |
| 11 | checkedpressed | active | 0x22 | |
| 12 | indeterminatehover | | 0x41 | |
| 13 | indeterminatepressed | | 0x42 | |
| 14 | loading | | 0x80 | |
| 15 | droptarget | | 0x100 | |
| 16 | checkedfocus | | 0x24 | |
| 17 | pressedfocus | | 0x6 | |
| 18 | placeholder | | 0x200 | Placeholder Text |
| 19 | placeholderhover | placeholder:hover | (0x201 resolves to hover) | |

Runtime resolution for combined bits [O, approximate reading of the decision
tree; ties U]: checked+focus, pressed+focus, checked+pressed, checked+hover,
checked, then hover+focus, focus, hover, pressed, droptarget, placeholder,
normal.

[O, [#231](https://github.com/OpenMaker-Dev/openmaker/issues/231)] The six English starter solutions' themes (FileMaker Pro
26.0.3), as OpenMaker resolves them:
- **Field styles.** Edit boxes, pop-up menus, drop-down lists, calendars,
  containers and checkbox sets give In Focus (4) a different border color
  than Normal: (0, 0.439, 0.812) on all four sides, where Normal is gray
  (0.784, 0.780, 0.800).
- **Border styles stay Normal's.** A plain edit box draws only its bottom
  border.
- **Other styles.** "fill-container" also changes a transparent background
  to opaque white. The "fill-inverted-*" styles change the border to white
  and its styles. A checkbox set changes its icon component's (4) border.

How FileMaker Pro draws these is unmeasured.

Theme CSS uses the pseudo-classes `normal`, `hover`, `pressed`, `focus`,
`placeholder`, `checked`, `active`, `activefocus`, `checkedfocus`, `disabled`,
`droptarget` [O]. `active` is used for selected tabs, button bar segments, and
slide dots, and `activefocus` for focused selected tabs; these map to checked
and checkedfocus [I, based on the WebDirect alias of checked being `active`].
`body:checked` is the current-row style used when
`-fm-use-body-current-row-style: true` [O].

### 6.5 Properties

[O] `__GLOBAL__sub_I_Property.cpp` @0x3456ea (IDs and value classes);
theme-CSS names from the FMEngine CSS keyword table (string region around
FMEngine 0x87b715) and the theme files. Value kinds are the `Property` value
field used [O for decoded ones, I otherwise].

| ID | CSS name | Value | ID | CSS name | Value |
|---|---|---|---|---|---|
| 1 | background-image | image list (none, gradient, url) | 101-104 | padding-top/right/bottom/left | length |
| 2 | background-position | position list | 105-108 | margin-top/right/bottom/left (on a field's text component, they inset its text: the starter lists' row fields set 0.875 / 0.25 / 0.625 / 0.25 em, and FileMaker prints their text lower than at the field's top [O, [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201)]; that the margins account for all of it is [I]) | length |
| 3 | background-size | size list | 109, 110 | width, height | length |
| 4 | background-repeat | repeat list | 111 | float | enum |
| 5, 6 | background-origin, background-clip | box list | 112 | clear | enum |
| 7 | background-color | color | 113, 114 | overflow-x, overflow-y | enum |
| 20-23 | border-top/right/bottom/left-color | color | 115 | overflow-style | enum |
| 24-27 | border-*-style | enum (none, hidden, dotted, dashed, solid, double, groove, ridge, inset, outset; decoded solid = 5, none = 1 [I]) | 116 | visibility | enum |
| 28-31 | border-*-width | length | 117-120 | top, right, bottom, left | length |
| 32-35 | border-top-right / bottom-right / bottom-left / top-left-radius | length (pt or %) | 121 | position | enum (static, relative, absolute, fixed) |
| 36 | border-image-source | image | 122 | box-shadow | box-shadow list |
| 37 | border-image-slice | border image info | 123 | box-sizing | enum |
| 38, 39 | border-image-width, border-image-outset | border image info | 124 | vertical-align | enum |
| 40 | border-image-repeat | repeat info | 500 | -fm-digit-set | enum |
| 60-63 | outline-width/style/color/offset | | 501, 502 | -fm-space-before, -fm-space-after | length |
| 64 | font-family | string (`-fm-font-family(...)` or family list) | 503 | -fm-tab-stops | paragraph tab list |
| 65 | font-weight | enum (normal, bold, 500, 600, 700, 900) | 504 | -fm-strikethrough | bool |
| 66 | font-stretch | enum (CSS keywords) | 505 | -fm-underline | enum (incl. word-underline, double-underline) |
| 67 | font-style | enum (normal, italic, oblique) | 506 | -fm-glyph-variant | enum (superscript, subscript, small caps [I]) |
| 68 | font-variant | enum | 507, 508 | -fm-paragraph-margin-left/right | length |
| 69 | font-size | length; default from theme `kBaseFontSize` | 509 | -fm-character-direction | enum |
| 70 | color | color | 510, 511 | -fm-tab-top-left/right-radius | length |
| 71 | direction | enum (ltr, rtl) | 512 | -fm-use-default-appearance | bool |
| 72 | line-height | length (unit `line`, e.g. `1line`) | 513 | -fm-icon | enum: up-arrow, down-arrow, calendar, classic-check, modern-check, checkmark, radio |
| 73 | block-progression | enum (tb, lr, rl) | 514 | -fm-icon-color | color |
| 74 | text-align | enum (left, center, right, justify) | 515 | -fm-tab-spacing | int |
| 75 | text-transform | enum (capitalize, uppercase, lowercase, none) | 516 | -fm-override-with-classic | bool |
| 76 | text-indent | length | 517 | -fm-table-background-color | color |
| 100 | display | enum (block, none, ...) | 518 | -fm-baseline-shift | length |
| 602 | -fm-fill-effect | int | 519 | -fm-icon-gradient | gradient |
| 609 | -fm-highlight-color | color | 615 | -fm-borders-baseline | bool |
| 610 | -fm-portal-alt-background | bool | 616 | -fm-body-alt-background | bool |
| 611 | -fm-text-vertical-align | enum (top, center, bottom) | 617 | -fm-texty-field | bool |
| 612 | -fm-tategaki | bool (vertical Japanese text) | 618 | -fm-box-shadow-persist | box-shadow |
| 613 | -fm-rotation | int (moved into object rotation) | 619, 620 | -fm-use-body/portal-current-row-style | bool |
| 614 | -fm-borders-between-reps | bool | 621 | -fm-icon-padding | length |
| | | | 625-627 | -fm-grid-width, -fm-grid-height, -fm-grid-content | |

**Text margins, insets and a placed field's height.** An edit box's text
component (61, component 3) can space its text with margins (105-108) or
with insets (117-120, with position 121 absolute).

- [O] Apex Blue, the theme of Pro's blank files, uses only margins: 0.4 /
  0.5 / 0.3 / 0.5 em, insets 0 / 0 / 0 / 0 pt, position absolute,
  box-sizing 1 (`593-create-new/Alpha Name.fmp12`).
- [O] When Pro places a field (a field added in Manage Database), it makes
  the edit box as high as the top and bottom border widths, the top and
  bottom margins, and one line of the text. The source is FileMaker Pro
  26.0.3, #593, 10:18Z and 12:06Z, on OpenMaker-made files with 12 pt Arial
  and 1 pt borders:
  - margins of 0, 2, 3, 4 and 5 pt top and bottom gave boxes of 16, 20,
    22, 24 and 26 pt;
  - on Pro's blank, 16 pt Helvetica Neue gave 31 pt.
- [O] Pro does not count the insets. With insets of 2 pt top and bottom
  (OpenMaker Plain before #853), every one of those boxes cut the bottoms
  of `Agjpqy`, in Form view and in the table row.
- [O] With insets 0 and margins of 2 / 4 / 2 / 4 pt, Pro placed the box
  20 pt high and drew `Agjpqy` whole in Form view. It also drew whole in a
  24 pt box that OpenMaker placed (#853, 12:52Z,
  `593-create-new/made/field-height-3/`).
- [I] So the text is drawn inside the insets as well as the margins, and
  the insets took 4 pt from its line.
- [O] The table row is set elsewhere (10.14). Pro's Table view still cut
  the text in its own 17 pt row, and kept it whole once the edit box's font
  was 16 pt.

Corner radii are four properties, one per corner (32 top right, 33 bottom
right, 34 bottom left, 35 top left), each a Length whose unit may be
relative (`%`, `em`, `line`) [O]. A corner may instead store `coordVal`
(`Property` field 13, `Coord {x, y}` of two Lengths in `Style.proto`), a
horizontal and a vertical radius, as CSS's `border-*-radius: x y` [O: the
six English starter solutions of FileMaker Pro 26.0.3 store their objects'
corners this way, `0pt 0pt` for square ones and `22pt 22pt` on a button of
Meetings' Meeting Details; reading equal x and y as one circular radius is
inferred]. [I] A rounded frame prints as it
displays: the fill takes the rounded shape, the borders follow the
corners, and a radius larger than half the frame's smaller side is
reduced to it, as in CSS. FileMaker Pro's printed or PDF output of a
rounded frame has not been compared.

#### Numeric style values checked against file-contained themes

[O, 2026-10-28] The Contacts template's embedded Universal Touch and Apex Blue
`ThemeStyles` were decoded with `om-layout` and compared with the installed
`universal_touch.css` and `apex_blue.css` rules for the same type, class,
component and state. `cargo run -p om-layout --example styles -- <file> 3`
prints that inventory; supplying object ids prints resolved component styles.
The CSS is comparison evidence only; OpenMaker renders the theme stored in the
file. No CSS or font asset is copied into the implementation.

| Property | Observed numeric values | Remaining inference / unknown |
|---|---|---|
| border style (24-27) | 1 none; 5 solid | Other keyword codes not checked here |
| font style (67) | 1 normal | 2 italic, 3 oblique follow keyword order but lack a checked CSS/protobuf pair |
| font weight (65) | 2 bold (`bold-inverted-text`, `emphasis-primary-text`, Apex `giant_text_bordered`) | 1 normal inferred; numeric-weight keyword codes not checked |
| text align (74) | 1 left; 2 center; 3 right (`left_aligned_*`, `center_aligned_*`, `right_aligned_*`) | 4 justify inferred |
| text transform (75) | 1 none (`fill-low-shadow-inverted-edit:placeholder`); 3 uppercase (`uppercase-secondary-text`, `separator-uppercase-highlight-buttonbar`) | 2 capitalize, 4 lowercase inferred; no such rule in the inspected installed theme CSS |
| text vertical align (611) | 0 top (`field`, `small_valigned_top_editbox`); 1 center (`button`, `small_center_aligned_text`); 2 bottom (`small_bottom_aligned_text`, `medium_valigned_btmright_editbox`) | These are zero-based, unlike text-align |

[O, FileMaker Pro 26.1.1] Browse draws no line on a Text object's edge
when its resolved border style is unset, even with a set width and
color. A Text object with only its top edge enabled retained red and
1-point width on all four sides. Only the top had a resolved style
(solid, 5), and only the top drew. Removing that last style left all
edges invisible and the Inspector showed None, with the width and color
still present. A control with all four styles set to solid drew all four
edges. Other object kinds and modes were not checked.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1091#issuecomment-7191245927)

[O] Where a field's text sits when its style stores no vertical align (611).
None of the 156 field objects of the six English starters resolves 611 from
the object, the layout or the theme, components 1 and 3. Their themes carry it
only for button bars and buttons (types 79, 80: 1, center). FileMaker Pro
26.0.3 draws an edit box's text at the top of its text box and a pop-up menu's
centered: Tasks' Task Details "Task" field, however tall its anchors make it,
its 64 pt attachment row field, and Meeting Details' Notes at the top
(walkthrough [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201), rank 2; 2x screenshots of the reopen and delete fixtures),
and Category, Priority and Status centered. The text box is the frame inset by
the border widths and the text component's margins (105-108; "Where a field's
text sits", 6.5). Those three pop-ups draw their text 13 pt inside the box's
left edge (box at 32 pt, text at 45 pt): their 12 pt left margin and the 1 pt
border they draw, as Preview insets a pop-up by the borders it draws (6.5).
Browse has not been seen with a pop-up whose left border is not drawn [U]. 114 of the starters' 138 edit boxes, drop-down lists and
calendars resolve margins:
- most resolve 0.875 / 0.25 / 0.625 / 0.25 em;
- the rest resolve 14 / 12 / 10 / 12 pt, or a 12 pt left margin;
- at 16 pt type the top margin is 14 pt, and with the 1 pt border that is the
  inset measured on those screenshots.

The application's own theme copies, compared:
- FileMaker Pro 26.0.3's Universal Touch, the starters' theme 1
  (`FileMaker Pro.app/Contents/Resources/Themes/universal_touch/universal_touch.css`),
  gives `edit_box` text these em margins and no vertical align.
- Its Enlightened (`Themes/enlightened/enlightened.css`) gives 0.5 / 0.5 / 0.25
  / 0.17 em.
- The Meetings starter's layouts use Enlightened (theme 2), yet store the
  Universal Touch values.

That Pro draws from the file's stored values, not the application's copy of
the theme, is [I]. A capture of Meetings' Notes with its Inspector would tell.
Drop-down lists and calendars at the top is inferred: no capture shows one with
a value.

[O, 2026-11-18] On copies of the Contacts starter using Universal Touch,
FileMaker Pro 26.0.3's Insert > Field made a 44 pt edit box. Both a field
placed in OpenMaker and a Pro-created field resized to 22 pt drew blank
when inactive in Browse. Each showed its value during entry, and the
saved value was visible on another layout and survived reopening. The
OpenMaker-placed field showed the value while inactive once resized to
44 pt. [I] The theme's combined 24 pt vertical padding leaves too little
room for text in the shorter frame. [U] Pro's default height for a Fields
sidebar drop and the dimensions used by other themes were not measured.
See [#315](https://github.com/OpenMaker-Dev/openmaker/issues/315).

[O] Compiled property 64 can carry `intVal`, rather than `stringVal`: it then
indexes the file font list at `25/5/<id>/16`. Contacts index 0 is
`arial,sans-serif`; index 2 is `Helvetica Neue`. The integer 0 is a valid font
reference, not an absent value. A hosted renderer needs catalog 25 along with
layout catalog 4 and theme catalog 6 to recover these names.

[O] Contacts field object 39 demonstrates why the border style must be applied
independently from width and color. Its normal self component has four 1 pt
widths and four opaque gray colors, but styles `[none, none, solid, none]`
(top/right/bottom/left). Only the bottom rule is painted. Its color and 16 pt
font are on `self` (component 1); its `text` component (3) contains geometry
only. Text painting therefore starts with the self text properties and applies
explicit text-component overrides.

[O] Contacts tab control 4 supplies the caption defaults (16 pt, gray) on self.
Its panel 5 has no explicit text properties. Panel self background is pale gray
in normal state 1 and white in checked state 7; its tab component 9 has a
transparent background in both states. The selected caption uses the enclosing
tab control's defaults and any panel text overrides. Contacts object 11 and
button 2 have uppercase transform 3, while object 49 has no transform and keeps
`First Name` in title case.

[O] An empty container uses placeholder state 18 for its caption color:
Contacts photo object 41 resolves normal color to black and placeholder color
to gray (0.701961 in each channel). Its layout has no explicit caption font
size or vertical-alignment property, so centering the empty-container caption
is a control behavior rather than an inferred normal text alignment.

Classic-theme-only properties absent from the engine table:
`-fm-text-baseline-width/-pattern/-color`, `-fm-fill-pattern`,
`-fm-line-pattern`, `-fm-alt-background-color`, `-fm-alt-fill-pattern` [O];
their handling [U].

**Where text sits.** FileMaker Pro 26.0.3's PDFs clip each piece of text to
its text box and draw it from the box's top left. The PDFs read are the
walkthrough's Task Details, Assignee Details, Content Details and Content List
([#201](https://github.com/OpenMaker-Dev/openmaker/issues/201)), and the 60-record Task List (#263), all from 2026-11-13.

- **Edit-box fields.** [O] The Task Details Task field's text is clipped to
  (55, 152)-(336, 170). The field's frame is (50, 137)-(341, 181). Its theme
  style stores border widths (28-31) of 1 point on all four sides, but a border
  style (24-27) only on the bottom, and text margins (105-108) of
  14 / 4 / 10 / 4 points.
  - So the text box is the frame inset 1 point on every side, then by the
    margins.
  - The inset counts each side's stored width, styled or not. It applies
    though the layout's "Delineate fields on current record only" keeps the
    border from printing.
  - Task List's Task field is inset the same way, on all 60 rows.
  - That this holds for other widths, styles and conditional borders is [I].
- **Text objects.** [O] All 26 labels and headings compared are 13-point and
  left-aligned, on those five layouts. Each is clipped to its frame less 1
  point on every side, though the objects store no border width, padding or
  margin. Example: Task Details' "TASK" label, frame (50, 904)-(309, 925) in
  Pro's flipped page space, has its text box at (51, 905)-(308, 924). That
  a label's overflowing text is cut at this clip, as a field's is, is [I]:
  no overflowing label was printed.
- **Button labels.** [O] Three header buttons were measured: Task Details'
  `<<Task>>`, and Assignee Details' `<<First Name>>` and `<<Last Name>>`. Each is
  13-point and top-aligned (611 = 0), with no border width stored.
  - Each label is clipped to its frame less 2 points at the top and 1 at the
    left, and none at the right or bottom. Example: frame (50, 845)-(337, 864),
    text box (51, 847)-(337, 864).
  - Whether a centered or bottom-aligned label, or one beside an icon, takes
    the same inset is [U].
- **First baseline.** [O] It sits a whole number of points below the text
  box's top: 12 for 13-point Arial and 14 for 16-point. Arial's ascent is
  1854/2048 em, 11.77 and 14.48 points at those sizes. That is every one of
  the 231 text runs in those PDFs, which use only those two sizes.
  - Rounding the ascent to the nearest point fits both sizes. That it holds
    at other sizes and in other fonts is [I].
  - It agrees with Browse's whole-point line pitch (13.1).
  - The pitch between a printed text's lines is [U].
- **Fields with an arrow or calendar icon.** [O] Eight fields were read, on
  Task List, Task Details, Content Details and Content List. Each shows its
  icon (5.4's `HasIcon`), and its icon component (4) stores a width (109) of
  30 points. Pro prints no icon, but the text box ends that width short of the
  frame's right edge:

  | Field | Display type | Border styles (T, R, B, L) | Text box inset (T / R / B / L) |
  |---|---|---|---|
  | Task List Status; Content Details and Content List Status and Type | pop-up menu (2) | none, none, solid, none | 14 / 34 / 11 / 4 |
  | Task Details Priority | pop-up menu (2) | solid on all four | 14 / 35 / 11 / 13 |
  | Content Details Location | drop-down list (1) | none, none, solid, none | 15 / 35 / 11 / 5 |
  | Task Details Due Date | drop-down calendar (6) | none, none, solid, none | 15 / 35 / 11 / 5 |

  All of them store 1-point border widths on all four sides. Their margins
  are 14 / 4 / 10 / 4 points, or 13 / 4 / 10 / 12 on Task Details' Priority.
  Task Details' Status and Category share Priority's style but printed no
  value, so Pro drew no text box for them.
  - A drop-down list or calendar is inset like an edit box, by every side's
    width.
  - A pop-up menu is inset only on the sides whose border style draws a
    line. On Task List's Status that is the bottom alone, so its text sits 1
    point further left and higher than an edit box's.
  - Both are then inset by the icon's width at the right.
  - Why pop-up menus differ is [U]. Other icon widths and units, icons when
    the text runs right to left, and fields without the icon (styled as edit
    boxes, 5.4) are [I].

### 6.6 Value types

| Type | Encoding | Type |
|---|---|---|
| Color | four floats 0..1 (r, g, b, a). CSS accepts `#rgb`, `#rrggbb`, `rgba(r, g, b, a)` with 0-255 or percentages, `transparent` | O |
| Length | `{value (double), unit, autoValue}`; unit enum: 0 none, 1 `%`, 2 `cm`, 3 `in`, 4 `mm`, 5 `pica`, 6 `pt`, 7 `em`, 8 `px`, 9 `line` | O (`FMStyle::Length::ParseUnit` @0x403d40) |
| em | resolved against font size (`FMStyle::Length::ConvertEmToPt`) | O |
| Image | `{type, gradient, libraryKey}`; type 0 = none (decoded); url images reference the file library | O/I |
| Gradient | `{type, startPos, endPos, startRadius, endRadius, colorStops[]}`; theme CSS uses `-webkit-gradient(linear, left top, left bottom, from(c1), to(c2))` | O |
| Box shadow | `{type, inset, color, horzOffset, vertOffset, blurDistance, spreadDistance}`; CSS `0pt 0pt 2pt 1pt rgba(...)`, `inset` | O |
| Border image | slice and width lists plus `fill`, repeat `stretch`/`repeat`; source is a themed PNG | O |
| Tab stops | `ParagraphTabInfo {position, type, fillChar, alignChar}` | O |

### 6.7 Cascade (how local overrides combine with theme styles)

[O] `Draco::ObjectStyle::GetValueBase` @0x324a9e,
`GetValueWithStateFallbackForObjectType` @0x3246ec,
`GetValueForNonNormalState` @0x324a0a, `GetValueFromThemeAndLayout` @0x3245d6,
`TraverseInCascadingOrder` @0x3229c4, `GetDefaultValue` @0x324cde.

For a request (component C, state S, property P) on an object:

1. For the object's style type T, then each parent of T (6.2), run the
   examiner below. Stop at the first value found.
2. Examiner order for one type:
   1. the object's own local style (`ObjectStyle.localStyles`);
   2. each custom style name on the object, last applied first: the
      layout-level definition (`LayoutDef.localStyles` / `KLayoutStyleDefs`,
      unless that layout marks the name deleted) and the theme definition, each
      looked up first for (name, T) and then for (name, any type);
   3. the type style for T in the layout-level styles, then in the theme.

   Theme and layout-level lookups are skipped when `-fm-override-with-classic`
   is set [O, `GetValueFromThemeAndLayout`].
3. If nothing was found and S is not `normal`, retry with the fallback chain
   for S: simple states fall back to `normal`; `focushover` to focus, hover,
   normal; `pressedfocus` to pressed, focus, normal; `checkedhover` to checked,
   hover, normal; `checkedpressed` to checked, pressed, normal; `checkedfocus`
   to checked, focus, normal; `indeterminatehover`/`indeterminatepressed` to
   indeterminate, then hover or pressed, then normal; `placeholderhover` to
   hover, normal.
4. Special case: `-fm-icon-color` on the `icon` component defaults to a border
   color: `inner_border`'s top border color when `-fm-fill-effect` is 2 (or 1
   on fields), otherwise `self`'s.
5. Otherwise the built-in default for P; `font-size` defaults to the theme's
   `kBaseFontSize` metadata.
6. A value flagged `inherited` on the `self` component resolves to the default.

Conditional formatting styles (`CFCondition.style`) override the object's
cascaded values while their formula is true [I, mechanism names
`ApplyConditionalFormatting`, `RuntimeCFCondition`].

Results for the normal state are cached per object (`BaseStyle::gCacheEnabled`).

## 7. Themes

### 7.1 Inventory

[O] 55 theme packages, byte-identical between the Windows and Mac installs
except for CRLF line endings (Windows `FileMaker Pro/Themes/`, Mac
`Contents/Resources/Themes/`). 1910 files: 390 CSS, 1185 manifest XML (11
locales), 331 PNG, 4 JPG.

| Platform | Count | Themes (directory names) |
|---|---|---|
| Desktop | 41 | Classic, bamboo, beach, black, blue, canyon, cool, cool_brown, cool_gray, cosmos, dark, desert, electric, enlightened, forest, green, ice, light, luminous, meadow, minimalist, ocean, onyx, pine, purple, quarry, red, retro, river, shell, silver, sky, sophisticated, stone, tranquil, vibrant, warm, warm_blue, warm_bronze, warm_green, wave |
| Touch | 13 | apex_blue, cool_gray_touch, enlightened_touch, luminous_touch, minimalist_touch, ocean_touch, onyx_touch, river_touch, sophisticated_touch, tranquil_touch, universal_touch, vibrant_touch, wave_touch |
| Print | 1 | enlightened_print |

All are version 6 except `apex_blue` and `universal_touch` (version 1, no
partials). IDs are `com.filemaker.theme.<dir>`. Built-in theme ID constants
also exist in FMEngine (`kFMLAYOUT_CLASSIC_THEME_ID`, `kFMLAYOUT_COOL_THEME_ID`,
`kFMLAYOUT_WARM_THEME_ID`, `kFMLAYOUT_TOUCH_THEME_ID`, `kFMLAYOUT_PRINT_THEME_ID`,
`kFMLayoutDeprecatedThemes`, ...).

### 7.2 Package format

[O] Each theme directory contains:

- `manifest.xml` plus `manifest_<locale>.xml` (br, de, es, fr, it, ja, ko, nl,
  sc, se): theme identity and metadata; locale files translate display names.
- `<name>.css`: the style sheet.
- `<name>_ja.css`, `<name>_ko.css`, `<name>_sc.css`: small overlays that
  replace `font-family` (and a few text margins) for Japanese, Korean, and
  Simplified Chinese.
- `images/`: `<name>Preview.png` and, for older themes, background textures and
  border-image PNGs referenced by `url(images/...)`.
- `partials/6/`: a CSS and manifest delta that upgrades a file copy of an older
  version of the theme to version 6 (`FMTheme::LoadPartials`,
  `FMTheme::kPartialFolderName`, `KBaseThemeVersion`).

Manifest root elements: `id`, `name`, `group`, `version`, `cssfile`,
`platform` (Desktop, Touch, Print), `preview`, `metadata`.

### 7.3 CSS dialect

Grammar [O]: FMEngine embeds a flex/bison CSS2-style parser (token names
`IMPORT_SYM`, `IMPORTANT_SYM`, `IDENT`, `HASH`, `CLASS`, `PSEUDOCLASS`,
`FUNCTION`; rules `ruleset`, `selectors`, `simple_selector`, `declaration`,
`prio`, `term`, `hexcolor`). Comments `/* */` are allowed. No at-rules are used.

Selector shape [O]: `element[.namedstyle][:state] .component`, for example
`edit_box.minimal_edit_box:hover .self`. Variants seen:

- `header text_box .self`: a text object inside a given part (part-scoped text
  styles, 168 uses);
- Classic and partials use older forms such as `field`, `edit_box:focus`,
  `field .baseline` (component omitted means self [I]);
- comma groups such as `portal .row, portal .row_alt` (Classic only).

Element vocabulary: the CSS names in 6.2. Component vocabulary: 6.3. State
vocabulary: 6.4. Named style classes: 274 distinct names across all themes,
for example `header_text`, `minimal_edit_box`, `accent_button_1`,
`body_button_bar`, `fill-inverted-edit`.

Property vocabulary [O, counts across all theme CSS]: `background-color`
(4322), `color` (4242), `border-color` (3325, shorthand), `font-family` (2940),
`box-sizing` (2516), `background-image` (2458), `-fm-icon-color` (2271),
`border-style`, `border-width`, `box-shadow`, `border-radius`, per-side border
longhands, `border-image-*`, `font-size`, `font-weight`, `font-style`,
`text-align`, `line-height`, `position`/`top`/`right`/`bottom`/`left`/
`width`/`height` (component geometry inside the object box), `padding-*`,
`margin-*`, `-fm-icon`, `-fm-icon-padding`, `-fm-text-vertical-align`,
`-fm-baseline-shift`, `-fm-table-background-color`, `-fm-tab-*`,
`-fm-use-*-current-row-style`, `-fm-portal-alt-background`,
`-fm-body-alt-background`, `-fm-fill-effect`, `-fm-box-shadow-persist`,
`-fm-use-default-appearance`, `background-position/-repeat/-size`,
`text-transform`, `display`.

Value syntax [O]: lengths in `pt` (18118 uses), `%`, `em`, and the unit `line`
(`line-height: 1line`, `1.2line`); colors `#hex`, `rgba()` with 0-255 or
percentages, `transparent`; one malformed bare `b2b2b2` in Classic.
`font-family` is either a CSS list (`arial, sans-serif`) or
`-fm-font-family(<family>, <codeset or PostScript name>)`, for example
`-fm-font-family(Helvetica Neue, HelveticaNeue-Light)` or a CJK family with
`shiftjis`. Codesets: roman, greek, cyrillic, centraleurope, shiftjis,
traditionalchinese, simplifiedchinese, oem, native, other, korean, dingbats,
turkish.

Component geometry: themes position components inside the object with
absolute coordinates, for example a text area's `scrollbar_track` at
`right: 0pt; width: 30pt`, a popover's `contents` at `top: 1.84em; bottom: 8pt`,
and slide dots with `border-radius: 50%; width: 0.66em` [O]. OpenMaker must lay
out components from these values, not from native control metrics [I].

### 7.4 Manifest metadata vocabulary

[O] `metadata` children become `ThemeMetadataSet {name, metadata: NameValuePair[]}`
in the file (decoded: set `charting` with `colorScheme = Muted Rainbow`, set
`colorpalette` with `swatch1 = #efeff4`, ...):

- `layoutbuilder`: `kBaseFontSize`, `kReportBreakFieldIndent`,
  `kBackgroundPadding{Top,Bottom,Left,Right}`, `kFirstPartPaddingTop`,
  `kLastPartPaddingBottom`, `kPartPadding{Left,Right}`,
  `k{Leading,Body,Trailing}PartPadding{Top,Bottom}`, `kVerticalFieldSpacing`,
  `k{TitleHeader,Header,Body,Footer,TitleFooter,TopNavPart,BottomNavPart,Summary}...MinSize`,
  `kInitialLabelStyle`.
- `charting/colorScheme`: Earth, Brown, Sea Glass, FileMaker Colors, Gray,
  Spring Desert, FileMaker Apps, Saturated, Muted Rainbow, Plum Tree, Primary
  Basics, Rainbow.
- `colorpalette/swatch1..swatch10`: theme color palette.
- `namedstyles/<class>`: display name for each named style class.

### 7.5 Themes inside a database file

[O] Catalog `KThemeDefs`; per theme `KStyleDefs` (`ThemeStyles`:
`typeStyleSets[] {objTypeIds[], styles}`, `customStyleSets[] {name, styles,
objTypeIds[]}`, `deletedCustomStyleSets[]`), `KMetadataDefs`
(`ThemeMetadata`), `KBaseThemeName`, `KBaseThemeVersion` (`FMTheme::LoadStylesCache`,
`LoadMetadataCache`, `GetBaseThemeName`, `GetBaseThemeVersion`). The Tasks
template carries copies of `com.filemaker.theme.universal_touch` and
`com.filemaker.theme.enlightened`.

**The catalog's keys** [O: the 66 starter files of FileMaker Pro 26,
2026-11-17]. The theme catalog is `[6]`:
- **Its index `[6, 1]`** is the common catalog index (schema-path.md 5.1):
  - the last issued id `00`;
  - a name list `01, <name key>` giving each theme's id;
  - the count `04`;
  - an entry `07, T` for each theme, empty;
  - the UUID `d8` and the mod info.

  In all 66 files the name list, the `07` entries and the members are the same set of themes, and `04` is their count. Contacts' index (all 11 languages) also has `18` = `01 02` and `20` = `01`. `18` names theme 2, which no Contacts layout uses. A blank file's index has `18` = `01 01`, its one theme, and `20` = `01` (file-format.md 8). That `18` is the theme a new layout gets is inferred [I].
- **A member `[6, 5, T]`:**
  - the display name `0a`, version `0b`, locale `0c`, group `0d`, platform `0f` and theme id `10` (`com.filemaker.theme.…`);
  - the resources `0e`: a preview image and other images, in chunks;
  - the styles `16` (`ThemeStyles`) and metadata `17` (`ThemeMetadata`);
  - the UUID `d8` and the stamp.

  Meetings' custom theme also has `18`, `19`, `d9` and `fc` [U].
- **Sizes and use:** the 110 members are 32-156 KB each, mostly `0e` and `16`. Every layout names a theme the catalog has. 22 of the 110 members are named by no layout: Contacts' theme 2 and Meetings' theme 1, in every language.
- **What a layout needs:** decoding a layout reads only its own theme's styles, `[6, 5, T, 16]` (`Layout::decode_from_tree`). The theme is named by the layout member's key `0b` (4.1).

## 8. Text and fonts

- **No fonts are shipped** in either install [O]. Themes reference system fonts:
  Helvetica Neue, Arial, Tahoma, Verdana, Palatino, Helvetica, plus CJK fonts in
  the locale overlays (Hiragino Kaku Gothic ProN, MS PGothic, MS PMincho, Gulim,
  Batang, AppleGothic, STXihei, STFangsong) [O].
- FMEngine contains a 335-entry family-name to PostScript-name map
  (for example `times new roman` to `TimesNewRomanPSMT`, Hiragino, Kozuka,
  Nanum) used for cross-platform matching [O, string region before 0x87b715].
- Each file has its own font catalog (`KFontList`, `DBFontCatalog`,
  `DBFontMapper`, `DBFontStyleTranslator`); styled text refers to fonts through
  it [O names, I semantics]. XML attributes `fontIdAttr`, `fontPSAttr`,
  `fontFamily`, `fontCodeSet`, `fontSize` [O].
- A font is a member `[19, 5, F]`, `F` a two-byte key [O: the 66 starter
  files, file-format.md 3.7]: `10` the family name (`Helvetica Neue`, or a
  CSS list such as `arial,sans-serif`); `03`, on faces, the face's
  PostScript name (`HelveticaNeue-Light`, `ArialMT`, `.SFNSText`); `02`
  (`KStatus`) is its code set as a one-byte key string, `01 00` or
  `01 fe` in the starter corpus; `d8` is its UUID and `fc` its stamp.
  [O from bin: FileMaker
  Pro 26.1.1 Mac arm64 DBEngine] `DBFontEntry::GetCodeSet` (`0x188d088`)
  and `SetCodeSet` (`0x188daf0`) read and write the code-set byte.
  `CreateNewPostScriptEntry` (`0x188cc00`) sets it to `fe` when creating
  an entry identified by its PostScript name. Other code-set values are
  unmeasured. A theme names a font the same two ways:
  `-fm-font-family(<family>, <code set or PostScript name>)` (7.3).
- Styled text model (`Draco::Text` in Support): Unicode text with character
  style runs (`CharacterStyle`: font, size, color, faces) and paragraph style
  runs (`ParagraphStyle`: alignment, indents, spacing, tabs); SCSU compression
  for storage (`SCSUCompressFromText`) [O names, I details]. Faces include bold,
  italic, underline (word, double), strikethrough, small caps, superscript,
  subscript, upper/lower/title case, condense/extend, highlight [I, from CSS
  emitter keywords and resource strings].
- Mac text engine: FileMaker's own line layout (`XTLineBreaker`, `XTextLine`,
  `XTextUnit`, `XTextWidthCache`, `XGraphicsTextLayout`) on CoreText
  (`XCoreTextLayout`, `XCoreTextStyle`, `CTFramesetter*`, `CTFrame*`) [O].
- Windows: Uniscribe (`USP10.dll` `ScriptItemize`, `ScriptShape`) in
  `FileMaker Pro.exe` and `ClientUI.dll`; Skia with `SkFontMgr_New_DirectWrite`
  in `ViewSystem.dll` for some text (`drawSimpleText`, `measureText`) [O].
- Vertical Japanese text (`-fm-tategaki`, `block-progression`, `writing-mode`)
  and right-to-left text (`direction`, `unicode-bidi`) are part of the model [O].
- ICU 76 is bundled (Windows `icu*76.dll`, imported by Support) [O].

[O, FileMaker Pro 26.1.1] At Actual Size, a 104 × 22 pt Text object
using Helvetica Neue 16 pt displays the complete label `Top edge only`.
A 102 × 22 pt control with the same style displays `All four edges`.
Both fit on one line. The point sizes and family come from the same
closed synthetic layout.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1104)

### 8.1 Stored character-style masks

[O, FileMaker Pro 26.0.3 Mac Support] `CharacterStyle::Read` (0x1f7fdc)
reads a length-prefixed payload: mask `u16`, file-local font ID `u16`, faces
`u16`, point size `u16`, RGBA `u32`, and, when present, an extension byte.
The fixed-width slots remain present even when their mask bits are clear.
`CharacterStyle::MergeDefault` (0x1f71bc) supplies unmasked attributes from
the default style; those slots must not become explicit text overrides.

| Mask | Explicit attribute / face bits |
|---|---|
| `0x0001` | font ID |
| `0x0002` | point size |
| `0x0004` | RGBA color |
| `0x0010` | bold (`0x0100`) |
| `0x0020` | italic (`0x0200`) |
| `0x0080` | face `0x1000` |
| `0x0100` | underline group (`0x04c0`) |
| `0x0200` | condense/extend group (`0x6000`) |
| `0x0400` | strikethrough (`0x0001`) |
| `0x0800` | face `0x0002` |
| `0x1000` | case group (`0x0030`) |
| `0x2000` | superscript/subscript group (`0x000c`) |

A set face-mask bit can explicitly turn a face off. A run that changes only
bold must inherit the default italic setting; a color-only run must inherit
the layout's font and point size. Unknown bits and extension bytes remain raw.

[O, FileMaker Pro 26.1.1] With the case-group mask `0x0030`, faces
`0x0010` display the run in uppercase, `0x0020` in lowercase, and
`0x0030` in title case. In a mixed Text object those commands changed
only the selected word and kept its stored spelling. The same uppercase
run over a complete Account Name symbol changed its display from `Admin`
to `ADMIN`. Accessibility returned the underlying case.
[Per-run native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1097#issuecomment-7192063512).

[O, FileMaker Pro 26.1.1] With a format applied to the whole object,
Uppercase retained `ß`
and the `ﬂ` ligature, and changed `étude` to `ÉTUDE`. Title Case
displayed `o'NEIL mACDONALD-sMITH 3RD aLPHA bETA` as
`O'neil Macdonald-smith 3rd Alpha Beta`: apostrophes and hyphens kept
their words together.
[Whole-object native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1097#issuecomment-7193070314).
Using the same character mappings and word boundaries within a run is
inferred. Precedence between object and run styles, and title-case breaks
at quotation marks, parentheses, slashes, and periods remain unmeasured.

### 8.2 Styled merge values

[O, FileMaker Pro 26.1.1, local synthetic file] Browse preserves the
stored formatting of literal text on either side of an inserted layout
symbol or merge field. A uniformly styled reference passes its style to
the replacement. Where the opening delimiters, reference name and
closing delimiters each had a different color, the entire replacement
used the opening delimiter's color. Treating other attributes within a
mixed reference the same way is inferred from this test. In the saved
split examples, the opening delimiter and name shared a run. The closing
delimiter kept its separate color through reopening. An empty field
removed just the replacement, and the suffix kept its styling and line.
A long field retained its style. Both lines of a two-line field used
the replacement's style, and the suffix kept its own style on the second
line. Different paragraph spacing and merge-variable styling were not
measured.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1030)

## 9. Rendering stack

### 9.1 View system

[O] `libViewSystem` (Mac) and `ViewSystem.dll` (Windows) provide:

- View tree: `IView`, `BaseView`, `ClipView`, `ScrollView`, `ShadowView`,
  `CVScrollableContent`, `CVScrollBar`, `NativeScrollerView`, `LegacyNativeView`,
  platform bridges (`PlatformViewMac`, `PlatformViewScrollerMac`).
- Graphics: `IGraphics` (rounded-rect fill and stroke inside the border, linear
  and radial gradients, arcs, paths, image boxes with nine-slice and tiling via
  `IGraphics::ImagedBox`, clip regions, affine transforms, physical pixel
  snapping, font metrics, `DrawNativeRichText`, `RenderText`), `CGGraphics`
  (Mac), `ClipGraphics`, `NullGraphics`, `GenericGraphicsRecording` (record and
  replay), `BinnedDirtyRegionManager` (invalidation), `ImageCacheEntry`,
  `AlphaSpriteImage`.
- Events: `EventSystem`, concrete mouse, key, gesture, and text-input (IME)
  events; `Animation` (`Animation::SlideData` for slide panels and transitions).
- Printing: `IPrintOperation`, `XViewPrintOperationCocoa`,
  `XPrintablePageWithViews`, `XMacPDFRecorder`, `XMacCGPDFDocumentCache`.

### 9.2 Layout object rendering (FileMakerPro)

[O] `LayoutObjectViewFactory` / `ProLayoutObjectViewFactory` /
`PrintingLayoutObjectViewFactory` create one view per layout object:
`TextLayoutObjectView`, `OptionFieldLayoutObjectView` (checkbox and radio),
`ButtonLayoutObjectView`, `PortalLayoutObjectView`, `TabLayoutObjectView`,
`TabPanelLayoutObjectView`, `DotLayoutObjectView`, `DotPanelLayoutObjectView`,
`SegmentedBarLayoutObjectView`, `PopoverLayoutObjectView`,
`ExternalLayoutObjectView`, `ShapeLayoutObjectView`, `GroupLayoutObjectView`,
`ContainerLayoutObjectView`, `UnsupportedLayoutObjectView`. Behavior is split
into Browse and Design variants (`BrowseFieldLayoutObjectBehavior`, ...).
Field editing uses `TextContentView` (caret, selection, IME, baselines).

Box painting: `CSSBoxRenderer` (`RenderBox`, `RenderBackground`,
`RenderBackgroundLayer`, `RenderBorder`, `RenderBorderStroke`,
`RenderInnerShadow`, `RenderMaskForInnerBorder`, per-corner radii, edge
quadrant clipping for mixed border colors, line dashes); specialized outlines
in `TabBoxRenderer` (tab shape) and `PopoverBoxRenderer` (popover with arrow).
Containers: `ContainerRenderer` (images, PDF, file icon with name, EPS, PICT,
META, references, sample data). Charts: `ChartHighLevelRenderer`.

Inferred paint order per object [I]: outer box shadow, background layers,
border (or border image), inner shadow, `inner_border` component, content
(text, icon, image), then child objects.

[O, FileMaker Pro 26.1.1 on macOS] On solid square rectangles and
buttons, an opaque outer shadow at horizontal +6 pt, vertical +3 pt, blur
4 pt and spread 2 pt extends about 0 pt left, 2-2.5 pt up, 11-11.25 pt
right and 8-8.4 pt down beyond the visible border. The inset shadow is
clipped at the inside edge of the border to layout-point precision, below
the button text and glyph. The two effects can be enabled together.
The matching outer rectangle property is 122 with one field-19 shadow
of type 1, inset 0, alpha 1 and the four lengths in points. [Native
measurement](https://github.com/OpenMaker-Dev/openmaker/issues/1034#issuecomment-7202993034)

[I] The outer extents fit an offset rectangle expanded by the spread,
with the blur extending about one blur distance beyond that edge.
The samples do not identify FileMaker's exact blur kernel.

### 9.3 Graphics backends

| | Mac | Windows |
|---|---|---|
| 2D | CoreGraphics (`CGGraphics`) | Skia (`skia.dll`): raster `SkBitmap`/`SkCanvas`, `SkPath`, `SkGradientShader::MakeLinear/MakeRadial`, `SkMaskFilter::MakeBlur` (shadows), `SkDashPathEffect`; plus GDI, GDI+, MSIMG32 |
| Text | CoreText under FileMaker's XText layout | Uniscribe; Skia + DirectWrite |
| GPU | [U] | skia.dll imports OPENGL32 but ViewSystem uses raster canvases [O] |

### 9.4 PDF

[O] Windows: `pdfium.dll`, imported by `FileMaker Pro.exe` and `Support.dll`
(container PDF display and thumbnails [I]). Mac: CGPDF (`CGPDFDocumentCache`)
for display; PDF output via an embedded PDFHummus writer (`PDFWriter`,
`ObjectsContext`, `FMPDF::Hummus_imp`) with FreeType for font embedding
(`FT_*`, `FreeTypeOpenTypeWrapper`).

### 9.5 Charts

[O] ChartDirector 7 (`libchartdir.7.dylib`, `chartdir70.dll`) behind the
`FMChart` model (`ChartModel`, `SeriesModel`, `AxisModel`, `LegendModel`,
`ChartFormatDefinition`, `ChartDataDefinition`) and renderers
(`ChartDirector_*ChartRenderer`), output as a bitmap with tooltip hit regions
(`ChartDirector_Render_OutputChart`). Color schemes match the theme metadata
names (Resources `Chart*ColourSchemeThumb.tiff`).

### 9.6 Web viewer

[O] Mac: WKWebView (`PlatformWKWebViewerMac`, `WKNavigationDelegate`,
`evaluateJavaScript:completionHandler:`). Windows: WebView2
(`WebView2Loader.dll`, `Microsoft.Web.WebView2.Core/Wpf.dll`). URL templates
for the web viewer setup dialog: `en.lproj/WebViewerTemplates.xml`.

Storage [O, 2026-11-02]:

- A web viewer is object type 13 whose `externalObj` (`LayoutObject` field
  11, `ExternalLayoutObject {type, subType, flags}`) has type `0x57454256`,
  the four-character code `WEBV`, subtype 0; its style's object type is 86.
  The Contacts template's "website" viewer stores
  `08 d6 84 95 ba 05 10 00 18 83 80 02` (flags `0x8003`).
- `externalObj.flags` holds the Web Viewer Setup options. FileMaker Pro
  26.0.3's `-[FMWebViewerSetupControllerMac OnOK:]` calls one
  `WebViewerSetupModel` setter per check box, and each setter rewrites one bit
  of the model's 16-bit flags (arm64 disassembly of the installed binary,
  `otool -tV -p`): `SetAllowInteraction` `0x1`, `SetDisplayContentFindMode`
  `0x2`, `SetDisplayProgressBar` `0x4`, `SetDisplayStatusMessages` `0x8`,
  `SetAutoEncodeURL` clears `0x20` when on (the bit means "do not encode"),
  `SetFMIntegration` ("Allow JavaScript to perform FileMaker scripts")
  `0x40`. The setup model does not touch `0x8000`, which most stored viewers
  have: FileMaker Pro 26.1.1 set it for a custom web address and not for a
  template (inferred, 10.19); it also stored `0x0080`, which no setter
  writes, together with JavaScript scripts (10.19). Across the 80 web viewers of the MBS 16.3
  examples, 62 store `0x800d`: FileMaker's defaults (interaction, progress
  bar, status messages, encoding on; Find mode and JavaScript scripts off).
  None of them allows JavaScript scripts, and none of their pages call
  `FileMaker.PerformScript`.
- The web address is a stored formula node (children 4 context, 5 formatted,
  6 Polish) under the object attribute with the two-byte key `00 00`:
  `4/5/<layout>/5/<object>/00 00/5`, in 69 of those 80 viewers (the others
  have no address). Contacts stores context `01 00` (none of its own; it is
  evaluated in the layout's), and its Polish holds the literal `"https://"`
  and two references to the Website field (field 7 of occurrence
  `d0 00 02`).
  `om-layout` decodes it as `ObjectCalcs::web_address`; `tests/web_viewers.rs`
  checks the shipped template.

[O, FileMaker Pro 26.1.1, macOS] With Automatically encode URL off,
a calculated `data:text/html,` address containing literal HTML and a
CSS color with a raw `#` displayed the styled text after the hash.
Data URL fragments, other media types and base64 were outside that
comparison. [Native check](https://github.com/OpenMaker-Dev/openmaker/issues/1129#issuecomment-7202980493)

OpenMaker's runtime (`om-gui` `web_viewer`, the `web-viewer` feature):

- Browse evaluates the address on the current record in Form view and on
  each row's record in List view, and shows it in a native view (wry): on
  Linux a WebKitGTK view whose X11 window is a child of its window's own X11
  window, or, with the Wayland backend, a page Browse paints itself (below),
  on macOS a WKWebView in a clipping view of its window's content
  view (below). The view lies at the
  object's rectangle, clipped to the document's scroll area
  and zoomed with it; the view hides while egui draws anything over it
  (menus, pop-ups, dialogs, popovers) and is destroyed when the object is no
  longer painted (another layout, mode or view, a panel not in front, a hide
  condition, the window or file closing). A different calculated address
  loads in the same view. "Allow interaction" off makes the page insensitive
  (on macOS a plain view over the page takes its clicks, which go on to the
  window);
  "Automatically encode URL" uses Insert from URL's encoding, and also
  encodes `#` in a `data:` URL. Views are incognito, without developer tools,
  new windows or downloads, and navigate only to http, https, data, blob,
  file and about URLs. WebKitGTK's DMA-BUF renderer draws blank pages where
  it cannot share its buffers with the X server, so at startup, unless
  `WEBKIT_DISABLE_DMABUF_RENDERER` or `WEBKIT_DMABUF_RENDERER_FORCE_SHM` is
  set, the process turns it off on an X server without DRI3, and has it
  send pages through shared memory (still rendered on the GPU) where DRI3
  hands out a device of NVIDIA's driver: there it cannot allocate its
  buffers, nor, without GBM, import them (measured through XWayland with
  NVIDIA 615 and Arch's WebKitGTK 2.54; an NVIDIA X server of its own, and
  other drivers' failures, unmeasured). Debian's and Ubuntu's WebKitGTK
  packages make a choice of their own on NVIDIA's driver
  (`disable-nvidia-dmabuf.patch`, from WebKit pull request 18614) before
  they read the variable. In Ubuntu 24.04's 2.52.6-0ubuntu0.24.04.2 it sends
  pages through shared memory as well, and honours the variable. In
  0ubuntu0.24.04.1 it turned the renderer off without shared memory, and
  Debian bug 1142771 reports that crashing pages that need compositing
  [reported, unmeasured here].
- On Linux (`web_viewer/gtk.rs`) the page bridge posts to a script message
  handler of its own, `openmaker` (wry's IPC drops messages from `data:`
  pages, whose URL it cannot parse). WebKit shows a page's message
  handlers to every frame of the page, and WebKitGTK does not say which
  frame posted a message (its `WebKitFrame` belongs to the web process's
  extension API), so the handler is registered in a script world of
  OpenMaker's own, where no script of the page or of its frames runs. Two
  scripts go into the main frame only, as each document starts and before
  the page's own scripts: the `FileMaker` object, in the page's world, and
  in OpenMaker's world a poster, which posts what `FileMaker` hands it
  through an element that only the two hold (it is taken out of the
  document at once). A view starts loading only once both halves and the
  handler are in place. Another site's frame in the page reaches neither the
  handler nor the main frame's `FileMaker`; a frame of the page's own
  origin can call `parent.FileMaker` [I, OpenMaker's choice; FileMaker
  Pro's scope for `FileMaker.PerformScript` in frames is unmeasured].
  Unlike macOS's token, nothing has to reach a document after it commits,
  and a page whose Content Security Policy forbids frames still posts.
  `tests/web_viewer_native.rs` embeds a frame from another loopback port,
  which posts to the handler and calls `parent.FileMaker`, and checks that
  neither reaches the bridge and no script runs, and that the first
  document's inline call does run. On both platforms the bridge's
  messages are bounded where they arrive, on the thread that hears the page
  and without waiting for the app: only well-formed ones are kept, and one
  longer than 16 MiB, or one that would put more than 256 messages or
  32 MiB (each counted as at least 1 KiB) in wait for the app, is dropped
  [I, OpenMaker's bounds; FileMaker's are unmeasured].
- On macOS (`web_viewer/appkit.rs`) AppKit's views live on the main thread,
  where the app's frames run: commands apply as they come, and a page is
  made at the end of the frame, with wry, as a child of the root window,
  whose handle eframe gives out, and then moved into its own container
  view in its window. The page bridge posts by asking a hidden frame to
  load an `openmaker-message:` URL, which the view's navigation handler
  takes and refuses; wry's IPC would drop `data:` pages' messages, and a
  message handler of OpenMaker's own needs code objc2 marks unsafe, which
  the workspace forbids. The handler hears every frame's navigations, so
  the URL carries a random token, known only to the bridge script injected
  into the main frame. Each document the main frame commits gets a new one
  from the OS's random source (without it the bridge stays off), and the
  last document's stops working as the next one commits, with what it
  posted that the app has not taken yet: another site's frame in the page
  cannot post, nor can a page that learned an earlier document's token. A
  page whose Content Security Policy forbids frames (`frame-src 'none'`)
  cannot post at all.
  For the same reason the history and loading state that Set Web Viewer's
  Back and Forward need are read through key-value coding. wry reports a
  load only when it commits and when it finishes, so each frame also looks
  at the page's `loading`: a navigation is loading from when it starts,
  and one that fails before committing ends a quarter of a second after it
  stops loading. Before handing a `data:text/html,` address whose
  payload starts with literal markup to Foundation, OpenMaker escapes
  raw hashes so they stay in the HTML even when auto-encoding is off.
  A trailing `#section` after the literal markup becomes page text
  [I, OpenMaker's choice; FileMaker Pro's behavior is unmeasured].
  An address Foundation cannot read as a URL shows a blank page
  [I, OpenMaker's choice; wry would panic on it]. Every window shows
  its own pages: another window's AppKit window is found as
  `web_viewer/viewports.rs` says (below). `tests/web_viewer_native.rs`
  runs on macOS too, with the window's mouse events given to the view under
  the page's button rather than real input (the test window is not made key,
  so as to leave the desktop alone), and `tests/web_viewer_windows_native.rs`
  reads a second window's List rows' colors from the system's
  `screencapture`.
- With `OPENMAKER_WEB_VIEWER_BACKEND=wayland` (opt-in for now,
  `web_viewer/wayland.rs`), a Linux build keeps its windows where winit
  puts them (Wayland on a Wayland session), and no view is a window:
  - **The page host.** WebKitGTK runs in `om-gui-webview`, a process of its
    own beside the program (or `OPENMAKER_WEB_VIEWER_HOST`), which takes
    the app's commands and sends its events as JSON lines on its stdin and
    stdout. Its only display is a Wayland compositor of the app's own
    (smithay, `web_viewer/wayland/display.rs`) with no screen, reached
    through a socket in a private directory that goes once the host has
    connected. Each view is a window there, titled with its id and sized to
    the object in points. The display's `wl_output` scale is the app's
    pixels per point rounded up (GTK 3 has whole scales only, so at 1.5 a
    page is drawn at 2 and scaled down), and the host gets no `GDK_SCALE`
    ([#1086](https://github.com/OpenMaker-Dev/openmaker/issues/1086)).
  - **Frames.** The display offers no DMA-BUF, so WebKitGTK sends its pages
    in shared memory (its web process still renders them on the GPU). The
    display reads each buffer through its pool's file rather than mapping
    it, which needs no `unsafe` code, and only once the sizes the buffer
    declares fit: sides no larger than the app's largest texture, and what
    reading and converting it take within 256 MiB beside the other views'
    frames waiting for the app. Each view keeps only its newest frame: the
    one waiting goes before its replacement is read.
  - **Painting.** Browse paints each view's newest frame as a texture in its
    object's place among the layout's objects. Objects stacked above a web
    viewer, menus, popovers and dialogs draw over the page, where the X11
    backend's views hide instead. A window needs no native window of its own
    for its pages.
  - **Pointer.** The pointer's moves, buttons and wheel over an interactive
    viewer go to the page; nothing egui draws above it gets them.
    Scrolling over the page scrolls it, not the layout.
  - **Keyboard.** A press in the page gives it the keyboard, and takes
    egui's focus from any field; a press elsewhere in the window takes the
    keyboard back. While the page has it, the window's keys go to the page
    before anything else in the window sees them: its shortcuts and Tab
    too, as with the X11 backend, where the page's own window takes them;
    egui's focus stays with the page meanwhile. Every key the page holds
    goes up when the keyboard moves, and when the app's window loses it.
    - **Keymap.** Keys go as Linux key codes with the modifier keys held.
      On a Wayland session the page's keymap is the session's: a connection
      of the app's own reads the compositor's keyboard keymap
      (`wayland/keymap.rs`). Elsewhere it is xkb's default. Only the
      keymap's first layout is used: the connection has no window, so the
      compositor never tells it which layout is active, and switching
      layouts (US to Russian, say) is not followed.
    - **Edit keys.** egui's Copy and Cut reach the page as Ctrl and the
      keys that type c and x in the page's keymap (their US places unless
      its first layout has them elsewhere, as Dvorak does). A Paste puts
      the app's clipboard's text on the display's own clipboard, offered
      only as the current selection, and then sends Ctrl and the keymap's
      v. egui sends Paste only when the
      app's clipboard holds text, so with it empty Ctrl+V does not reach
      the page at all.
    - **Input methods.** The input method's text goes to the page through
      `zwp_text_input_v3`: its preedit, shown in the page as it is
      composed, and its commit. The input method's window is placed by the
      caret the page reports.
    - **Order.** Keys in the same frame as a click reach the page before
      the click.
  - **Pop-ups.** A page's menus, `<select>` lists and tooltips are the
    display's pop-ups. The app draws each one over everything in the
    window, at its place from its view's corner, so it can lie beyond the
    object (`WebViewers::paint_popups`), and the pointer's input over it
    goes to it; a press in one keeps the input until its release, wherever
    the pointer goes. A press in the page, or anywhere outside the pages'
    pop-ups, closes them, and the app stops showing them at once. While a
    menu is open, the keys reach it through its page. A page has at most 8
    pop-ups open at once, within 16384 points of its view's corner, and
    their pictures hold at most 64 MiB in the app: one more, or one
    farther, is closed as it opens, and one beyond the bytes is not shown.
    Limits: a pop-up stays within the app's window, and a pop-up the page
    moves after it opened keeps its first place.
  - **Cursor.** The cursor over a page follows WebKit's hit test: a hand
    over a link, a text cursor over editable text, the arrow elsewhere. A
    page's own CSS cursors are not followed.
  - **Clipboard.** The clipboard follows the keyboard: the page with it may
    set the display's selection and is offered the one a paste puts there.
    Text a page copies becomes the app's clipboard's, read from the page's
    selection through a pipe once the display has taken it; a newer copy
    replaces one still being read, and a copy of more than 16 MiB is left
    out whole. The app's clipboard's text is pasted in the page as above,
    written to at most 4 pages' pipes at once. Both ways run on the
    display's loop without blocking, and a page that has not finished in
    5 s loses its transfer. Text is all that is bridged: no images, no
    rich text, no primary selection.
  - **The host is another process.** Both ways are bounded. A command
    line longer than 16 MiB (a page's address, a function's parameters) is
    refused as it is written, and that load or call fails; the Set Web
    Viewer or Perform JavaScript in Web Viewer that asked for it fails with
    error 3 [I, OpenMaker's choice; FileMaker unmeasured]. A view whose
    first address was refused is made, empty, by its next load. Commands
    wait in a queue bounded by number and bytes, and the host holds one at
    a time. Its event lines are bounded too, and its events are checked as
    any page's are. A host that ends fails its views, as a native view
    whose browser went away does. Set Web Viewer and Perform JavaScript in
    Web Viewer then fail with error 3 [I, OpenMaker's choice; FileMaker's
    error for a web viewer whose engine ended is unmeasured].

  Its design, and how the alternatives were set aside, is
  [#323](https://github.com/OpenMaker-Dev/openmaker/issues/323)'s study.
  `tests/web_viewer_composited.rs` runs it in the real app under Xvfb: a
  page painted in its object with no child window, a text object above
  the viewer drawn over it, a real click reaching the page's bridge, real
  keys typed in the page's text box, and a `<select>`'s list drawn below
  the page and choosing with Down and Return. An input method, and the
  session's keymap, were not tried (Xvfb has neither).
- On Linux the X11 backend's views can only be children of X11 windows, so a build with
  the feature puts its windows on winit's X11 backend whenever an X server
  answers at startup (`om_gui::native_options`). On a Wayland session, where
  winit would otherwise choose Wayland, that is XWayland, and no environment
  variable is needed. When no X server answers (XWayland turned off), or a
  library that winit's X11 backend opens is missing (libX11, libX11-xcb,
  libXcursor, libXi, libxkbcommon-x11), the windows stay on Wayland, and
  every web viewer shows "Web viewer: unavailable on this display." Builds without the feature stay on native
  Wayland. Through XWayland:
  - **Scale** comes from the X server: XSETTINGS `Xft/DPI`, then the
    `Xft.dpi` resource, then each monitor's size in RandR (winit 0.30: its
    pixel density over 96 dpi, rounded to twelfths, at least 1), unless
    `WINIT_X11_SCALE_FACTOR` is set. What a desktop at a higher scale shows
    depends on its compositor. Measured 2026-11-26 with the mock provider,
    comparing the same build through XWayland and natively on Wayland
    (`DISPLAY` unset), by screenshot
    ([evidence](../evidence/xwayland-scale/README.md)):
    - **sway 1.12 (wlroots, headless)** gives XWayland the output's logical
      size (a 1920 × 1080 output is 960 × 540 pixels on 254 × 143 mm at
      scale 2, 1280 × 720 on 339 × 190 mm at 1.5) and stretches X windows by
      the output's scale. With no XSETTINGS or `Xft.dpi` the app draws at
      1, so its windows have the native windows' size but are blurred, at 2
      and at 1.5. Native Wayland windows are sharp at both, 1.5 included
      (winit follows fractional scales). [I] Raising
      `WINIT_X11_SCALE_FACTOR` would only enlarge the stretched window (not
      tried).
    - **Hyprland 0.56 with `xwayland:force_zero_scaling`** (Omarchy's
      default) does not stretch X windows: XWayland has the monitor's
      pixels (1920 × 1080 on 600 × 340 mm at scale 2 as at 1) and no
      `Xft.dpi`. At scale 1 the app draws at 1, as natively. At scale 2 it
      still draws at 1: sharp, but half the size of the native window. [I]
      `WINIT_X11_SCALE_FACTOR=2` (or an `Xft.dpi` of 192) would give it the
      desktop's scale (not tried).
    - WebKitGTK, in the app's process, takes GDK's scale from
      `GDK_SCALE`, which some desktops set for X11 apps (Omarchy sets 2);
      its pages then draw at that scale within objects the app draws at 1
      ([#1086](https://github.com/OpenMaker-Dev/openmaker/issues/1086)).
  - **Input** is X11's, with XIM for input methods (not measured).
  - **The clipboard** is X selections (arboard), which the compositor shares
    with Wayland applications.

  A test runs the real event loop with `WAYLAND_DISPLAY` naming no socket.
  `tests/web_viewer_native.rs`, built from
  [#1087](https://github.com/OpenMaker-Dev/openmaker/pull/1087)'s first
  revision (its shared-memory choice on NVIDIA's driver; without it the page
  stays blank there,
  [#1085](https://github.com/OpenMaker-Dev/openmaker/issues/1085)), passed
  on Hyprland's XWayland at scale 1 with `GDK_SCALE` unset; with
  `GDK_SCALE=2` its click on the page misses
  ([#1086](https://github.com/OpenMaker-Dev/openmaker/issues/1086)). A native
  Wayland backend was studied and prototyped in
  [#323](https://github.com/OpenMaker-Dev/openmaker/issues/323).
- Every window shows its own views. eframe 0.37 gives out only the root
  viewport's native window; another window's viewport is an immediate
  viewport whose X11 window OpenMaker finds from the server
  (`web_viewer/viewports.rs`): a connection of its own watches the screen's
  root window for new top-level windows, and the one created by eframe's
  connection (an X11 window id carries its connection's resource-id base)
  between the moment before the viewport is shown and the start of its code
  is that viewport's, since eframe creates the window there. A title that
  matches breaks a tie, and anything else leaves the window without views
  and with a note. On macOS the moment is read from AppKit instead: the
  window `NSApplication.windows` lists at the start of a viewport's code that
  it did not list just before the viewport was shown is that viewport's,
  since winit makes it there on the main thread; when that moment shows
  several, the one of them with the viewport's title, then or on a later
  frame. A window that was there before is never taken.
  When eframe recreates a viewport's window (some builder
  changes), or a window moves to the root viewport, its views are made
  again in the new native window. Where egui embeds a window's viewport
  instead (a backend without more native windows), the window is an egui
  window inside the root window, drawn in the root's coordinates: its views
  are children of the root's native window at the objects' places there,
  clipped to the egui window's content, and they hide under whatever egui
  paints above that window (another embedded window raised over it, menus,
  pop-ups); the root's own views hide under the embedded window. A test
  checks the native windows in eframe's real event loop under Xvfb, and
  another, on a WebKitGTK build, renders a page in a second window's List
  rows, each row its own record's, with native and with embedded viewports
  (`tests/web_viewer_windows_native.rs`).
- [Documented] [Claris, Working with web viewers on layouts](https://help.claris.com/en/pro-help/content/web-viewers.html)
  describes pages in Form and List; in Table view only in the header,
  footer and top or bottom navigation parts. On portals: "You can't place a
  web viewer in a portal. If you place a web viewer in a portal, the web
  viewer appears as an object on the layout that overlaps the portal." A
  web viewer on a popover within a portal is allowed, and its context is
  "the portal's first record, not the portal's current record".
  [I, OpenMaker] Each List row in sight has a view of its own, keyed by its
  record and destroyed when the row scrolls out of sight; a List's header
  and footer, like Form view, have one view that follows the current record.
  Set Web Viewer and Perform JavaScript in Web Viewer reach the current
  record's row (a step run before that row is in sight waits for it, as for
  any view about to be made), and a page's `FileMaker.PerformScript` runs in the window on
  its current record, whichever row's page called it (FileMaker's choice
  for either is unmeasured). A window shows at most 32 pages at once
  (OpenMaker's bound: each is a native browser view); further viewers show a
  note, as does a viewer whose page would be more than 32,767 pixels across
  or down (OpenMaker's bound: X11 sizes and places windows in 16 bits). A web viewer stored inside a portal is painted once, over the
  portal at its own place, as an object of the layout on the layout's
  record, and the portal's rows skip it (`paint_portal_web_viewers`);
  whether FileMaker stores such a viewer as a portal child at all is
  unmeasured. Only what painting would show counts: the active tab or
  slide panel, and containers whose hide condition shows them on the
  layout's record. A viewer on a popover inside a portal shows nothing,
  since OpenMaker opens no popover inside a portal; FileMaker shows it,
  when the popover is open, in the portal's first record's context. In
  Table view the parts its Table View Properties include (4.2) show their
  web viewers, one page each that follows the current record, as a List's
  header and footer do; the table's own rows show none
  ([#306](https://github.com/OpenMaker-Dev/openmaker/issues/306)). Pro's
  page there is unmeasured.
  Synthetic painter tests check each List row's own placement and address
  and the portal viewers' single placements (not under a closed popover or
  an inactive panel), and the registry test that commands reach the current
  row's view.
- Set Web Viewer (Reset to the calculated address, Reload, Go Forward, Go
  Back, Go to URL) and Perform JavaScript in Web Viewer name the object
  exactly (an empty name: the active object, Perform JavaScript only) and
  wait for the UI's answer; the function gets its parameters as text and the
  step waits for it to return (30 s at most). A page still loading fails the
  step with 5 [I], a function that is missing or throws with 5 [I], and a
  viewer with no live view (not shown, a window whose native window is not
  known, unsupported platform) with 116 [I]; none reports success.
- With "Allow JavaScript to perform FileMaker scripts" the page gets
  `FileMaker.PerformScript ( script ; parameter )` and
  `FileMaker.PerformScriptWithOption ( script ; parameter ; option )`: the
  script is found by name (any case) in the file that owns the viewer's
  window, with the parameter as text, and started with the option
  (`options-for-starting-scripts`: 0 Continue queues it behind the running
  scripts, 1 Halt, 2 Exit, 3 Resume, 4 Pause, 5 Suspend and Resume runs it at
  once). A callback is routed by the view's window id, window UUID, session
  epoch, file, layout and object; one from a destroyed view, a closed
  window or an earlier session is dropped.
- Not yet: a Windows (WebView2) adapter, views in
  native Wayland windows, pages on cards (a card's own layer counts as
  covering them: [#1028](https://github.com/OpenMaker-Dev/openmaker/issues/1028)), Find mode's "Display content in Find
  mode", progress bar and status messages, `GetLayoutObjectAttribute`'s
  `source`/`content` of a live page, `fmp:` URLs from pages, Reset clearing
  the history, and layout objects stacked above a viewer (the native view
  covers them).

### 9.7 Icons and glyphs

[O] `DefaultGlyphs/`: 147 single-color 24x24 SVGs (`a<n>_<name>.svg`,
`a<n>_<name>Outline.svg`, `_default.svg`) used for button icons. SVGs embedded
in databases mark fill elements with `class="fm_fill"`, which the renderer
recolors with `-fm-icon-color` [O class name, I recoloring]. FMEngine embeds
small SVGs for the `-fm-icon` arrow, calendar, and check glyphs.

## 10. Serialization and transport

### 10.1 In-file storage map

[O] From FMEngine functions that use each DBEngine key constant:

| Data | Location (HBAM key path, relative) | Format |
|---|---|---|
| Layout catalog | `KLayoutDefs` | catalog |
| Layout definition | layout + `KType` (`FMLayout::GetLayoutDef`) | `LayoutDef` protobuf |
| Object store | layout + `KList` (`FMLayout::LoadLayout` @0x2ceb54) | `LayoutObjectStore` protobuf |
| Parts | layout + `KCustomList`, each part + `KType` (`LayoutPartList::Load` @0x2db2e6) | `PartDef` protobuf |
| Layout-level styles | `KLayoutStyleDefs` | `ThemeStyles` protobuf |
| Layout metadata | `KMetadataDefs` | [U] |
| Layout script triggers | `KScriptTriggers` (`30`) | trigger cache and records, [file-metadata.md](file-metadata.md) section 7 |
| Layout guides | layout + `KGuides` (`16`) | guide list, a big-endian i16 count followed by the guides (below) |
| Table-view layout | layout + `0d` | the layout's own table view (`FMLayout::HasTableLayout`), objects at `0d/05/<object>` with their own trigger caches ([file-metadata.md](file-metadata.md) 7.1); a new file's, member by member, in 10.14 |
| Per-object attributes | section 5.5 | calc formulas and binary records [U] |
| Themes | `KThemeDefs`, `KStyleDefs`, `KMetadataDefs`, `KBaseThemeName`, `KBaseThemeVersion` | `ThemeStyles`, `ThemeMetadata` protobuf |
| Images (graphics, icons) | file library `31/5/<libraryKey>/<QuadChar>` | SVG originals at QuadChar `SVG `; [file corroboration and decoder](library-images.md) |

Each protobuf value is read through an `HBCharIStream` over the HBAM position
and parsed with `ParseFromIstream` [O, `LayoutDef::Read` @0x2c979a, REA Evidence
`ev_27e8f820a04654acb990a93b7a469680a290ee3f6de7c68e64a40089782d4126`].

[O from bin, FileMaker Pro 26.1.1 Mac arm64 FMEngine]
`FMLayout::GetGuideList` (`0x2d6dd0`) reads `KGuides` and
`FMLayout::SaveLayout` (`0x2d1b1c`) writes it when the guide list is
loaded. `GuideList::Read` (`0x2ee668`) and `Write` (`0x2ee848`) use a
big-endian i16 count. Each `TGuide` record (`Read` at `0x2ee304`,
`Write` at `0x2ee3d4`) is 13 bytes: one byte, a big-endian u32 flag
word, and a big-endian eight-byte word. The last word's coordinate
interpretation and the first byte's values need a nonempty native sample.
The observed `00 00` leaf is an empty list
([file-format.md](file-format.md) 3.7).

### 10.2 Server to client

[I] FMEngine reads layouts only through `Draco::HBPosition` / `HBCharIStream`,
the same API it uses for local files; none of the DBEngine `.proto` files
mention layouts, themes, or styles. A hosted layout therefore most likely
reaches the client as the same HBAM records, fetched through DBEngine's remote
file access. Confirming this and the request granularity belongs to the network
workstream ([network-protocol.md](network-protocol.md)).

### 10.3 Other representations

- XML clipboard and Save-as-XML [O tag names]: `kXMLEngine_*_Tag` constants
  (`Layout`, `Object`, `Bounds`, `PartsList`, `Part`, `Field`, `Text`,
  `StyledText`, `CharacterStyle`, `ParagraphStyle`, `Portal`, `TabControl`,
  `TabPanel`, `SlideControl`, `SlidePanel`, `PopoverButton`, `PopoverPanel`,
  `ButtonBar`, `WebViewer`, `Chart`, `Conditions`, `ScriptTriggers`,
  `Tooltip`, `LocalCSS`, `FullCSS`, `CustomStyles`, `Theme`, ...). Styles are
  written as CSS text there (`BaseStyle::SerializeToCSS`, `DeserializeFromCSS`).
- Database Design Report: `en.lproj/DDR/*.xsl` [O] document the DDR XML object
  vocabulary (`Field`, `Text`, `Rect`, `RRect`, `Oval`, `Line`, `Portal`,
  `TabControl`, `SlideControl`, `PopoverButton`, `GroupButton`, `ButtonBar`,
  `ExternalObject`, `Graphic`).
- WebDirect CSS: `BaseStyle::SerializeToIWPCSS`, `SerializeToWebCSS`,
  `FMStyle::ObjectType::GetIWPName` emit browser CSS using Vaadin classes
  (`v-table-cell-content`, `v-tabsheet-tabcontainer`) and `iwp-`, `iwps_`,
  `fm-cf-` classes [O].

### 10.4 Direct observation in shipped templates

`DynamicTemplates/` holds six templates (Assets, Contacts, Content_Management,
Inventory, Meetings, Tasks), each as 11 localized `.fmp12` files plus
`info*.json` (GUID, `Clients: ["Pro"]`, localized names) and preview images
[O]. The files are HBAM7 databases (header string `HBAM7` near offset 0x0f);
text in catalog records is XOR-0x5A encoded while protobuf blobs are stored
raw [O].

Decoded from `Tasks/en.fmp12` [O] with `tools/fmp12_find_layout_objects.py`
and `tools/pb_raw_decode.py` (offsets are file offsets):

- 0xb6bd: `LayoutDef` fragment: backgroundStyle custom `secondary-background`
  (type 20), boxStyle (type 21), tableGridStyle local borders, baseWidth 1024,
  customMargins 36 pt.
- 0xb836 onward: `PartDef`s: top navigation (type 12) 0/44, header 44/30,
  leading sub-summary 74/30, body 104/60.
- 0x3d3cb onward: 112 decodable `LayoutObject` records spread over several layouts: button bars, buttons,
  text objects with custom styles (`small-primary-text`), fields (displayType 6,
  objectTypeId 66, fieldSpec {table 1065090, field 2}), a portal
  (tableAliasKey 1065091, numRows 3, initialRow 1, four children), rectangles
  (`knockout-shape`).
- 0x38483 onward: `ThemeMetadataSet`s; 0x73627: `CustomStyleSet minimal_edit_box`
  (6.1).

### 10.5 A layout member's indexes and a new object's records

[O, 2026-11-02, the shipped `DynamicTemplates/*/en.fmp12`, read-only with
`tools/fmp12_keytree.py` and `tools/fmp12_find_layout_objects.py`] Besides
`02` (`LayoutDef`), `03` (parts), `05` (object attributes), `07` (object
store) and `10` (name), a layout member `[4, 5, L]` holds indexes over its
objects:

| Key | Holds | Type |
|---|---|---|
| `01, <occurrence>, <field>, <object>` | which objects show which fields: the occurrence's and the field's catalog key segments (Contacts: `d0 00 02`, field `01`), then the object's key. A field object's entry has an empty value (156 entries across the six templates); other values (`00` 37, `0a` 34, `02` 11, `09` 10, `0d` 1) sit under portals (field `00` with a portal object's key), merge texts and buttons, and `0d` under a web viewer for each field its address reads (10.19). A layout without references holds an empty list at `01, 00` | O; values other than empty but `09` (10.17) and `0d`: U |
| `03, <part>, 07` | the part's top-level objects: a big-endian `u32` count, then each object's key string after a byte giving its length (`02 01 33` for object 51, `03 02 80 49` for object 201). Every part has one (183 of 183, 67 of them empty with count 0), stored inline, its count matching its entries, in the order of the root stack (183 of 183). Every top-level object is in its part's list (853 of 853, 264 of them field objects); one list names a key that is not a top-level object | O; meaning I: the objects each part shows (below) |
| `05, 00` | the key string (length byte, serial key) of an object key: on each of 47 layouts with objects, never below a key of the object store, equal to the largest on 37 and above it on 10 (objects deleted); absent on the 25 without objects. One layout has an attribute record keyed above it | O; meaning I: the last object key issued |
| `08, <name key>` | the named objects: the object name's collation key (as a catalog's name list, schema-path.md 5.6) to the object's key string (`01 03`) | O |
| `0a, <top, left, n>` | every field object, portal, button bar, tab and slide control and most buttons, but no text, shape or line: 349 of 349 field objects, 40 of them outside `15`. The key is three `u16`s: the object's position (a top-level object's bounds' top and left, whole points on every one) and its pre-order index in the object tree (roots in `rootChildKey` order, each container's children in `childkey` order after it; 620 of 620 entries). The value is nine bytes: a `u16` length of the object's key string, the key string (length byte, serial key) zero-padded to three bytes, a `u16` (0 for fields, other values on some buttons and on objects in portals) and a `u16` repetition (a field showing several repetitions has an entry for each, at its own position). Nested under a container's entry for its contents; a portal's own entry has no value (10.8) | O; the second `u16` of the value and what FileMaker uses the index for: U |
| `15, <n>` (21) | the tab order: `n` a `u16` from 1 up, with gaps, each to the same nine-byte value as the object's `0a` entry | O; meaning I |
| `fc`, `fc 01` to `fc 03`, `d8`, `d9`, `f0`, `f1` | the stamp, UUIDs, member flags and view flags (4.2, file-metadata.md 1.1) | O |

[O, 2026-11-10] The counts above are from 12 files FileMaker saved (two
shipped templates, FileMaker Server's sample file, seven plug-in example
solutions and two oracle files; 72 layout members, read-only), whose
object trees all reach every object once.

Every object in the templates has an attribute record `05, <object>` with
an empty name `10` and two UUIDs `d8` and `d9` (62 of 62 in Contacts); a
field object in the tab order adds `07` (its position) and usually `0d`
(data formatting). A plain label's text is `01` as a plain SCSU value
without formatting `ff` ("First Name", object 5 of Contacts layout 2),
with `0d` = `00 00`.

The templates' edit boxes are `LayoutObject` fields 1, 2, 3, 4, 5, 8 in
that order. The style is `ObjectStyle {localStyles: {self, normal:
{613: 0}}, objectTypeId: 61}`, the rotation only; `fieldObj` is `flags,
fieldSpec {tableKey, fieldKey, repetition 1}, numOfReps 1, inputMode 0,
displayType 0, keyboardType 1, scrollbarStyle 1`, every field written even
when zero. Field flags vary (`0x1200e0`, `0x120020`, `0x200e0`, `0xe0`);
`0xe0` is common to every enterable one. Text objects are fields 1, 2, 3,
4, 5, 7 with `textObj {flags 2}` and style type 70, most often anchored
left and top (`0x30000000`). Object flags `0x30004000` add the tooltip bit
(5.2).

[O, 2026-11-10, FileMaker Pro 26.0.3, issue #166] A copy of the Contacts
template with a field and an edit box (object 51) that OpenMaker added
without an `0a` or `15` entry or a raised `05, 00` passed Check Consistency
and opened without a warning. Layout mode showed the edit box bound to the
field; Browse did not show it (neither on screen nor in the accessibility
tree), also in a new FileMaker Pro process. After FileMaker Pro moved the
object one point and saved the layout, Browse showed it and took a value.
That save added the object to the body part's list `03, 02, 07` (count 16
to 17, `02 01 33` appended after the stored entries), added `0a, <558, 33,
50>` and `15, 28` (one past the last) for it, set bit `0x400000` in its
flags (`kSelected`, 5.2.1), wrote a table view `0d` for the layout with `LayoutDef`'s
`tblCustomRowHeight` and `tblHeaderSize` set (0 to 21 and 24), rewrote the
object store (the same objects, stored in reverse order, the root stack
unchanged) and left `05, 00` at 50.

[O, 2026-11-10] A second file, placed the same way but with the object's
`0a` entry and `05, 00` raised to 51 (and no part list entry), also passed
its consistency check, and Browse again did not show the edit box, in a
new FileMaker Pro process before any change in FileMaker Pro. So the `0a`
entry alone does not make Browse show it.

[O, 2026-11-10, issue #166] A third file, placed as described below (the
part list entry, the `0a` entry and `05, 00` raised; one stored Text field
and its edit box, no label, on one Form layout of a starter solution
copy), passed Check Consistency (101 of 101 blocks) and opened without a
warning. On first open, before any visit to Manage Database or Layout
mode, Browse showed the edit box, with no records and with one, and it
took a value that stayed after commit, close and reopen. Manage Database
listed the field (Text, not global, one repetition) and Layout mode the
edit box bound to it. After FileMaker Pro saved a field comment and moved
the box one point, the file passed Check Consistency (108 of 108 blocks)
and kept the field, the comment and the value through reopening. The
14-point-high box clipped its unfocused text under the theme's padding;
focused entry worked.

[I] Browse shows a part's objects from the part's list, so a top-level
object missing from it is not shown. Of the save's changes, the part list
and `0a` are the two that every field object FileMaker saved has (853 of
853 top-level objects are in a part list; 349 of 349 field objects in
`0a`); 40 of 349 are outside `15`, 344 lack `0x400000`, and the template's
layout 3 has no `0d`. The third file shows that the two entries together
are enough for this case; whether the part list entry alone is, is not
isolated.

[O, 2026-11-18, FileMaker Pro 26.0.3] On a Contacts starter copy with
a new layout, New Record left no field active while the new layout's
only field was absent from `15`. Insert > Field added Pro's new field to
`15`; saving also added the previously placed field. On a duplicate of
Contact Details, the stored order kept its 24 entries and the two portal
fields appended by OpenMaker. Tab reached the portal fields and skipped
the placed Website field, which was absent from the stored order even
though Set Tab Order proposed a number for it. [U] Where Pro inserts a
new field into a populated custom order and what it does inside a tab or
slide panel. See [#316](https://github.com/OpenMaker-Dev/openmaker/issues/316).

What OpenMaker's placement writes (`om_data::layout_placement`,
`docs/authoring.md`, "Fields on layouts") follows these: an edit box and a
label encoded as above with flags `0x30000000` and field flags `0xe0`,
appended to the stored object store (a proto2 parser merges repeated fields
across the message, so the stored bytes stay as they are and the new
objects come last, on top); their attribute records with `10`, `d8`, `d9`
(and the label's `01`, `0d`); the field's `01` entry; the edit box and
the label at the end of the body part's list `03, <body>, 07` (the count
raised, every stored entry and byte kept); the edit box's `0a` entry (its
frame's top and left, which must be whole points, as no fractional
position was seen there; the number of objects in the tree as its
pre-order index, since it is the last root; its key string, 0 and
repetition 1); the same reference appended to `15` with the new position
in the edit box's attribute `07`; `05, 00` raised to the last new key;
and the layout's stamp. Appending after the greatest stored position
preserves the existing order and gaps. Object keys are the next after
every key the layout uses, every key its part lists, object index and
tab order name, and `05, 00`. A body part without a list, or with one
whose count does not match its entries, is refused: every part FileMaker
saved has a readable one. The label stays outside both `0a` and `15`.
New objects are unnamed, so placement leaves `08` alone.
A text placed on its own (the desktop's Text
tool, `place_text`) is the same text object and attribute record at the
end of the body's list, with `05, 00` raised to its key and nothing else
written. The field's `labelKey` (5.2), which 23 fields of the starters
set to their label, is not written: [U] whether FileMaker Pro sets it
when it places a field with a label, and what it uses it for. [U] What
FileMaker Pro's Text tool and its Create label write. [O, 2026-11-13,
`FMDeveloperTool` of FileMaker Server 26.0.3] On a copy of the English
Contacts starter, a layout OpenMaker added (10.6) with a field and its
label and then a text on its own passed `--checkConsistency` (102 of 102
blocks, 0 bad); Save as XML lists the body's three objects: an Edit Box
showing Company, a Text "Company" and a Text "Hello from OpenMaker", at
the frames given. FileMaker Pro 26 showed and kept an edit box
placed this way (the third file above); other field types, labels, layouts
and object kinds have not been opened in FileMaker Pro.

### 10.6 A new layout's records

[O, 2026-11-13, FMUpgradeTool and FMDeveloperTool of FileMaker Server
26.0.3 on a copy of the English Contacts starter solution] A patch whose
`AddAction` holds a `LayoutCatalog` with one `Layout` element adds the
layout (Save as XML lists it; `--checkConsistency` reports no bad blocks).
The element named only the layout's name, width, table occurrence, theme,
one Body part of a given height without style, its `UUID` and its
`<Options>` (the view flags of 4.2). Key by key against the unpatched copy,
the new layout is:

- in the layout catalog's index `[4, 1]` (the common catalog index,
  schema-path.md 5.1): the last issued id `00` raised by one (`01 04` to
  `01 05`), a name list entry `01, <name key>` with the id, the display
  order entry `03, <u32 position>` at the next position (5, after the
  four stored ones) with the id, the member count `04` raised, a member
  entry `07, <id>` (below), and the index's stamp `fc`, `fc01`-`fc03`;
- a member `[4, 5, <id>]` (below).

Nothing else of the file changed but the file's history `[2, 8]`, `[2, 9]`
and what FMUpgradeTool rewrites in every layout it saves: a table view
`0d` and the theme key `0b` on the stored layouts that lacked them, and
their stamps. No privilege set, theme or other catalog changed.

The member entry `[4, 1, 07, <id>]`, as the stored layouts' entries:

| Key | Value |
|---|---|
| `02` | `{2: tableAliasKey, 3: menuSetKey}` (`10 82 81 41 18 00`: occurrence 1065090, menu set 0) |
| `10` | the name (SCSU, XOR `5a`) |
| `30` | `u32` 0 |
| `f0` | `u64` 0: a layout (1 a folder, 2 a folder's end, `0x10` a separator "-", on the stored members) |
| `f1` | `u64` the view flags (4.2), as given |

The layout member `[4, 5, <id>]`:

| Key | Value |
|---|---|
| `01, <occurrence>` | an empty node under the layout's occurrence key (`d0 00 02`): the field reference index, no fields |
| `02` | `LayoutDef` (below) |
| `03, 00` | `01 01`: the last part key issued |
| `03, 01, 02` | the body's `PartDef` (below) |
| `03, 01, 07` | `u32` 0: the part's list of objects, empty (10.5) |
| `07` | empty value: the object store, no objects |
| `08`, `0a`, `15` | empty nodes: the named objects, the object index, the tab order |
| `0b` | `01 01`: the theme's key (the patch's `LayoutThemeReference`) |
| `10` | the name |
| `17`, `21` | empty values, as on every stored layout of the starter solutions |
| `d8`, `d9` | UUIDs: `d9` the patch's `UUID`, `d8` a new one |
| `f1` | the view flags |
| `fc`, `fc01`-`fc03` | the member's stamp |

With no style given, FMUpgradeTool wrote FileMaker's defaults:

- `LayoutDef`: `tableAliasKey` the occurrence, `menuSetKey` 0,
  `backgroundStyle` `{objectTypeId 20}` and `boxStyle` `{objectTypeId 21}`
  without a custom style, the same `tableGridStyle` as Contact Details
  (0.5 gray borders on types 20 and 21, none on 22 and 23, style 5 on 24
  and 25, 1 on 26 and 27, 0.25 and 0 point lines on 28 to 31),
  `baseWidth` the width, `maxWidth` 0, `printColumnWidth` 0,
  `printColumnCount` 2, `customMargins` 36 points each side,
  `tblCustomRowHeight` 0, `tblHeaderSize` 0, `layoutType` 0,
  `orientation` 1, `layoutClientType` 0;
- the body's `PartDef`: `type` 4, `flags` 0, `pageBreakAfter` 0,
  `offset` 0, `size` the height, `style` `{objectTypeId 34}`, `altStyle`
  `{objectTypeId 35}`.

The flags were stored as given (`0x10f3080d`, Contact Details' Form-only
flags). A second patch, a copy of Contact Details' element without
objects, kept its styles and got `0x14f3080d`. The third-party table of
4.2 names bit `0x4000000` `customColumnOrder`, a Table View option [O,
third party]; what it does is [U].

[O] The stored catalogs number layouts without gaps (the six English
starter solutions: positions 1 to n, ids 1 to n, `04` = n). Layout names
may repeat: Assets has two separators named "-"; the first has the plain
name key `02 23 00 00 00`, the second the same key followed by its id
(`02 23 00 00 00 07`, layout 7). [I] A name already in the list gets the
member's id after its key, so the list keeps one entry per member.

[U] What FileMaker Pro's New Layout/Report dialog writes for Computer and
Form: its parts and their heights, its width, its theme, where the layout
goes in the order and whether it fills the name with the occurrence's
name. Claris's help says the layout then shows only in the view chosen
and opens in Layout mode ([Claris: New Layout/Report](https://help.claris.com/en/pro-help/content/new-layout-report.html)).

What OpenMaker writes for a new layout (`om_data::layout_catalog`,
`docs/authoring.md`, "New layouts") follows the patch's records: the
member with these keys, the `LayoutDef` and body `PartDef` above
byte for byte for a given occurrence, width and height (a unit test holds
FMUpgradeTool's bytes), the index entries and the member's entry, UUIDs
`d8` (the request's, which identifies a repeated request) and `d9`, the
member's stamp without `fc` (schema-path.md 8.1)
and the index's stamp counted up; and, since #814, the stored Table View
layout `0d` of 10.14, which the patch's layout did not have. [O, 2026-11-13] A copy of the Contacts
starter with two layouts OpenMaker added this way ("OpenMaker Form", and a
second "Contacts", whose name list key ends with its id 6) passed
`FMDeveloperTool --checkConsistency` (102 of 102 blocks, 0 bad), and its
Save as XML lists both; the first's `<Layout>` element is the
FMUpgradeTool-made one's but for its name, body height and stamps. That
FileMaker Pro opens such a file and shows the layouts is not yet checked.

### 10.7 An object's position

[O, 2026-11-13, the English Contacts starter's Contact Details] An
object's position is held in two places of the layout member: its
`bounds` in the object store `07` (5.2), and, for the objects the object
index lists (10.5: fields, portals, button bars, tab and slide controls,
most buttons), the key of its `0a` entry, which begins with its top and
left in whole points. The attribute record's `07` is not a position: its
values (`00 1a`, `00 08`, `00 0b` on Contact Details) are small numbers
where the objects' tops are tens to hundreds of points, and only objects in
the tab order have one; it is read as the object's place in the tab order
[I]. A part's list of its objects (10.5) depends on which part holds the
object, not on where in it.

[O, 2026-11-10, issue #166] FileMaker Pro's own save of a moved object
rewrote the whole object store (the same objects, in reverse order, the
root stack unchanged), added the moved object to its part's list and to
`0a`, set its `kSelected` bit and wrote a table view `0d`.

What OpenMaker writes when it moves or resizes an object
(`om_data::layout_objects`, `docs/authoring.md`, "Moving objects"): only
that object's `bounds`, rewritten in place in the stored object store (the
four doubles, at their stored offsets; every other byte, other objects and
unknown fields included, as stored), its top-level `0a` entry keyed again
at the new top and left with its tree position and value kept, and the
layout's stamp. Only top-level objects without contents, within the part
that lists them, are moved, so no part list or nested `0a` entry changes.
[O, 2026-11-13] A copy of the Contacts starter on which OpenMaker moved
and resized a drop-down list field (object 37) and a text (object 45) on
Contact Details passed `FMDeveloperTool --checkConsistency` (101 of 101
blocks, 0 bad). Its Save as XML differs from the starter's only in the two
objects' `<Bounds>` (as written) and content hashes and the layout's
modification stamp. That FileMaker Pro opens it and shows the objects there
is not yet checked.

### 10.8 A deleted object's records

[O, 2026-11-13, the six English starter solutions and FileMaker Server's
English sample file, read-only; 69 layout members] What the files
FileMaker saved hold around objects it deleted, and the index details a
deletion depends on:

- **The object index `0a` follows the object tree after deletions.**
  Every entry is at its object's pre-order place (10.5), on the 6 layouts
  whose `05, 00` is above every stored key (objects deleted) as on the
  others, so the places after a deleted object close up [I: FileMaker
  renumbers them, or rebuilds the index, when it saves].
- **A portal's own entry has no value.** 13 entries hold no value, each a
  list node at a portal's place, with the portal's contents' entries
  nested below it (as many as the portal has children on 12 of 13; the
  Server sample's layout 1 has 3 entries for a portal of 5 children).
- **The field reference index `01`** holds a node for the layout's own
  occurrence on every layout, empty on the 31 without objects; another
  occurrence's node only while fields of it are shown below it (no empty
  one); and no field node without objects under it.
- **Attribute records of deleted objects.** 10 records (Server sample
  layouts 15, 30 and 66) are keyed by no stored object, all below
  `05, 00`. None has a name `10` or UUIDs `d8`, `d9`; each holds only
  `17`, `16`, or `01` and `0d`. [I] What FileMaker left of deleted objects'
  records; every record of a stored object has `10`, `d8` and `d9`.
- `05, 00` (the last key issued) is not lowered: the 6 layouts above keep
  it above every stored key, so a deleted object's key is not issued again.
- **Objects name other objects by key** (5.2): 23 of the top-level
  objects without contents are texts a field's `labelKey` names (its label);
  a popover's `buttonKey` and a popover button's `popoverKey` name each
  other, a tab control its default and current panels, a button bar its
  active segment. [U] What FileMaker does to a field's `labelKey` when its
  label is deleted.

[O, 2026-11-13, FileMaker Pro 26.0.3's Duplicate Layout and Delete Layout
on fresh copies of the English Contacts starter, compared key by key with
the starter] **The file library counts its uses.** Each library item
`[31, 5, N]` (library-images.md) holds a use count at `12` (a `u32`).
Duplicating Contact Details raised each count by one for every object of
the copy that shows the item: item 4 by 2 (buttons 6 and 12), item 5 by 3
(buttons 17, 23, 34), and items 2, 3, 6, 7 and 8 by 1, each item getting
its stamp. Deleting Contact List lowered the counts of the items its
objects showed: item 2 from 2 to 1 (stamped), and item 1, used only by
Contact List's button 7, from 1 to 0, which removed the item `[31, 5, 1]`
with its entries in the library's index `[31, 1]` (`06, 1`, `07, 1` and
`08, <the item's 16-byte key 04>, 1`), lowered the index's count `04`
(8 to 7) and stamped the index. Buttons count by their `libraryKey`
(button 3 shows item 3, whose count is 1); the starter's item 4 counts 1
for its two buttons. [I] Graphics and style images count the same way.

What OpenMaker writes when it deletes an object (`om_data::layout_objects`,
`docs/authoring.md`, "Deleting objects"): an object that shows a library
item (a button's, popover button's or graphic's `libraryKey`, or an
`Image.libraryKey` in its own styles) is refused, as OpenMaker does not
maintain the counts yet; so is an object another one names by key (unless
that one is deleted with it) and a popover button, whose popover would stay
without one. For any other object it removes the object's
definition and root key from the object store (every other stored byte
kept), its whole attribute record (OpenMaker's choice: the leftovers above
show FileMaker may keep some keys), its field references (dropping a field
node it empties, and an occurrence node it empties unless it is the
layout's own), its named-object and tab-order entries (the other tab
order numbers kept, as FileMaker's tab orders have gaps, 10.5), its entry
in its part's list (the count lowered), and its `0a` entry, keying every
other entry, nested ones included, at its object's new place; and it
stamps the layout. `05, 00` stays. A layout whose `0a` entries are not at
their objects' places is refused rather than renumbered. On the 69
members above, 636 top-level objects have no contents; 125 of them show a
library item and 23 are labels a field names, and each of the other 488
could be deleted this way, a second deletion on each result planning too.

[O, 2026-11-13, `FMDeveloperTool` of FileMaker Server 26.0.3] On a copy of
the English Contacts starter, OpenMaker deleted three objects of Contact
Details through the provider: a rectangle early in the tree (object 36, so
9 `0a` entries after it were renumbered), a named edit box in the tab
order (38, Last Name) and a text (45). `--checkConsistency` found no
problems (101 of 101 blocks, 0 bad). Its Save as XML differs from the
starter's, exported in the same session, only in the three objects, the
body's object count (16 to 13), the layout's modification stamp, and the
Table View column list, which FileMaker derives for a layout without a
stored table view: Last Name, still shown by another object, moves from
second to last. Deleting button 3 (before the library rule) also passed
the check, with 35 `0a` entries renumbered, those under the tab control
included, but left item 3's count at 1 with no use, where FileMaker
removes the item. That FileMaker Pro opens the file, shows the layout
without the objects and keeps its tab order working is not yet checked.

[U] What FileMaker Pro writes when it deletes an object: whether it
renumbers or rebuilds `0a`, drops the whole attribute record, prunes
`01` the same way, and renumbers `15`.

### 10.9 A duplicated layout's records

[O, 2026-11-13, FileMaker Pro 26.0.3's Layouts > Duplicate Layout on
Contact Details of a fresh copy of the English Contacts starter, the file
closed after it; compared key by key with an untouched copy] Pro named the
copy "Contact Details Copy", put it last in the Layouts menu after the
folder (outside it) and showed it. Of the file, only the layout catalog
`[4]`, the library `[31]` and the file's open/close history `[2, 6]`,
`[2, 8]`, `[2, 9]` changed.

The copy's member `[4, 5, 5]` is the source's `[4, 5, 3]` with:

- the name `10` "Contact Details Copy";
- a new UUID `d8` and no `d9` (the source has one);
- each object's attribute record with a new `d8`, its `d9` (the source
  UUID) kept, and nothing else changed;
- the stamp: the source's count `fc` (`01 03`), with Pro's user `fc01` and
  time `fc03`;
- the object store `07` rewritten: [I] the same objects serialized again,
  as Pro's save of a moved object rewrote it (10.7).

Its definition, parts and their lists, field references `01`, named objects
`08`, object index `0a`, tab order `15`, last issued key `05, 00`, theme,
view flags and the empty `17` and `21` are the source's, byte for byte;
object keys stay the source's, as they are the layout's own.

The index `[4, 1]`:

- the last issued id `00` raised (4 to 5);
- a name list entry for the copy's name key, with no id after it (the
  name is new);
- display position 5, after the folder's end at 4;
- the count `04` raised;
- an entry `07, 5` that is the source's `07, 3` but for the name `10`
  (`02`, `30`, `f0`, `f1` the source's);
- the stamp `fc` raised by 2 (`01 0d` to `01 0f`), with Pro's user and
  time.

The library `[31]`: the use count `12` of each item the copy's buttons show
raised by the number of them (10.8), and the item's stamp count `fc` raised
by the same number (item 4, shown by two buttons: `12` 1 to 3, `fc` 2 to 4;
item 5, three buttons: 3 to 6, 4 to 7), its user `fc01` and time `fc03`
kept. No item was added or removed.

What OpenMaker writes (`om_data::layout_catalog::plan_duplicate`,
`docs/authoring.md`, "Duplicating layouts"): the same member, index and
library changes, but for the keys named below. A layout whose styles show
a library image is refused, as how FileMaker counts those uses is [U]; so
are folders and separators.

[O, 2026-11-13] OpenMaker duplicated Contact Details on a copy of the same
untouched file through the provider. Over the whole layout catalog `[4]`
and library `[31]`, its file holds the same 1,809 keys as Pro's, with the
same values but for:

- UUIDs: the copy's `d8` and each object's `d8` (new on both sides);
- the stamps' user `fc01` and time `fc03` of the copy and of the index;
- the index's count `fc`: Pro's 15, OpenMaker's 14 (raised by 1). [I] Pro
  stamps the index twice in a duplicate; what the second change is, is not
  known;
- the copy's object store `07`, which Pro serialized again: both decode to
  the same 50 objects and root list and are as long; Pro's lists the
  object definitions in another order, OpenMaker's keeps the source's bytes.

Outside them, Pro also rewrote the file's open/close history `[2, 6]`,
`[2, 8]` and `[2, 9]`, which it writes on any open and close; OpenMaker's
write keeps them as stored. The library's use counts and item stamps are
equal. `FMDeveloperTool --checkConsistency` found no problems in
OpenMaker's file (106 of 106 blocks). The two files' Save as XML differ
only in the file name, the layout catalog's modification count (15 and
14), the users, times and UUIDs, and the 50 objects' hashes; the objects'
contents read the same. That FileMaker Pro opens OpenMaker's copy is not
yet checked.

The duplicated layout has no stored table view `0d` (10.1). Whether Pro
gives a table view's object records new `d8`s, and how it counts their
library uses, is not measured: OpenMaker renews them as it renews the
layout's own [I] and refuses a table view whose objects show a library
item.

### 10.10 A deleted layout's records

[O, 2026-11-13, FileMaker Pro 26.0.3's Layouts > Delete Layout... on
Contact List of a fresh copy of the English Contacts starter, confirmed,
the file closed after it; compared key by key with an untouched copy] Pro
asked "Permanently delete this layout?" (Cancel, Delete) and then showed
Contact Details, the next layout, in Layout mode. In the file:

- the member `[4, 5, 2]` is gone;
- the index `[4, 1]`: the name list entry naming the layout is gone; its
  display position (2) is gone and the later positions moved up (3 and 4
  are now 2 and 3, with their values); the count `04` is lowered (4 to 3);
  the entry `07, 2` is gone; the last issued id `00` stays (4); the stamp
  `fc` is raised by 1 (`01 0d` to `01 0e`), with Pro's user and time;
- the library `[31]`: item 1, shown only by Contact List's button 7, is
  gone (`[31, 5, 1]`), with its index entries `[31, 1, 06, 1]`,
  `[31, 1, 07, 1]` and `[31, 1, 08, <its key 04>, 1]` (the key's node
  emptied and removed); the library index's count `04` lowered (8 to 7)
  and its stamp `fc` raised by 1 (`01 10` to `01 11`) with Pro's user and
  time. Item 2, shown by Contact List's button 11 and Contact Details'
  button 42, has its count `12` lowered (2 to 1) and its stamp count raised
  by 1, its user and time kept.

Nothing else of the schema changed. Contact Details' button 3 still goes
to Contact List's id (its step is unchanged), so references to a deleted
layout stay as stored. Outside the catalogs Pro also changed its open/close
history `[2, 6]`, `[2, 8]`, `[2, 9]` and wrote `[d0 00 02, 07, 01]`, the
Contacts occurrence's record-level cache (schema-path.md: not schema),
which its window's records account for [I], not the deletion.

What OpenMaker writes (`om_data::layout_catalog::plan_delete`,
`om_data::library_uses::release`, `docs/authoring.md`, "Deleting
layouts"): the same. A folder or separator, the file's last layout, a
layout whose styles show a library image and one whose stored table view
`0d` shows a library item are refused.

[O, 2026-11-13] OpenMaker deleted Contact List on a copy of the same
untouched file through the provider. Over the whole file its key tree
holds the same keys and values as Pro's but for the stamps' user `fc01`
and time `fc03` of the layout index and the library index (their counts
are equal), `[2, 6]`, `[2, 8]` and `[2, 9]`, which Pro rewrote and
OpenMaker keeps as stored, and `[d0 00 02, 07, 01]`, which Pro wrote and
OpenMaker does not. `FMDeveloperTool --checkConsistency` found no problems
(100 of 100 blocks); the two files' Save as XML differ only in the file
name and the layout catalog stamp's user and time (both modification
count 14). That FileMaker Pro opens OpenMaker's file is not yet checked.

### 10.11 Layout Setup's records

[O, 2026-11-13, FMDeveloperTool 26.0.3's Save as XML of the six English
starter solutions, the same bytes in the macOS and Windows installers,
paired key by key with their stored catalogs: 38 layouts, folders and
separators] Each `<Layout>` element names the layout's options as stored:

- `<Options>` is the view flags `f1`, equal in the member
  `[4, 5, L, f1]` and in its entry `[4, 1, 07, L, f1]` on all 38. The
  starters hold three values: `0x10f30a0b` (List view the default, Form and
  Table view not available), `0x10f3080d` (Form only) and `0xf30801`
  (all three, Form the default: the folders, their ends and the
  separators);
  what each bit means is in 4.2.
- `hidden="True"` ("Include in layout menus" cleared) is bit `0x08` of the
  member flags `f0`: on Tasks' Assignee Chooser, Add Assignee and
  Assignments `f0` is `08` and on its separator "-" `18`, in the member and
  the entry; no other member has the bit. A layout's entry always has
  `f0`; its member has one only when it is not 0.
- `width` is `LayoutDef.baseWidth` (1024, and 668 on Assignee Chooser and
  Add Assignee).

[O, 2026-11-13, FileMaker Pro 26.0.3 on two fresh copies of the English
Contacts starter, compared key by key with an untouched copy] Layout Setup
on Contact Details, the layout saved on leaving Layout mode:

- **Renamed** "Contact Details Renamed": the member's name `10` and its
  entry's `[4, 1, 07, L, 10]`; in the name list `[4, 1, 01]` the entry
  under the old name's key is gone and one under the new name's key
  (10.6) has the same value, the layout's id.
- **List view made available and the default:** `f1` from `0x10f3080d`
  to `0x10f30a09` (`0x4` cleared, `0x200` set) in the member and the
  entry.
- Both: the member's stamp `fc` and the index's raised by 1, with Pro's
  user and time; the member's object store `07` written again; the stamp
  counts `fc` of the library items `[31, 5, N]` the layout's objects show
  raised (by 2 after the rename, by 4 after the views; their use counts
  `12` unchanged); the file's history `[2, 8]`, `[2, 9]`. Nothing else
  changed.

[O, 2026-11-14, FileMaker Pro 26.0.3 on five fresh copies of the
Contacts starter, one General checkbox of Layout Setup changed on Contact
Details in each, compared key by key with an untouched copy (the
walkthrough's comment of 05:14Z on [#71](https://github.com/OpenMaker-Dev/openmaker/issues/71))] Each checkbox moved one bit of
`f1`, from `0x10f3080d`, by the same bit in the member and its entry:

| Checkbox | Bit | The bit set means the option is | Changed to |
|---|---|---|---|
| Save record changes automatically | `0x10` | off | `0x10f3081d` |
| Enable Quick Find | `0x8000` | off | `0x10f3880d` |
| Delineate fields on current record only | `0x1` | on | `0x10f3080c` |
| Show field frames when record is active | `0x20` | on | `0x10f3082d` |
| Show current record indicator in List View | `0x10000000` | off | `0x00f3080d` |

Each save also wrote the object store `07` again, to the same bytes in
all five copies, and moved the stamps and the file's history; nothing else
told one checkbox from another.

[O, 2026-11-15, FileMaker Pro 26.0.3, [#243](https://github.com/OpenMaker-Dev/openmaker/issues/243): two disposable copies of the
Tasks starter compared with an untouched copy, one closed directly from
Layout mode] Layout Setup's **Show records from** pop-up listed the file's
four table occurrences and `Manage Database...`. Changing Task Details from
Tasks to Assignees raised no alert. The Layout mode header then named
Assignees; Browse showed zero records in the tested file. The existing field
objects remained, with their stored `fieldSpec` occurrence and field IDs
unchanged. Direct Tasks fields showed `::` before their names in Layout
mode.

The new serial-key value (1065092 instead of 1065090) appeared in both the
layout definition's `tableAliasKey` (`[4, 5, 3, 02]`) and the catalog
entry's `[4, 1, 07, 3, 02]`. The definition also gained protobuf field 17
with value 0. The field-reference index `[4, 5, 3, 01]` stayed
byte-identical. Pro rewrote the 8,468-byte object store and changed the
middle `u16` of seven object-index entries `0a` and six tab-order entries
`15` from `0x9726` to 0. It added a node under the new occurrence key,
raised the layout and catalog stamps, and wrote the file's history.
The object store's decoded field references did not change. Which of its
other changes, or the added field 17, occur on a no-change save is
unmeasured. How the old field objects render against a populated new
occurrence is also unmeasured.

[O] FMUpgradeTool 26.0.3 does not apply a patch's `ReplaceAction` or
`ModifyAction` to an existing `Layout`: given the Contacts starter with
Contact Details renamed, its List view made available, made the default,
or left out of the layout menus, it reports the patch applied and writes a
file byte for byte the source.

[I] Not measured: the bits Pro writes for Table view as the default
(`0x4000`, and whether it clears `0x200`), "Include in layout menus"
changed in Layout Setup (where it is stored is above), and a rename to a
name another layout has. OpenMaker sets one default-view bit and clears
the other, sets the five General bits as the table says, and writes the
rest as above; it keeps the object store and
the library items' stamps as stored (`docs/authoring.md`, "Layout
Setup").

### 10.12 A layout's width

[O, 10.11's pairing] Save as XML's `width` is `LayoutDef.baseWidth` on
every layout of the six English starters (1024, and 668 on Tasks'
Assignee Chooser and Add Assignee); no other key holds a layout's width.
FileMaker Pro sets it through `FMLayout::SetBaseWidth` [O: symbol].

[O, 2026-11-14, FileMaker Pro 26.0.3: Contact Details' right edge dragged
from 1024 to 900 pt on a fresh copy of the Contacts starter, the layout
saved; compared key by key with an untouched copy] Pro wrote:
- the definition `[4, 5, L, 02]` with `baseWidth` 900 and every other byte
  as stored, byte for byte what OpenMaker writes;
- the member's stamp, its count raised by 1;
- the object store `07` again, as on every layout save (10.7);
- the layout index's count `[4, 1, fc]` raised by 1, its user and time
  kept;
- the file's history `[2, 8]`, `[2, 9]`.

Its drag does not stop at the rightmost object: objects beyond the new
edge stay where they are.

OpenMaker writes the definition and the member's stamp as Pro does. It
keeps the object store and the index's count as stored (`docs/authoring.md`,
"Layout width"). FileMaker Pro opened OpenMaker's 900 pt file with no
alert, showed the edge at 900 pt, and saved a width change back.

[I] Not measured: Pro's bounds for the width, and what the index's count
does for FileMaker's clients.

[O, 2026-11-15, FileMaker Pro 26.0.3, three separate copies of the same
Contacts baseline, Contact Details saved and closed after one change each]
The index count also rose for three other layout-only saves:

| One Pro save | Closed-file object check | `[4, 1, fc]` | `[4, 5, 3, fc]` |
|---|---|---|---|
| Move the standalone First Name header text | Object 49's left and right moved 30 pt | `01 0d` to `01 0e` | `01 03` to `01 04` |
| Delete that text | Object 49 absent; its neighboring field and merge button kept | `01 0d` to `01 0e` | `01 03` to `01 04` |
| Place a new text | New object 51 holds `OM788` | `01 0d` to `01 0e` | `01 03` to `01 04` |

The intended object change was decoded from each closed file, so these
counts are measured for a move, a deletion and a new text, not inferred
from the width case ([#241](https://github.com/OpenMaker-Dev/openmaker/issues/241)).

### 10.13 A field's control

[O, 2026-11-14, the six English starter solutions' 156 field objects,
decoded and paired with their Save as XML] A field object's control is its
`displayType` (5.4) and its style's object type, together. Save as XML
names the control as the object's `type` and as `<Display Style>`; a list
control names its `<ValueListReference>`, the object's `valueListKey`.

| Control | `displayType` | Style type | In the starters |
|---|---|---|---|
| Edit Box | 0 | 61 (67 for a container) | 90, and 24 containers |
| Drop-down List | 1 | 62 | 13 |
| Pop-up Menu | 2 | 63 | 17 |
| Checkbox Set | 3 | 64 | 1 |
| Radio Button Set | 4 | 65 | none: FMUpgradeTool's write below |
| Drop-down Calendar | 6 | 66 | 11 |

- **Custom style names are per object type:** `fill-secondary-popup` only
  on pop-up menus, `fill-secondary-dropdown` only on drop-down lists,
  `fill-secondary-calendar` only on calendars, `fill-edit` and
  `border-edit` only on edit boxes.
- **Flags:** every drop-down list, pop-up menu and calendar has the field
  flags `0x400` (the icon) and `0x80000`; no edit box or checkbox set has
  either. They come with how those objects were made, not with the
  control: Pro's Inspector keeps the flags when it changes the control
  (below).

[O, 2026-11-14, FMUpgradeTool 26.0.3] A patch adding a layout to the
Assets starter with one field object in each of the six controls (the
same text field and value list for the five list and edit controls, a
date field for the calendar) writes each as the `displayType` given and the
style type in the table, 65 for the radio button set. It keeps the value
list on an edit box, and the flags as the patch gives them.
`--checkConsistency` reports no bad blocks.

[O, 2026-11-14, FileMaker Pro 26.0.3 on macOS, one change, compared key
by key with the untouched file] Pro's Inspector, changing the Contacts
starter's Last Name field object on Contact Details from Edit Box to
Pop-up Menu with the Title value list, rewrites that one object:
- `displayType` 2 and style type 63, in place;
- `valueListKey`, which the object did not have, inserted in field number
  order, right after `displayType`;
- the field flags kept as stored (`0x1200e0`, without `0x400` or
  `0x80000`);
- its bounds and every other byte as stored. The object had no custom
  style name.

Every other object, root key and field of the object store keeps its
bytes, but Pro writes the object definitions in an order of its own, not
the stored one. The layout member's modification count goes up by one,
and so does the layout index's (`[4,1]` `fc`). The modification counts
of seven image library items (`[31,5,n]`, library-images.md) go up by
two, their values unchanged [I: unexplained; the Layout mode session, not
the control, is the likely cause].

[I] Not measured: Pro's Inspector on the other controls, on an object with
a custom style name, and back to an edit box. OpenMaker writes the display
type, value list and style type as above, keeps the flags, keeps the
objects in their stored order, and drops the custom style names, so the
object takes its new type's default from the file's theme
(`docs/authoring.md`, "Field controls").

### 10.14 A stored Table View layout `[4, 5, L, 0d]`

[O, 2026-11-14, FileMaker Pro 26.0.3, #806: two blank files Pro made with
Create New compared leaf by leaf (research comment of 05:42Z), and one Table
View selection on each of several transplant files (walkthrough comments of
05:29Z-05:30Z, 05:45Z and 05:46Z)] Pro's Create New stores a table-view layout on its
blank file's layout by the end of creation, before the file is ever shown as
a table. The two blanks' subtrees are byte-identical (11 leaves), so nothing
in it is per file. Its members, relative to `0d` (sizes and parses observed;
roles marked):

| Path | Bytes | What it holds | |
|---|---|---|---|
| `01, 00, 00, <column>` | 0 | the column's field reference: occurrence 0, field 0, object 2 | O; role I |
| `01, <occurrence>` | list | the layout's own occurrence, empty, as on a layout without fields | O |
| `03, 00` | 2 | the last part key (`01 02`) | O |
| `03, 01, 02` | 85 | `PartDef`: type 1 (Header), offset 0, 24 pt, a style with a local background | O |
| `03, 01, 07` | 4 | the header's object list: none | O |
| `03, 02, 02` | 33 | `PartDef`: type 4 (Body), flags `0x200`, offset 24, 16 pt, the body and alternate body styles | O; `0x200`: U |
| `03, 02, 07` | 7 | the body's object list: object 2 | O |
| `05, 00` | 2 | the last object key issued: 2 | O |
| `05, 02, 0d` | 213 | the column's data format record (number, date and time settings) | O; role I |
| `07` | 691 | the object store: root 2; object 2 a field object (type 1) showing occurrence 0, field 0, bounds 24, 0, 40, 120, flags `0x34000080` (anchored left and top, `kEmptyColumn`, `kTableView`, 5.2.1), field flags 0, one repetition, keyboard and scroll bar 1, local styles | O; I: the table's "Create Field" column |
| `08` | list | named objects: none | O |
| `0a, 00 18 00 00 00 00` | 9 | the column in the object index (top 24, left 0, first) | O |
| `15, 00 01` | 9 | the column first in the tab order | O |

What it does [O, #806]:

- A layout with neither a field object nor a `0d` makes Pro quit when it is
  shown as a table, from the toolbar or View > View as Table (SIGABRT: an
  Objective-C exception rethrown out of the run loop; the abort message is
  not in the report).
- The same layout with Pro's `0d` copied onto it draws an empty table:
  one record, Create Field, no named column, as Pro's own blank does.
- A `0d` with Header and Body parts and an empty object store (`07` of
  length 0), which Pro wrote when the theme of such a layout was changed,
  does not help: Pro still quits. The column object is what counts.
- A layout with a field object can draw as a table without `0d`; Pro
  stores one at the first table visit. This does not ensure the right
  columns: a fresh open of the #815 OpenMaker file, with one field object
  and no `0d`, showed that field twice. No layout of the starter
  solutions stores `0d` (0 of 418), and each of them, but separators, has
  fields.
- Layout Setup's OK with Save Layout adds `16` and `fc` (2 bytes each) to
  the member and no `0d`; Table View Properties' OK writes none either.
- Create Field in Pro's table: the object store grows to two objects
  (1,333 bytes), the body's `PartDef` to 77 bytes, and each object gets its
  own 213-byte `05, <key>, 0d`; the object index and tab order gain an entry.
- A field Pro's Field tool places on such a layout, saved before any table
  visit [O, #815: `Alpha-placed-field.fmp12`, CreatedBy placed without a
  label on Pro's blank, against the blank]: Pro rewrites `0d`. The empty
  column 2 and its record `05, 02` are gone. The field's column 3 and a new
  empty column 4 follow, in that order, in the root stack, the body's list
  and the tab order (1, 2), with `0a` keys (24, 0, 0) and (24, 120, 1), the
  field references `01, <occurrence>, <field>, 03` and `01, 00, 00, 04`,
  a 213-byte `05, <key>, 0d` each, and the last key 4. The column: bounds
  24, 0, 45, 120; flags anchored left and top, `kTableView` and `0x400000`
  [I: `kSelected`, editing state, 5.2.1]; field flags `0x20`; `numOfReps`
  -1 [U]; local styles. The empty column: 24, 120, 45, 136 (16 wide), the
  flags of column 2. The body part is 21 pt high [I: the row Pro computes
  for the field's style]. A fresh open shows one column and Create Field.
  The field object on the form (34 x 252 pt) does not set the column's
  size.
- A field Pro places through Manage Database on an OpenMaker Plain file
  [O, #593 and #853, `593-create-new/made/field-height-2` to
  `field-height-5`]: Pro rewrites `0d` and `LayoutDef` with the header and
  row that follow the theme's **edit box** font.
  - **Edit box at 12 pt Arial** (the theme before #853's second change):
    - a 20 pt header and a 17 pt row: the header's `PartDef` 20 pt, the
      body's at 20, 17 pt, the column 20 to 37 and 0 to 120,
      `LayoutDef.tblCustomRowHeight` / `tblHeaderSize` 17 / 20;
    - the same for text margins of 0 to 5 pt, insets of 0 or 2 pt, vertical
      alignment top, and a line height of 1.4 lines.
  - **Edit box at 16 pt Arial** (`field-height-5/editbox-16pt`): 24 and
    21 pt, the column 24 to 45, as for Apex Blue's 16 pt Helvetica Neue.
  - **Cell text** is drawn at 12 pt either way. In the 2x captures the word
    is 73 px wide in both tables, against 97 px for the 16 pt Form text;
    Apex Blue's cells are 12 pt too. The 17 pt rows cut the bottoms of
    `Agjpqy`, the 21 pt rows kept them.
  - [I] Pro sizes the row from the edit box's font and draws the cells at
    the field's (12 pt here; a 12 pt default where the theme stores none,
    as in Apex Blue). An edit box no larger than its field text gets a row
    too short for its own cells.
  - Since #853's second change, OpenMaker Plain's edit box is 16 pt and
    OpenMaker stores a 21 pt row.
- With no stored `0d`, Pro shows a field placed on the layout as two
  columns of it, whether OpenMaker or Pro placed it, and writes a `0d` with
  both at the first table visit [O, #815 research comments of 06:05Z and
  06:11Z].
- Deleting that field object in Pro's Layout mode, the last field of the
  layout [O, #814: `Alpha-delete-last.fmp12`, against the placed-field
  file; F88]: the field's column 3 and the empty column 4 go, with their
  records, field references, `0a` and `15` entries; a new empty column 5
  stands alone at 24, 0, 45, 120 (full width again), first in the body's
  list, the object index (24, 0, 0) and the tab order, its reference
  `01, 00, 00, 05`, and the last key 5. A fresh open draws the table. Pro
  never leaves a layout without a `0d`: its New Layout (Computer, Form)
  also stores this one-placeholder record at creation
  (`Alpha-new-empty.fmp12`).

Native layout edits also maintain this record [O, 2026-11-14, FileMaker
Pro 26.0.3, local copies, #815 and #814]:

- Placing an existing Text field on a Pro-made blank's Form layout,
  saving and closing before any Table visit, rewrites its `0d`. The one
  empty column becomes a real field column and a new empty column. In
  this copy, `0d, 07` grew from 691 to 1,309 bytes; the real field gained
  a reference under `0d, 01`, and both columns had object-index,
  tab-order and attribute entries. On a fresh open Table View showed the
  one named field column and the `Create Field` placeholder. This case
  began with a stored `0d`; placement on a layout without one is a
  separate case (#815).
- Deleting that sole field object from another saved copy removed its
  main-layout field reference, object-index and tab-order entries. The
  main object store and body object list became empty. Its `0d` stayed:
  Pro removed the real column and rewrote the remaining placeholder as
  one empty column with a new object key. A fresh Table visit drew
  `Create Field` without terminating.
- New Layout/Report > Computer > Form, with no field placed, stored an
  `0d` on the new layout at creation, before any Table visit. It had the
  same 11-leaf, one-empty-column structure as the blank file's first
  layout, though two same-length payloads differed. The wizard allowed
  Form view only, so View as Table was initially disabled. Enabling Table
  View in Layout Setup on a test copy made the empty table draw.

These native routes to a fieldless layout kept a usable `0d`. Whether
Pro's UI can produce one without a field object or a usable `0d` by
another route remains unmeasured.

OpenMaker writes this record on the layout of a file it makes
(`layout_catalog::table_view`, docs/new-files.md) with its own encoders:
the members above, the parts styled by the theme's header, body and
alternate body styles and the column by its edit-box style (no local
styles), and no attribute record `05, 02`. The body's flags `0x200` are
kept as observed. Its row is 21 pt, not the blank's 16: the row Pro
writes for OpenMaker Plain's 16 pt edit box when it places a field (#853;
files made before store 17 or 16).

When OpenMaker places a field on a layout with a stored `0d`
(`layout_placement::plan`, #815), it writes the field's column as Pro's
Field tool does above: the empty columns taken out, the field's column
(120 wide, after the field columns there are) and a new empty column (16
wide) after it with the next two keys, their field references, body list
entries, the object index and tab order for the columns as they stand,
and the last key. It differs from Pro's: no `0x400000`; the theme's
styles; the row keeps the body's height as stored (21 pt in a file
OpenMaker made since #853's second change, 17 or 16 in one made before).
Its format records and `numOfReps` are below. Field flags `0x20` are kept
as observed.

When OpenMaker deletes field objects (`layout_objects::plan_deletion`,
#814), the columns of the fields no remaining field object shows are taken
out of a stored `0d` the same way, and the empty column renewed after the
columns kept: 16 wide beside columns, full width (120) alone, as above. A
layout left with no field object and no `0d` (a starter layout, which has
none) gets the record a new layout stores. A `0d` with no column (an
empty object store, as Pro writes one on a theme change, above) is given
the empty column in its body's row, both when a field is placed and when
the last field goes. A `0d` OpenMaker does not change (an object that is
not a top-level column, a column taken out before one kept) is kept as
stored while a field object stays, its columns still showing fields of
the table; when no field object would be left, the deletion is refused
instead, since Pro cannot show that layout as a table.

[O, #806 round 2, walkthrough comments of 05:45Z and 05:46Z] Pro draws the
table without the column's data format record: OpenMaker's encoding without
`05, 02` (file 5) and the same with Pro's 213-byte record added (file 6)
both open with no alert and show one record and Create Field, the same.

[O, 2026-11-14, FileMaker Pro 26.0.3, #886, `227-place-portal-tableview/`
and the twin pairs] Saving the layout does depend on the record. With Table
View turned on in Layout Setup and the layout saved, Pro rewrote an
OpenMaker-written view whose columns had no `05, <column>, 0d` with no
column at all: an empty object store, an empty body list, no `0a`, `15` or
`01` entries, and the last key raised. Pro then quit on View as Table, as
above. On its own twin, the same steps kept the field's column and renewed
the empty one. In five Pro-made files every column's record is the same
213 bytes (the Create New blanks Alpha and Beta Gamma, a third blank, New
Layout on the Tasks starter, the part twins' blank), and Pro copies it,
unchanged, to each column it adds and to the field object it places [O];
[I] its number, date and time settings follow the system's locale (English
on that Mac). Pro's field columns store `numOfReps` -1 (an `int32` in ten
varint bytes), its empty column 1. [I] Pro renews the empty column on
save from a column's record, so columns without one leave nothing.

The record's bytes are the `KFormat` streams of data-format.md: the
first two the text height (14 on Pro's blanks and the Tasks starter, 10 on
an OpenMaker-built Create New file Pro saved: the theme's, not the
locale's), then General numbers and as-entered dates and times, which is
the whole difference between the files. OpenMaker writes the record field
by field (`om_layout::data_format::DataFormat::encode`): on the empty
column of each view it makes (New Layout, Create New), and on each column
it adds and on the field object it places, the stored empty column's
record (else any column's) read and written again, refused when that does
not give it back byte for byte; else a new field's settings
(data-format.md, "A new field's record"), with a text height of 0. And
`numOfReps` -1 on a field's column. It leaves the stored row height (Pro wrote 21 points on the
starters' theme, and on a Create New file's 17 with a 12 pt edit box and
21 with a 16 pt one, #853) and the columns'
local styles, which Pro copies from the empty column with changes, as
stored [U: whether Pro needs them].

[U] What `0x200` means on the body part.

### 10.15 A part's page options

[O, 2026-11-14, the six English starter solutions' 66 parts, decoded and
paired with their Save as XML] A part's `<Definition>` names its
`PartDef`: `kind` the type, `size` the height, `absolute` the offset, and
`Options` the `flags`. Every starter part's `Options` is 0, but the
Details layouts' bodies, 1024 (`0x400`, 4.3); no starter part has
`pageBreakAfter`. A leading sub-summary's `<FieldReference>` names its
break field: in each of the five (Contact List, Content List, Task List,
Assignee List, Assignee Chooser) the field's id is the field segment of
`breakFieldKey` and the occurrence's id is the serial value of its
occurrence segment (`d0 00 02` is 1065090, `d0 00 04` 1065092).

[O, 2026-11-14, FMDeveloperTool 26.0.3's Save as XML of a Contacts copy
OpenMaker wrote] A body with `flags` 5 and `pageBreakAfter` 3 is written
`Options="5" restartEachGroup="3"`; a body with `flags` `0x540`,
`Options="1344"`; a sub-summary's new break field, its `<FieldReference>`.
Save as XML names `pageBreakAfter` `restartEachGroup` [I: whether it is
"Page break after every N occurrences", or ties to restarting page
numbers, is for FileMaker Pro twins].

[O, 2026-11-14, FileMaker Pro 26.0.3, six twins on fresh starter copies,
one Part Definition change each, compared key by key with the untouched
copy (the research thread's comment on [#71](https://github.com/OpenMaker-Dev/openmaker/issues/71))] On Contact Details' body,
`flags` went from `0x400` to:

| Part Definition checkbox | Bit |
|---|---|
| Page break before each occurrence | `0x1` |
| Page break after every N occurrences | `0x2`, and N in `pageBreakAfter` (0 to 3) |
| Restart page numbers after each occurrence | `0x4` |
| Allow part to break across page boundaries | `0x40` |
| Discard remainder of part before new page | `0x100` (with `0x40`: Pro enables it only once breaking across pages is on) |

On Task List's leading sub-summary, "when sorted by" Status to Category
changed one byte of `breakFieldKey`, the field segment `04` to `05`. Each
save also wrote the file's history (`[2, 6]`, `[2, 8]`, `[2, 9]`), the
layout index's and the member's modification counts and stamps, the
layout's object store `07` again, and seven image library items' counts.

Part Definition, on each part type selected (without saving), enables:

| Part type | Before | Every N | Restart | Break across | Discard |
|---|:---:|:---:|:---:|:---:|:---:|
| Title header (0) |  |  | yes |  |  |
| Header (1) |  |  | yes |  |  |
| Leading grand summary (2) |  | yes | yes | yes |  |
| Leading sub-summary (3) | yes | yes | yes | yes |  |
| Body (4) | yes | yes | yes | yes | once Break across is on |
| Trailing grand summary (6) | yes | yes |  | yes |  |
| Footer (7) |  |  | yes |  |  |
| Title footer (8) |  |  |  |  |  |

Before each occurrence and after every N exclude each other: enabling one
clears the other. N is 1 to 32,767: Pro refuses more with "The selected
value must be in the range 1 to 32767, inclusively." The alternate and
active row states are enabled for a body only; their storage is [U]
(4.3). A trailing sub-summary was not selected.

[U] Pro's untouched Contacts baseline and its Tasks twin hold a layout
member key `[4, 5, L, 16]` = `00 00` that the starter copies OpenMaker
reads lack; what writes it and what it means are unexplained. Operation
19 does not write it.

[I] Not measured: what Pro stores for N when "after every N" is turned
off, Discard on parts other than a body, and the trailing sub-summary's
options. OpenMaker writes the options in place, refuses those
(`docs/authoring.md`, "Part options"), and keeps the history, index
count, object store and library counts as stored.

### 10.16 A text's words

[O, 2026-11-14, the six English starter solutions' layout members, read
key by key] A text object's words are its attribute record's `01`
(`[4, 5, L, 05, O, 01]`), as #666's placement found for a label (10.5).
Of the starters' 132 text objects:
- 121 hold one plain value: SCSU, XOR `5a`, as a leaf. None of the 147
  such values in the starters (texts and button labels) holds a line
  break; the longest is 53 bytes.
- 11 are merge fields, in pieces under a 12-byte header (the SCSU length,
  the UTF-16 length and `4`): `<<`, the field's reference encoded with
  control characters, `>>`. Their `0d` is a full data format.
- None has formatting `ff` (styled text, rich_text.rs) or a text
  calculation `02`.

[O, 2026-11-14, FileMaker Pro 26.0.3, three twins off one Contacts starter
copy, each saved after one edit of Contact Details (layout 3) in Layout
mode, compared key by key with the copy before (the research thread's
comment on [#71](https://github.com/OpenMaker-Dev/openmaker/issues/71))] Retyping text object `0b` "Company" as "PROFILE" (7
characters to 7) and as "PROFILE INFORMATION" (7 to 19), and the header's
label `31` "First Name" as "Given Name":
- writes `01` again as one plain value (SCSU, XOR `5a`), the frame kept
  as stored even for the longer words;
- sets the object's flag `0x400000` (`kSelected`, 5.2.1): the object was
  selected when the layout was saved;
- writes the object store again (for the standalone text, in an order of
  its own, as on other layout saves), the layout's and the index's
  modification counts and stamps, the file's history (`[2, 6]`, `[2, 8]`,
  `[2, 9]`) and seven image library items' counts.

Nothing else of the record (`0d`, the name, the UUIDs) or of the member
changed.

OpenMaker writes `01` as Pro does, byte for byte for these three changes
(`om_scripts::text_node`: one leaf for a short single-byte text, else
pieces under a header), and keeps the object store, the index count and
the library counts as stored (`docs/authoring.md`, "Texts on layouts").

[I] Not measured: how Pro stores a long text, a line break, styled words
or a retyped merge field. OpenMaker refuses styled words, merge fields,
text calculations and line breaks.

### 10.17 A new portal

[O, 2026-11-14, FileMaker Pro 26.0.3, a twin pair off the English Tasks
starter (the research thread's comment on [#71](https://github.com/OpenMaker-Dev/openmaker/issues/71), read here key by key):
a blank Form layout `Portal Probe`, `[4, 5, 0d]`, saved; then one portal
drawn in its body with the Portal tool, related `Attachments` chosen in
Portal Setup with its other options at their defaults, only
`Attachments::Description` added in Add Fields to Portal, saved] The save
wrote, in the layout member:

| Key | What Pro wrote |
|---|---|
| `07` | the object store, empty before: root `1`; object 2 first, then object 1. Object 1 is the portal: type 9, flags `0x30400000` (anchored left and top, `kSelected`), bounds 158, 134, 433, 743, a local style holding only the rotation and type 83, `childkey` 2, and `portalObj` tableContextKey 0, tableAliasKey 1065091 (the occurrence `d0 00 03`), portalFlags `0x10`, initialRow 1, numRows 7.0, scrollbarStyle 1. Object 2 is an edit box at 4, 4, 35, 253, relative to the portal (249 by 31), flags anchored left and top, `FieldLayoutObject.flags` `0x20`, showing repetition 1 of field 2 of that occurrence, one repetition, the keyboard and scroll bar settings of a placed field (5.4) |
| `01, d0 00 03, 00, 01` | `09`: the portal's own reference, under field `00` |
| `01, d0 00 03, 02, 02` | empty: the field's reference |
| `05, 00` | `01 02`: the last key issued, the field's |
| `05, 01` | the portal's record: `d8` only |
| `05, 02` | the field's record: a 213-byte data format `0d` (as each table view column has, 10.14) and `d8` |
| `03, 02, 07` | the body's list: the portal alone (`00 00 00 01 02 01 01`) |
| `0a, (158, 134, 0)` | the portal's entry: no value; nested in it the field's entry at (163, 139, 1), value as a field's (10.5) |
| `15, 00 01` | the field first in the tab order, the same value |
| `16` | `00 00` (as Layout Setup's save wrote it, 10.14) |
| `0d` | the stored table view rewritten as for a field the Field tool places (10.14): a column 3 for `Attachments::Description` and a new empty column 4, each with its 213-byte format record, and the body 21 pt high; `LayoutDef.tblCustomRowHeight` 0 to 21 |

The field's index entry is at its drawn place: the portal's top and left,
plus the field's relative top and left, plus the portal's drawn top and
left borders (1 and 1 in the Tasks theme's new portal). The starters
agree [O, the six English starters, read-only]: on each of their 6
top-level portals every nested entry is 1 point below and 0 points right
of the portal plus the field's relative place, their style drawing a top
border and no left one, as `FieldBounds` places a portal's fields (5.4).
All 10 starter portals have the reference `01, <occurrence>, 00,
<portal> = 09`, flags `0x111` or `0x101` and 2 to 5 rows.

What OpenMaker writes (`om_data::layout_placement::plan_portal`,
`docs/authoring.md`, "Portals on layouts"): the same keys and values, the
store byte for byte Pro's on this layout but for `kSelected`, the
attribute records with an empty name and both UUIDs as for its other
placements, and not `16`, the table view's format records, its 21-pt row
or `tblCustomRowHeight`, as for a placed field. Several fields go side by
side in the first row, each 4 points apart and at most 249 wide, no
higher than 31 points or the row allows [OpenMaker's choice: the twin has
one field]. A FileMaker Server 26.0.3 `FMDeveloperTool` found no problems
in a copy of the before file with two such portals, and its Save as XML
read both back (2026-11-14).

[U] Not measured: how Pro lays out several fields, sizes a field in a
short row, places a portal with no field or over other objects, and what
it writes for a portal of an unrelated occurrence or the layout's own.

### 10.18 Adding, deleting and resizing parts

[O, 2026-11-14, FileMaker Pro 26.0.3, Part Setup's Create and Delete and a
dragged part boundary on Pro's Alpha blank, each saved and closed:
`227-parts/` (a Body-only layout) and `227-parts-objects/` (a Top
Navigation and a Body holding one field), compared key by key with their
before files] Each save changed, besides the stamps and the stored Table
View's renewed empty column, only the layout's parts `03`, and with objects
below a change, their frames in `07` and their `0a` keys:

- **Parts are keyed 1 to n from the top.** Adding a part re-keys the parts
  below it, and each part's whole subtree moves with it: a Header added
  above a Body took key 2 and the Body, with its `PartDef` and its object
  list, moved to key 3. `03, 00` holds the last key (`01 02` became
  `01 03`). The six starters agree: all 66 parts readable are keyed in
  order, and all 371 objects their lists name sit inside their part.
- **A new part's `PartDef`:** its type, flags 0, page break 0, its offset
  (the heights above it) and size, and its type's style object type
  twice: Header 32, Title Header 31, Leading Grand Summary 33, Footer 38
  (6.2); its object list empty (`00 00 00 00`). Pro made a Header 110
  points high and a Footer, Title Header or Leading Grand Summary 22.
- **The parts below a change move by it, and so do their objects:** a
  110-point Header moved the Body's field from top 158 to 268, in its frame
  and its `0a` key; a 22-point Title Header or Leading Grand Summary moved
  it 22; a Footer moved nothing.
- **Deleting** an empty Header gave back the before file's parts byte for
  byte. A part that holds objects is deleted with them, after Pro asks
  "Delete this part and all objects in it?".
- **Resizing** wrote the part's `PartDef` size alone (658 to 771). Pro
  stopped an upward drag of a Body holding a field at 135 points, the
  field intact; an empty Body went down to 58.

What OpenMaker writes (`om_data::layout_placement::plan_parts`,
`docs/authoring.md`, "Parts"): the same, byte for byte on all eight twins
but for the `kSelected` bit Pro clears on an object it saves unselected,
which OpenMaker keeps as stored. It refuses adding a sub-summary (it
needs a break field) or deleting one (a layout may have several), a second
part of a type a layout has once, deleting a part
that lists objects or the only part, a size that leaves an object below
its part's end or is not whole points, and parts not keyed and stacked as
above.

[U] How Pro chooses a new part's height, where its 135-point stop comes
from (the field ends at 192, 82 points into the Body), and what it writes
for a sub-summary or a part added between two existing parts of other
kinds.

### 10.19 A new web viewer's records

[O, 2026-11-26, FileMaker Pro 26.1.1 for macOS, #896; four copies of the
English Contacts starter, Contact Details, each with one web viewer drawn
with the Web Viewer tool at 40/300/440/500 points and set up in Web Viewer
Setup, then saved, closed and compared key by key with the starter and with
a copy Pro opened and saved without a viewer]

| Copy | Setup | `externalObj.flags` |
|---|---|---|
| W1 | Custom Web Address `"https://example.com/?q=" & Contacts::Website`; interaction, Find mode and JavaScript scripts on; progress bar, status messages and encoding off | `0x80e3` |
| W2 | Custom Web Address `"https://example.com"`, the dialog's defaults | `0x800d` |
| W3 | W1 opened again: address `"https://example.org"`, progress bar on | `0x80e7` |
| W4 | the Google Maps (US) template, its five fields set to a related occurrence's fields, the defaults | `0x000d` |

A new viewer's setup starts on Google Maps (US) with interaction, progress
bar, status messages and encoding on, and Find mode and JavaScript scripts
off. In all four the layout member gained:
- **The object**, on top of the root stack, as 9.6 describes it: type 13,
  key 51 (the next), flags `0x30400000` (anchored left and top, and
  `kSelected`, 5.2.1), the bounds, the style `{localStyles {self normal
  613: 0}, objectTypeId 86}` and `externalObj {WEBV, 0, flags}`. Pro
  rewrote the object store, and every other object's definition was byte
  for byte the stored one.
- **Its attribute record** `05, <viewer>`: the address under `00 00` as
  a formula node `05` with context `04` = `01 00`, the formatted stream
  `05` and the Polish stream `06` (for a lone text literal the two
  streams are the same bytes); the data format `0d` = `00 0a`; and a UUID
  `d8`. No name `10` and no `d9`, which every object of the shipped
  templates has.
- **Its indexes**: an entry at the end of the body's list `03, <body>, 07`,
  an object index entry `0a, <300, 40, 50>` with the nine-byte value
  every indexed object has, the same value in the tab order at one past
  the last entry (`15, 001c`), and `05, 00` raised to its key.
- **Field references** `01, <occurrence>, <field>, <viewer>` of value `0d`,
  one per field the address reads: one for W1, none for W2, five (to the
  related occurrence) for W4. W3's new address read no field, and W1's
  reference was gone; the viewer kept its UUID.
- Nothing else of the layout: no table view `0d` for it, and the layout's
  catalog entry `[4, 1, 07, L]` and `16` were byte for byte the starter's.
  The member's and the catalog's stamps changed.

Save as XML's `<WebViewer>` calculations matched the stored formatted
text, and recompiled to the stored Polish.

[I] `0x8000` marks a custom address: W2 and W4 have the same check boxes
and differ only by it, which explains why most stored viewers have it
(9.6).

[O, 2026-11-26, FileMaker Pro 26.1.1 for macOS, #896's follow-up: a blank
file, one viewer drawn with the Web Viewer tool at 40/300/440/500 points,
Custom Web Address `"https://example.com"`, the defaults, saved and closed
(`0x800d`); four copies of it, each with one check box changed in Web
Viewer Setup, saved and closed. Pro showed no warning or confirmation. The
object stores matched byte for byte apart from the flags, and the stored
calculation was the same in all five.]

| Copy | Change from the defaults | `externalObj.flags` |
|---|---|---|
| W5 | Allow JavaScript to perform FileMaker scripts on | `0x80cd` |
| W6 | Display content in Find mode on | `0x800f` |
| W7 | Automatically encode URL off | `0x802d` |
| W8 | Allow interaction with web viewer content off | `0x800c` |

Turning JavaScript scripts on alone set `0x0040` and `0x0080` together.
The other three each changed only their own bit and left `0x0080` clear.
With W1 and W3, every value Pro stored with JavaScript scripts on has
`0x0080`, and none without them does: an observation over these seven
values, not a rule found in code. [U] What `0x0080` does on its own,
whether turning JavaScript scripts off on a viewer that has them clears it,
the progress bar or status messages turned off alone, templates, and
combinations of several changes other than W1's and W3's.

[O, 2026-11-28, FileMaker Pro 26.1.1 for macOS, #896's native check] A
copy of the Contacts starter in which OpenMaker placed a viewer with
JavaScript scripts on (`0x80cd`, the address `"https://example.com"`)
opened with no alert. The viewer showed in Browse mode, Web Viewer Setup
showed "Allow JavaScript to perform FileMaker scripts" on, and Recover
reported 0 for all 215 of its codes.

What OpenMaker writes for a new viewer (`om_data::layout_placement::
plan_web_viewer`, `docs/authoring.md`, "Web viewers on layouts"): the
records above, but for `kSelected` in the object's flags, which no
placement writes (10.5), and the object store, to which the viewer is
appended with every stored byte kept. It writes only the option
combinations measured above (W1's, W2's, W3's and W5's to W8's flags)
with a custom address, and refuses the others; and only an address whose formatted
stream, printed with the file's names as Web Viewer Setup shows it and
compiled again as text with them, gives back the same tokens and the
stored Polish stream, as Pro's four did (names that begin or end a binding
with a colon are refused); whose fields are each stored with their occurrence (`TABLE_PRESENT`, as W1's),
and which calls no custom or plug-in function. [O, 2026-11-26] For W2's and W1's setups
on the same starter's Contact Details (at 40/300/320/500, clear of the
tab control), OpenMaker Server's records outside the object store were
byte for byte Pro's, but for the viewer's UUID and the stamp's user and
time. That FileMaker Pro opens such a file, shows the viewer and passes
Recover is not yet checked.

## 11. Installer "FileMaker Pro 26.pdf"

[O] `re/extracted/win/sfx/FileMaker Pro 26.0.3/Files/FileMaker Pro 26.pdf` is a
574-byte INI file, not a PDF document: a "Package Definition File" for managed
deployment (`[PDF] Version = 2.0`, `[Package Definition] Name = FileMaker Pro 26`,
`Version = 26.0.3.300`, `Publisher = Claris International Inc.`,
`[Setup] CommandLine = msiexec /m "FileMaker Pro 26.mif" /qn /I "FileMaker Pro 26.msi"`,
`UnInstallKey = {02B3FE9E-BE53-43EB-9A2E-19CD90831985}`). It is not a source of
layout documentation. Note its 26.0.3.300 version string versus the installer
name 26.0.3.307a.

## 12. Open questions

1. Z-order direction of `rootChildKey` / `childkey` (back to front assumed).
2. What the `LayoutObject.flags` bits known only by their DWARF names do
   (5.2.1: `0x20000`, the table-view and alignment bits), `FieldLayoutObject.flags`,
   `PartDef.flags`, `LayoutDef.flags` beyond the view bits, `portalFlags`
   (5.3.2: what `0x20`, `0x40`, `0x200` and the unnamed `0x2` do),
   `tabStyle`, `popoverObj.position`, button `displayType`, `dotObj.flags`.
3. `layoutType` values and the responsive layout model (`layoutClientType`,
   `maxWidth`, `FMLayout::OpenResponsive`).
4. Binary formats of the per-object records in 5.5 (calc storage, extended
   attributes) and of `KExternalOptions` for charts; external object
   four-char codes other than `WEBV` (web viewers: 9.6). (Script trigger lists: resolved in
   [file-metadata.md](file-metadata.md) section 7.)
5. Remaining text object/name storage variants and unmodeled styled-text
   attributes (`CharacterStyle::Read`, `ParagraphStyle::Read`); character
   attribute masks are resolved in 8.1.
6. Built-in default values per property (`GetDefaultValue`,
   `LayoutObjectFactory::GetDefaultStyles`, `DefaultStylesMapping`), needed when
   neither the object nor the theme sets a property.
7. Exact handling of the `active` / `activefocus` pseudo-classes and of the
   Classic-only properties when themes are compiled.
8. Component box layout when a theme gives no geometry (for example icon size
   and placement in drop-downs), and native-look controls
   (`-fm-use-default-appearance`).
9. Remaining binary image stream variants beyond the file-contained SVG library
   decoded in [library-images.md](library-images.md).
10. Server-to-client transport of layout records (10.2), for the network
    workstream.
11. The window-level switch with which FileMaker hides objects that have a
    hide condition without evaluating it (5.5.2).
12. The meaning of `LayoutDef` field 17 and of the middle `u16` in a
    layout's `0a` object index and `15` tab order when its record occurrence
    changes (10.11, [#243](https://github.com/OpenMaker-Dev/openmaker/issues/243)).
13. Whether FileMaker Pro computes a field object's content when its hide
    condition holds without evaluating `Self` (a summary over a large found
    set behind `Get ( FoundCount ) > 1000`), and when it computes it after
    the condition stops holding (#1023). Unmeasured; a hidden object shows
    nothing either way.
14. For the Layout Setup alert option (4.2), compare the not-modifiable
    field case and defaults on layouts created in a new file. The
    stored flag and zero-record click case are measured.
15. For the other 26.1.1 options (5.3.2, 5.3.4, 5.4.1): a layout saved by
    FileMaker Pro 26.1.1, to confirm the bits as stored; whether popover
    buttons offer the calculated icon; what FileMaker Pro 26.0 shows for
    each (the bits and `91` have no reader there [I]).

## 13. Implications for OpenMaker

What must be matched to render hosted layouts faithfully:

1. **Read the protobuf layout records** (`LayoutDef`, `PartDef`,
   `LayoutObjectStore`, `ThemeStyles`, `ThemeMetadata`) and keep unknown fields
   and unknown object types instead of failing, mirroring FileMaker's own
   forward-compatibility rule.
2. **Implement a numeric style engine, not a browser CSS engine.** Properties,
   components, states, and object types are closed enumerations (section 6).
   Resolve values with the exact cascade and state-fallback order of 6.7,
   including the object-type parent chain, named-style precedence, layout-level
   overrides, deleted custom styles, the icon-color special case, and
   `kBaseFontSize`.
3. **Use the theme stored in the hosted file**, not files from a Claris
   install. A parser for the theme CSS dialect (7.3) is still useful for tests
   and theme import, and the shipped CSS-to-protobuf pairs (6.1) make good
   golden tests.
4. **Box painting must reproduce `CSSBoxRenderer`**: per-side border colors and
   widths with corner radii (including percentages), border images (nine-slice,
   stretch or repeat, `fill`), outer and inset shadows with blur and spread,
   linear and radial gradients, the `inner_border` component, component geometry
   from absolute positions inside the object, 0.25 pt hairlines, and physical
   pixel snapping.
5. **Geometry**: point units with double precision, parts stacked by offset and
   size, child coordinates relative to their container, rotation, and anchor
   rules with ancestor constraints (5.7) when the window is resized.
6. **Interactive state model**: hover, pressed, focus, checked (active tab,
   segment, slide dot), placeholder, disabled, drop target, current and
   alternate rows, combined with the bit rules of 6.4.
7. **Text**: complex shaping, bidi, CJK, and vertical text through a modern
   shaping stack on Ubuntu, plus a font-substitution table for Mac and Windows
   family and PostScript names, because no fonts ship with FileMaker and hosted
   files reference Helvetica Neue, Arial, Tahoma, Hiragino, and MS fonts. Line
   height uses the `line` unit relative to font metrics; vertical text alignment
   and baseline shift are style properties. Expect small metric differences and
   plan tolerance-based visual tests against Mac and Windows screenshots.
8. **Per-object behaviors depend on calculations** evaluated at render time:
   hide conditions, conditional formatting, tooltips, placeholders, button
   labels, popover titles, tab labels, active segment, merge fields. The layout
   renderer therefore needs the calculation engine. How Browse treats a hide
   condition it cannot evaluate as FileMaker would (pending data, unread
   privilege metadata, an undecodable formula) is in
   [conditional-visibility.md](../conditional-visibility.md).
9. **Embedded content**: a web view for web viewers, a chart renderer that
   matches ChartDirector output closely enough, image and PDF rendering for
   containers and graphics, and single-color SVG icons recolored through
   `-fm-icon-color`.
10. **Verification oracle**: WebDirect on the test server emits browser CSS for
    the same style model (10.3). Capturing its CSS and screenshots for a known
    layout gives a second reference besides FileMaker Pro screenshots.

### 13.1 Browse paragraph leading probes

[O, FileMaker Pro 26 macOS arm64, 2026-10-30] A local scratch text object
containing four `Hgj` lines, with its saved CSS font and size decoded after
each capture, gives these pitches at 100% zoom:

| Arial size | Line multiple | Pitch in layout points |
|---|---|---|
| 10 | 1 | 11 |
| 16 | 1 | 18 |
| 18 | 1 | 21 |
| 18 | 0.8 | 16.8 |

This matches rounding the font's natural ascent-minus-descent-plus-leading
pitch to a whole layout point **before** applying the line multiple. Arial's
raw ratio is `2355 / 2048`; using rounded epaint metrics would incorrectly
round 10pt Arial to 12pt. Fractional leading accumulates without rounding each
row's pitch: the last case has 34, 33, 34 physical pixels between baselines
on the 2x display. The legacy `fontHeight` remained 10 while CSS sizes changed
to 16 and 18, so it is not a runtime line pitch.

The Browse renderer reads primary-face metrics using the same font parser as
epaint, caches at most 64 immutable font-data identities, and applies explicit
line multiples to that rounded pitch. No spacing data leaves epaint's natural
height unchanged. Absolute paragraph lengths and `em` lengths retain their
own units. Other fonts and paragraph/style precedence still need native
probes; these observations do not establish full text-layout parity.

## Appendix: key functions (FMEngine x86_64 unless noted)

| Address | Symbol | Used for |
|---|---|---|
| 0x2c979a | `Draco::FMLayout::LayoutDef::Read` | LayoutDef storage |
| 0x2c9904 | `Draco::FMLayout::LayoutDef::Deserialize` | LayoutDef fields |
| 0x2ceb54 | `Draco::FMLayout::LoadLayout(LayoutObjectStore&, LayoutPartList&)` | key map |
| 0x2db2e6 | `Draco::LayoutPartList::Load` | parts storage |
| 0x2da44e | `Draco::LayoutPart::Definition::Deserialize` | PartDef fields |
| 0x2dad14 | `Draco::LayoutPart::CalculateObjectType` (table 0x8e4a10) | part style types |
| 0x3ae2c0 | `Draco::LayoutObjectStore::Load` | object tree |
| 0x3dceee | `Draco::LayoutObjectFactory::Create` | wire type table |
| 0x394dae | `Draco::LayoutObject::Deserialize` | bounds, style, rotation |
| 0x3939a4 | `Draco::LayoutObject::GetConstrainedAutoresizingFlags` | anchors |
| 0x3235cc | `Draco::ObjectStyle::GetObjectType(LayoutObject const&)` | style type mapping |
| 0x3d22de | `Draco::FieldLayoutObject::GetDisplayType` | control styles |
| 0x324a9e | `Draco::ObjectStyle::GetValueBase` | cascade |
| 0x3229c4 | `Draco::ObjectStyle::TraverseInCascadingOrder` | cascade order |
| 0x3246ec | `Draco::ObjectStyle::GetValueWithStateFallbackForObjectType` | state fallback |
| 0x3456ea | `__GLOBAL__sub_I_Property.cpp` | property IDs |
| 0x34fa1a | `__GLOBAL__sub_I_ObjectType.cpp` | object type IDs |
| 0x358f80 | `__GLOBAL__sub_I_Component.cpp` | component IDs |
| 0x35d64a | `__GLOBAL__sub_I_State.cpp` | state IDs |
| 0x35d31a | `FMStyle::State::Transition::GetState` | runtime state bits |
| 0x403d40 | `FMStyle::Length::ParseUnit` | length units |
| 0x2d2422 | `Draco::FMLayout::IsViewStyleEnabled` (mask 0x89ace0) | views |
