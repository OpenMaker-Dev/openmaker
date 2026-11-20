# Portable Save Records as PDF (desktop first slice)

Script step 144 now writes an actual, searchable vector PDF from the current
layout. It supports the current record or the current found set in provider order.
It does not navigate the user's window to render records. A header uses the first
record on its page; a footer uses the last. Fields (including related fields,
calculations, globals and summaries) use the existing calculation host and the
same number/date/time display formatter as Browse. Calculation failures refuse
output rather than substituting blank fields. Record values use their calculated
text representation; preservation of the original numeric/date input spelling and
per-character record-value styles is not implemented.

`om_layout::pagination` plans physical pages in points, independently of the GUI
and of PDF encoding. Each placement identifies its source part, selected-record
index and physical top edge. The PDF backend consumes those placements, not a
screen image. [Preview](preview.md) consumes the same selected-page scene, including object
stacking, field bindings and measured text lines. Calculated Get(PageNumber) uses
the physical page context; advanced layout merge symbols remain refused.

## Supported layout and output contract

- One column, one body part, optional ordinary header/footer repeated on each
  page, first-page title replacements, once-only leading/trailing grand
  summaries and leading/trailing sub-summaries of sorted break-field groups,
  including nested breaks. See [report sections](report-sections.md) for
  anchors, group summaries, costs and limits.
  Bodies and summary parts remain whole and move to the next page when needed.
  Objects that slide up and parts they resize print at their measured heights
  ([report sliding](report-sliding.md)).
