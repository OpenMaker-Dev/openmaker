//! Report pagination in layout points, independent of a screen or PDF backend.
//!
//! One column with indivisible body records, repeated headers/footers,
//! first-page title replacements, once-only grand summaries and sorted
//! sub-summary groups. Navigation parts never print. Advanced part rules
//! are refused rather than approximated. Placements retain source part and
//! record indexes so Preview and printing can reuse exactly the same plan.
//! A part whose sliding objects resize it ([`paginate_with`]) is measured
//! for each record, on the physical page it is placed on.
//!
//! [`RecordGroups`] places the sub-summary parts around each record. Report
//! pagination and Browse List view share it, so both show a group's parts
//! in the same places.

use crate::{BreakField, Layout, PartKind, Rect};

#[derive(Clone, Debug, PartialEq)]
pub struct PageSize {
    pub width: f64,
    pub height: f64,
    /// Insets (not coordinates): top, left, bottom, right.
    pub margins: Rect,
}

impl Default for PageSize {
    fn default() -> Self {
        Self {
            width: 612.0,
            height: 792.0,
            margins: Rect {
                top: 36.0,
                left: 36.0,
                bottom: 36.0,
                right: 36.0,
                ..Rect::default()
            },
        }
    }
}

impl PageSize {
    pub fn for_layout(layout: &Layout) -> Result<Self, String> {
        crate::paper::PaperSettings::initial(layout)?.for_layout(layout)
    }

    pub fn printable_width(&self) -> f64 {
        self.width - self.margins.left - self.margins.right
    }
    pub fn printable_height(&self) -> f64 {
        self.height - self.margins.top - self.margins.bottom
    }
}

/// Body `PartDef.flags` bit `0x400`, which the starter solutions' Details
/// layouts set and their list layouts do not. Its meaning is not decoded:
/// FileMaker Pro 26 shows the same Part Setup row-state options for both
/// kinds of body, and prints a Details body the same on every page
/// (layout-model.md 4.3). It changes no page plan and no part style.
pub const DETAILS_BODY_FLAG: u64 = 0x400;

/// `LayoutDef.flags` bit `0x400`: Layout Setup > Printing's "Print in 2
/// columns" is checked (layout-model.md 4.2, #925). The stored column count
/// and width stay as they were when it is unchecked, so they do not tell.
pub const PRINT_IN_COLUMNS: u64 = 0x400;

#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub part: usize,
    /// Index in the caller's selected records, in found-set order.
    pub record: usize,
    /// Top edge on the physical page, including the top margin.
    pub top: f64,
    /// Printed height: the part's own unless sliding resized it.
    pub height: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportPlan {
    pub paper: PageSize,
    pub pages: Vec<Vec<Placement>>,
}

/// The found set's groups at one sub-summary break field, as the caller's
/// actual sort defines them. Pagination never infers groups from values or
/// record storage order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakGroups {
    pub field: BreakField,
    /// The break field's position in the found set's sort (0: first key).
    pub depth: usize,
    /// Each selected record's group, in selected order: adjacent records
    /// share a group exactly when their numbers are equal.
    pub groups: Vec<usize>,
}

/// The distinct break fields of the layout's sub-summary parts, in part
/// order. A caller resolves each against the found set's sort and passes
/// the groups of those it is sorted by to [`paginate`]; parts whose break
/// field is not sorted do not print. Malformed keys are refused here.
pub fn report_breaks(layout: &Layout) -> Result<Vec<BreakField>, String> {
    let mut fields = Vec::new();
    for part in &layout.parts {
        if !matches!(
            part.kind(),
            PartKind::LeadingSubSummary | PartKind::TrailingSubSummary
        ) {
            continue;
        }
        let field = part
            .break_field()?
            .ok_or("A sub-summary part has no break field")?;
        if !fields.contains(&field) {
            fields.push(field);
        }
    }
    Ok(fields)
}

/// Plan fixed-height parts, as the layout stores them.
pub fn paginate(
    layout: &Layout,
    records: usize,
    paper: PageSize,
    breaks: &[BreakGroups],
) -> Result<ReportPlan, String> {
    paginate_with(layout, records, paper, breaks, |part, _, _| {
        Ok(layout.parts[part].height)
    })
}

