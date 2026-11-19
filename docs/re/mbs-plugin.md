# The MBS plug-in's idle script, alert, folder and PDF document commands

How a third-party FileMaker plug-in, the MBS FileMaker Plugin by
MonkeyBread Software, behaves for the commands OpenMaker implements a
clean-room subset of ([plugins.md](../plugins.md)). The source is the
plug-in's public documentation (mbsplugins.eu, read 2026-11-17), marked
[O, docs]. Nothing here was measured against the plug-in running in
FileMaker Pro; what follows from the documentation without being stated is
[I].

## 1. How calculations call it

- [O] A stored calculation calls the plug-in as plug-in function id `MBSP`,
  function 1: the single `MBS ( ... )` entry point (calc-encoding.md 7,
  opcode `1d`, arguments last first). Its first argument is the command's
  name as text, the rest are the command's parameters. Observed in the
  stored formulas of a hosted solution's scripts.
- [O, docs] Each command returns `OK` on success, or an error message.
  [I] The plug-in's error messages begin `[MBS]`.
- [I] Command names are not case-sensitive.
- Without the plug-in, FileMaker Pro evaluates the call as `?` and the
  script goes on, with `Get ( LastError )` 0 [O, #247 on OpenMaker's
  harness, which matches FileMaker Pro without the plug-in].

OpenMaker implements the commands of sections 2 to 5, which occur in a
hosted solution's scripts: an idle script that shows an alert, a folder on
the desktop, deleting files and converting paths, and a document made of
pages of other PDFs, written on and saved. [plugins.md](../plugins.md)
says what OpenMaker does with each and what it refuses. Every other
command stays `?`, as in FileMaker Pro without the plug-in.

## 2. The idle script

`MBS ( "FM.RunScriptIdle" ; DelaySeconds ; FileName ; ScriptName {;
ScriptParameter } )` [O, docs]:

- runs the script "whenever no user activity has been for the given amount
  of time"; "Moving the mouse or using keyboard will reset the counter";
- ScriptName is the script's exact name, or (plug-in 6.0 and later) the
  script's id "in the current file";
- the parameter is text, read with `Get ( ScriptParameter )`;
- not available on FileMaker Server.

`MBS ( "FM.StopScriptIdle" )` "Stops any idle timer" [O, docs].

Inferred, not documented for these two commands:

- [I] There is one idle script at a time: a second `FM.RunScriptIdle`
  replaces the first. "Stops any idle timer" suggests a single one.