- Navigation parts do not print. A layout with Layout Setup's "Use fixed page
  margins" checked (bit `0x40` of its flags, [layout model](re/layout-model.md#42-views-form-list-table))
  prints within its stored margins (`customMargins`); fixed margins that
  leave no printable area refuse output. Every other layout prints within the
  printer's margins: 18 / 40 / 18 / 18 points (top, bottom, left, right), on
  every paper. A layout wider than the printable width prints what fits
  ([wide layouts](#wide-layouts)). FileMaker stores the margins with the box
  unchecked too: the starter layouts keep 36 points and FileMaker Pro 26
  prints them at the printer's margins on US Letter with Any Printer. With
  fixed 36-point margins, Pro moved the same Task List text 18 points right
  and down, as OpenMaker does (#609). The printer's margins for other papers,
  other printers and Windows are unmeasured; OpenMaker has no printer and
  uses the US Letter ones. A layout with the box checked and no stored
  rectangle prints at the printer's margins; that is OpenMaker's choice.
  [Page Setup](page-setup.md) supplies shared file-session paper/orientation.
  Before explicit settings, paper defaults to US Letter; layout orientation 0
  or 1 is portrait and 2 is landscape.
  Print Setup (42) restores supported common key `87` settings; opaque native
  records remain unapplied. Scaling other than 100% is explicitly unsupported.
- Top-level static text, single-repetition text/number/date/time fields, merge
  fields, rectangles, straight lines, plain ovals,
  [static layout pictures](#static-layout-pictures), and
  [buttons, their icons, and groups](#buttons-and-groups). Geometry is in layout points.
  Solid colors/borders, font size, regular/bold face, horizontal/vertical alignment,
  padding and measured line wrapping are supported. Header/footer/body backgrounds
  use the layout style cascade. A body prints in its own style on every record:
  where a body keeps "Use alternate row state" is not decoded (layout-model.md
  4.3), so alternate row fills do not print. The two starter bodies read in
  FileMaker, Task Details and Task List, have the option off. Fonts are
  covered in [report fonts](#report-fonts).
- Output paths use the transfer steps' platform path lists, variable expansion,
  relative Documents paths and Create folders convention. With dialog On (or no
  output path), a desktop modal offers the file as a save panel does (Save As
  and Where, opening on the step's own output file or Untitled.pdf in
  Documents), the record scope, Save and Cancel. The file chosen is a native
  path, given to the step in FileMaker's form (`files::chosen_path`, as Export
  Records' dialog does): `/private/tmp/report.pdf` on macOS is
  `file:/Macintosh HD/private/tmp/report.pdf` (script-runtime.md, Files). The
  panel is OpenMaker's own chooser, not the platform's, and was not compared
  with FileMaker Pro's.
  File > Save/Send Records As > PDF and the toolbars' Save as PDF run this step
  with its dialog ([desktop client](desktop-client.md#export-records-and-savesend-records-as)).
  Automatically open uses the existing system-open helper after successful saving.
- The report is fully calculated, planned and encoded before creating a staging
  file beside the destination. Only a completed file replaces the destination.
  Cancellation returns error 1, no records returns 401, unsupported content/options
  return 3, and save failures return 800. Other calculation/provider errors retain
  their codes. Normal script alerts include the reason; error capture suppresses
  the alert as for other script steps.

The supported slice refuses bodyless reports, responsive layouts, layouts set
to print in columns (Layout Setup's "Print in 2 columns", bit `0x400` of the
layout's flags; the column count and width FileMaker Pro keeps with it off
print in one column, #925),
nonzero part flags (except body bit `0x400`, which the starter solutions'
Details layouts set and FileMaker prints without visible effect,
layout-model.md 4.3),
malformed or unsupported sub-summary breaks and sorts, break
keys on other parts, unknown part options or explicit part breaks,
split/oversize parts, objects inside any container but a group, a portal or
a tab control's front panel, the tab controls [Tab controls](#tab-controls) lists,
the portals [Portals](#portals) lists, dynamic pictures, the button icons
[Buttons and groups](#buttons-and-groups) lists, option controls and repeating fields, rotated objects, the rounded frames
[Rounded frames](#rounded-frames) lists,
hide/label calculations, the conditional formatting
[Conditional formatting](#conditional-formatting) lists, layout merge symbols (including page
numbers), mixed rich text, italic/decorated text,
colors outside 0-1, unsupported glyphs/shaping, text overflow in anything but a
field that does not slide ([field text](#field-text)), and object/page overflow. Ordinary
flat character and paragraph styles, including uniform paragraph leading, can
be used. Common object flags other than the anchor, plain-text/tooltip,
tab-order (`kTabOrderOmit`, which nothing printed shows) and slide-up bits,
and on a line its start corner, refuse output. A line runs
between the corners its start bits name, as Browse draws it (layout-model.md
5.3.1), without Browse's half-width extension of its ends. Sliding left and Hide when
printing are decoded but refused ([report sliding](report-sliding.md)). The supported slice does not silently clip oversized
objects or drop records returned incompletely by the provider.

Target/container/variable/currently-open-PDF destinations, blank records,
email attachment creation, encryption/restricted permissions, metadata,
page ranges and page layouts and magnifications other than Default are
explicitly refused. With Restore On, slots 0-7 accept only their decoded
neutral numeric defaults. The other options:
- **Append to existing PDF** (`kOption`) writes one new document of the
  existing file's pages followed by the report's, which the PDF worker
  assembles as it does DynaPDF's documents ([plugins](plugins.md)). Each page
  is copied with what it refers to, except that a reference to the old
  catalog or page tree becomes `null`. With no file at the path it writes the
  report alone [I: FileMaker's answer there is unmeasured]. It refuses with
  3, and leaves the file as it was, rather than drop part of it:
  - a file the worker does not read as a PDF;
  - one whose catalog holds more than its page tree (bookmarks, an
    interactive form, a structure tree, names, page labels, an XMP metadata
    stream, an open action, a page mode, viewer preferences);
  - one whose last trailer holds more than `/Size` and `/Root`, read as
    written: document information of any kind, file identifiers (`/ID`),
    encryption, an earlier revision (`/Prev`), or any other key, however its
    name is spelled; and one whose cross-references are a stream;
  - one with optional content, or a page whose `/Parent` chain is deeper than
    the 64 ancestors the copy reads inherited attributes from;
  - an initial view other than Page Only, since the new document sets none;
  - more pages in the two documents than one assembly takes (100,000).

  OpenMaker's own reports have none of these, so saving and then appending
  in a script builds one document, but a PDF from FileMaker or another
  producer is usually refused. With a file to append to, it refuses without
  the PDF worker too. The existing file is read up to 512 MiB. The new file
  is saved only over the file that was read, by its size and modification
  time, or, with none, only where there still is none; a file that changed
  or appeared meanwhile is kept and the step fails with 800. Appends in one
  OpenMaker run one at a time, so two scripts appending to one file keep
  both reports. Another program that changes the file in the moment between
  that check and the save can still lose its change. FileMaker's own append
  was not compared.
- **Initial view (slot 8)** is honored in the PDF's catalog (script-steps.md
  11.7, 12.5):
  - 0 Page Only writes no `/PageMode`, the PDF default;
  - 1 Bookmarks Panel and Page writes `/UseOutlines`; the PDF has no
    bookmarks;
  - 2 Pages Panel and Page writes `/UseThumbs`.
- **Page layout (slot 9) and magnification (slot 10)** are accepted only at
  0 Default, which writes nothing.
- **The page-from calculation.** A step keeps it in slot 4 beside the print
  permission while All pages is chosen. It is accepted unused and not
  evaluated [I: FileMaker keeps From's default with All pages; its use then
  is unmeasured]. With a page range it still refuses output.
- **Everything else refuses output:** other values, the page-to position,
  and calculations at the metadata and password positions.

Restore Off ignores stale option slots, as native storage does. [Desktop Print](printing.md) reuses
this complete builder. New PDF building steps and a complete native printing UI
remain outside this feature. A privilege set that denies printing
disables Print, Page Setup and Save as PDF, and a script's Print and Save
Records as PDF then fail with 3, as in FileMaker Pro 26.0.3
([privilege sets](desktop-client.md#privilege-sets)).

This is synthetic acceptance, not a measurement of native FileMaker desktop
printing parity. FileMaker Pro 26's Preview and PDF of the starters' Task List
and Meeting List were compared, each with one record
([preview](preview.md#starter-list-pages-compared-with-filemaker)). Other layouts,
multi-page reports, native paper/printer acceptance and advanced typography
remain unmeasured.

## Page background

Each page is painted as FileMaker Pro 26's PDFs paint it (layout-model.md 4.3):
1. The layout background's fill covers the printable area.
2. The parts are drawn over it.
3. From the bottom of the last part down to the bottom of the printable area,
   the body part's fill continues, without its borders.

So a starter Details page is gray down to its bottom margin, as in Pro, and a
list page is white below its last row.

A part edge whose border style is unset draws no line, in Preview, PDF, Print
and Browse. The starter lists' headers and sub-summaries store a width and
color on all four sides but a style only on the top and bottom, and Pro
strokes only those two (layout-model.md 4.3). Browse following Pro here is
inferred. Layout objects still draw an edge with an unset style, as
before. When a page has a footer, OpenMaker
stops that fill at the footer and uses the body's fill even if a summary part
ends the page. Both choices are inferred, since Pro was measured only on pages
that end with a body.

## Field borders and fill

A layout that delineates fields on the current record only (Layout Setup,
layout option bit `0x1`, layout-model.md 4.2) prints its fields without
their borders or fill. Claris documents that with this option a field's
borders and fill show on the current record in List View, "but not when you
print, print to PDF, or preview the records being browsed"
([Showing field borders and fill for the current record](https://help.claris.com/en/pro-help/content/field-borders-fill.html)).
The field's text, a container's picture, and its content box (inset by the
borders) stay. A conditional format's fill or border is left out too. That
FileMaker leaves those out is inferred.

Measured in FileMaker Pro 26.0.3's PDFs (#263, [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201)), on starter layouts, all of
which set the option:
- the starter lists' field underlines do not print;
- Task Details' Priority, Status and Category pop-ups print no fill or border
  (their `fill-secondary-popup` style sets a light fill);
- the Details layouts' empty photo fields (`fill-container`) print no fill.

Claris also documents that fields in header, footer, summary and navigation
parts never show borders and fill. OpenMaker does not apply that rule yet. No
starter layout depends on it, since all of them delineate.

## Field text

A field's text is laid out inside its border widths and then its text
component's padding and margins (properties 101-104 and 105-108,
layout-model.md 6.5). Margins in em are
multiplied by the font size; absolute units are converted to points, and a
margin in percent or lines, or given as an automatic keyword, refuses the
report.

The starter lists' row fields set 0.875 em above, 0.625 em below and 0.25 em at
the sides: at 16 points, 14 / 10 / 4 points. FileMaker Pro prints that row text
lower than OpenMaker did before the margins applied (the walkthrough, [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201));
that these margins are the whole difference is inferred.

Text too tall for a field that does not slide prints cut at its text box,
instead of refusing the report. The text box is the field's frame less the
border widths, padding and text margins above and below, across the field's
width.
- The text is laid out from the top of its box, whatever its vertical
  alignment.
- A line that runs past the text box's bottom is cut there. Its text stays in
  the PDF.
- Lines that start below the field are left out, also from the PDF's text.
- A glyph wider than the text box takes a line of its own and is cut at the
  field's sides.
- Text whose exact metrics fit is still cut where its
  [rounded baseline](#where-text-sits) carries the last line's descent past
  the box.

Claris documents that only the data that fits within a field's boundaries
prints, and that sliding only shrinks a field
([Printing records](https://help.claris.com/en/pro-help/content/printing-records.html)).

Measured in FileMaker Pro 26.0.3 ([#201](https://github.com/OpenMaker-Dev/openmaker/issues/201), 2026-11-13):
- **Task List.** With the task "Prepare the quarterly budget review for the
  marketing team", Pro prints "…review for" on the first line and shows
  nothing of the second. Its PDF text still contains "the marketing team".
- **The clip path.** Pro's PDF of Task Details clips a field's text to the
  frame inset by the field's border and then by its text margins. The Task
  field's frame is (50, 137)-(341, 181), with a 1-point border and margins of
  14 / 4 / 10 / 4 points. Its text clip is (55, 152)-(336, 170).

So a 44-point starter field has a text box that ends 33 points down. The
second line's tallest glyphs start lower than that, so none of them shows.

On that field Pro's inset counts the border width stored for every side,
whatever the side's style says (layout-model.md 6.5). The starter edit boxes
store 1 point on all four sides but a style on the bottom only, and Pro's clip
is inset 1 point on all four. OpenMaker insets every field's text this way.

That this rule holds beyond the edit boxes measured is inferred: other widths
and styles, sliding fields, and borders a conditional format changes.

With Liberation Sans ([report fonts](#report-fonts)) OpenMaker breaks the first
line where Pro does, after "…review for". Lines Pro keeps in its text below the
second are unmeasured.
Text objects and button labels are cut the same way
([where text sits](#where-text-sits)). A sliding field's text, and every other
overflow, still refuse (error 3). A container's picture and file name keep
their earlier insets: text margins do not move them.

## Where text sits

OpenMaker places report text as FileMaker Pro 26 does on the starters
(layout-model.md 6.5, "Where text sits"):
- **Text boxes.**
  - A field's text box is its frame less its border widths, then its margins
    ([field text](#field-text)).
  - A pop-up menu counts only the sides whose border draws a line.
  - A field that shows its arrow or calendar icon (pop-up menu, drop-down list
    or calendar) also keeps the icon's stored width clear at the right: 30
    points on the starters. The icon itself does not print, as in Pro. So a
    starter list's Status value wraps and is cut 30 points sooner than an edit
    box's would be: Task List's Status text box is 192 points wide, not 220.
  - An icon width in em is multiplied by the font size; absolute units are
    converted to points. Any other relative unit, or an automatic keyword,
    refuses the report (error 3), as a text margin does.
  - A text object's is its frame less 1 point on every side.
  - A button label's is its frame less 2 points at the top and 1 at the left.
- **The first baseline** sits the font's ascent, rounded to a whole point,
  below the text box's top: 12 points for 13-point Arial, 14 for 16-point.
  Later lines follow at the same pitch as before.

Measured against Pro's PDFs of five starter layouts, 13- and 16-point Arial:
- every edit-box value, pop-up, drop-down and calendar value, label, group
  heading and header button label now starts where Pro's does, to 0.01 point;
- before, labels were 1 point left and 1.23 points high, button labels 1 point
  left and 2.23 high, and values 0.48 point low. Pop-up values were a further
  1 point right and low, and their text boxes 30 points too wide.

**Inferred:**
- the rule at other sizes and in other fonts, Inter included;
- where a long pop-up, drop-down or calendar value wraps. Pro's text box was
  measured, but no long value in one was printed;
- labels that are centered or right-aligned;
- button labels that are centered, bottom-aligned or beside an icon.

**A label that overflows its text box** is cut there, as a field's text is,
instead of refusing the report. Pro clips every label's text to its text box,
so overflowing text is cut there; no overflowing label was printed, so that is
inferred from the clip. A label whose text only just fit its frame's width
now wraps inside the narrower text box and is cut, as Pro's would be.

## Report fonts

Reports set text in one of two bundled font families and embed the whole font
file in the PDF:
- Text whose style's first font family is Arial or Helvetica prints in
  **Liberation Sans 2.1.5**, Regular or Bold. It is designed to be
  metric-compatible with Arial. The 39 character widths in Pro's embedded ArialMT
  (Task Details, [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201)) match Liberation Sans to within 1/1000 em. One wrap point
  was compared: Task List's long task name breaks after "…review for" in both.
  That other lines wrap and cut where Pro's do is inferred from those
  measurements. The starters' text is "arial,sans-serif" or "Arial" (405
  objects, 11 of them bold).
- All other text, including text with no stored family (24 starter objects),
  prints in **Inter**, Regular or SemiBold, as before.

Bold is weight 2, or 600 and up. Italic text still refuses (error 3): neither
family's italic faces are bundled, and no starter text is italic. Liberation
Sans stands in for Helvetica as well, though Helvetica's widths differ from
Arial's; that is inferred adequate, not measured. Browse is unchanged: it uses
the system Arial where macOS ships it and Inter elsewhere. Preview draws the
same bundled faces the PDF embeds.

## Wide layouts

Claris documents that only objects within the margins print, and that the
fix for a layout too wide for the page is to move its objects left of the
page edge or to print landscape
([FileMaker 12 help, troubleshooting layouts](https://help.claris.com/archive/help/12/fmp/en/html/create_layout.9.61.html)).
The starter solutions' layouts are 1024 pt wide (two are 668 pt), so on US
Letter they need this:

- An object whose left edge is at or beyond the printable width does not
  print, nor does anything it holds. A line on the edge itself prints the
  half of its stroke inside. It is not read or checked, so an object
  reports refuse (a portal, say) does not refuse the report there.
- An object that crosses the printable width prints cut at it, by the part's
  clip. Its text is still laid out in its whole frame, and the cut glyphs stay
  in the PDF's text.
- A layout with an object beyond the printable width and a sliding object in
  the same part is refused (error 3): whether the object left out still holds
  what slides, or resizes the part, is unmeasured.

The layout is not printed across extra pages. FileMaker cuts an object that
crosses the edge rather than leaving it out: Task List's Category heading
(layout x 547-772) prints cut at the 594-point printable edge in its Preview and
PDF (measured, #263), and OpenMaker cuts it there too. Other layouts are not
compared.

## Static layout pictures

A top-level Graphic object (type 3) shows the file-library image named by its
`libraryKey`, from the same `Layout.library_images` bytes Browse uses
([library images](re/library-images.md)). Preview, Save Records as PDF and Print
prepare one shared scene: no record, field or container content is fetched for a
static picture.

- Formats: embedded PNG, JPEG, GIF, BMP, TIFF and WebP through `image` 0.25;
  GIF/TIFF/WebP use the first static image, not animation or multiple pages.
  EXIF orientation is applied and samples become 8-bit RGBA with straight alpha.
  Embedded ICC profiles and gamma are ignored, as in Browse. Each side is at
  most 8192 pixels; encoded data and decoded samples are limited to 256 MiB.
- Static SVG has a separate `resvg` rasterizer: its own colors and alpha are
  preserved, without the button-icon tint path. It targets 288 dpi at the
  object's printed size, capped at 8192 pixels a side and 16 megapixels.
  Text uses the bundled fonts only: Inter where the SVG names no family, and
  Liberation Sans for any family that is not bundled (usvg falls back to a
  generic family, which would otherwise draw nothing). Browse draws the same
  SVG through `egui_extras` with no fonts, so its text shows in reports but
  not in Browse [I, OpenMaker; FileMaker unmeasured].
- SVG is drawn in a worker process, never in om-gui's: usvg and resvg
  allocate and recurse in ways no check before them bounds (reference
  cycles through clip paths, masks, filters and patterns; pattern content
  cloned per shape; arcs flattened into billions of segments; filter
  results at the filter's whole region). `om-gui-pdf-worker --svg`, the PDF
  worker's executable (#566), draws one picture with its allocator capped
  512 MiB past its input; om-gui kills it after 30 seconds, reads back at
  most the picture's pixels, runs one at a time, and takes any failure for
  an SVG that does not draw (error 3). Without the worker beside om-gui,
  SVG pictures refuse. Before building the tree the worker refuses input
  past 4 MiB (expanded for SVGZ) or 50,000 XML nodes, XML entity
  declarations, `<image>` (resvg is built without raster decoding, which
  Browse's SVG loader shares) and `use`/`feImage` references outside the
  picture. No network or filesystem resolver runs.
- Geometry: like Browse (egui 0.37 `Image::paint_at`), the picture stretches
  over the object's whole frame, without keeping its aspect ratio, cropping or
  centering. Root stacking, part clipping, part placement and page repetition are
  those of every report object. The object's own fill and borders paint below it.
- PDF: each distinct picture is embedded once per document as an 8-bit
  DeviceRGB, Flate-compressed image XObject, with a DeviceGray soft mask when any
  pixel is transparent; it is drawn with `cm`/`Do` into its frame, top row
  first. Preview uploads the same decoded samples as a texture (premultiplied
  once by egui). Resampling of enlarged pictures is viewer-dependent; Preview
  filters linearly like Browse.
- Decoded pictures are cached per owner: once per generated document, and per
  Preview session for the displayed layout snapshot (cleared on layout changes).
  The retained static-picture cache is capped at 512 MiB of encoded bytes and RGBA. SVG entries
  also include their raster size. Entries are reused only for identical encoded bytes, so a library key reused by
  another file, layout or snapshot never shows stale pixels.
- Refusals (error 3, before any output file or print job): a missing library
  entry, damaged or oversized image data or an SVG resource/budget refusal, a zero library
  key, a nonzero `mixColor` or undecoded GraphicLayoutObject fields, and the
  existing rotation, nesting/group, hide/label/conditional and object-flag
  guards. As in Browse, `pictFormat` is not interpreted (it appears to name the
  stored stream format, which is unverified); the stream signature decides.
  Preview reports a picture larger than the display's texture limit as
  unavailable.

Popover button icons and FileMaker's fit, crop and tint of a
static picture remain outside this slice. Native FileMaker output acceptance
for pictures is unmeasured.

## Container fields

A container field object prints what Browse shows for it
([desktop client](desktop-client.md#browse-mode)): its picture placed by the
object's graphic format (crop, reduce, enlarge, proportions, alignment;
[data-format.md](re/data-format.md#graphic-format)) and clipped to the field's
content box (inside each side's border width, drawn or not, and its
padding), any other file as a document icon above its name in the field's
text style, and data that does not decode as an empty field.

- The data is read for the printed record: Save Records as PDF and Print read
  the library member through the script host, waiting for it; Preview reads it
  through the window's calculation cache and shows "Loading report values…"
  until it arrives. A read that fails refuses the report (error 3 or the
  provider's code); it never prints an empty field in its place.
- Pictures (PNG, JPEG, GIF, BMP, TIFF, WebP and the preview FileMaker stores
  beside a PDF or HEIC picture, containers.md 6.2) are decoded with Browse's
  limits (16,384 pixels a side, 256 MiB), turned by their EXIF orientation,
  and kept at most at 300 pixels an inch of their printed size and 8,192
  pixels a side, once per data, frame and format in a document or Preview
  session. One picture pixel is one point before scaling, as in Browse.
- A container field keeps its frame when it slides: it does not shrink.

FileMaker's own printed containers were not compared; a GIF prints its first
frame; PDFs without a stored preview, movies and sounds print as files.

## Buttons and groups

An ordinary button (layout object type 10) prints as Browse draws it: its
style's frame and fill, then its icon, then its label, laid out and
measured like a text object's (the label may hold merge fields). A group
(type 8, including a group that acts as a button) prints its own frame, then
its members, depth first in `childkey` order, each at its group's origin
plus its own bounds, as Browse stacks them. Groups may hold groups. The
button's script step and the group's are not used when printing.

A button's icon (its `libraryKey`'s library image) prints as Browse paints
it when its display type is 1 or 4 (layout-model.md 5.6.1). That covers all 30
of the starter solutions' icon buttons within US Letter's printable width: 29
have 1, Button Setup's "Image only", and Add Assignee's labelled button has 4,
"Icon at left, text at right".

- A square of the icon size, at most the frame's smaller side.
- **Type 1:** the square sits alone in the middle of the frame, and a label the
  object stores does not print. FileMaker Pro 26 prints Task Details' "Add
  Assignee" button this way: the "+" alone ([#201](https://github.com/OpenMaker-Dev/openmaker/issues/201)).
- **Type 4:** the square is centered 6 points plus half its side from the
  frame's left edge, and the label is laid out from the icon size plus 10
  points.
- An SVG is tinted as Browse tints it (the icon component's color, else
  the frame's top border color, fills its `fm_fill` shapes) and drawn by
  the renderer Browse uses (resvg, through `egui_extras`), stretched into
  the square. It is embedded as a picture at 4 pixels a point, not as
  vectors. A PNG, JPEG, GIF, BMP, TIFF or WebP icon is decoded as a static
  picture, untinted.
- Conditional formatting colors the frame and label, not the icon, as in
  Browse.

Still refused (error 3, before any output file or print job):

- an icon placed above, below or right of its label (display types 2, 3
  and 5 as inferred in layout-model.md 5.6) or with any other display type,
  which Browse does not draw either, an icon missing from the library or that does not
  decode, a button that contains objects, and a button or group with fields
  this version does not decode;
- popover buttons, button bars and every other container: an object inside
  anything but a group, a portal or a tab control's front panel refuses
  the report ([Portals](#portals), [Tab controls](#tab-controls)), as does
  an object no container walk reaches (a containment cycle);
- a group, or a group's member, set to slide up, and a member whose top is
  in another part than its group's: how FileMaker slides grouped objects is
  unmeasured;
- what refuses any object: rotation, hide/label calculations, unknown
  flags, the rounded frames [Rounded frames](#rounded-frames) lists and the
  conditional formatting [Conditional formatting](#conditional-formatting)
  lists.

That FileMaker Pro prints buttons and groups this way is inferred from the
model and the Browse rendering, not compared with FileMaker's output.

## Portals

A portal prints the rows it shows without scrolling, as Claris documents
for printing ("Printing records": only the data visible without
scrolling prints), laid out as Browse lays them out:

- Rows sit inside the frame's borders, each the frame's height less its
  top and bottom borders over its number of rows. The first row shows the
  related record the portal's initial row names; the scroll bar does not
  print.
- Each row is filled in the portal's row style (component 12), an
  alternate row also in its alternate style (component 13) when property
  610 is set, and its borders in that style are drawn after every row and
  its objects; a row with no related record prints its fill and borders
  only. A row only partly inside the frame prints none of the objects that
  would cross the frame's bottom (Browse cuts them there).
- The portal's objects, which must lie in its first row inside the frame,
  print in each row with a related record, moved by the frame's left and
  top borders and the row; their fields, container fields, merge fields,
  conditional formatting and `LayoutObjectUUID` read the row's record
  (`Context::with_portal`, row number from 1), as a Browse portal row does.
- Related records come in the relationship's order: the script host's
  related records for PDF and Print, the window's portal records for
  Preview.

Refused (error 3, before any output file or print job):

- a portal that sorts (flag `0x8` with a stored order) or filters its rows,
  whose order is not read for reports yet;
- a portal of the layout's own table, which shows the found set;
- a portal or a portal's object set to slide up, a group or portal inside
  a portal, an object outside the first row, and portal options this
  version does not decode.

That FileMaker Pro prints portal rows this way is inferred from Claris's
help and Browse; its printed output has not been compared.

## Tab controls

A tab control prints the panel its window shows, as FileMaker prints the
front tab: the one picked in Browse (Preview, its page plan included, reads
the window's choice, Print and Save Records as PDF the script's window
snapshot; a window New Window opens starts with none picked), else, as
Browse first shows it, its default panel, else its current one, else its
first.

- The control's frame, then the front panel in its active state (7), what
  the panel holds, and then every tab: each at its stored left edge and
  width, the control's tab height high, in its panel's style, active for
  the front tab and normal for the others, rounded by the panel's equal
  corner radii as other frames are, its label centered in the panel's
  text style (else the control's size and color).
- Other panels and what they hold are neither printed nor checked, as
  Browse paints only the shown panel: an object a report would refuse
  there does not refuse it.

Refused (error 3, before any output file or print job): slide controls, a
tab control or panel that slides up, a tab control inside a portal, a
control holding anything but panels, and tab control or panel options this
version does not decode.

That FileMaker prints the window's front tab is inferred from its
display; a panel a script brings to the front with Go to Object is the
front one from that step on. FileMaker's printed tab output has not been
compared.

## Conditional formatting

A text object's, field's or button's conditional formats print as Browse
shows them, record by record:

- Each condition is evaluated on the record the object prints for (in a
  header the page's first record, in a footer its last, in a sub-summary
  part its group's first or last record), with the report's page number.
  `Self` is the field's value for a field, otherwise the object's printed
  text, and `LayoutObjectUUID` is the object's, as in Browse.
- The formats whose condition holds lay their styles over the object's, in
  stored order: fill, borders, and the text's font, size, color and weight,
  which also replace a stored character style's, as Browse lays them. A
  condition that gives an error or `?` holds, as in Browse.
- Preview evaluates them with the window's calculations, PDF and Print with
  the script host's: the same formulas on the same records. `Get (
  WindowMode )` is 2 in Preview and 3 in PDF and Print output (Claris: 3
  while printing is in progress; that a PDF counts as printing is
  inferred), so a format such as the starter solutions' `Get ( WindowMode )
  = 0` shows in Browse only.

Refused (error 3, before any output file or print job):

- conditional formatting on any other object (a rectangle, line, oval,
  picture or group);
- on an object set to slide up: pages are planned without conditions;
- a condition whose style sets corner radii;
- what a condition makes unsupported elsewhere, such as italic text.

That FileMaker Pro prints conditional formatting this way is inferred from
the model and Browse; its printed output has not been compared.

## Web viewers

OpenMaker does not print a web viewer's page (#278). So that a layout with a
web viewer still prints, and the gap shows on the page rather than as a blank
box, a web viewer prints:
- its own frame and fill;
- the caption "Web viewer: OpenMaker does not print web pages." on a plate,
  styled as Browse labels a viewer it shows no page in: 12-point caption gray
  text on a light plate with a hairline border, 6 by 3 points of padding, 4
  points above the viewer's bottom edge, wrapped to the viewer's width.

The plate is centered on the part of the viewer within the printable width,
so a viewer that crosses the page edge still shows it. That is the case for
Contact Details in the Contacts starter on US Letter. A viewer too small for
the caption refuses the report (error 3). So does a web viewer in a portal
row. Charts and plug-in objects, the other external objects, stay refused.

What FileMaker prints for a web viewer (presumably the page it shows) has
not been measured. The caption is OpenMaker's own, not FileMaker behavior.

## Rounded frames

A frame's corner radii are style properties 32-35, one per corner
(layout-model.md 6.5), each a length or a horizontal and vertical pair
(`coordVal`, which the starter solutions' themes use; a pair of equal radii
is one circular radius). When all four corners share one absolute radius (pt,
px, cm, in, mm or pica; a corner without one counts as 0), the report draws
the frame as Browse does: the fill takes the rounded shape, and the border
is stroked once along the corners, inside the bounds. A radius larger than
half the frame's smaller side is reduced to it, as CSS does. This applies to
text objects, fields, rectangles, buttons and groups; theme buttons usually
have rounded corners. Preview draws the same radius, rounded to whole points
as Browse's frames are.

Refused (error 3, before any output file or print job):

- corners of different radii;
- an elliptical corner (a pair of different radii);
- a radius in `%`, `em` or `line`, or an automatic value;
- a rounded frame whose border is not the same width, color and solid style
  on every side;
- a rounded frame whose border is as wide as, or wider than, its smaller
  dimension (OpenMaker refuses a collapsed or reversed inset);
- a rounded frame on a picture, and a rounded part.

Rounded-rectangle objects (type 4) stay refused, as other unsupported kinds
are. That FileMaker Pro prints rounded frames this way is inferred from the
model and Browse; FileMaker's printed or PDF output has not been compared.

## Translucent colors

A translucent fill, border, line or text color prints blended over what is
beneath it, as Browse draws it. Browse's colors keep 8 bits of opacity, and so
do reports: the alpha is rounded to 255ths.
- **PDF and Print.** Each translucent paint operation runs in its own saved
  graphics state, with an `ExtGState` setting that opacity (`/ca` and `/CA`).
  The page lists one such state per opacity it uses. Later drawing is opaque
  again.
- **Preview** paints the same colors with egui's alpha blending.
- Fully transparent colors draw nothing, as before. A color with a component
  outside 0-1 still refuses (error 3).

The starters' case is Meetings > Meeting Details: its DURATION button fills at
50% while `IsEmpty ( Duration )` holds (#622). That FileMaker prints it blended
is inferred from Browse; its printed output has not been compared.

## Focused acceptance

`cargo test -p om-gui --lib pdf_` exercises the real desktop runtime/host, generates
found-set (three pages) and current-record (one page) output from synthetic data,
keeps the selected record/index, and verifies cancellation, an append
without the PDF worker and unsupported security options preserve existing
output (`tests/script_engine.rs` appends with the worker). The found set is deliberately shuffled;
fields include a calculation using `Get ( RecordNumber )` and a formatted currency
value. A second check refuses text overflow and a summary part. A small pagination
unit check verifies repeated parts, whole-record transitions and oversize refusal.
The grouped check saves `grouped-sub-summaries.pdf` (and a `grouped-sorted.pdf`
copy of its sorted three-page output) before the unsorted and refused runs.

`cargo test -p om-gui --lib pictures` covers static pictures with anonymous
pixels generated in the test: a transparent PNG over a colored frame below a
label and a stacked patch, an asymmetric JPEG stretched into every body, and the
PNG repeated in each page's footer. The three-page PDF and the fake Print job are
byte-identical, embed one PNG (with soft mask) and one JPEG, and are rasterized
in-process with `hayro` to check known pixels for alpha, orientation, stacking
and placement; headless Preview checks the same points on its first and last
pages. Damaged and missing pictures leave the saved PDF unchanged and spool
nothing; tinted and nested pictures refuse, as do SVGs with images, outside
references or entities, while SVG text in a family that is not bundled still
draws (unit tests draw SVG in-process). `cargo test -p om-gui --test
report_svg_worker` drives the worker executable: an SVG comes back drawn at
the size asked for and at its own, and a clip-path cycle, nested pattern
clones, a huge arc and a huge filter region each fail in the worker while
the test process carries on. Decoding is compared with
Browse's loader, including an EXIF-rotated JPEG, and reused library keys with
other bytes are decoded again.

Set `OM_PDF_ARTIFACT_DIR` to retain those PDFs; otherwise the test uses an owned
temporary directory. Render every page with `pdftoppm`, inspect the page transitions
and repeated parts, and use `pdfinfo`/`pdftotext -layout` to confirm page count,
record order, calculated values and embedded Unicode text. No customer layouts or
records are used. GUI/workspace tests require the normal xvfb environment on Linux.

## Primary references and dependencies

- [Claris Save Records as PDF](https://help.claris.com/en/pro-help/content/save-records-as-pdf.html)
  describes current layout formatting, record scope, paths, dialog/cancel and output
  options. Native slot/chunk evidence is in `docs/re/script-steps.md` sections 11.6-11.7;
  this feature deliberately does not reinterpret platform PrintSpec chunks.
- [pdf-writer 0.15.0](https://docs.rs/pdf-writer/0.15.0/pdf_writer/),
  [font API](https://docs.rs/pdf-writer/0.15.0/pdf_writer/writers/struct.Type0Font.html)
  and [license manifest](https://docs.rs/crate/pdf-writer/0.15.0/source/Cargo.toml.orig):
  MIT OR Apache-2.0. Writes page trees, paths, content streams and embedded CID fonts.
- [ttf-parser 0.25.1](https://docs.rs/ttf-parser/0.25.1/ttf_parser/struct.Face.html):
  MIT OR Apache-2.0. Measures the same glyph advances used by the PDF font widths.
- [tempfile 3.27.0 persist](https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist):
  MIT OR Apache-2.0. Stages output in the destination directory and atomically replaces
  it. The output file is synced before replacement; directory crash durability is
  not promised.
- Bundled Inter font license: `crates/om-gui/assets/fonts/Inter-LICENSE.txt` (OFL 1.1).
- Bundled Liberation Sans font license: `crates/om-gui/assets/fonts/LiberationSans-LICENSE.txt` (OFL 1.1).