/// Plan with each flowing part's printed height from `height(part, record,
/// page)`: at most the stored height, for that record on that zero-based
/// physical page. A part moved to a fresh page is measured again there, so
/// page-dependent content measures as it prints. Headers, footers and their
/// title replacements keep their stored heights.
pub fn paginate_with<E: From<String>>(
    layout: &Layout,
    records: usize,
    paper: PageSize,
    breaks: &[BreakGroups],
    mut height: impl FnMut(usize, usize, usize) -> Result<f64, E>,
) -> Result<ReportPlan, E> {
    let flow = report_flow(layout, records, &paper, breaks)?;
    let stored = |part: Option<usize>| part.map_or(0.0, |i| layout.parts[i].height);
    let page_parts = |first: bool| {
        (
            if first {
                flow.title_header.or(flow.header)
            } else {
                flow.header
            },
            if first {
                flow.title_footer.or(flow.footer)
            } else {
                flow.footer
            },
        )
    };
    let capacity = |first| {
        let (header, footer) = page_parts(first);
        paper.printable_height() - stored(header) - stored(footer)
    };
    if capacity(true) < 0.0 {
        return Err(
            String::from("The first-page header and footer exceed the printable height").into(),
        );
    }
    let mut measure = |part: usize, record: usize, page: usize| -> Result<f64, E> {
        let measured = height(part, record, page)?;
        if !measured.is_finite() || measured < 0.0 || measured > layout.parts[part].height + 0.001 {
            return Err(String::from("A report part was measured outside its bounds").into());
        }
        Ok(measured)
    };
    let too_tall = || -> E {
        String::from("A report part is taller than the page; splitting parts is unsupported").into()
    };
    // The flowing content is independent of the source part order/offsets.
    // Each item is placed whole, or moved once to a fresh ordinary page.
    // This also permits a title-only cover and a trailing-grand-only last page.
    let mut pages = Vec::new();
    let mut flow_parts: Vec<Placement> = Vec::new();
    let mut used = 0.0;
    let finish_page = |pages: &mut Vec<Vec<Placement>>, placed: &mut Vec<Placement>| {
        let first = pages.is_empty();
        let (header, footer) = page_parts(first);
        let first_record = placed.first().map_or(0, |p| p.record);
        let last_record = placed.last().map_or(records - 1, |p| p.record);
        let mut placements = Vec::new();
        if let Some(part) = header {
            placements.push(Placement {
                part,
                record: first_record,
                top: paper.margins.top,
                height: layout.parts[part].height,
            });
        }
        placements.append(placed);
        if let Some(part) = footer {
            placements.push(Placement {
                part,
                // Current Claris help defines the title footer's record as
                // the last in the found set, rather than the last on page 1.
                record: if first && flow.title_footer.is_some() {
                    records - 1
                } else {
                    last_record
                },
                top: paper.height - paper.margins.bottom - layout.parts[part].height,
                height: layout.parts[part].height,
            });
        }
        pages.push(placements);
    };
    let leading_sub = |part: usize| layout.parts[part].kind() == PartKind::LeadingSubSummary;
    for (index, &(part, record)) in flow.items.iter().enumerate() {
        let mut part_height = measure(part, record, pages.len())?;
        // A leading sub-summary prints on the page of the first record it
        // heads: with any leading sub-summaries after it, it moves to the next
        // page together with that record's body when the body does not fit
        // (FileMaker Pro 26, layout-model.md 4.3).
        // On an empty first page this can leave a title-only cover.
        let title_cover =
            pages.is_empty() && (flow.title_header.is_some() || flow.title_footer.is_some());
        if leading_sub(part) && (!flow_parts.is_empty() || title_cover) {
            // The run's height on `page`: this part, any leading
            // sub-summaries after it, and the record body they head.
            let mut run = |page: usize| -> Result<f64, E> {
                let mut total = measure(part, record, page)?;
                for &(next, next_record) in &flow.items[index + 1..] {
                    total += measure(next, next_record, page)?;
                    if !leading_sub(next) {
                        break;
                    }
                }
                Ok(total)
            };
            if used + run(pages.len())? > capacity(pages.is_empty()) + 0.001
                && run(pages.len() + 1)? <= capacity(false) + 0.001
            {
                finish_page(&mut pages, &mut flow_parts);
                used = 0.0;
                part_height = measure(part, record, pages.len())?;
            }
        }
        if used + part_height > capacity(pages.is_empty()) + 0.001 {
            if flow_parts.is_empty()
                && !(pages.is_empty()
                    && (flow.title_header.is_some() || flow.title_footer.is_some()))
            {
                return Err(too_tall());
            }
            finish_page(&mut pages, &mut flow_parts);
            used = 0.0;
            // Measured again on the page it now prints on.
            part_height = measure(part, record, pages.len())?;
            if part_height > capacity(false) + 0.001 {
                return Err(too_tall());
            }
        }
        let (header, _) = page_parts(pages.is_empty());
        flow_parts.push(Placement {
            part,
            record,
            top: paper.margins.top + stored(header) + used,
            height: part_height,
        });
        used += part_height;
    }
    finish_page(&mut pages, &mut flow_parts);
    Ok(ReportPlan { paper, pages })
}

