//! Portal viewport geometry and read-only row painting. A row reuses the
//! layout's first-row objects with a translated origin and a distinct data
//! context; it never uses the parent page as its own record.

use std::ops::Range;
use std::sync::Arc;

use egui::{pos2, vec2, Rect, ScrollArea, Sense, Ui, UiBuilder, Vec2};
use om_layout::{Layout, LayoutObject, ResolvedStyle};

use super::layout_cache::LayoutCache;
use crate::calc::{Evaluator, PortalRow};

const SCROLLS: i32 = 0x1;
/// "Place row for new record at top" (FileMaker Pro 26.1, layout-model.md
/// 5.3.2): the row for a new related record comes first, in a portal that
/// does not scroll.
const NEW_ROW_TOP: i32 = 0x400;

#[derive(Clone, Copy, Debug)]
pub(super) struct PortalGeometry {
    pub viewport: Rect,
    pub row_height: f32,
    pub first_record: usize,
    pub scrolls: bool,
    /// The rows without a record come before the records
    /// ([`NEW_ROW_TOP`]).
    pub new_row_top: bool,
}

impl PortalGeometry {
    pub fn new(object: &LayoutObject, rect: Rect, style: &ResolvedStyle) -> Option<Self> {
        let portal = object.portal.as_ref()?;
        if !rect.is_finite() || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return None;
        }
        let border = |side: usize| super::border_width(style, side).unwrap_or(0.0);
        let viewport = Rect::from_min_max(
            rect.min + vec2(border(3), border(0)),
            rect.max - vec2(border(1), border(2)),
        );
        if !viewport.is_positive() {
            return None;
        }
        let count = if portal.num_rows.is_finite() {
            portal.num_rows.clamp(1.0, 10_000.0)
        } else {
            1.0
        };
        // Anchoring a portal taller exposes additional rows; it does not
        // stretch the fields of every row along with the frame.
        let original_height = object
            .bounds
            .as_ref()
            .map(|bounds| bounds.height() as f32)
            .filter(|height| height.is_finite() && *height > 0.0)
            .unwrap_or(rect.height());
        let inner_height = (original_height - border(0) - border(2)).max(1.0);
        Some(Self {
            viewport,
            row_height: (inner_height / count as f32).max(1.0),
            first_record: portal.initial_row.saturating_sub(1).max(0) as usize,
            scrolls: portal.flags & SCROLLS != 0,
            // FileMaker Pro 26.1.1 takes the top row only while the portal
            // does not scroll (`DataEntryModel::IsVisibleTopNewPortalRow`).
            new_row_top: portal.flags & NEW_ROW_TOP != 0 && portal.flags & SCROLLS == 0,
        })
    }

    pub fn slots(&self) -> usize {
        (self.viewport.height() / self.row_height).ceil().max(1.0) as usize
    }

    /// Logical rows that intersect a clip, with one extra edge row for
    /// partially visible text/borders. No offscreen row needs field data.
    pub fn visible(&self, clip: Rect, total: usize) -> Range<usize> {
        let clip = self.viewport.intersect(clip);
        if !clip.is_positive() {
            return 0..0;
        }
        let start = ((clip.top() - self.viewport.top()) / self.row_height)
            .floor()
            .max(0.0) as usize;
        let end = ((clip.bottom() - self.viewport.top()) / self.row_height)
            .ceil()
            .max(0.0) as usize;
        start.min(total)..end.min(total)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PortalPaintRow {
    /// No record exists for a configured relationship's creation template.
    /// It must only paint empty field outlines, never evaluate or edit a row.
    pub context: Option<PortalRow>,
    /// Without a record: the index of the record the user began in a
    /// creation row this row shows, or that typing into the creation row
    /// begins (the next index).
    pub new: Option<usize>,
    /// The row's number in the portal, from 1.
    pub number: usize,
    /// Add to the document origin when painting the first-row objects.
    pub origin_delta: Vec2,
    /// The portal's records are being read again: the row is one it showed
    /// last, at the number it had then.
    pub stale: bool,
}

pub(super) struct PortalStyles {
    frame: ResolvedStyle,
    normal: ResolvedStyle,
    alternate: ResolvedStyle,
    active: ResolvedStyle,
    use_alternate: bool,
    use_active: bool,
}

/// A portal's frame and row components use the file's style cascade.
pub(super) fn styles(cache: &LayoutCache, layout: &Layout, object: &LayoutObject) -> PortalStyles {
    let style = |component| cache.formatted_style(layout, object, &[], component, 1);
    let enabled = |property| cache.property_set(layout, object, 1, 1, property);
    PortalStyles {
        frame: style(1),
        normal: style(12),
        alternate: style(13),
        active: style(28),
        use_alternate: enabled(610),
        use_active: enabled(620),
    }
}

/// The portal that contains layout object `object`: the nearest portal
/// in its parent chain.
pub(crate) fn enclosing(layout: &Layout, object: u64) -> Option<u64> {
    let objects = &layout.objects;
    let mut at = objects.get(object)?.parent;
    // A chain longer than the store is a cycle.
    for _ in 0..=objects.objects.len() {
        let parent = objects.get(at?)?;
        if parent.kind() == om_layout::ObjectKind::Portal {
            return Some(parent.id);
        }
        at = parent.parent;
    }
    None
}

/// What painting a portal found.
pub(super) struct Shown {
    /// A row was clicked: the row takes the click from the record behind
    /// it, which must still become the current record.
    pub clicked: bool,
    /// The related record of the row clicked, and the row's number.
    pub selected: Option<(u64, usize)>,
    /// Its rows are not known yet (their data is being fetched).
    pub pending: bool,
}

/// Paint the portal's visible related records, then the `new_rows` records
/// the user began in its creation row and the creation row. Empty row
/// slots retain their row styles and borders but never receive the parent
/// record's values.
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    ui: &mut Ui,
    layout: &Layout,
    styles: PortalStyles,
    object: &LayoutObject,
    rect: Rect,
    (parent, new_rows): (u64, usize),
    (selected, deleted): (Option<u64>, &[u64]),
    calc: Option<&Evaluator<'_>>,
    (read, pending): (Option<Arc<[u64]>>, bool),
    paint: impl FnMut(&mut Ui, PortalPaintRow),
) -> Shown {
    let mut shown = Shown {
        clicked: false,
        selected: None,
        pending: false,
    };
    let Some(geometry) = PortalGeometry::new(object, rect, &styles.frame) else {
        return shown;
    };
    let Some(occurrence) = object
        .portal
        .as_ref()
        .and_then(|portal| om_layout::occurrence_id(portal.table_alias_key))
    else {
        return shown;
    };
    // `read`: the records as read now, or while they are read again
    // (`pending`), those the portal showed last
    // (`DocumentState::portal_records`, #536).
    shown.pending = pending;
    // Records the user or a script deleted with the parent's next commit
    // are gone from the portal already.
    let records = read.map(|records| {
        records
            .iter()
            .copied()
            .filter(|record| !deleted.contains(record))
            .collect::<Vec<u64>>()
    });
    let creation_row =
        records.is_some() && calc.is_some_and(|calc| calc.portal_allows_creation(object));
    let known = records.is_some();
    let records = records.as_deref().unwrap_or_default();
    let clicked = paint_rows(
        ui,
        object,
        rect,
        parent,
        PortalRecords {
            layout_id: layout.id,
            styles,
            geometry,
            occurrence,
            records,
            new_rows: if known { new_rows } else { 0 },
            creation_row,
            selected,
            stale: shown.pending,
        },
        paint,
    );
    shown.clicked = clicked.is_some();
    // The clicked row's index counts from the portal's first row, before
    // its initial row.
    shown.selected = clicked.and_then(|(index, record)| Some((record?, index + 1)));
    shown
}