- [I] The idle script runs once; calling `FM.RunScriptIdle` again
  schedules it again. The documentation says idle schedules repeat only for
  the related `Schedule.StartScriptOnIdle` ("our idle schedules repeat
  automatically"), which `FM.StopScriptIdle` "Does not stop": they are two
  mechanisms. Scripts observed in a hosted solution read the same way.
- [I] The script runs as the plug-in API's script start does, in
  FileMaker's current window with its record; a field being edited stays
  active.

## 3. The alert

[O, docs] The `Dialog` commands build one alert, which `Dialog.Run` shows:

| Command | Effect |
|---|---|
| `Dialog.Reset` | "Resets all dialog options." |
| `Dialog.SetMessage ; Text` | the message |
| `Dialog.SetMode ; Mode` | the style: on macOS Warning, Information, Critical, Error, Note, Stop |
| `Dialog.SetTimeout ; Seconds` | "the plugin will run a timer to close the window after a given time period" |
| `Dialog.SetDefaultButton ; Text {; NoDefault }` | the default button's title; "When an empty string, a default localized button title ("OK" in English) is used" |
| `Dialog.SetAlternateButton ; Text` | the alternate button's title |
| `Dialog.Run` | shows it modally and returns which button was pressed: "On Mac returns text of the buttons clicked"; on a timeout "The result of the dialog will be "Timeout"" |

The alert commands are not available on Linux or FileMaker Server [O,
docs].

[I] An alternate button with empty text is not shown.

## 4. The desktop folder and folder creation

`MBS ( "Folders.UserDesktop" )` [O, docs]:
- "Queries path to user's desktop folder."
- "This is a native path"; `Path.NativePathToFileMakerPath` converts it
  for script steps.
- "Returns path or error."
- Offered on macOS, Windows and iOS, not on Linux or FileMaker Server.

`MBS ( "Files.CreateDirectory" ; Path )` [O, docs]:
- "Creates a directory." Path is the "Native path to the new directory to
  be created".
- "Returns "OK" or error message"; "Returns "OK" if the directory already
  exists."
- "For macOS & iOS creates folders recursively as needed", and on Windows
  from plug-in 10.5.
- Offered on macOS, Windows, Linux, FileMaker Server and iOS. Its former
  name was `Files.CreateFolder`.

Inferred, not documented:
- [I] The desktop path has no closing separator: scripts observed in a
  hosted solution append `"/"` before the next folder's name. The
  documentation's examples join names with `Path.AddPathComponent`.
- [I] A file at the path, or at a folder above it, is an error.

## 4a. Deleting files and converting paths

`MBS ( "Files.Delete" ; Path )` [O, docs]:
- "Deletes a file directly without moving it to trash."
- Path is "The native file path to the file to be deleted"; since plug-in
  6.3 a list of paths, one per line, which "Stops at the first error".
- "Works also for directories and can easily remove whole directory
  hierarchies!", but "On linux the Files.Delete function doesn't delete
  folders".
- Since 9.2 a path ending in `//` or `\\` is refused.
- "Returns OK or error message."

`MBS ( "Path.FileMakerPathToNativePath" ; Path {; PassThroughNativePath } )`
[O, docs]:
- Converts a FileMaker path, "useful for "filemac:" and "filewin:" path
  specification", to a native path. On macOS, "FileMaker expects to have the
  Disk Name in front"; the root disk's native path starts without it, other
  disks' with `/Volumes/name`.
- It works on a path list, picking the line for this platform (an example
  with `file:`, `fileLinux:`, `fileMac:` and `fileWin:` lines; `filelinux:`
  since 12.0).
- "This function does not work for relative paths."
- PassThroughNativePath, default 1 since 11.0: "pass through a native path
  directly and avoid conversion, if the path is valid already".

Not documented: what either returns for a file that does not exist or a
list without this platform's line. [I] Which native paths count as "valid
already": OpenMaker passes through one naming something that exists or in a
folder that exists.

## 4b. Line breaks

`MBS ( "Text.ReplaceNewLine" ; Text ; Mode {; Wrap } )` [O, docs]:
- formerly `String.ReplaceNewline`;
- Mode "0: nothing", "1: CR (Mac)", "2: LF (Unix)", "3: CRLF (Windows)",
  "4: <br> for html", "5: <br /> for xml"; "add 8 to remove duplicate
  newlines";
- Wrap is "Optional whether to wrap lines longer than given number of
  characters", 0 by default;
- the function "Can remove, convert to Mac, Windows and Linux specific end
  of line characters"; text styling is discarded, and output stops at the
  first NULL byte. Its examples remove line breaks with Mode 0.

[I] Which line endings it reads (OpenMaker reads CRLF, CR and LF, each one
line break), what "duplicate newlines" are (OpenMaker removes a line
break that follows another), and how Wrap breaks lines.

## 5. DynaPDF documents

[O, docs] `DynaPDF.Initialize ; LibraryPath {; Serial}` loads the DynaPDF
library from a native path; without a serial number DynaPDF runs in demo
mode. "Before you can create DynaPDF instances ... you need to use
DynaPDF.Initialize". The commands below "check for a license".

| Command | Documented [O, docs] |
|---|---|
| `DynaPDF.New {; Serial}` | "Returns reference number or error"; numbers "starting at 20000 and counting up for each new context"; to be paired with `DynaPDF.Release`. |
| `DynaPDF.OpenPDFFromFile ; PDF ; Path {; PasswordType ; Password}` | opens a PDF "for importing from a file", a native path; "Returns 0, file handle or error message". |
| `DynaPDF.OpenPDFFromContainer ; PDF ; Container` | the same for container data. |
| `DynaPDF.ImportPDFPage ; PDF {; PageIndex ; ScaleX ; ScaleY}` | "Imports a page from the import PDF and appends it"; "Returns OK or error message". |
| `DynaPDF.ImportPDFFile ; PDF {; DestPage ; ScaleX ; ScaleY}` | "Imports all pages", "including interactive objects such as annotations, bookmarks, form fields"; DestPage default 1, to append GetPageCount + 1; at existing pages "It is possible to import multiple PDF files on the same or overlapping destination page"; returns "the last page number that was touched". |
| `DynaPDF.OpenOutputFile ; PDF ; Path` | "opens the output file into which the PDF file should be written"; a native path. |
| `DynaPDF.Save ; PDF {; FileName ; Preview}` | with an output file, "just returns OK after closing the file"; otherwise "Returns container value with PDF file". |
| `DynaPDF.GetPageCount ; PDF` | the document's page count. |
| `DynaPDF.EditPage ; PDF ; Page` / `DynaPDF.EndPage ; PDF` | "prepares a page for editing", appending pages up to Page; "An open page must be closed with EndPage()"; "Returns OK". |
| `DynaPDF.SetFont ; PDF ; Name {; Style ; Size ; Embed ; CodePage}` | "Returns font handle"; Style 1 italic, 2 bold, 4 underline, 8 strikeout, or words; Embed default 1. |
| `DynaPDF.SetFillColor ; PDF ; Value...` | components "must be in the range from 0 through 1", as many as the color space has (3 for RGB). |
| `DynaPDF.RGB ; R ; G ; B` | "Blue * 65536 + Green * 256 + Red"; `RGB ( 0 ; 0 ; 255 )` is 16711680. |
| `DynaPDF.SetTextRect ; PDF ; PosX ; PosY ; Width ; Height` | the rectangle WriteFText writes into; Height -1 means no page break. |
| `DynaPDF.WriteFText ; PDF ; Align ; Text {; PlainText}` | left, center, right or justify; "If no callback was set, the text continues on the next page by using the same output rectangle"; without SetTextRect the rectangle is the crop box (else the media box) less 50 units on each side (manual). |
| Format tags (manual) | `\FC[UI32]` font color, `\FS[float]` font size, `\FT[...]` a font by handle, PostScript name, Family Name or Full Name. |
| `DynaPDF.SetBBox ; PDF ; Box ; Left ; Top ; Right ; Bottom` | on the open page; the four numbers are "lower left x", "lower left y", "upper right x", "upper right y". |
| `DynaPDF.GetPageWidth ; PDF` / `DynaPDF.GetPageHeight ; PDF` | "the width of the currently open page"; "If no open page can be detected the return value is the default width which will be used for newly created pages". Manual: "The page height refers to the media box of a page. The real size is maybe smaller if a crop box is present"; with SetUseVisibleCoords true the crop box's; "A PDF unit represents 1/72 inch". Rotation is not mentioned. |
| `DynaPDF.GetPageBBox ; PDF ; PageIndex {; Box}` | PageIndex "in range from 1 to DynaPDF.GetPageCount"; Box "MediaBox, CropBox, ArtBox, BleedBox or TrimBox", default MediaBox; "Returns rectangle as string with four numbers for left, top, right and bottom". No sample output. Formerly DynaPDF.GetPageBounds. |
| GetPageBBox (manual, rendering engine) | "all bounding boxes with exception of the media box are optional and maybe not available"; then the box is zero and the function returns false. The manual's rectangle structs order their fields Left, Bottom, Right, Top. |
| `DynaPDF.EditPage` (docs and manual) | Say nothing of a call while another page is open. |
| `DynaPDF.CreateTextField ; PDF ; Name ; Parent ; Multiline ; MaxLen ; PosX ; PosY ; Width ; Height` | "Returns field handle or error"; Parent "Parent group field if any or -1"; MaxLen "Maximum length of the text or 0". Manual: "A text field requires a font. If no font is active the standard font Helvetica is used"; the size is "auto" if ffMultiline is not set and the active font's size is 1 or no font is active; "the width and height is measured incl. the line width of the border", which is the graphics state's (SetLineWidth), "either 0, 1, 2, or 3 units"; in bottom-up coordinates PosX, PosY is "the lower left corner"; the handle is "a value greater or equal zero". |
| `DynaPDF.CreateCheckBox ; PDF ; Name ; ExpValue ; Checked ; Parent ; PosX ; PosY ; Width ; Height` | "If 1, the check box appears checked"; Parent "-1" or a group. Manual: check boxes "use always the standard font ZapfDingbats", sized by the active font's size, automatically when it is 1 or no font is active; "The export value must not be an empty string"; the off state is always "Off"; a name is required for a standalone check box. SetCheckBoxChar: "Default value = ccCheck" (the glyph is shown as a picture). SetCheckBoxDefState: "The default state of a new check box is always identical with the visible state". |
| `DynaPDF.CreateSigField ; PDF ; Name ; Parent ; PosX ; PosY ; Width ; Height` | "Returns handle or error message"; the border from SetLineWidth, 0 to 3. |
| `DynaPDF.SetFieldBackColor` / `SetFieldBorderColor` / `SetFieldTextColor ; PDF ; Color` | For "newly created interactive form fields"; "Blue*65536+Green*256+Red"; transparent background or border: "NO_COLOR (4294967281)". Back color "Default value = NO_COLOR", border "Default value = 0 // Black"; the text color's default is not given. |
| `DynaPDF.CreateButton` (docs) | "Buttons don't print unless you set the Print flag"; other fields' default flags are not documented. |
| SetBorderStyle, SetLineWidth (manual) | The border styles; neither gives a default. |
| Field groups (manual, "Fields with identical names" and the field structure's remarks) | "When creating two fields with the same name and type then DynaPDF creates automatically a field group"; its group field "contains the field name of the children as well as the field value, default value, and tooltip"; the children take "the field flags, background, border and text color, border width, and the border style" from it and can override them; "children of such a group do not contain a field name"; "A field group cannot contain different field types"; group fields "contain always the same value with exception of check boxes. If a field group consists of check boxes the export value is also taken into account". |
| `DynaPDF.CreateButton ; PDF ; Name ; Caption ; Parent ; PosX ; PosY ; Width ; Height` (docs and manual) | Name "must be a unique name for the button field"; "It is not allowed to create two buttons with an identical name"; "Buttons requires a font, if no font was set before Helvetica is used to draw the caption by using the code page 1252"; "The caption is centred horizontally and vertically"; "By default the caption is drawn on multiple lines if it does not fit into a single line"; "Buttons are excluded from printing by default"; background and border may be `NO_COLOR`; the border from the line width and border style; events OnMouseUp, OnMouseDown, OnMouseExit, OnFocus, OnBlur. |
| [`DynaPDF.FlattenAnnots ; PDF {; Flags}`](https://www.mbsplugins.eu/DynaPDFFlattenAnnots.shtml) | Draws annotation appearances on pages, then deletes annotations; returns annotations remaining. Default None (0) uses print state; UseViewState (1) uses visibility; MarkupAnnots (2) keeps non-markup types such as links; VisibleMarkupAnnots (3) combines them. Missing appearances are not generated. FormFields (16) is separate; ordinary form fields are kept without it. [I] OpenMaker bakes only its own generated highlights, removes its own appearance-less links unless markup-only is set, and leaves its own widgets interactive. It refuses imported annotations and other flags. Its remaining count excludes widgets; native count/handle behavior is unmeasured. The retained raw step vector and formula metadata confirm two enabled calls without explicit flags (repetition 1); default flags are evidenced, but native execution and geometry remain unmeasured. [I] To preserve drawing order, OpenMaker composes pending page content and older baked appearances in memory before baking newly flattened highlights; later text is drawn above them. Generated widgets, captions, actions and retained links stay separate. |
| [`DynaPDF.HighlightPattern ; PDF ; Text {; HighlightColor ; CaseInsensitive}`](https://www.mbsplugins.eu/DynaPDFHighlightPattern.shtml) | Requires an open page and returns locations found. Color defaults to yellow, packed RGB; CaseInsensitive 1 ignores case (including umlauts). Since 7.4 it creates highlight annotations. [I] OpenMaker uses literal/nonoverlapping extraction matches, Unicode lowercase for insensitive matching, tight glyph-ink bounds and rectangular Multiply appearances with print flag. Pending text is rendered with Save's renderer and composed in memory before extraction. Only horizontal outline glyph runs located unambiguously in extraction with matching global match offsets are located; repeated run text and uncertain geometry are refused. Exact native padding, opacity and matching are unmeasured. |
| [`DynaPDF.FindPattern ; PDF ; Text`](https://mbsplugins.eu/DynaPDFFindPattern.shtml) | Searches text on the current open page, opened with EditPage, and returns the number of locations. [I] OpenMaker counts literal, case-sensitive, non-overlapping matches in the page text extracted by its existing PDF worker (calc-values.md's PDF extraction rules). Pending text is rendered with Save's renderer and composed in memory before extraction; this is inferred support for the observed write-then-search order. Matching, spacing, ligature expansion and text ordering have not been compared with MBS. |
| [`DynaPDF.WebLink ; PDF ; PosX ; PosY ; Width ; Height ; URL`](https://www.mbsplugins.eu/DynaPDFWebLink.shtml) | Inserts a web link on the current open page; URL must be 7-bit ASCII; returns a handle >= 0 or an error. Manual: border uses the current line width, stroke color and line dash pattern; [SetLinkHighlightMode](https://www.monkeybreadsoftware.com/DynaPDF-Manual/dynapdf_801.shtml) defaults to hmNone. [I] OpenMaker uses black, solid stroke until setters are supported, counts handles from 0 and leaves the print flag unset. Native behavior is unmeasured. |
| `DynaPDF.CreateJSAction ; PDF ; Script` | "Returns number or error"; DynaPDF does not check whether the script is valid; Acrobat's interpreter runs it; the examples convert line breaks with `Text.ReplaceNewLine` (Mode 2) first and attach the action on `OnCalc` or `OnMouseUp`. |
| `DynaPDF.AddActionToObj ; PDF ; ObjectType ; ObjectEvent ; Action ; Object` | ObjectType "Action, Annotation, Bookmark, Catalog, Field, Page or PageLink"; the events listed by name; Object "e.g. a field number for an ObjectType = Field"; "Returns OK or error"; "It is possible to add multiple actions to one object", which DynaPDF does not check; "The OnMouseUp event is the standard event supported by all objects". |
| PDF 1.7, 12.7.3.2 and 12.7.4.2.3 | Fields of one fully qualified name without partial names of their own "are different representations of the same underlying field; they should differ only in properties that specify their visual appearance". A check box's `V` names the appearance state shown; a widget's on state is its export value. |
| `DynaPDF.SetPageCoords` (manual) | "Default value = pcBottomUp". [I] OpenMaker keeps the defaults: SetPageCoords, SetFontOrigin and SetUseVisibleCoords are not among its commands and stay `?`. |

- [I] `DynaPDF.GetPageBBox` answers its four numbers one per line, with six
  decimals: the documentation gives the order (left, top, right, bottom)
  but no separator or format, and scripts observed in a hosted solution
  read the values that way.
- [I] `DynaPDF.Release` returns `OK`.
- [I] FileName and Preview apply only to the container data Save returns;
  after `DynaPDF.OpenOutputFile` a FileName changes nothing.
- [I] `DynaPDF.ImportPDFPage`'s result is `OK`, not a page number or a
  count.

## Unknowns

- What the plug-in does when `FM.RunScriptIdle`'s file or script is not
  found, and its exact error texts.
- Whether `SetTimeout` 0 means no timeout.
- What Escape does in the alert, and the icon each mode shows.
- In which window the idle script runs when the window that scheduled it
  is no longer the current one.
- The order of a script's triggers when the idle script commits a record
  while a field is active.
- The error texts of `Folders.UserDesktop` and `Files.CreateDirectory`, and
  whether `Files.CreateDirectory` resolves `..` or accepts a relative path.
- Whether the desktop path ever ends with a separator.
- What `DynaPDF.New` and the import commands do after `DynaPDF.Initialize`
  failed, or in demo mode; the error texts of the DynaPDF commands; how
  `DynaPDF.ImportPDFFile` draws pages over existing ones.
- `DynaPDF.EditPage` while another page is open. The public examples
  always call EndPage first.
- `DynaPDF.GetPageBBox`'s separator and number format (inferred from
  observed use), and what it returns for a box the page does not have.
- Whether GetPageWidth, GetPageHeight and GetPageBBox follow a page's
  `/Rotate`, and DynaPDF's default page size.
- The default border style, line width and field text color; the flags
  (print) of fields other than buttons; how DynaPDF draws a field's
  appearance (the check's size and place, a text field's padding); the
  field handles' numbering, and the handle a group's later fields get;
  which value a check box group keeps when boxes of two export values are
  made checked; whether a group's later fields may differ in flags.
- A button's caption without a font or at size 1, its padding inside the
  border, and its highlight; the action handles' numbering; whether
  `OnMouseUp` is written as the widget's `/A` or as an `/AA` entry; how
  several actions on one object chain.
- What `DynaPDF.SetFillColor` does with components past 1.
- WriteFText's default leading, where its first line sits in the
  rectangle, and whether PosY is the rectangle's top or bottom edge
  (OpenMaker reads it as the top [I]).
- Whether DynaPDF embeds Helvetica when Embed is 1.