/// A report's printing parts and its flowing (part, record) items, in order.
struct Flow {
    header: Option<usize>,
    footer: Option<usize>,
    title_header: Option<usize>,
    title_footer: Option<usize>,
    items: Vec<(usize, usize)>,
}

fn report_flow(
    layout: &Layout,
    records: usize,
    paper: &PageSize,
    breaks: &[BreakGroups],
) -> Result<Flow, String> {
    let def = layout.def.as_ref().ok_or("The layout has no definition")?;
    // A layout FileMaker Pro saved with "Print in 2 columns" unchecked keeps
    // a column count of 2 and a width; only the flag says columns print.
    let columns = def.flags & PRINT_IN_COLUMNS != 0;
    if columns || def.layout_client_type == 2 {
        return Err("Multi-column and responsive reports are unsupported".into());
    }
    let numbers = [
        paper.width,
        paper.height,
        paper.margins.top,
        paper.margins.left,
        paper.margins.bottom,
        paper.margins.right,
        def.base_width,
    ];
    if numbers.iter().any(|n| !n.is_finite() || *n < 0.0)
        || paper.printable_width() <= 0.0
        || paper.printable_height() <= 0.0
        || def.base_width <= 0.0
    {
        // A layout wider than the printable width prints what fits: the
        // report builder leaves out objects beyond it and cuts those that
        // cross it (Claris: only objects within the margins print).
        return Err("The layout does not fit the printable page width or margins".into());
    }
    if records == 0 {
        return Err("There are no records to print".into());
    }
    let (mut header, mut body, mut footer) = (None, None, None);
    let (mut title_header, mut title_footer) = (None, None);
    let (mut leading, mut trailing) = (None, None);
    // Sub-summary parts in layout order: (part, break field, leading).
    let mut subsummaries: Vec<(usize, BreakField, bool)> = Vec::new();
    let mut ranges: Vec<(f64, f64)> = Vec::new();
    for (i, p) in layout.parts.iter().enumerate() {
        if !p.top.is_finite() || !p.height.is_finite() || p.top < 0.0 || p.height <= 0.0 {
            return Err("Invalid layout part geometry".into());
        }
        if ranges
            .iter()
            .any(|(top, bottom)| p.top < *bottom && p.bottom() > *top)
        {
            return Err("Overlapping layout parts are unsupported".into());
        }
        ranges.push((p.top, p.bottom()));
        // FileMaker prints a body with this bit as it prints one without.
        let flags = match p.kind() {
            PartKind::Body => p.flags & !DETAILS_BODY_FLAG,
            _ => p.flags,
        };
        let unsupported_options = flags != 0 || p.page_break_after != 0 || !p.other.is_empty();
        let slot = match p.kind() {
            PartKind::TopNavigation | PartKind::BottomNavigation => continue,
            PartKind::LeadingSubSummary | PartKind::TrailingSubSummary => {
                if unsupported_options || !push_subsummary(&mut subsummaries, i, p)? {
                    return Err("Duplicate sub-summary parts, page breaks, part flags and \
                         unknown part options are unsupported"
                        .into());
                }
                continue;
            }
            PartKind::Header => &mut header,
            PartKind::Body => &mut body,
            PartKind::Footer => &mut footer,
            PartKind::TitleHeader => &mut title_header,
            PartKind::TitleFooter => &mut title_footer,
            PartKind::LeadingGrandSummary => &mut leading,
            PartKind::TrailingGrandSummary => &mut trailing,
            kind => return Err(format!("Unsupported report part: {kind:?}")),
        };
        if slot.replace(i).is_some() || unsupported_options || !p.break_field_key.is_empty() {
            return Err(
                "Duplicate parts, part flags, breaks and unknown part options are unsupported"
                    .into(),
            );
        }
    }
    let body = body.ok_or("The report needs a body part")?;
    let groups = RecordGroups::from_parts(layout, &subsummaries, breaks.to_vec(), records)?;
    let mut items: Vec<(usize, usize)> = leading.map(|part| (part, 0)).into_iter().collect();
    for record in 0..records {
        items.extend(groups.leading(record).map(|band| (band.part, record)));
        items.push((body, record));
        items.extend(groups.trailing(record).map(|band| (band.part, record)));
    }
    items.extend(trailing.map(|part| (part, records - 1)));
    Ok(Flow {
        header,
        footer,
        title_header,
        title_footer,
        items,
    })
}