struct PortalRecords<'a> {
    layout_id: u64,
    styles: PortalStyles,
    geometry: PortalGeometry,
    occurrence: u64,
    records: &'a [u64],
    new_rows: usize,
    creation_row: bool,
    /// The related record of the row the user selected.
    selected: Option<u64>,
    stale: bool,
}

fn paint_rows(
    ui: &mut Ui,
    object: &LayoutObject,
    rect: Rect,
    parent: u64,
    rows: PortalRecords<'_>,
    mut paint: impl FnMut(&mut Ui, PortalPaintRow),
) -> Option<(usize, Option<u64>)> {
    let PortalRecords {
        layout_id,
        styles,
        geometry,
        occurrence,
        records,
        new_rows,
        creation_row,
        selected,
        stale,
    } = rows;
    let records = records.get(geometry.first_record..).unwrap_or_default();
    let slots = geometry.slots();
    // The rows without a record: those begun in the creation row, then
    // the creation row.
    let unsaved = new_rows.saturating_add(usize::from(creation_row));
    let total = if geometry.scrolls {
        records.len().saturating_add(unsaved).max(slots)
    } else {
        slots
    };
    // Scoped to the window's UI: windows can show the same portal row.
    let id = ui.scope_id().with(("portal", layout_id, object.id, parent));
    let mut child = ui.new_child(UiBuilder::new().max_rect(geometry.viewport));
    child.set_clip_rect(ui.clip_rect().intersect(geometry.viewport));
    // show_rows includes the UI's item spacing in its row pitch. Portal
    // rows touch; applying this inside its callback would be too late.
    child.spacing_mut().item_spacing.y = 0.0;
    let mut borders = Vec::new();
    let mut clicked = None;
    let mut paint_row = |ui: &mut Ui, index: usize, row_rect: Rect| {
        // The row's place among the records and the rows without one,
        // which come after them, or first with `new_row_top`. It is
        // numbered where it is drawn, as scripts number it
        // (`WorkerHost::portal_rows`) [I: how FileMaker numbers a row drawn
        // at the top is unmeasured].
        let top = geometry.new_row_top && unsaved > 0;
        let place = match top {
            true if index < unsaved => records.len() + index,
            true => index - unsaved,
            false => index,
        };
        let record_index = geometry.first_record + index;
        let mut style = styles.normal.clone();
        if styles.use_alternate && record_index % 2 == 1 {
            style.overlay(&styles.alternate);
        }
        let record = records.get(place).copied();
        if styles.use_active && record.is_some() && record == selected {
            style.overlay(&styles.active);
        }
        let mut fill = style.clone();
        fill.border_width = [None; 4];
        super::paint_box(ui, row_rect, &fill, om_layout::ObjectKind::Rectangle);
        style.background = None;
        borders.push((row_rect, style));
        // At the top only the first rows are without a record: the slots
        // past the records stay empty.
        let new = if top {
            (index < unsaved).then_some(index)
        } else {
            place
                .checked_sub(records.len())
                .filter(|new| *new < unsaved)
        };
        if record.is_none() && new.is_none() {
            return;
        }
        // FileMaker's tab order does not stop on portal rows, so a row only
        // senses clicks; Sense::click() would also make it focusable. A row
        // shown while the records are read again takes no click: its
        // record may have left the portal.
        let response = ui.interact(row_rect, id.with(record_index), Sense::CLICK);
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Row,
                true,
                format!("Portal Row {}", record_index + 1),
            )
        });
        if response.clicked() && !stale {
            clicked = Some((record_index, record));
        }
        let mut row_ui = ui.new_child(
            UiBuilder::new()
                .id_salt((object.id, record_index))
                .max_rect(row_rect),
        );
        row_ui.set_clip_rect(ui.clip_rect().intersect(row_rect));
        paint(
            &mut row_ui,
            PortalPaintRow {
                context: record.map(|record| PortalRow {
                    occurrence,
                    record,
                    number: record_index + 1,
                }),
                new: new.filter(|_| record.is_none()),
                number: record_index + 1,
                origin_delta: row_rect.min - rect.min,
                stale,
            },
        );
    };
    if geometry.scrolls {
        let output = ScrollArea::vertical()
            .id_salt(id)
            .auto_shrink([false, false])
            .max_height(geometry.viewport.height())
            .min_scrolled_height(geometry.viewport.height())
            .show_rows(&mut child, geometry.row_height, total, |ui, rows| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for index in rows {
                    let (row, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), geometry.row_height),
                        Sense::hover(),
                    );
                    paint_row(ui, index, row);
                }
            });
        super::note_scroll_area(&child, output.inner_rect);
    } else {
        for index in geometry.visible(child.clip_rect(), total) {
            let row = Rect::from_min_size(
                pos2(
                    geometry.viewport.left(),
                    geometry.viewport.top() + index as f32 * geometry.row_height,
                ),
                vec2(geometry.viewport.width(), geometry.row_height),
            );
            paint_row(&mut child, index, row);
        }
    }
    // Portal fields may be taller than their row. Their opaque fills must
    // not cover the shared rules; paint those after all visible row content.
    for (rect, style) in borders {
        super::paint_box(&child, rect, &style, om_layout::ObjectKind::Rectangle);
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Id;
    use om_layout::{PortalObject, Rect as LayoutRect};

    fn portal(rows: f64, initial: i32, flags: i32) -> LayoutObject {
        LayoutObject {
            id: 1,
            object_type: 9,
            bounds: Some(LayoutRect {
                top: 0.0,
                left: 0.0,
                bottom: 102.0,
                right: 202.0,
                ..Default::default()
            }),
            portal: Some(PortalObject {
                table_alias_key: 1_065_090,
                num_rows: rows,
                initial_row: initial,
                flags,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn row_height_inset_initial_row_and_resize_are_independent() {
        let object = portal(5.0, 3, SCROLLS);
        let style = ResolvedStyle {
            border_width: [Some(1.0); 4],
            border_style: [Some(3); 4],
            ..Default::default()
        };
        let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(202.0, 162.0));
        let geometry = PortalGeometry::new(&object, rect, &style).unwrap();
        assert_eq!(geometry.viewport.min, pos2(21.0, 41.0));
        assert_eq!(geometry.row_height, 20.0);
        assert_eq!(geometry.first_record, 2);
        assert_eq!(geometry.slots(), 8);
        assert!(geometry.scrolls);
        assert_eq!(
            geometry.visible(Rect::from_min_max(pos2(0.0, 72.0), pos2(400.0, 119.0)), 100),
            1..4
        );
    }

    #[test]
    fn nonfinite_and_empty_geometry_cannot_create_unbounded_rows() {
        let object = portal(f64::NAN, -10, 0);
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(202.0, 102.0));
        let geometry = PortalGeometry::new(&object, rect, &ResolvedStyle::default()).unwrap();
        assert_eq!(geometry.row_height, 102.0);
        assert_eq!(geometry.first_record, 0);
        assert_eq!(geometry.visible(Rect::NOTHING, 10_000), 0..0);
        assert!(PortalGeometry::new(&object, Rect::NOTHING, &ResolvedStyle::default()).is_none());
    }

    #[test]
    fn portal_rows_repeat_translate_clip_and_leave_empty_slots_empty() {
        let context = egui::Context::default();
        let layout = Layout::default();
        let object = portal(5.0, 2, 0);
        let rect = Rect::from_min_size(pos2(30.0, 50.0), vec2(202.0, 102.0));
        let geometry = PortalGeometry::new(&object, rect, &ResolvedStyle::default()).unwrap();
        let mut painted = Vec::new();
        context
            .run_ui(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    paint_rows(
                        ui,
                        &object,
                        rect,
                        7,
                        PortalRecords {
                            layout_id: layout.id,
                            styles: styles(&LayoutCache::default(), &layout, &object),
                            geometry,
                            occurrence: 2,
                            records: &[10, 20, 30],
                            new_rows: 0,
                            creation_row: false,
                            selected: None,
                            stale: false,
                        },
                        |ui, row| painted.push((row, ui.clip_rect())),
                    );
                });
            })
            .drop_without_applying_deltas();
        assert_eq!(
            painted
                .iter()
                .map(|(row, _)| row.context.unwrap().record)
                .collect::<Vec<_>>(),
            [20, 30]
        );
        assert_eq!(painted[0].0.context.unwrap().number, 2);
        assert_eq!(painted[0].0.origin_delta, Vec2::ZERO);
        assert!((painted[1].0.origin_delta - vec2(0.0, geometry.row_height)).length() < 0.001);
        for (row, clip) in &painted {
            let row_rect = Rect::from_min_size(
                rect.min + row.origin_delta,
                vec2(geometry.viewport.width(), geometry.row_height),
            );
            assert!(row_rect.contains_rect(*clip));
            assert!(geometry.viewport.contains_rect(*clip));
        }
        painted.clear();
        context
            .run_ui(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    paint_rows(
                        ui,
                        &object,
                        rect,
                        7,
                        PortalRecords {
                            layout_id: layout.id,
                            styles: styles(&LayoutCache::default(), &layout, &object),
                            geometry,
                            occurrence: 2,
                            records: &[],
                            new_rows: 0,
                            creation_row: false,
                            selected: None,
                            stale: false,
                        },
                        |ui, row| painted.push((row, ui.clip_rect())),
                    );
                });
            })
            .drop_without_applying_deltas();
        assert!(
            painted.is_empty(),
            "empty rows must not paint the parent record"
        );
    }

    #[test]
    fn configured_creation_template_has_no_record_context() {
        let context = egui::Context::default();
        let layout = Layout::default();
        let object = portal(5.0, 1, 0);
        let rect = Rect::from_min_size(pos2(30.0, 50.0), vec2(202.0, 102.0));
        let geometry = PortalGeometry::new(&object, rect, &ResolvedStyle::default()).unwrap();
        for creation_row in [false, true] {
            let mut painted = Vec::new();
            context
                .run_ui(egui::RawInput::default(), |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        paint_rows(
                            ui,
                            &object,
                            rect,
                            7,
                            PortalRecords {
                                layout_id: 0,
                                styles: styles(&LayoutCache::default(), &layout, &object),
                                geometry,
                                occurrence: 2,
                                records: &[10, 20],
                                new_rows: 0,
                                creation_row,
                                selected: None,
                                stale: false,
                            },
                            |_, row| painted.push(row.context),
                        );
                    });
                })
                .drop_without_applying_deltas();
            assert_eq!(painted.len(), if creation_row { 3 } else { 2 });
            assert_eq!(painted[0].unwrap().record, 10);
            assert_eq!(painted[1].unwrap().record, 20);
            if creation_row {
                assert!(
                    painted[2].is_none(),
                    "a template must not invent a record ID"
                );
            }
        }
    }

    /// "Place row for new record at top" (`0x400`) draws the creation row
    /// first, numbered 1, in a portal that does not scroll; a scrolling
    /// portal keeps it last, as FileMaker Pro 26.1.1 does.
    #[test]
    fn the_new_record_row_comes_first_when_the_portal_asks_and_does_not_scroll() {
        let context = egui::Context::default();
        let layout = Layout::default();
        let rect = Rect::from_min_size(pos2(30.0, 50.0), vec2(202.0, 102.0));
        for (flags, top) in [
            (NEW_ROW_TOP, true),
            (NEW_ROW_TOP | SCROLLS, false),
            (0, false),
        ] {
            let object = portal(5.0, 1, flags);
            let geometry = PortalGeometry::new(&object, rect, &ResolvedStyle::default()).unwrap();
            assert_eq!(geometry.new_row_top, top, "{flags:#x}");
            let mut painted = Vec::new();
            context
                .run_ui(egui::RawInput::default(), |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        paint_rows(
                            ui,
                            &object,
                            rect,
                            7,
                            PortalRecords {
                                layout_id: 0,
                                styles: styles(&LayoutCache::default(), &layout, &object),
                                geometry,
                                occurrence: 2,
                                records: &[10, 20],
                                new_rows: 0,
                                creation_row: true,
                                selected: None,
                                stale: false,
                            },
                            |_, row| {
                                painted.push((row.context.map(|c| c.record), row.new, row.number))
                            },
                        );
                    });
                })
                .drop_without_applying_deltas();
            let expected = if top {
                vec![(None, Some(0), 1), (Some(10), None, 2), (Some(20), None, 3)]
            } else {
                vec![(Some(10), None, 1), (Some(20), None, 2), (None, Some(0), 3)]
            };
            assert_eq!(painted, expected, "{flags:#x}");
        }
    }

    #[test]
    fn shared_row_rules_paint_after_opaque_field_contents() {
        let context = egui::Context::default();
        let layout = Layout::default();
        let object = portal(5.0, 1, 0);
        let rect = Rect::from_min_size(pos2(30.0, 50.0), vec2(202.0, 102.0));
        let geometry = PortalGeometry::new(&object, rect, &ResolvedStyle::default()).unwrap();
        let rule = egui::Color32::BLUE;
        let output = context.run_ui(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let mut appearance = styles(&LayoutCache::default(), &layout, &object);
                appearance.normal.border_width[2] = Some(0.25);
                appearance.normal.border_style[2] = Some(3);
                appearance.normal.border_color[2] = Some(om_layout::Color {
                    red: 0.0,
                    green: 0.0,
                    blue: 1.0,
                    alpha: 1.0,
                });
                paint_rows(
                    ui,
                    &object,
                    rect,
                    7,
                    PortalRecords {
                        layout_id: 0,
                        styles: appearance,
                        geometry,
                        occurrence: 2,
                        records: &[10, 20],
                        new_rows: 0,
                        creation_row: false,
                        selected: None,
                        stale: false,
                    },
                    |ui, row| {
                        let field = Rect::from_min_size(
                            rect.min + row.origin_delta,
                            vec2(rect.width(), geometry.row_height + 1.0),
                        );
                        ui.painter().rect_filled(field, 0, egui::Color32::WHITE);
                    },
                );
            });
        });
        let last_field = output.shapes.iter().rposition(|shape| {
            matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == egui::Color32::WHITE)
        }).unwrap();
        let rules: Vec<_> = output.shapes.iter().enumerate().filter_map(|(i, shape)| {
            matches!(&shape.shape, egui::Shape::LineSegment { stroke, .. } if stroke.color == rule)
                .then_some(i)
        }).collect();
        assert_eq!(rules.len(), 5);
        assert!(rules.iter().all(|i| *i > last_field));
        output.drop_without_applying_deltas();
    }

    #[test]
    fn a_large_portal_only_paints_viewport_rows_and_scrolls_them() {
        let context = egui::Context::default();
        let layout = Layout::default();
        let object = portal(5.0, 1, SCROLLS);
        let rect = Rect::from_min_size(pos2(30.0, 50.0), vec2(202.0, 102.0));
        let geometry = PortalGeometry::new(&object, rect, &ResolvedStyle::default()).unwrap();
        let records: Vec<u64> = (1..=1000).collect();
        let mut first_row = 0;
        for frame in 0..12 {
            let mut painted = Vec::new();
            let mut input = egui::RawInput {
                time: Some(frame as f64 * 0.1),
                ..Default::default()
            };
            if frame == 1 {
                input.events = vec![
                    egui::Event::PointerMoved(rect.center()),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: vec2(0.0, -400.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: egui::Modifiers::NONE,
                    },
                ];
            }
            context
                .run_ui(input, |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        paint_rows(
                            ui,
                            &object,
                            rect,
                            7,
                            PortalRecords {
                                layout_id: layout.id,
                                styles: styles(&LayoutCache::default(), &layout, &object),
                                geometry,
                                occurrence: 2,
                                records: &records,
                                new_rows: 0,
                                creation_row: false,
                                selected: None,
                                stale: false,
                            },
                            |_, row| painted.push(row.context.unwrap().record),
                        );
                    });
                })
                .drop_without_applying_deltas();
            assert!(!painted.is_empty());
            assert!(
                painted.len() <= 7,
                "a thousand-row portal must virtualize: {painted:?}"
            );
            if frame == 0 {
                assert_eq!(painted[0], 1);
            }
            first_row = painted[0];
        }
        assert!(
            first_row > 1,
            "wheel scrolling must advance the related row viewport"
        );
    }

    #[test]
    fn the_wheel_over_an_open_popover_scrolls_it_and_not_a_portal_it_covers() {
        let context = egui::Context::default();
        let layout = Layout::default();
        let covered = portal(5.0, 1, SCROLLS);
        let inside = LayoutObject {
            id: 2,
            ..portal(5.0, 1, SCROLLS)
        };
        let covered_rect = Rect::from_min_size(pos2(30.0, 50.0), vec2(202.0, 102.0));
        let popover = Rect::from_min_size(pos2(20.0, 40.0), vec2(300.0, 220.0));
        let inside_rect = Rect::from_min_size(pos2(60.0, 120.0), vec2(202.0, 102.0));
        // Over both portals, so either could take the wheel.
        let pointer = pos2(100.0, 140.0);
        let surface = Id::unique("popover surface");
        let records: Vec<u64> = (1..=1000).collect();
        let rows = |object: &LayoutObject, rect: Rect| PortalRecords {
            layout_id: layout.id,
            styles: styles(&LayoutCache::default(), &layout, object),
            geometry: PortalGeometry::new(object, rect, &ResolvedStyle::default()).unwrap(),
            occurrence: 2,
            records: &records,
            new_rows: 0,
            creation_row: false,
            selected: None,
            stale: false,
        };
        let (mut covered_first, mut inside_first) = (0, 0);
        for frame in 0..12 {
            let mut input = egui::RawInput {
                time: Some(frame as f64 * 0.1),
                ..Default::default()
            };
            if frame == 1 {
                input.events = vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: vec2(0.0, -400.0),
                        phase: egui::TouchPhase::Move,
                        source: egui::MouseWheelSource::Unknown,
                        modifiers: egui::Modifiers::NONE,
                    },
                ];
            }
            let (mut under, mut over) = (Vec::new(), Vec::new());
            context
                .run_ui(input, |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        // As Form view paints: the layout, then the popover.
                        let wheel = super::super::hold_popover_wheel(ui, Some(surface));
                        paint_rows(
                            ui,
                            &covered,
                            covered_rect,
                            7,
                            rows(&covered, covered_rect),
                            |_, row| under.push(row.context.unwrap().record),
                        );
                        super::super::release_popover_wheel(ui, wheel);
                        super::super::popover_surface(ui, popover, surface);
                        paint_rows(
                            ui,
                            &inside,
                            inside_rect,
                            7,
                            rows(&inside, inside_rect),
                            |_, row| over.push(row.context.unwrap().record),
                        );
                    });
                })
                .drop_without_applying_deltas();
            covered_first = under[0];
            inside_first = over[0];
        }
        assert_eq!(covered_first, 1, "the covered portal must not scroll");
        assert!(inside_first > 1, "the popover's own portal scrolls");
    }

    #[test]
    fn rows_report_clicks_but_are_not_tab_stops() {
        let context = egui::Context::default();
        let layout = Layout::default();
        let object = portal(5.0, 1, 0);
        let rect = Rect::from_min_size(pos2(30.0, 50.0), vec2(202.0, 102.0));
        let geometry = PortalGeometry::new(&object, rect, &ResolvedStyle::default()).unwrap();
        let frame = |events: Vec<egui::Event>| {
            let mut clicked = false;
            context
                .run_ui(
                    egui::RawInput {
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            clicked = paint_rows(
                                ui,
                                &object,
                                rect,
                                7,
                                PortalRecords {
                                    layout_id: layout.id,
                                    styles: styles(&LayoutCache::default(), &layout, &object),
                                    geometry,
                                    occurrence: 2,
                                    records: &[10, 20],
                                    new_rows: 0,
                                    creation_row: true,
                                    selected: None,
                                    stale: false,
                                },
                                |_, _| {},
                            )
                            .is_some();
                        });
                    },
                )
                .drop_without_applying_deltas();
            clicked
        };
        assert!(!frame(Vec::new()));
        let tab = egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        for _ in 0..4 {
            assert!(!frame(vec![tab.clone()]));
            assert_eq!(
                context.memory(|memory| memory.focused()),
                None,
                "Tab must not stop on a portal row"
            );
        }
        let second_row = pos2(rect.left() + 10.0, rect.top() + geometry.row_height * 1.5);
        let click = |pressed| egui::Event::PointerButton {
            pos: second_row,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        assert!(frame(vec![
            egui::Event::PointerMoved(second_row),
            click(true),
            click(false),
        ]));
    }

    #[test]
    fn the_row_viewport_insets_exactly_the_borders_the_frame_paints() {
        let context = egui::Context::default();
        let object = portal(5.0, 1, 0);
        let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(202.0, 102.0));
        for (border_style, painted) in [
            (Some(1), Some(false)),
            (Some(5), Some(true)),
            (None, None),
            (Some(0), None),
            (Some(2), None),
            (Some(3), None),
        ] {
            let style = ResolvedStyle {
                border_width: [Some(2.0); 4],
                border_style: [border_style; 4],
                ..Default::default()
            };
            let output = context.run_ui(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    super::super::paint_box(ui, rect, &style, om_layout::ObjectKind::Portal);
                });
            });
            let rules = output
                .shapes
                .iter()
                .filter(|shape| matches!(shape.shape, egui::Shape::LineSegment { .. }))
                .count();
            output.drop_without_applying_deltas();
            assert!(rules == 0 || rules == 4, "{border_style:?}");
            if let Some(painted) = painted {
                assert_eq!(rules == 4, painted, "{border_style:?}");
            }
            let viewport = PortalGeometry::new(&object, rect, &style).unwrap().viewport;
            let inset = if rules == 4 { 2.0 } else { 0.0 };
            assert_eq!(
                viewport,
                Rect::from_min_max(rect.min + vec2(inset, inset), rect.max - vec2(inset, inset)),
                "{border_style:?}"
            );
        }
    }
}
