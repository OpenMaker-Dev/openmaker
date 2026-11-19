# Plug-ins

OpenMaker loads no FileMaker plug-in: there is no plug-in API, installer or
`Install Plug-In File` ([#91](https://github.com/OpenMaker-Dev/openmaker/issues/91)). A calculation that calls a plug-in function
evaluates as it does in FileMaker Pro without that plug-in, to `?`.

The exception is a bounded subset of the MBS FileMaker Plugin's commands,
implemented clean-room from the plug-in's public documentation
(re/mbs-plugin.md), so that a script can run a script after an idle period,
ask with a simple alert, make a folder on the user's desktop, and make a PDF
of pages of other PDFs, the commands observed in a hosted solution's
scripts.

## What runs

In a script on the desktop (`om_scripts::runtime::plugins`, with the
desktop's clock and alert in `om-gui`'s `plugin_ui`):

- `FM.RunScriptIdle ( Delay ; FileName ; ScriptName or id {; Parameter } )`
  schedules the calling file's script, by name or id. FileName must be the
  calling file's own name (its `Get ( FileName )`, compared without case).
  Another file's script is refused with an error rather than looked up or
  opened (OpenMaker's choice; the plug-in runs it). It replaces any
  scheduled before.
  - The script runs once nobody has moved the pointer, typed, clicked or
    scrolled for the delay in any of the app's document windows, and no
    script is running. Input in the Script Workspace's own window does not
    count (OpenMaker's limit).
  - It runs in the window whose script scheduled it, with that window's
    layout and record, and gets the parameter as `Get ( ScriptParameter )`.
  - It runs once; the scheduling script schedules it again (inferred,
    re/mbs-plugin.md 2).
- `FM.StopScriptIdle` cancels it.
- `Dialog.Reset`, `SetMessage`, `SetMode`, `SetTimeout`,
  `SetDefaultButton` and `SetAlternateButton` build the alert, and
  `Dialog.Run` shows it.
  - It is modal, with the default button rightmost; Return chooses it.
  - `Dialog.Run` returns the label of the button chosen, or `Timeout` when
    the timeout closes it.
  - An empty default button is "OK".
- Each command returns `OK`, or an error text beginning `[MBS]`.

- `Folders.UserDesktop` returns the desktop folder as a native path
  (`/home/user/Desktop`, `C:\Users\user\Desktop`), without a closing
  separator: the folder `Get ( DesktopPath )` names. Without one, it
  returns an error.
- `Files.CreateDirectory ( Path )` creates the folder at a native path,
  with the folders above it that are missing, through the desktop's own
  file operations for scripts (`om-gui` `scripting/files.rs`). A folder
  already there is `OK`. `.` and `..` are resolved on the path's text
  first. It returns an error, and makes nothing, for:
  - a relative path, a NUL, or `..` above the root ("Invalid path.");
  - a file at the path or above it ("Failed to create directory.").
- `Files.Delete ( Path )` deletes the file at a native path, or the files
  of a list of paths, one per line, in turn, stopping at the first error.
  It returns `OK`, or an error, deleting nothing more, for:
  - a relative path, a NUL, `..` above the root, or a path ending in `//`
    or `\\` ("Invalid path.");
  - a file that does not exist ("File not found.");
  - a folder ("OpenMaker deletes files, not folders.");
  - a data file a script of any open database has open, however the path
    names it ("The file is in use."). Off Unix, where a file's identity is
    not at hand, any open data file refuses it.

  A link is deleted itself, not what it names.
- `Path.FileMakerPathToNativePath ( Path {; PassThroughNativePath } )`
  returns the native path of the first line of a FileMaker path list that
  is for this platform. It reads that line by the rules path lists follow
  in script steps:
  - `file:` (or no prefix) and this platform's own prefix apply;
  - another platform's prefix is skipped, never read by this platform's
    rules, so a `filemac:` path on Linux or Windows is no path at all;
  - a list without a line for this platform returns "No path for this
    platform.";
  - a relative path returns "Invalid path.";
  - a closing `/` stays.

  With PassThroughNativePath 1, the default, a line without a prefix that
  is already a native path, naming something that exists or in a folder
  that exists, is returned as it is. On Windows, a line in Windows' own
  form after its prefix (`filewin:D:\test\a.jpg`, as the plug-in's
  examples write it) rather than FileMaker's (`filewin:/D:/test/a.jpg`)
  is not converted (OpenMaker's limit, unmeasured on Windows).
- `Text.ReplaceNewLine ( Text ; Mode {; Wrap } )` (or by its old name,
  `String.ReplaceNewline`) returns the text with each line break, CRLF,
  CR or LF, replaced as Mode says:
  - 0 removes it;
  - 1 CR, 2 LF, 3 CRLF;
  - 4 `<br>`, 5 `<br />`;
  - 8 more also removes a line break that follows another.

  The text ends at its first NUL. It works in any calculation, as it
  changes nothing but its result. Mode and Wrap are read as whole numbers
  the way FileMaker compares numbers, at their display precision (so
  `( 1 / 3 ) * 3` is 1). A Wrap other than 0 (empty is 0), text that is
  not a number included, is refused, and so is any other Mode, including
  a fraction such as 2.5.

DynaPDF's commands that make a document of pages of other PDFs and save it
are answered by OpenMaker's own engine (`om_scripts::runtime::dynapdf`):
- `DynaPDF.New` opens a document and returns its reference number, from
  20000 up; `DynaPDF.Release` closes it.
- `DynaPDF.OpenPDFFromFile` (a native path) and
  `DynaPDF.OpenPDFFromContainer` open the PDF the imports read, and return
  0.
- `DynaPDF.ImportPDFPage ; PDF ; Page` appends that page and returns `OK`.
  `DynaPDF.ImportPDFFile ; PDF ; GetPageCount + 1` appends every page and
  returns the last page number. `DynaPDF.GetPageCount` counts the document's
  pages.
- `DynaPDF.OpenOutputFile ; PDF ; Path` names the file, at a native path,
  that `DynaPDF.Save` writes; Save returns `OK`.
- On a page `DynaPDF.EditPage ; PDF ; Page` opened (until
  `DynaPDF.EndPage`; an import and Save close it first), text is written:
  - `DynaPDF.SetFont ; PDF ; Name ; Style ; Size {; Embed ; CodePage}` sets
    the font and returns its handle. Arial and Helvetica, regular (`0`) or
    bold (`2`, `"bold"`), are set in Liberation Sans, of the same metrics,
    and embedded, as reports set them. CodePage 1252 or Unicode.
  - `DynaPDF.SetFillColor ; PDF ; R ; G ; B`, components from 0 to 1, sets
    the text's color; `DynaPDF.RGB ; R ; G ; B` packs one, `Blue * 65536 +
    Green * 256 + Red`. `DynaPDF.SetLineWidth` sets the width of the
    borders of the form fields and web links made after it (1 at first).
  - `DynaPDF.SetTextRect ; PDF ; X ; Y ; Width ; Height` sets the rectangle
    `DynaPDF.WriteFText ; PDF ; Align ; Text` writes into;
    `DynaPDF.WriteFTextEx ; PDF ; X ; Y ; Width ; Height ; Align ; Text`
    names its own. Y is the rectangle's top edge, in the page's bottom-up
    coordinates; a height of -1 is no limit.
    - Text aligns left, center or right.
    - Lines break at each line break and between words at the rectangle's
      width. A line wider than the rectangle, a word longer than it or the
      spaces before a word, is refused.
    - Lines are spaced by the font's ascent less its descent, never less
      than its size, nor so little that a line reaches into the one above.
    - Within the text, `\FS[size]`, `\FT[name]` (`Helvetica`,
      `Helvetica-Bold`, `ArialMT`, `Arial-BoldMT`) and `\FC[color]` (a
      packed color) change what follows.
  - `DynaPDF.SetBBox ; PDF ; Box ; X1 ; Y1 ; X2 ; Y2` sets the page's Media,
    Crop, Trim, Bleed or Art box, lower left then upper right.
- Page geometry, in points:
  - `DynaPDF.GetPageWidth ; PDF` and `DynaPDF.GetPageHeight ; PDF` measure
    the open page's media box, as SetBBox left it.
  - `DynaPDF.GetPageBBox ; PDF ; PageIndex {; Box}` gives a box of page
    PageIndex (from 1, not the open page): `MediaBox` (the default),
    `CropBox`, `TrimBox`, `BleedBox` or `ArtBox`. It answers a line each
    for its left, top, right and bottom, with six decimals:
    `0.000000¶792.000000¶612.000000¶0.000000`. A page's media and crop
    boxes may be inherited from its page tree.

  The boxes are read by the PDF worker when the PDF is opened, for its
  first 10,000 pages, at the precision they are written.
- Form fields on the open page, each returning its handle (0, 1, ... in
  the order created):
  - `DynaPDF.CreateTextField ; PDF ; Name ; Parent ; Multiline ; MaxLen ;
    PosX ; PosY ; Width ; Height`, empty;
  - `DynaPDF.CreateCheckBox ; PDF ; Name ; ExpValue ; Checked ; Parent ;
    PosX ; PosY ; Width ; Height`, its on state named by ExpValue, checked
    or not;
  - `DynaPDF.CreateSigField ; PDF ; Name ; Parent ; PosX ; PosY ; Width ;
    Height`, unsigned.

  PosX and PosY are the lower left corner, and the rectangle includes the
  border, drawn at the line width (`DynaPDF.SetLineWidth`, 1 at first).
  `DynaPDF.SetFieldTextColor`, `SetFieldBorderColor` and
  `SetFieldBackColor` set the colors of the fields created after them:
  packed colors (`DynaPDF.RGB`), or for the border and background
  `4294967281`, none. They start black, black and none. A text field
  shows the active font, Helvetica or Helvetica-Bold, at its size, or at
  an automatic size when it is a single line and the font is 1 point or
  not set; a check box's check is sized the same way.

  Fields of one name and type are one field, a field group [O, the
  DynaPDF manual]. The field holds the name, type, flags and value, and
  each call's field is one of its widgets, with its own rectangle, colors,
  border and appearance. A check box group's value is the export value of
  its checked boxes, or `Off`. Each box shows checked when its own export
  value is the group's (PDF 1.7, 12.7.4.2.3). How a viewer then toggles
  the boxes is the viewer's; it has not been checked here.

- `DynaPDF.FindPattern ; PDF ; Text` counts literal, case-sensitive,
  non-overlapping matches on the open, unrotated page. It uses the PDF
  worker's existing text extraction (including its inferred spacing and
  ligature expansion), without OCR; matching has not been compared with
  MBS. A pattern must be a nonempty single line, without controls, at
  most 4096 UTF-8 bytes. Pending page text is rendered with the same host
  renderer as Save and composed with the imported page in the worker,
  in memory, before searching. Search writes no file and changes no source
  or pending text. Widget values and button captions are not page text.
- `DynaPDF.HighlightPattern ; PDF ; Text {; HighlightColor ; CaseInsensitive}`
  adds a highlight annotation for each located match on the open imported
  page, returning its count. Color defaults to yellow (packed RGB); flag 1
  uses Unicode lowercase matching, flag 0 is case-sensitive. The same
  pattern bounds and pending-text composition as FindPattern apply.
  OpenMaker locates horizontal outline glyph runs unambiguously in the
  extracted text and checks their match offsets against the existing
  extractor. If it cannot locate every match, it writes none and returns an
  error. Rotated/skewed/mirrored or
  Type 3 glyphs, missing Unicode mappings, and matches split across glyph
  runs that cannot be reconciled are refused, as are overlapping duplicate
  locations, repeated run text with ambiguous positions and runs whose
  spacing differs from extraction. Ligatures use their whole glyph's bounds.
  At most 10,000 highlights are held per document.
  Tight ink bounds, rectangular Multiply appearances, opacity 1 and print
  flag are OpenMaker's inferred choices, not measured MBS parity. The
  saved PDF retains editable annotations until FlattenAnnots is called.
- `DynaPDF.FlattenAnnots ; PDF {; Flags}` bakes OpenMaker-created highlight
  appearances into page content. Flags may be None (0, default), UseViewState
  (1), MarkupAnnots (2), or VisibleMarkupAnnots (3). Its highlights print and
  are visible, so either state draws them. Markup-only flags keep URI links;
  otherwise OpenMaker removes its links without drawing their border, since
  they have no explicit appearance. Fields and their actions remain interactive.
  It returns the number of regular annotations remaining (links), excluding
  widgets; native count and subsequent handle behavior are unmeasured.
  Imported pages with annotations and other flags (including FormFields) are
  refused before any change. When editable highlights and pending text coexist,
  it composes page content and older baked appearances in memory first, then
  bakes the new highlights above that text; subsequent text is drawn above them.
  Widgets, captions, actions and retained links stay separate and interactive.
  The snapshot consumes one source slot and shares the 512 MiB source-byte bound;
  lack of room or a rendering/worker failure leaves the document unchanged.
  Highlights share the cumulative 10,000-highlight document bound even after a
  snapshot. This is a bounded generated-annotation path, not general imported
  annotation or form flattening.
- `DynaPDF.WebLink ; PDF ; X ; Y ; Width ; Height ; URL` adds a URI
  link annotation on the open, unrotated page, returning its handle from
  0. The URL is kept as opaque printable 7-bit ASCII (at most 64 KiB).
  OpenMaker never opens it. The border uses the current line width,
  black stroke and a solid dash pattern; highlight is disabled (hmNone).
  Stroke color and dash setters are unsupported. Link handles counting
  from 0 and the annotation's nonprinting flag are inferred. At most
  10,000 links are held per document. Native and viewer behavior have
  not been checked.
- Buttons and their actions:
  - `DynaPDF.CreateButton ; PDF ; Name ; Caption ; Parent ; PosX ; PosY ;
    Width ; Height` makes a push button, after `DynaPDF.SetFont` (Arial
    or Helvetica, regular or bold, at a size other than 1). Its caption is
    centered across and down, on more lines when it does not fit one, in
    the field text color, inside its border. Buttons do not print, and no
    two may share a name.
  - `DynaPDF.CreateJSAction ; PDF ; Script` keeps a JavaScript as text and
    returns its handle (0, 1, ... in the order made).
  - `DynaPDF.AddActionToObj ; PDF ; "Field" ; "OnMouseUp" ; Action ;
    Field` makes the field's widget run the action when clicked (its
    `/A`). One action may run from several fields; it is written once.

  OpenMaker never runs a script: it is copied into the PDF for a PDF
  viewer to run.

  The PDF worker writes each field as a widget on its page, after the
  page's own annotations, and lists it, or its group, in the document's
  interactive form (`om-calc` `container_analysis/pdf_form.rs`). Each
  state has an appearance stream, in the standard fonts Helvetica,
  Helvetica-Bold and ZapfDingbats, which readers have and which are not
  embedded. A button's caption is laid out by the desktop, as written
  text is, and drawn in its appearance as a form, in Liberation Sans,
  embedded.

  The desktop (`om-gui` `scripting/host/pdf_text.rs`) lays the text out
  when it is written and draws it at Save, a page for each page written
  on, in one PDF. The PDF worker draws each of its pages over its page,
  after the page's content, under a resource name the page does not use.
  The page's own content and resources are kept as they were.

A page is copied with everything it refers to: its content, resources,
fonts, images and annotations. Streams keep their encoded bytes and filters,
an object pages share is copied once, and inherited boxes and resources are
written on the page. The PDFs are read and the document written in the PDF
worker process, with its memory, time and size bounds
(`om_calc::functions::pdf_worker`), never in the client. Files are read and
written through the same file layer as the folder commands.

Any commit, revert or record change is made by the scheduled script's own
steps, through the same operations as any script.

## Limits

- **Native behavior is unmeasured.** None of this has been compared with
  the plug-in in FileMaker Pro. Where the documentation is silent the
  behavior is inferred (re/mbs-plugin.md marks which).
- **Only these forty-nine commands.** Every other command still evaluates
  to `?`. That includes `Trace`, the other `DynaPDF` commands (including
  FlattenForm) and `Files.LaunchFile`.
- **DynaPDF form fields: what is inferred and what is refused.** None of
  it has been compared with the plug-in.
  - **Inferred**, where the documentation is silent:
    - the border is solid;
    - text starts black;
    - the line width starts at 1;
    - the check is ZapfDingbats `4`, the check mark, centered, at 80% of
      the box inside the border when its size is automatic;
    - every widget other than a button prints;
    - Arial is shown as Helvetica, of the same widths;
    - a button's caption sits inside its border, centered by the box
      around its lines;
    - `OnMouseUp` is the widget's activation action (`/A`).
  - **Refused with an error:**
    - a field with a parent (a group or radio button);
    - a field of a name already made as another type of field (the
      manual: "A field group cannot contain different field types");
    - a text field of a name already made with other flags (Multiline or
      MaxLen), which PDF allows only for how a group's widgets look;
    - a check box that would leave its group's value unclear: a second
      export value made checked, or a box of the checked value made
      unchecked. Which one DynaPDF keeps is not documented.
    - no open page, or a turned one;
    - an empty name or export value;
    - a name with a period, which would make the field another's child;
    - an export value `Off`, the off state's name, or one holding NUL;
    - an empty rectangle;
    - a check box with no room inside its border for an automatic check;
    - a button before `DynaPDF.SetFont`, or with the font at size 1: the
      size a caption takes then is not documented;
    - a second button of a name (the manual: not allowed);
    - a caption that does not fit inside its button's border, its glyphs'
      outlines included, which the button's appearance would clip;
    - an action on anything other than a field, or on an event other
      than `OnMouseUp`;
    - a second action on a field's `OnMouseUp`: DynaPDF allows several
      but does not document how they chain;
    - beyond OpenMaker's own bounds: a corner or border width past
      10,000,000 points, a name longer than 1,024 bytes, an export value
      longer than 127 bytes (PDF's limit for a name);
    - a multiline text field before `DynaPDF.SetFont` (the manual names
      no size for it);
    - transparent text;
    - more than 10,000 fields in a document.
  - **The fields of an imported PDF** keep their widgets on their pages
    but are not fields of the new document's form, as before.
- **DynaPDF page geometry: what is inferred and what is refused.** None of
  it has been compared with the plug-in.
  - **Inferred:** GetPageBBox's lines and six decimals. The documentation
    gives the order (left, top, right, bottom) but no separator or number
    format; scripts observed in a hosted solution read the values one per
    line, with six decimals.
  - **Refused with an error:**
    - **A turned page (`/Rotate`).** Whether the plug-in's sizes and boxes
      follow the turn is undocumented.
    - **A box the page does not have**, or one that is not exactly four
      numbers. DynaPDF's manual reports such a box unavailable; what the
      plug-in returns is undocumented.
    - **A size past a double's range.**
    - **GetPageWidth or GetPageHeight with no page open.** The plug-in
      answers its default page size, which is undocumented.
    - **A page past a PDF's first 10,000.**
  - **The crop box** does not change GetPageWidth or GetPageHeight. The
    manual says they measure the media box; SetUseVisibleCoords, which
    would make them measure the crop box, is not among the commands here.
- **DynaPDF text: what OpenMaker refuses rather than guess.** Each returns
  an error and leaves the document as it was:
  - **EditPage while another page is open.** It is undocumented; the public
    examples always call EndPage first.
  - **EditPage past the last page.** The plug-in appends pages of a default
    size; OpenMaker edits only pages the document has.
  - **A fill color outside 0 to 1.** The documented range; what the plug-in
    does with one is unmeasured.
  - **Fonts and styles other than Arial and Helvetica, regular or bold.**
  - **Format tags other than `\FS`, `\FT` and `\FC`.**
  - **Justified text.**
  - **Text that passes a limited rectangle's height.** The plug-in
    continues it on another page.
  - **A line wider than its rectangle**, a word longer than it or the
    spaces before a word. Whether DynaPDF breaks it or lets it pass the
    rectangle is not documented.
  - **A character Liberation Sans has no glyph for.**
  - **More than 10,000 text blocks or 1 MB of text in a document**
    (OpenMaker's bounds).
  - **Formatted text with no text rectangle.** The plug-in's default is the
    page's crop box less 50 points, which the commands here do not know.
  - **Writing on a turned page (`/Rotate`).**
- **DynaPDF text: OpenMaker's choices.** The Y of a text rectangle is its
  top edge, and lines are spaced by ascent less descent. The documentation
  calls Y the "Y-Coordinate of output rectangle" without saying which edge
  (OpenMaker's reading); the second is the manual's rule for `\LD` without
  typographic metrics. The default leading is
  undocumented. A line under a larger one is moved down so the two do not
  overlap, and lines wrap at the last space that fits. The faces are
  always embedded, whatever Embed says.
- **No DynaPDF library, version or license.** `DynaPDF.Initialize`,
  `DynaPDF.IsInitialized` and the version and license commands stay `?`,
  as without the plug-in: OpenMaker claims no DynaPDF library. A script
  that checks `DynaPDF.IsInitialized ≠ 1` and calls `DynaPDF.Initialize`
  therefore sees it fail. **That `DynaPDF.New` and the commands above
  then work, without `DynaPDF.Initialize`, is OpenMaker's departure:** the
  plug-in requires `DynaPDF.Initialize` first. OpenMaker's engine has no
  demo mode and adds no watermark.
- **DynaPDF imports only after the last page and at scale 1.**
  `DynaPDF.ImportPDFFile` at a destination that already has pages, which
  the plug-in draws over them, and either import with a scale factor other
  than 1 return an error. So does a PDF opened with a password; one
  encrypted with an empty password reads. A document opens at most 63
  PDFs (OpenMaker's bound: the PDF worker reads 64, one of them the text
  Save draws).
- **DynaPDF saves only to the file `DynaPDF.OpenOutputFile` named.**
  `DynaPDF.Save` without one, which the plug-in answers with container
  data, returns an error. The document has no outlines (bookmarks) or
  structure tree, and its interactive form holds only the fields DynaPDF
  commands made: a copied page keeps its annotations, form field widgets
  among them, but they are no form's fields. Nor does it
  keep a PDF's optional content (layer) configuration, without which
  content the configuration hides would show, so a PDF that has layers is
  refused when opened.
- **DynaPDF needs the PDF worker.** Without its worker executable a
  command that reads or writes a PDF returns an error. A session holds at
  most 16 documents, a document 10,000 pages and 512 MB of PDFs it read,
  and one PDF read is at most 256 MB (OpenMaker's bounds).
- **Only in scripts on the desktop.** Layout calculations and the server
  (Perform Script on Server, schedules) still evaluate the commands to `?`.
  The plug-in itself does not offer the idle, alert or desktop folder
  commands on the server, but it does offer `Files.CreateDirectory` there;
  OpenMaker's server leaves that `?`.
- **`Files.CreateDirectory`, `Files.Delete` and the DynaPDF commands act
  only in a step's
  calculation.** In a calculation the script engine evaluates outside a
  step (a button's script parameter, a layout's calculation) they are `?`
  and do nothing. That is OpenMaker's choice: FileMaker Pro calls the
  plug-in from any calculation.
- **`Folders.UserDesktop` answers on Linux.** The plug-in offers it on
  macOS, Windows and iOS only.
- **The error texts are OpenMaker's.** The plug-in's documentation gives
  none for the folder, delete, path and DynaPDF commands.
- **`Files.Delete` deletes no folder.** The plug-in deletes a folder with
  everything in it on macOS and Windows, and none on Linux; OpenMaker
  refuses a folder on every platform.
- **`Get ( InstalledFMPlugins )` does not list the plug-in.** Scripts that
  check for it take their path for a FileMaker Pro without it.
- **The idle script needs its window.** If the window that scheduled it
  closes, it does not run in another one.
- **No options beyond these.** The alert has no title, other button,
  suppression checkbox, icon of the system's or sheet form.
- **Escape does nothing in the alert.**
- **Bounds of OpenMaker's own.** An alert timeout over a day is no
  timeout, and an idle delay over a day is an error.
