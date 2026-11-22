# Title, grand-summary and sub-summary report parts

Preview, Save Records as PDF and desktop Print share one fixed-height,
one-column report plan and content scene. Ordinary body reports can also contain
one each of TitleHeader (0), LeadingGrandSummary (2), TrailingGrandSummary (6)
and TitleFooter (8), plus sorted LeadingSubSummary (3) and TrailingSubSummary (5)
groups. Types and source geometry follow
[the decoded layout model](re/layout-model.md#43-parts); no layout names or
customer data control this behavior.

The title header replaces the ordinary header on physical page 1; the title
footer replaces the ordinary footer there. Subsequent pages use ordinary parts.
The title header uses the first selected record. The title footer uses the last
selected record, following the **current** Claris help (older help described the
last record on page 1). Ordinary headers/footers use the first/last content anchor
on their page. A title-only cover uses first/last selected record anchors.

The leading grand flows once before the first body, anchored to the first selected
record; the trailing grand flows once after the last body, anchored to the last
selected record. Summary fields there use the existing overall found-set engine,
including when output scope is the current record. Body order remains the selected
found-set order. Preparation does not select a Browse record or change the sort.

## Sorted sub-summary groups

A sub-summary part prints only when the found set is sorted by its break
("when sorted by") field, as in FileMaker; otherwise it is omitted. A group is a
run of found records whose sort values, up to and including the break field,
are equal: the same groups `GetSummary ( summary ; break )` and restarting running
summaries use (`om_data` `FileCalc::found_groups`). Groups come from the found
set's actual sort and order, never from record storage order or a guess.
Browse List view places the parts around its records with the same planner
(`om_layout::pagination::RecordGroups`, [desktop client](desktop-client.md)).

- A leading sub-summary prints before the first record of each group, anchored to
  that record; a trailing one prints after its last record, anchored to it
  (Claris: first record for leading, last for trailing). Leading parts of nested
  breaks print outermost first, trailing parts innermost first. Both may exist
  for one break field.
- A summary field of the layout's own occurrence placed in a sub-summary part
  (directly or as a merge field) shows its group's value, as `GetSummary` does:
  totals, counts, averages, minimums, maximums, deviations and lists over the
  group, a running summary's value at the end of the group, a fraction of the
  group's total. Grand summary parts, bodies and headers keep found-set values.
- Other fields are calculated on the anchor record. Calculations, including
  nested summaries and `ExecuteSQL`, keep the found set, the owning file context
  and the actual physical page, as in every other part.
- Groups can cross physical pages; parts remain whole and move to a fresh page
  as before. Repeated headers/footers, title/grand parts, Get(PageNumber) and
  selected Print page ranges keep their existing meaning. Current-record output
  prints that record's groups, with summaries over its groups in the found set.

Break keys decode strictly (layout-model.md 4.3). Planning refuses, before any
PDF is staged or spooled and with an explicit Preview message: malformed or
other-file break keys, break fields that are not fields of the layout's table,
summary or container break fields, duplicate parts for one break field and side,
part flags other than body bit `0x400` (which prints;
[portable PDF](portable-pdf.md#supported-layout-and-output-contract)), page
breaks and unknown part options, a sort whose break order
disagrees with the layout's part order (leading parts top-down from the outermost
break, trailing parts bottom-up), a group break after a sort key of another table
(see below), and summary fields of another occurrence (a related set) or a later
repetition in a sub-summary part. Unsupported summary kinds refuse as calculation
errors.

Sort keys that are related fields keep their place as an explicit unresolved key
(`om_data::host::UNRESOLVED_SORT_FIELD`), never a same-named field of the layout's
table, so later keys cannot group without them. `GetSummary`, restarting running
summaries and subtotaled fractions read such groups as unsorted (empty), and
reports refuse. Grouping by related keys is not implemented.

The groups' keys, and the inputs of the summaries the parts print, read the
reader's variables [I: unmeasured in FileMaker]. Preview uses the window's
variables. Save Records as PDF and Print use the script's lent variables
for report grouping and summaries. Each read takes a snapshot, and
assignments by an input stay within that read
([#615](https://github.com/OpenMaker-Dev/openmaker/issues/615),
[calc values](re/calc-values.md), "The host's own reads of a field").
Reports refuse with 3 for these cases.

- a sort key up to a break that reads as `?` on any record;
- while the reader has variables, a sort key up to a break that is an unstored
  calculation which may read one (`Expr::variable_reads`, through the custom
  functions it calls and the unstored calculations it references; one that
  cannot be resolved counts as may), since the found set was sorted without
  them [I: whether FileMaker's sort evaluates such a key with the
  script's or window's variables is unmeasured]. The same check applies
  to the sort prefix of a `GetSummary` break, even on a report without
  sub-summary parts.

### Costs and limits

Planning a grouped report reads the ordered found set's IDs and each found
record's sort values up to the deepest sorted break once. Ungrouped layouts still
plan from the found count without reading any record. Preview reads groups once
per found-set revision and count through the window's asynchronous calculation
cache (Session caches them with the plan; page navigation and record-range loads
reuse them, while a refresh, write or new found set re-reads them). The script
host reads them synchronously for PDF/Print and for Preview page movement. Each
summary value in a sub-summary part reads its summary's found-set run, so output
cost is linear in the found set per placed summary value. Grouped reports are
bounded to 10,000 found records (`report::MAX_GROUPED_RECORDS`) and refuse above
that, as do reports whose parts resize by sliding.

## Bounds and acceptance

One body is required. Bodyless (summary-only) reports, sliding left, splitting,
explicit page breaks/restarts ("page break before/after each occurrence"),
nonzero part flags other than body bit `0x400`, unknown part options, columns
and responsive forms remain refused. Existing unsupported objects, typography, page ranges and opaque output
options remain bounded by [portable PDF](portable-pdf.md) and [Print](printing.md).

Parts remain whole: when the next part will not fit, it moves to a fresh page.
A leading sub-summary moves to the next page with its group's first record when
it would fit at the foot of a page but the record would not. The move happens
only if the two fit together on a fresh page, measured as they print there; a
run too tall for any page is placed as before. FileMaker Pro 26 does this with
one level of grouping (measured, layout-model.md 4.3). That several leading
sub-summaries in a row move together with the record they head is inferred.
On a first page shortened by a title header or footer, the move can leave a
title-only cover. Paging can also produce a grand-only final page. A part that
cannot fit on an eligible page refuses output without looping or clipping. The
content callback carries the selected-record index, the actual physical page and
a sub-summary part's break field; Get(PageNumber), including nested summary/SQL
evaluation, never infers its page from the record's first appearance. Preview
requests every placement's record anchor, including a title footer's last record
and a grand-only page. Existing revision/range and paper invalidation remain in
effect.

The existing paper, orientation, 100% scale, printer margins, root stacking, font
substitution ([report fonts](portable-pdf.md#report-fonts)), formatting, async calculation, draft/commit and atomic PDF
contracts remain. Print renders the complete report before submission.

Focused regressions: pagination unit checks for nested leading/trailing groups
crossing a page, unsorted breaks, sort/part order and inconsistent-group refusal
and malformed keys; the break-key decoder; sort-key resolution; one script-host
run that saves a nine-record, three-page PDF grouped by Region (East 2250, North
2375 crossing pages 1-2, West 1000; grand 5625), byte-identical fake Print, the
same page count for Preview page movement, unsorted omission and an atomic
refusal after a related sort key; and a headless Preview test showing the same
three group pages and anchors without changing the Browse record or found set.
The earlier title/grand checks (shuffled nine records, four pages, overall total
5625, page summaries on a grand-only page) still apply. Artifact environment
variables are documented in [Preview](preview.md) and [portable PDF](portable-pdf.md).

This extends partial C41 ([client-progress.md](client-progress.md)). Objects that slide up and resize sub-summary and
grand parts are in [report sliding](report-sliding.md). Sliding left, bodyless
summary reports, part page breaks,
related-key grouping, native FileMaker geometry, printer hardware and native
acceptance remain unmeasured or unimplemented. All fixtures and rendered artifacts
are synthetic. No live/customer/FileMaker interaction is part of this acceptance.

Primary semantics consulted 2026-11-02:

- [Claris layout part types](https://help.claris.com/en/pro-help/content/layout-part-types.html):
  first-page title replacement, title record scope, found-set grand summaries,
  sub-summary subtotals per sorted break-field group, and the first (leading) or
  last (trailing) record of the group for other fields.
- [Claris GetSummary](https://help.claris.com/en/pro-help/content/getsummary.html):
  the summary over the current group when sorted by the break field, blank
  otherwise, and a grand summary when the break field is the summary itself.
- [Claris adding a layout part](https://help.claris.com/en/pro-help/content/adding-layout-part.html):
  single instances and a page-sized title header as a cover.
- [Claris chart part context](https://help.claris.com/en/pro-help/content/placing-chart-in-layout-part.html):
  first/last found-record context for leading/trailing grands. Applying those
  anchors to ordinary non-summary fields is the portable model's interpretation;
  native non-summary grand-field acceptance is unmeasured.