/// Note sub-summary part `index` of the layout (`part`): its break field
/// and side, in layout order. `false` when another part already has that
/// break field and side.
fn push_subsummary(
    subsummaries: &mut Vec<(usize, BreakField, bool)>,
    index: usize,
    part: &crate::Part,
) -> Result<bool, String> {
    let field = part
        .break_field()?
        .ok_or("A sub-summary part has no break field")?;
    let lead = part.kind() == PartKind::LeadingSubSummary;
    if subsummaries
        .iter()
        .any(|(_, f, l)| *f == field && *l == lead)
    {
        return Ok(false);
    }
    subsummaries.push((index, field, lead));
    Ok(true)
}

/// A sub-summary part shown for a group: once before the group's first
/// record (leading) or after its last (trailing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    /// Index in the layout's parts.
    pub part: usize,
    /// Its break ("when sorted by") field.
    pub field: BreakField,
    pub leading: bool,
}

/// The sub-summary parts around each record of a found set, from the groups
/// of the break fields it is sorted by. Before a record come the leading
/// parts of the groups it opens, outermost break first; after it come the
/// trailing parts of the groups it closes, innermost first. Each is shown
/// for its group's first (leading) or last (trailing) record. A part whose
/// break field the found set is not sorted by never shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordGroups {
    /// The showing parts, outermost break first, each with the index of
    /// its groups in `breaks`.
    active: Vec<(Band, usize)>,
    breaks: Vec<BreakGroups>,
    records: usize,
}

impl RecordGroups {
    /// The groups of `records` found records (`breaks`, as the found set's
    /// actual sort defines them) at the layout's sub-summary parts, as
    /// Browse shows them. It refuses malformed break keys, two parts for
    /// one break field and side, parts out of the sort's nesting, and
    /// groups that do not nest. Part options that only printing reads
    /// (page breaks, part flags) are not read here.
    pub fn new(layout: &Layout, breaks: Vec<BreakGroups>, records: usize) -> Result<Self, String> {
        let mut subsummaries = Vec::new();
        for (i, part) in layout.parts.iter().enumerate() {
            if matches!(
                part.kind(),
                PartKind::LeadingSubSummary | PartKind::TrailingSubSummary
            ) && !push_subsummary(&mut subsummaries, i, part)?
            {
                return Err(
                    "Two sub-summary parts for one break field and side are unsupported".into(),
                );
            }
        }
        Self::from_parts(layout, &subsummaries, breaks, records)
    }

