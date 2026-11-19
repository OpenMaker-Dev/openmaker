# Paginated Preview

Preview displays the found set as physical pages for the same ordinary,
one-column body reports supported by [portable PDF](portable-pdf.md), including
[title, grand-summary and sorted sub-summary parts](report-sections.md).
Form, List and Table have the same printed page plan. Headers and footers repeat;
a whole body moves to the next page when it will not fit. Navigation parts and
field placeholders do not print. Layout orientation, the printer's margins
(see [list pages](#starter-list-pages-compared-with-filemaker)), root stacking,
text wrapping, field calculations, merge fields and display formatting use the
shared report preparation contract. Arial and Helvetica text shows in Liberation
Sans and other text in Inter, as it prints
([report fonts](portable-pdf.md#report-fonts)). A dotted outline marks the printable margins.

The toolbar's page number, slider and Previous/Next buttons, Records > Go to
Record > First/Previous/Next/Last, keyboard navigation, and script Go to
Record/Request/Page move through physical pages in Preview. They do not select a
Browse record or change the found set/sort. Script By Calculation selects a page;
end-of-report movement retains the runtime's error 101 and Exit after last loop
behavior. Entering Preview retains the existing commit and mode-trigger/veto
boundary; preparation, painting and page movement are read-only. Each window/file
owns its page position. New layout/Preview entry starts on the first page.

Scrollbars and wheel movement stay within the displayed page. Each page starts
at its top on first display and retains its own scroll position when revisited.
View > Zoom In/Out/Actual Size and zoom shortcuts scale paper and content together;
application chrome retains its size. Zoom is the existing per-window zoom level.

The shared pagination plan uses the found count. Objects that slide up and the
parts they resize ([report sliding](report-sliding.md)) follow the same plan
as PDF and Print. A part that resizes needs the found set's record ids and
its shrinking fields' values, measured once per found set like groups. A layout with
sub-summary parts also needs the found set's groups: they are read once per found
set through the window's asynchronous calculation cache and kept with the plan
until a refresh, write or new found set (bounded to 10,000 records; see
[report sections](report-sections.md#costs-and-limits)). Page records must match
the grouped found set, otherwise Preview reports that the found set changed. Only
record fetch windows intersecting the selected physical page are requested through
the normal worker, with its window/file routing, epoch and Session revision checks. Field
calculation data is requested asynchronously; calculations such as summaries or
Get(RecordNumber) may require found-set IDs or dependent record values. No complete
report-body download or PDF/screen capture is used as the Preview model. Incomplete
provider ranges, failed reloads, missing calculation contexts, unsupported layouts,
and calculation errors display an explicit unavailable state. Loading never keeps
the previous page's scene on screen. Get(PageNumber) in calculated report fields
uses the physical page context in both Preview and PDF, including nested
summary calculations. Exporting does not change the user's window mode or
leave the report page in subsequent script calculations. Advanced layout merge
symbols such as `{{PageNumber}}` and `{{PageCount}}` remain explicitly refused.

The portable PDF refusal contract also applies to Preview: bodyless reports,
unsupported sub-summary sorts/breaks, advanced part options, sliding left, split parts,
layouts set to print in columns, responsive layouts, sorted, filtered or sliding portals, slide controls, objects inside any container but a group, a portal or a tab control's front panel,
dynamic pictures, the button icons portable-pdf.md lists, unsupported text/styles and overflow remain
unavailable, except a field's text, which shows cut at the field
([field text](portable-pdf.md#field-text)) as it prints. A layout wider than the page shows what fits
([wide layouts](portable-pdf.md#wide-layouts)). Buttons, their icons, and groups show as they print
([buttons and groups](portable-pdf.md#buttons-and-groups)), and frames with equal absolute corner
radii show rounded ([rounded frames](portable-pdf.md#rounded-frames)).
A web viewer shows its frame and the caption "Web viewer: OpenMaker does not
print web pages." instead of its page, as Print and the PDF do
([web viewers](portable-pdf.md#web-viewers), #278); charts and plug-in objects
remain unavailable.
Conditional formatting on text, fields and buttons shows record by record
([conditional formatting](portable-pdf.md#conditional-formatting)). Static PNG/JPEG/GIF/BMP/TIFF/WebP/SVG layout pictures and container fields are shown from the same decoded scene
as PDF and Print ([static layout pictures](portable-pdf.md#static-layout-pictures),
[container fields](portable-pdf.md#container-fields)).
Pages show the layout background behind the parts and the body's fill below
the last part, as Pro's PDFs do ([page background](portable-pdf.md#page-background)).
Translucent colors show blended over what is beneath them. The PDF and Print
get the same blend from a graphics state with the color's opacity, not a color
flattened over an assumed backdrop
([translucent colors](portable-pdf.md#translucent-colors)).
There is no exact-print field-list fallback. [Page Setup](page-setup.md) now supplies
shared file-session paper/orientation for Preview and PDF; scale stays at 100%.
Preview's Print button runs desktop [Print](printing.md), which a privilege set that denies
printing disables ([privilege sets](desktop-client.md#privilege-sets)); native printer settings
remain outside this slice. Data exports
retain their existing behavior. This is synthetic functional acceptance. Apart
from the two starter list pages [below](#starter-list-pages-compared-with-filemaker),
native FileMaker desktop acceptance and advanced event parity remain unmeasured.

Focused checks use the existing synthetic PDF fixture: nine shuffled records on
three pages, repeated parts, per-record calculations, formatted currency and a
page-number calculation. The headless GUI test shrinks the provider fetch window
to two rows, renders first/last physical pages, navigates toolbar and Records menu,
and preserves the Browse record. Separate checks exercise script movement/loop
exit and window/file isolation plus stale-range/reload-failure handling. A
grouped check renders the three Region group pages of the grouped PDF fixture,
including a group crossing pages, and keeps the Browse record. Set
`OM_PREVIEW_ARTIFACT_DIR` to retain first/last (and `grouped-*`) PNG renders. The existing PDF tests
continue to check searchable output, refusal/cancellation and root stacking.

## Starter list pages compared with FileMaker

Task List and Meeting List, one record each, were read in FileMaker Pro 26.0.3
for macOS on US Letter with Any Printer, in Layout Setup, the Inspector,
Preview and a saved PDF (#263, [#107](https://github.com/OpenMaker-Dev/openmaker/issues/107), 2026-11-13). OpenMaker's pages were
rendered from the same starter layouts through the shared report builder.

Measured in FileMaker, and what OpenMaker does:

| FileMaker | OpenMaker |
|---|---|
| One 612 x 792 point portrait page at 100% | Same |
| Row text sits lower in its field than OpenMaker drew it (walkthrough, [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201)) | The fields' stored text margins now apply: 14 points from the top at 16 points ([field text](portable-pdf.md#field-text)) |
| The row fields' 1-point gray bottom border, shown in Browse, prints in neither Preview nor the PDF | Same: fields print without borders or fill on layouts that delineate fields on the current record only ([field borders and fill](portable-pdf.md#field-borders-and-fill)) |
| A thin full-width line at each body's bottom prints | Same |
| "Use fixed page margins" is unchecked; content starts 18 points from the left and top, and Task List's Category heading (layout x 547-772) is cut at the printable edge, 594 points | Same: a layout with fixed margins off prints within the printer's 18 / 40 / 18 / 18 points ([portable PDF](portable-pdf.md#supported-layout-and-output-contract), #609) |
| The "Task" heading's ink starts about 33 points from the left and 30 from the top | Same, to about a point |

Inferred, not measured:
- That the text margins are the whole row text difference. Where FileMaker
  cuts text too long for its field is measured: at the text box, read from
  its PDF's clip path, which also insets by the field's border
  ([field text](portable-pdf.md#field-text)).
- That "Delineate fields on current record only" is why FileMaker leaves the
  field borders out; a report has no current record.
- Where page breaks fall. FileMaker was not measured with more than one page.
  OpenMaker breaks both lists at 11 bodies a page with either margins: their
  parts are multiples of 30 points, so 720- and 734-point printable heights
  hold the same parts.
- The 40-point bottom margin with fixed margins off. It is what Layout Setup and
  Page Setup show for Any Printer on US Letter; other papers and printers are
  unmeasured.

Primary semantics: [Preview](https://help.claris.com/en/pro-help/content/previewing-data.html),
[Go to Record/Request/Page](https://help.claris.com/archive/fm20/en/pro-help/content/go-to-record-request-page.html),
[fixed margins](https://help.claris.com/en/pro-help/content/page-margins.html).
