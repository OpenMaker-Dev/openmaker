# Sliding objects and resizing parts in reports

Preview, Save Records as PDF and desktop Print close up blank space the way
FileMaker's "Sliding & Visibility" options describe, for objects that slide
**up**. All three outputs use one plan and one page scene
(`report::plan_report`, `report::prepare_page`), so their page breaks, part
heights and object positions are the same. Browse is unchanged: sliding
applies to printed pages only.

## Decoded options

The options are bits of `LayoutObject.flags` (protobuf field 3). They were
decoded from FileMaker Pro 26.0.3 ([layout model 5.8](re/layout-model.md#58-printing-and-sliding)):

| Option (Inspector) | Bit | OpenMaker |
|---|---|---|
| Sliding up | `0x10` | supported |
| Sliding up based on: Only objects directly above | `0x8000` set | supported |
| Sliding up based on: All objects above | `0x8000` clear | supported |
| Also resize enclosing part | `0x40` | supported |
| Sliding left | `0x20` | refused |
| Hide when printing | `0x200` | supported: the object and what it holds are left out |

`0x8000` and `0x40` mean nothing without `0x10`. FMEngine's slide record reads
them only when `0x10` is set, so OpenMaker ignores them on objects that do not
slide up. `om_layout::sliding` names the bits, and `LayoutObject::slide_up`
reads them.

## Portable model

The rules follow Claris's
[Removing blank spaces in printouts](https://help.claris.com/en/pro-help/content/removing-blank-spaces.html)
and [Troubleshooting layouts](https://help.claris.com/archive/help/13/fmp/en/html/create_layout.9.68.html).
Each printed part (one record's body, one group's sub-summary, a grand
summary) is laid out on its own:

- **Fields shrink.** A field set to slide up shrinks to the height its
  wrapped text needs, including padding. Its fill and borders shrink with it.
  An empty sliding field collapses to nothing and paints nothing. Field
  objects that do not slide never shrink. Other objects (text, merge text,
  shapes, lines, pictures) slide but never shrink.
- **Objects rise.** An object set to slide up rises by the least that its
  nearest objects above it rose (moved plus shrank). "Nearest" means not
  separated from it by another such object. With *All objects above* these
  are the nearest objects across the whole part. With *Only objects directly
  above* they are the nearest objects that overlap it horizontally. So:
  - the gap between neighbours stays, and an empty field's own height is
    removed, so a column closes up as a whole;
  - an object that does not slide rises by nothing, so it holds the objects
    below it ("a line that is not set to slide" blocks them);
  - with nothing above it, an object does not move;
  - *All objects above* keeps rows level across columns, while *Only
    objects directly above* lets each column close up on its own.
- **No new overlaps.** An object never rises over an object above it that
  overlaps it horizontally. This clamp only matters when a neighbour is held
  by objects in another column.
- **Parts shrink.** A body, sub-summary or grand-summary part shrinks by the
  least that its lowest *resize enclosing part* objects rose. The gap below
  them stays.
- **Fixed parts.** Claris: "Headers, footers, title headers, title footers,
  and top navigation and bottom navigation parts never shrink or slide up."
  An object in a header, footer, title header or title footer set to slide
  up, or to resize its part, prints where it is placed, as one that does not
  slide: none of the sliding refusals below apply to it, and a field there
  whose text does not fit is cut at its box like any field that does not
  slide. (Navigation parts do not print at all, so their objects are left
  out whatever they are set to.) That FileMaker ignores the options
  there, rather than refusing them in some other way, is read from that
  sentence; no FileMaker output with a sliding header object was compared.

Pagination (`om_layout::pagination::paginate_with`) places each part at its
printed height. A part is measured with the record and the physical page it
lands on. If it moves to a new page, it is measured again there, so content
such as `Get ( PageNumber )` measures as it prints. Group anchors, group
summaries, title/grand parts, current-record output, Get ( PageNumber ) and
Print page ranges keep their existing meaning ([report sections](report-sections.md)).
When a page is drawn, each part's layout must produce the height it was
planned with. Content that changed in between refuses the page; it is never
drawn overlapping.

## Refusals

Each refusal returns error 3 with a reason. PDF and Print refuse before any
file is staged or job spooled. Preview shows the reason instead of a page.

- Sliding left.
- An object that slides up in a part that holds an object hidden when
  printing or one beyond the printable width: whether FileMaker lets it
  slide into the space of what is left out is unmeasured.
- An object that slides up and overlaps another object in its part. Whether
  it would natively move over, under or out of the other object is
  unmeasured.
- An object that would be cut off by its resized part. For example, a fixed
  object near the bottom of a part whose higher objects resize it. Set it to
  slide up and resize the part too.
- Everything the ordinary report contract still refuses
  ([portable PDF](portable-pdf.md)): rotation, hide and label
  calculations, repeating fields, text overflow other than a field's text
  that does not slide (it prints cut), and so on; and groups, portals and
  conditionally formatted objects that are set to slide up, which print
  only when they do not slide. Sliding never relaxes these.

## Costs and limits

- If no part resizes, the plan is still made from the record count. Sliding
  then moves objects within fixed-height parts on the pages being prepared,
  and nothing more is read.
- If a part resizes, its height depends on each record. The report is then
  measured over the whole selected found set, and is limited to 10,000 records
  (`MAX_GROUPED_RECORDS`, shared with grouped reports).
  - Measuring reads only the shrinking fields' values. Those values are read
    again when each page is drawn, so they are calculated twice.
  - Preview reads the found set's record ids once, through the window's
    calculation cache, as it does for grouped reports. It requests every
    record's data at once, and measures while values are pending so that one
    pass requests everything. It shows "Loading report…" until the plan is
    complete.
  - The plan is kept until the found set, data generation or paper changes.
    Measuring does not depend on Browse's record pages.
  - Script page movement in Preview, PDF and Print measure synchronously
    through the script host.
- Geometry tolerances are 0.01 point. FMEngine's vertical slide calculation
  (`SlideDeltaCalculator::GetSlideDV`) compares edges with 4-point margins
  whose role is not decoded, so objects within 4 points of each other may
  slide differently natively.
- Sliding left, repeating-field repetitions, portal row omission, panel
  controls, merge-field line collapse and part page breaks are not
  implemented.

Native parity is **unmeasured**: no FileMaker output was compared. The
"nearest objects" reading of *All objects above* and *Only objects directly
above*, the clamp, the shrinking of borders and fill, and the refusals are
OpenMaker's interpretation of the evidence above.

## Acceptance

All fixtures are synthetic. `sliding_document` is an address block grouped by
Region:

- Name, Street, Suite and Notes fields slide up based on the objects directly
  above them, and resize the body. Suite is empty for even ids; Notes has
  0-3 lines.
- A fixed label holds the blue patch below it.
- A separator line slides based on all objects above it and resizes the body.

Its tests are:

- `sliding_address_blocks_close_up_and_resize_parts_in_pdf_print_and_preview_pages`
  (script host):
  - saves two pages where fixed bodies need three, and the fake Print job is
    byte-identical;
  - Preview page movement reaches page 2;
  - each Name, Street, Suite and Notes line sits at the expected PDF text
    origin, and empty Suites leave no text;
  - group headings and Get ( PageNumber ) stay on their pages;
  - rasterized pixels show the fixed patch in place and the rule risen with
    Notes;
  - a slide-left variant keeps the earlier PDF and spools nothing.
- `sliding_preview_measures_shrunk_parts_like_the_pdf_and_keeps_browse_state`
  (headless Preview, record ranges of two): shows the same two pages, body
  heights and object offsets, then keeps the Browse record and found set
  without writes.
- `sliding_refuses_unsupported_options_overlaps_and_cut_off_objects`:
  - the overlap clamp (a 60-point body);
  - refusals of slide left, sliding beside a hidden object, overlap,
    cut-off and rotation;
  - header and footer objects set to slide up, one of them overlapping
    another, print byte for byte as the same objects without the option,
    and a sliding header field whose text does not fit prints cut.
- `measured_parts_flow_by_printed_height_and_remeasure_on_their_new_page`
  (om-layout): measured pagination, re-measurement on the next page, and
  refusal of out-of-bounds heights.

`OM_PDF_ARTIFACT_DIR` keeps `sliding.pdf` and its `sliding-hayro-page-*.png`
renders. `OM_PREVIEW_ARTIFACT_DIR` keeps `sliding-first.png` and
`sliding-last.png`.

Preview remeasures cached pages when script variables or the window's calculation context change, including updates to other windows of that file. A targeted object refresh also expires the plan so newly measured text does not retain an old part height. This does not alter the found set or current record.