    /// The showing parts ordered by sort depth (outermost first). A break
    /// the found set is not sorted by shows nothing, as in FileMaker. Parts
    /// must follow the sort's nesting (leading parts top-down from the
    /// outermost break, trailing parts bottom-up); other arrangements and
    /// inconsistent group data are refused rather than reinterpreted.
    fn from_parts(
        layout: &Layout,
        subsummaries: &[(usize, BreakField, bool)],
        breaks: Vec<BreakGroups>,
        records: usize,
    ) -> Result<Self, String> {
        for (i, b) in breaks.iter().enumerate() {
            if b.groups.len() != records
                || breaks[..i]
                    .iter()
                    .any(|other| other.field == b.field || other.depth == b.depth)
                || !subsummaries.iter().any(|(_, field, _)| *field == b.field)
            {
                return Err("The report groups do not match its sub-summary parts".into());
            }
        }
        let mut active: Vec<(Band, usize)> = subsummaries
            .iter()
            .filter_map(|&(part, field, leading)| {
                let index = breaks.iter().position(|b| b.field == field)?;
                Some((
                    Band {
                        part,
                        field,
                        leading,
                    },
                    index,
                ))
            })
            .collect();
        for lead in [true, false] {
            let mut sides: Vec<_> = active.iter().filter(|(b, _)| b.leading == lead).collect();
            sides.sort_by(|a, b| {
                layout.parts[a.0.part]
                    .top
                    .total_cmp(&layout.parts[b.0.part].top)
            });
            if sides
                .windows(2)
                .any(|pair| (breaks[pair[0].1].depth < breaks[pair[1].1].depth) != lead)
            {
                return Err(
                    "Sort the found set by the sub-summary break fields in the layout's part order"
                        .into(),
                );
            }
        }
        active.sort_by_key(|(_, index)| breaks[*index].depth);
        // A group of an outer break must also end every inner group.
        for pair in active.windows(2) {
            let (outer, inner) = (&breaks[pair[0].1].groups, &breaks[pair[1].1].groups);
            if (1..records).any(|i| outer[i] != outer[i - 1] && inner[i] == inner[i - 1]) {
                return Err("The report groups do not nest in sort order".into());
            }
        }
        Ok(Self {
            active,
            breaks,
            records,
        })
    }

    /// Whether no sub-summary part shows: the found set is sorted by none
    /// of their break fields.
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    /// The leading parts shown before `record`'s body: the groups it is
    /// the first record of, outermost break first.
    pub fn leading(&self, record: usize) -> impl Iterator<Item = Band> + '_ {
        self.active
            .iter()
            .filter(move |(band, index)| {
                let groups = &self.breaks[*index].groups;
                band.leading
                    && record < self.records
                    && (record == 0 || groups[record - 1] != groups[record])
            })
            .map(|(band, _)| *band)
    }

    /// The trailing parts shown after `record`'s body: the groups it is the
    /// last record of, innermost break first.
    pub fn trailing(&self, record: usize) -> impl Iterator<Item = Band> + '_ {
        self.active
            .iter()
            .rev()
            .filter(move |(band, index)| {
                let groups = &self.breaks[*index].groups;
                !band.leading
                    && record < self.records
                    && (record + 1 == self.records || groups[record + 1] != groups[record])
            })
            .map(|(band, _)| *band)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayoutDef, Part};
    #[test]
    fn repeats_parts_and_keeps_whole_records() {
        let layout = Layout {
            def: Some(LayoutDef {
                base_width: 540.0,
                ..Default::default()
            }),
            parts: [(1, 0.0, 40.0), (4, 40.0, 200.0), (7, 240.0, 40.0)]
                .into_iter()
                .map(|(part_type, top, height)| Part {
                    part_type,
                    top,
                    height,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let plan = paginate(&layout, 7, PageSize::default(), &[]).unwrap();
        assert_eq!(plan.pages.len(), 3);
        assert_eq!(
            plan.pages[1].iter().map(|p| p.record).collect::<Vec<_>>(),
            [3, 3, 4, 5, 5]
        );
        assert_eq!(plan.pages[2][1].top, 76.0);
        assert_eq!(plan.pages[2].last().unwrap().top, 716.0);
        let mut bad = layout.clone();
        bad.parts[1].height = 800.0;
        assert!(paginate(&bad, 1, PageSize::default(), &[]).is_err());
        bad.parts[1].part_type = 3;
        assert!(paginate(&bad, 1, PageSize::default(), &[]).is_err());
    }

    #[test]
    fn nested_sorted_groups_open_close_and_cross_pages() {
        // Break keys for fields 1 (outer) and 2 (inner) of occurrence 5.
        let key = |field| vec![0x06, 0x05, 0x00, 0x01, 0x05, 0x01, field];
        let mut top = 0.0;
        let layout = Layout {
            def: Some(LayoutDef {
                base_width: 540.0,
                ..Default::default()
            }),
            parts: [
                (1, 40.0, None),
                (3, 30.0, Some(1)),
                (3, 20.0, Some(2)),
                (4, 100.0, None),
                (5, 20.0, Some(2)),
                (5, 30.0, Some(1)),
                (6, 50.0, None),
                (7, 40.0, None),
            ]
            .into_iter()
            .map(|(part_type, height, field)| {
                let part = Part {
                    part_type,
                    top,
                    height,
                    break_field_key: field.map(key).unwrap_or_default(),
                    ..Default::default()
                };
                top += height;
                part
            })
            .collect(),
            ..Default::default()
        };
        let field = |field| BreakField {
            occurrence: 5,
            field,
        };
        assert_eq!(report_breaks(&layout).unwrap(), [field(1), field(2)]);
        let outer = BreakGroups {
            field: field(1),
            depth: 0,
            groups: vec![0, 0, 0, 1, 1, 1, 1],
        };
        let inner = BreakGroups {
            field: field(2),
            depth: 1,
            groups: vec![0, 0, 1, 2, 2, 3, 3],
        };
        let paper = PageSize::default();
        let plan = paginate(&layout, 7, paper.clone(), &[inner.clone(), outer.clone()]).unwrap();
        let parts = |page: &[Placement]| {
            page.iter()
                .map(|p| (p.part, p.record, p.top))
                .collect::<Vec<_>>()
        };
        assert_eq!(plan.pages.len(), 2);
        assert_eq!(
            parts(&plan.pages[0]),
            [
                (0, 0, 36.0),
                (1, 0, 76.0),
                (2, 0, 106.0),
                (3, 0, 126.0),
                (3, 1, 226.0),
                (4, 1, 326.0),
                (2, 2, 346.0),
                (3, 2, 366.0),
                (4, 2, 466.0),
                (5, 2, 486.0),
                (1, 3, 516.0),
                (2, 3, 546.0),
                (3, 3, 566.0),
                (7, 3, 716.0)
            ]
        );
        // The open outer and inner groups continue onto the next page.
        assert_eq!(
            parts(&plan.pages[1]),
            [
                (0, 4, 36.0),
                (3, 4, 76.0),
                (4, 4, 176.0),
                (2, 5, 196.0),
                (3, 5, 216.0),
                (3, 6, 316.0),
                (4, 6, 416.0),
                (5, 6, 436.0),
                (6, 6, 466.0),
                (7, 6, 716.0)
            ]
        );
        // A break the found set is not sorted by prints nothing.
        let outer_only = paginate(&layout, 7, paper.clone(), std::slice::from_ref(&outer)).unwrap();
        assert!(outer_only
            .pages
            .iter()
            .flatten()
            .all(|p| p.part != 2 && p.part != 4));
        // The sort must nest as the parts do, and groups must be consistent.
        let swapped = [
            BreakGroups {
                depth: 1,
                ..outer.clone()
            },
            BreakGroups {
                depth: 0,
                ..inner.clone()
            },
        ];
        assert!(paginate(&layout, 7, paper.clone(), &swapped)
            .unwrap_err()
            .contains("part order"));
        let crossing = BreakGroups {
            groups: vec![0, 0, 0, 0, 1, 1, 1],
            ..inner.clone()
        };
        assert!(paginate(&layout, 7, paper.clone(), &[outer.clone(), crossing]).is_err());
        assert!(paginate(&layout, 6, paper.clone(), std::slice::from_ref(&outer)).is_err());
        let mut bad = layout;
        bad.parts[2].break_field_key = vec![0x03, 0x02, 0x01, 0x05];
        assert!(report_breaks(&bad).is_err());
        assert!(paginate(&bad, 7, paper, &[outer]).is_err());
    }

    /// FileMaker Pro 26's Task List with 60 records, a first group of 22
    /// (#263): group B's heading would fit at the foot of page 2, but its
    /// first record would not, so the heading starts page 3 above it.
    #[test]
    fn a_leading_sub_summary_moves_to_the_next_page_with_its_first_record() {
        let key = vec![0x06, 0x05, 0x00, 0x01, 0x05, 0x01, 1];
        let mut top = 0.0;
        let layout = Layout {
            def: Some(LayoutDef {
                base_width: 540.0,
                ..Default::default()
            }),
            parts: [(1, 30.0, false), (3, 30.0, true), (4, 60.0, false)]
                .into_iter()
                .map(|(part_type, height, keyed)| {
                    let part = Part {
                        part_type,
                        top,
                        height,
                        break_field_key: if keyed { key.clone() } else { Vec::new() },
                        ..Default::default()
                    };
                    top += height;
                    part
                })
                .collect(),
            ..Default::default()
        };
        let groups = BreakGroups {
            field: BreakField {
                occurrence: 5,
                field: 1,
            },
            depth: 0,
            groups: (0..60).map(|r| usize::from(r >= 22)).collect(),
        };
        // 720 printable points: the header, 11 bodies and one heading fill
        // page 2 exactly when the heading stays there.
        let plan = paginate(&layout, 60, PageSize::default(), &[groups]).unwrap();
        let page = |n: usize| {
            plan.pages[n]
                .iter()
                .map(|p| (p.part, p.record))
                .collect::<Vec<_>>()
        };
        assert_eq!(plan.pages.len(), 6);
        // Page 2: the header and records 11-21, no heading.
        assert_eq!(page(1).len(), 12);
        assert!(page(1).iter().all(|&(part, _)| part != 1));
        assert_eq!(page(1).last(), Some(&(2, 21)));
        // Page 3: the header, group B's heading, then record 22.
        assert_eq!(&page(2)[..3], [(0, 22), (1, 22), (2, 22)]);

        // The run is measured on the page it would move to: a body that
        // shrinks there (700 points here, 100 there) still takes its
        // heading along.
        let field = BreakField {
            occurrence: 5,
            field: 1,
        };
        let two = |groups: Vec<usize>| BreakGroups {
            field,
            depth: 0,
            groups,
        };
        let mut tall = layout.clone();
        tall.parts.remove(0);
        tall.parts[0].top = 0.0;
        tall.parts[1].top = 30.0;
        tall.parts[1].height = 700.0;
        let plan = paginate_with(
            &tall,
            2,
            PageSize::default(),
            &[two(vec![0, 1])],
            |part, record, page| -> Result<f64, String> {
                Ok(match (part, record, page) {
                    (0, _, _) => 30.0,
                    (_, 0, _) => 600.0,
                    (_, _, 0) => 700.0,
                    _ => 100.0,
                })
            },
        )
        .unwrap();
        let parts = |n: usize| {
            plan.pages[n]
                .iter()
                .map(|p| (p.part, p.record))
                .collect::<Vec<_>>()
        };
        assert_eq!(parts(0), [(0, 0), (1, 0)]);
        assert_eq!(parts(1), [(0, 1), (1, 1)]);

        // A title header that leaves no room for the first record makes a
        // title-only cover rather than stranding the first heading.
        let mut titled = tall;
        titled.parts.insert(
            0,
            Part {
                part_type: 0,
                height: 620.0,
                ..Default::default()
            },
        );
        titled.parts[1].top = 620.0;
        titled.parts[2].top = 650.0;
        titled.parts[2].height = 80.0;
        let plan = paginate(&titled, 1, PageSize::default(), &[two(vec![0])]).unwrap();
        let parts = |n: usize| {
            plan.pages[n]
                .iter()
                .map(|p| (p.part, p.record))
                .collect::<Vec<_>>()
        };
        assert_eq!(plan.pages.len(), 2);
        assert_eq!(parts(0), [(0, 0)]);
        assert_eq!(parts(1), [(1, 0), (2, 0)]);
    }

    #[test]
    fn title_replacements_grands_and_cover_pages_fit_without_splitting() {
        let mut top = 0.0;
        let mut layout = Layout {
            def: Some(LayoutDef {
                base_width: 540.0,
                ..Default::default()
            }),
            parts: [
                (0, 80.0),
                (1, 40.0),
                (2, 100.0),
                (4, 140.0),
                (6, 500.0),
                (7, 40.0),
                (8, 60.0),
            ]
            .into_iter()
            .map(|(part_type, height)| {
                let part = Part {
                    part_type,
                    top,
                    height,
                    ..Default::default()
                };
                top += height;
                part
            })
            .collect(),
            ..Default::default()
        };
        let paper = PageSize::default();
        let plan = paginate(&layout, 9, paper.clone(), &[]).unwrap();
        assert_eq!(plan.paper, paper);
        let parts = |page: usize| {
            plan.pages[page]
                .iter()
                .map(|p| (p.part, p.record, p.top))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parts(0),
            [
                (0, 0, 36.0),
                (2, 0, 116.0),
                (3, 0, 216.0),
                (3, 1, 356.0),
                (3, 2, 496.0),
                (6, 8, 696.0)
            ]
        );
        assert_eq!(
            parts(1),
            [
                (1, 3, 36.0),
                (3, 3, 76.0),
                (3, 4, 216.0),
                (3, 5, 356.0),
                (3, 6, 496.0),
                (5, 6, 716.0)
            ]
        );
        assert_eq!(
            parts(2),
            [(1, 7, 36.0), (3, 7, 76.0), (3, 8, 216.0), (5, 8, 716.0)]
        );
        assert_eq!(parts(3), [(1, 8, 36.0), (4, 8, 76.0), (5, 8, 716.0)]);
        assert_eq!(plan.pages.len(), 4);
        // A full-page title can be a cover; flow resumes on ordinary paper.
        layout.parts[0].height = 720.0;
        let mut offset = 0.0;
        for part in &mut layout.parts {
            part.top = offset;
            offset += part.height;
        }
        layout.parts.pop(); // No title footer.
        layout.parts.pop(); // No ordinary footer on the cover.
        let cover = paginate(&layout, 1, paper.clone(), &[]).unwrap();
        assert_eq!(
            cover.pages[0],
            [Placement {
                part: 0,
                record: 0,
                top: 36.0,
                height: 720.0,
            }]
        );
        assert_eq!(cover.pages[1][0].part, 1);
        layout.parts[4].height = 721.0;
        assert!(paginate(&layout, 1, paper, &[])
            .unwrap_err()
            .contains("splitting parts"));
    }

    #[test]
    fn measured_parts_flow_by_printed_height_and_remeasure_on_their_new_page() {
        let mut top = 0.0;
        let layout = Layout {
            def: Some(LayoutDef {
                base_width: 540.0,
                ..Default::default()
            }),
            parts: [(1, 40.0), (4, 200.0), (6, 100.0), (7, 40.0)]
                .into_iter()
                .map(|(part_type, height)| {
                    let part = Part {
                        part_type,
                        top,
                        height,
                        ..Default::default()
                    };
                    top += height;
                    part
                })
                .collect(),
            ..Default::default()
        };
        // Body heights shrink by record; the grand summary is 100 high on
        // page 1 and 60 on any later page.
        let mut calls = Vec::new();
        let plan = paginate_with(
            &layout,
            6,
            PageSize::default(),
            &[],
            |part, record, page| {
                calls.push((part, record, page));
                Ok::<_, String>(match part {
                    1 => [200.0, 50.0, 120.0, 0.0, 200.0, 80.0][record],
                    2 if page == 0 => 100.0,
                    _ => 60.0,
                })
            },
        )
        .unwrap();
        let parts = |page: usize| {
            plan.pages[page]
                .iter()
                .map(|p| (p.part, p.record, p.top, p.height))
                .collect::<Vec<_>>()
        };
        // Printable 720 - header 40 - footer 40 = 640 per page.
        assert_eq!(
            parts(0),
            [
                (0, 0, 36.0, 40.0),
                (1, 0, 76.0, 200.0),
                (1, 1, 276.0, 50.0),
                (1, 2, 326.0, 120.0),
                (1, 3, 446.0, 0.0),
                (1, 4, 446.0, 200.0),
                (3, 4, 716.0, 40.0)
            ]
        );
        assert_eq!(
            parts(1),
            [
                (0, 5, 36.0, 40.0),
                (1, 5, 76.0, 80.0),
                (2, 5, 156.0, 60.0),
                (3, 5, 716.0, 40.0)
            ]
        );
        // Record 5 (80 high) does not fit page 1's remaining 70 points and
        // is measured again on page 2; the grand summary only there.
        assert!(calls.contains(&(1, 5, 0)) && calls.contains(&(1, 5, 1)));
        assert!(!calls.contains(&(2, 5, 0)));
        // A height outside the stored part is refused, as is a measurement
        // error, before any plan exists.
        assert!(
            paginate_with(&layout, 1, PageSize::default(), &[], |_, _, _| Ok::<
                _,
                String,
            >(
                201.0
            ))
            .unwrap_err()
            .contains("outside its bounds")
        );
        assert_eq!(
            paginate_with(&layout, 1, PageSize::default(), &[], |_, _, _| Err(
                "pending".to_owned()
            ))
            .unwrap_err(),
            "pending"
        );
    }
}
