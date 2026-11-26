//! The content area below the layout bar: the file's rendered layout.
//!
//! In FileMaker this region is drawn entirely from the file's layout and
//! theme (see `docs/re/layout-model.md`). Browse-mode Form view paints the
//! rich layout through `document` when supplied by the provider. Otherwise,
//! for layouts without geometry, OpenMaker draws data-driven stand-ins from the
//! layout's field list and records: generic Form, List and Table views,
//! Find-mode criteria inputs and a Preview page. Layout mode uses the same
//! real geometry for selection/inspection, falling back only when unavailable.
//!
//! The native provider supplies rich layouts; explicit sample providers may
//! supply only geometry or generic rows.

use std::borrow::Cow;

use egui::emath::GuiRounding as _;
use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, Layout, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, UiBuilder,
};
use egui_extras::{Column, TableBuilder};
use om_data::{Field, FieldKind, GeomRect, LayoutGeometry, ObjectGeom, Record};

use crate::app::{Action, FindState, LayoutTool, ViewMode};
use crate::icons::{self, Icon};
use crate::session::{Geometry, Page, Session};
use crate::theme;
use crate::widgets;

/// Width of a field label column in the generic Form view.
const LABEL_WIDTH: f32 = 140.0;
/// Width of one field in the generic List view.
const LIST_COLUMN: f32 = 180.0;
/// Inset of a part or object caption from its box's top-left corner.
const CAPTION_INSET: egui::Vec2 = vec2(4.0, 3.0);

/// Browse mode: the current layout's records in the chosen view.
pub(crate) fn browse(
    ui: &mut Ui,
    session: &Session,
    view: ViewMode,
    document: &mut crate::document::DocumentState,
    calc: Option<&crate::calc::Evaluator<'_>>,
    actions: &mut Vec<Action>,
) {
    // A layout the account may not see: FileMaker Pro's gray page showing
    // `<No Access>` (#137's reading), and nothing read through it; blank
    // when the window opened on it (#696).
    if session.layout.is_some_and(|id| !session.shows_layout(id)) {
        no_access_page(ui, !session.blank_start);
        return;
    }
    document.scroll.begin_paint(session);
    document
        .scroll
        .calculations(calc.map(crate::calc::Evaluator::content_revision));
    // Register the canvas behind its controls. A field, tab, button, or row
    // consumes its own click; only an otherwise unused canvas click commits.
    let background = ui.interact(
        ui.max_rect(),
        ui.scope_id().with("browse_canvas"),
        Sense::click(),
    );
    if matches!(view, ViewMode::Form | ViewMode::List) {
        if let crate::session::RenderState::Loaded(layout) = &session.render {
            crate::document::show(ui, session, layout, document, view, calc, actions);
            commit_canvas_click(&background, session, document, actions);
            return;
        }
    }
    if view == ViewMode::Table {
        table(ui, session, document, calc, actions);
        commit_canvas_click(&background, session, document, actions);
        return;
    }
    let Some(page) = loaded_page(ui, session) else {
        return;
    };
    // The views of a layout not drawn move Tab through the current
    // record's cells (`DocumentState::text_edit`).
    document.resolve_tab(ui, session, false);
    document.settle_completions(ui);
    match view {
        ViewMode::List => list_view(ui, session, page, document, calc, actions),
        // Form view; Table view went above.
        _ => match &session.geometry {
            Geometry::Loaded(geometry) => geometry_view(ui, geometry, document),
            Geometry::Loading => padded(ui, |ui| {
                ui.spinner();
            }),
            Geometry::Unavailable => form_view(ui, session, document, calc, actions),
        },
    }
    commit_canvas_click(&background, session, document, actions);
}

/// Browse's Table view: the found set's grid, and on a drawn layout the
/// parts its Table View Properties include around it
/// (`document::show_table`), zoomed as Form and List view are.
fn table(
    ui: &mut Ui,
    session: &Session,
    document: &mut crate::document::DocumentState,
    calc: Option<&crate::calc::Evaluator<'_>>,
    actions: &mut Vec<Action>,
) {
    // The grid, its header, its scroll areas and the parts in the
    // document's zoom layer.
    let factor = document.zoom.factor();
    // The last column fills the width the table's previous frame
    // left: at a new zoom the table is laid out once more before it
    // shows, or it keeps the old zoom's width until the next input.
    let shown = ui.scope_id().with("table zoom");
    if ui.data(|data| data.get_temp::<f32>(shown)) != Some(factor) {
        ui.data_mut(|data| data.insert_temp(shown, factor));
        ui.ctx().request_discard("Table view zoom changed");
    }
    crate::document::zoom::show(ui, ui.available_size() / factor, factor, |ui| {
        // The canvas below the rows, in their layer (the window's
        // canvas is under it): a click there that no cell takes
        // commits the record.
        let canvas = ui.interact(
            ui.max_rect(),
            ui.scope_id().with("table_canvas"),
            Sense::click(),
        );
        let grid = |ui: &mut Ui,
                    document: &mut crate::document::DocumentState,
                    actions: &mut Vec<Action>| {
            let Some(page) = loaded_page(ui, session) else {
                return;
            };
            // Table view moves Tab through the current record's cells
            // (`DocumentState::text_edit`).
            document.resolve_tab(ui, session, false);
            document.settle_completions(ui);
            table_view(ui, session, page, document, calc, actions);
        };
        match &session.render {
            crate::session::RenderState::Loaded(layout) => {
                crate::document::show_table(ui, session, layout, document, calc, actions, grid)
            }
            _ => grid(ui, document, actions),
        }
        commit_canvas_click(&canvas, session, document, actions);
    });
}

fn commit_canvas_click(
    response: &egui::Response,
    session: &Session,
    document: &crate::document::DocumentState,
    actions: &mut Vec<Action>,
) {
    if response.clicked() && !document.pending && !document.changes(session).is_empty() {
        actions.push(Action::CommitRecord);
    }
}

/// The gray page that stands in for a layout the account cannot see. No
/// theme surface has this role.
const NO_ACCESS_PAGE: Color32 = Color32::from_gray(0xC8);

/// A layout the account's privilege set gives no access: a gray page, with
/// `<No Access>` in its middle when `text` (a layout gone to, not the one
/// the window opened on).
fn no_access_page(ui: &mut Ui, text: bool) {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, NO_ACCESS_PAGE);
    if text {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "<No Access>",
            egui::FontId::proportional(14.0),
            theme::TEXT,
        );
    }
}

/// The loaded page, or a loading/empty message in its place.
fn loaded_page<'a>(ui: &mut Ui, session: &'a Session) -> Option<&'a Page> {
    let Some(page) = session.page.as_ref() else {
        padded(ui, |ui| {
            if session.layout.is_some() || session.layouts_loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Loading...").color(theme::WEAK_TEXT));
                });
            } else {
                ui.label(RichText::new("This file has no layouts.").color(theme::WEAK_TEXT));
            }
        });
        return None;
    };
    if page.data.total == 0 {
        padded(ui, |ui| {
            ui.label(RichText::new("No records").color(theme::WEAK_TEXT));
        });
        return None;
    }
    Some(page)
}

fn padded(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(20, 16))
        .show(ui, add_contents);
}

fn display_value(value: Option<&String>) -> &str {
    value.map_or("", String::as_str)
}

/// Calculate a displayed column when the provider deliberately left it out
/// of the page. Providers without a calculation source keep their values.
fn column_value<'a>(
    record: &'a Record,
    column: usize,
    field: &Field,
    calc: Option<&crate::calc::Evaluator<'_>>,
) -> Cow<'a, str> {
    if let Some(calc) = calc {
        if let Some(value) = u64::try_from(record.id)
            .ok()
            .and_then(|id| calc.named_field_value(&field.name, id))
        {
            return Cow::Owned(calc.display(&value));
        }
    }
    Cow::Borrowed(display_value(
        record.values.get(column).and_then(Option::as_ref),
    ))
}

/// A cell's text, or FileMaker Pro's `<No Access>` for one the account may
/// not see (`Session::no_access`).
trait Hidden<'a> {
    fn hidden(self, hidden: bool) -> Cow<'a, str>;
}

impl<'a> Hidden<'a> for Cow<'a, str> {
    fn hidden(self, hidden: bool) -> Cow<'a, str> {
        if hidden {
            Cow::Borrowed("<No Access>")
        } else {
            self
        }
    }
}

fn calculated_column(field: &Field, calc: Option<&crate::calc::Evaluator<'_>>) -> bool {
    calc.is_some_and(|calc| calc.computes_named(&field.name))
}

fn field_visible(ui: &Ui, width: f32) -> bool {
    ui.is_rect_visible(Rect::from_min_size(
        ui.next_widget_position(),
        vec2(width, ui.spacing().interact_size.y),
    ))
}

fn field_label(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Label::new(RichText::new(text).color(theme::CAPTION))
            .selectable(false)
            .truncate(),
    )
}

/// A bordered box that reads as a FileMaker field, showing `value`.
///
/// Compact boxes (List view) truncate to one line and pass clicks through to
/// the record; full boxes (Form view) wrap and allow text selection.
fn field_box(ui: &mut Ui, width: f32, value: &str, compact: bool) {
    let text = RichText::new(value).color(theme::TEXT);
    let label = if compact {
        egui::Label::new(text).truncate().selectable(false)
    } else {
        egui::Label::new(text).wrap()
    };
    egui::Frame::new()
        .fill(theme::CONTROL_BG)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(3.0)
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.set_min_height(16.0);
            ui.add(label);
        });
}

fn form_field_width(ui: &Ui) -> f32 {
    (ui.available_width() - 24.0).clamp(160.0, 420.0)
}

/// One record's fields as label/value rows, like a FileMaker default layout.
fn form_view(
    ui: &mut Ui,
    session: &Session,
    document: &mut crate::document::DocumentState,
    calc: Option<&crate::calc::Evaluator<'_>>,
    actions: &mut Vec<Action>,
) {
    let Some((fields, record)) = session.current_record() else {
        padded(ui, |ui| {
            ui.spinner();
        });
        return;
    };
    let frame = document.scroll.begin(ui, "form_scroll", None);
    let mut scroll = egui::ScrollArea::vertical().id_salt("form_scroll");
    if let Some(offset) = frame.offset() {
        scroll = scroll.scroll_offset(offset);
    }
    let scroll = scroll.stick_to_bottom(frame.at_end());
    let output = scroll.auto_shrink([false, false]).show(ui, |ui| {
        document.scroll.origin(ui, ui.max_rect().min);
        padded(ui, |ui| {
            egui::Grid::new("form_view")
                .num_columns(2)
                .min_col_width(LABEL_WIDTH)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    for (index, field) in fields.iter().enumerate() {
                        let label = ui
                            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                                field_label(ui, &field.name)
                            })
                            .inner;
                        let width = form_field_width(ui);
                        if calculated_column(field, calc) && !field_visible(ui, width) {
                            ui.allocate_exact_size(
                                vec2(width, ui.spacing().interact_size.y),
                                Sense::hover(),
                            );
                            ui.end_row();
                            continue;
                        }
                        let response = (!calculated_column(field, calc))
                            .then(|| {
                                document.text_edit(
                                    ui,
                                    session,
                                    session.current,
                                    field,
                                    form_field_width(ui),
                                    None,
                                    actions,
                                )
                            })
                            .flatten();
                        if let Some(response) = response {
                            response.labelled_by(label.id);
                        } else {
                            let value = column_value(record, index, field, calc)
                                .hidden(session.no_access(session.current, &field.name));
                            field_box(ui, form_field_width(ui), &value, false);
                        }
                        ui.end_row();
                    }
                });
        });
        frame.hold(ui);
    });
    document.scroll.finish(frame, &output);
}

/// Form view of a layout with known geometry: each part as a band, labelled
/// in a strip at the left, and each object as an outlined box labelled with
/// its kind, where the layout places it. One point draws as one pixel.
///
/// TODO(renderer): styles, field names and values, and object content.
fn geometry_view(
    ui: &mut Ui,
    geometry: &LayoutGeometry,
    document: &mut crate::document::DocumentState,
) {
    let strip_width = part_strip_width(ui, geometry);
    let width = geometry.width as f32;
    let size = vec2(strip_width + width, geometry.height as f32);
    let frame = document.scroll.begin(ui, "geometry_view", None);
    let mut scroll = egui::ScrollArea::both();
    if let Some(offset) = frame.offset() {
        scroll = scroll.scroll_offset(offset);
    }
    let scroll = scroll.stick_to_bottom(frame.at_end());
    let output = scroll
        .id_salt("geometry_view")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let (area, _) = ui.allocate_exact_size(size, Sense::hover());
            // On whole pixels, so 1 pt outlines and rules stay crisp.
            let origin =
                pos2(area.left() + strip_width, area.top()).round_to_pixels(ui.pixels_per_point());
            let painter = ui.painter().clone();

            // The part-label strip runs down the whole visible height, like
            // Layout mode's.
            let strip = Rect::from_x_y_ranges(area.left()..=origin.x, ui.clip_rect().y_range());
            painter.rect_filled(strip, CornerRadius::ZERO, theme::BAR_BG);
            painter.vline(
                origin.x - 0.5,
                strip.y_range(),
                Stroke::new(1.0, theme::BORDER),
            );
            // The layout's right edge.
            painter.vline(
                origin.x + width - 0.5,
                origin.y..=origin.y + size.y,
                Stroke::new(1.0, theme::HAIRLINE),
            );

            for part in &geometry.parts {
                let top = origin.y + part.top as f32;
                let bottom = top + part.height as f32;
                painter.hline(
                    origin.x..=origin.x + width,
                    bottom - 0.5,
                    Stroke::new(1.0, theme::POPUP_BORDER),
                );
                let tab =
                    Rect::from_x_y_ranges(area.left()..=origin.x - 1.0, top..=bottom).shrink(2.0);
                if tab.is_positive() {
                    painter.rect_stroke(
                        tab,
                        CornerRadius::same(2),
                        Stroke::new(1.0, theme::BUTTON_BORDER),
                        StrokeKind::Inside,
                    );
                    box_caption(ui, tab, &part.kind_name);
                }
            }

            let outline = Stroke::new(1.0, theme::BUTTON_BORDER);
            for object in &geometry.objects {
                let rect = layout_rect(origin, &object.rect);
                if rect.width() < 1.0 || rect.height() < 1.0 {
                    // A line, or an object too thin to outline or caption.
                    let ppp = ui.pixels_per_point();
                    painter.line_segment(
                        [rect.min, rect.max].map(|point| point.round_to_pixel_center(ppp)),
                        outline,
                    );
                    continue;
                }
                painter.rect_stroke(rect, CornerRadius::ZERO, outline, StrokeKind::Inside);
                box_caption(ui, rect, &object.kind_name);
            }
            frame.hold(ui);
        });
    document.scroll.finish(frame, &output);
}

/// Wide enough for the longest part name in its tab.
fn part_strip_width(ui: &Ui, geometry: &LayoutGeometry) -> f32 {
    let longest = geometry
        .parts
        .iter()
        .map(|part| widgets::text_width(ui, &part.kind_name, theme::small()))
        .fold(0.0, f32::max);
    // The tab's and the caption's insets on both sides, and the strip's edge.
    (longest + 2.0 * (2.0 + CAPTION_INSET.x) + 1.0)
        .max(48.0)
        .ceil()
}

/// `rect` in screen coordinates, for a layout whose top-left is at `origin`.
fn layout_rect(origin: Pos2, rect: &GeomRect) -> Rect {
    Rect::from_two_pos(
        origin + vec2(rect.left as f32, rect.top as f32),
        origin + vec2(rect.right as f32, rect.bottom as f32),
    )
}

/// A small caption in the top-left corner of `rect`, clipped to it. A real
/// label rather than painted text, so it is visible to accessibility.
fn box_caption(ui: &mut Ui, rect: Rect, text: &str) {
    let inner = Rect::from_min_max(
        rect.min + CAPTION_INSET,
        rect.max - vec2(CAPTION_INSET.x, 0.0),
    );
    if !inner.is_positive() {
        return;
    }
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Min)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.add(
        egui::Label::new(
            RichText::new(text)
                .font(theme::small())
                .color(theme::CAPTION),
        )
        .selectable(false)
        .truncate(),
    );
}

/// Every record on the page as one row of fields under a header, like a
/// FileMaker list layout's header and body parts.
fn list_view(
    ui: &mut Ui,
    session: &Session,
    page: &Page,
    document: &mut crate::document::DocumentState,
    calc: Option<&crate::calc::Evaluator<'_>>,
    actions: &mut Vec<Action>,
) {
    let fields = &page.data.fields;
    let header = egui::Frame::new()
        .fill(theme::TOOLBAR_BG)
        .inner_margin(egui::Margin {
            left: 28,
            right: 12,
            top: 6,
            bottom: 6,
        });
    let header_rect = header
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                for field in fields {
                    // As wide as a field box (its width plus margins), so each
                    // heading sits over its field.
                    ui.allocate_ui_with_layout(
                        vec2(LIST_COLUMN + 14.0, 16.0),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            ui.set_min_width(LIST_COLUMN + 14.0);
                            ui.add_space(6.0);
                            ui.add(
                                egui::Label::new(RichText::new(&field.name).color(theme::TEXT))
                                    .selectable(false)
                                    .truncate(),
                            );
                        },
                    );
                }
            });
        })
        .response
        .rect;
    ui.painter().hline(
        header_rect.x_range(),
        header_rect.bottom(),
        Stroke::new(1.0, theme::HAIRLINE),
    );
    let stride = 32.0 + ui.spacing().item_spacing.y;
    let selected = session.current as f32 * stride;
    let frame = document.scroll.begin(
        ui,
        ("record_list", page.key.layout_id),
        Some(selected..=selected + 32.0),
    );
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("record_list", page.key.layout_id))
        .auto_shrink([false, false]);
    if let Some(offset) = document.list_scroll(session, session.current as f32 * 32.0, ui) {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    if let Some(offset) = frame.offset() {
        scroll = scroll.scroll_offset(offset);
    }
    let scroll = scroll.stick_to_bottom(frame.at_end());
    let output = scroll.show_rows(
        ui,
        32.0,
        usize::try_from(page.data.total).unwrap_or(usize::MAX),
        |ui, rows| {
            document.scroll.origin(
                ui,
                ui.max_rect().min - vec2(0.0, rows.start as f32 * stride),
            );
            ui.spacing_mut().item_spacing.y = 0.0;
            let current_visible = rows.contains(&(session.current as usize));
            let origin = ui.max_rect().min - vec2(0.0, rows.start as f32 * stride);
            for row in rows {
                let index = row as u64;
                if let Some((_, record)) = session.record_at(index) {
                    list_row(
                        ui,
                        session,
                        document,
                        calc,
                        fields,
                        record,
                        index,
                        index == session.current,
                        actions,
                    );
                } else {
                    ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
                    actions.push(Action::LoadRecordRange(index));
                }
            }
            if !current_visible {
                document.offscreen_editor(
                    ui,
                    session,
                    origin + vec2(28.0, session.current as f32 * stride + 5.0),
                    22.0,
                    None,
                    actions,
                );
            }
            frame.hold(ui);
        },
    );
    document.scroll.finish(frame, &output);
}

#[allow(clippy::too_many_arguments)]
fn list_row(
    ui: &mut Ui,
    session: &Session,
    document: &mut crate::document::DocumentState,
    calc: Option<&crate::calc::Evaluator<'_>>,
    fields: &[Field],
    record: &Record,
    index: u64,
    selected: bool,
    actions: &mut Vec<Action>,
) {
    let frame = egui::Frame::new()
        .fill(if selected {
            theme::CURRENT_ROW
        } else {
            theme::CONTROL_BG
        })
        .inner_margin(egui::Margin {
            left: 28,
            right: 12,
            top: 5,
            bottom: 5,
        });
    let response = frame
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                for (column, field) in fields.iter().enumerate() {
                    if calculated_column(field, calc) && !field_visible(ui, LIST_COLUMN + 14.0) {
                        ui.allocate_exact_size(
                            vec2(LIST_COLUMN + 14.0, ui.spacing().interact_size.y),
                            Sense::hover(),
                        );
                        continue;
                    }
                    if calculated_column(field, calc)
                        || document
                            .text_edit(ui, session, index, field, LIST_COLUMN, None, actions)
                            .is_none()
                    {
                        let value = column_value(record, column, field, calc)
                            .hidden(session.no_access(index, &field.name));
                        field_box(ui, LIST_COLUMN, &value, true);
                    }
                }
            });
        })
        .response;
    let rect = response.rect;
    // The current-record indicator FileMaker draws at the left of a list row.
    if selected {
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, vec2(4.0, rect.height())),
            CornerRadius::ZERO,
            theme::SELECTED,
        );
    }
    ui.painter().text(
        pos2(rect.left() + 16.0, rect.center().y),
        Align2::CENTER_CENTER,
        format!("{}", index + 1),
        theme::small(),
        theme::WEAK_TEXT,
    );
    ui.painter().hline(
        rect.x_range(),
        rect.bottom(),
        Stroke::new(1.0, theme::SEPARATOR),
    );
    let response = response.interact(Sense::click());
    if response.clicked() {
        if selected {
            commit_canvas_click(&response, session, document, actions);
        } else {
            actions.push(Action::GoToRecord(index));
        }
    }
}

fn is_numeric(kind: FieldKind) -> bool {
    matches!(kind, FieldKind::Number)
}

/// FileMaker's native Table view: a spreadsheet grid with column headers.
fn table_view(
    ui: &mut Ui,
    session: &Session,
    page: &Page,
    document: &mut crate::document::DocumentState,
    calc: Option<&crate::calc::Evaluator<'_>>,
    actions: &mut Vec<Action>,
) {
    // The drop-down lists the layout shows with "Auto-complete using value
    // list", by field: their items complete a cell's typing (#870).
    // Of several, the first in the layout's order (inferred).
    let mut completing: std::collections::BTreeMap<&str, &om_layout::LayoutObject> =
        Default::default();
    if let crate::session::RenderState::Loaded(document) = &session.render {
        for object in &document.layout.objects.objects {
            let flagged = object.field_obj.as_ref().is_some_and(|field| {
                field.display_type == om_data::choice::DROP_DOWN_LIST && field.auto_completes()
            });
            if let Some(name) = document.field_bindings.get(&object.id).filter(|_| flagged) {
                completing.entry(name.as_str()).or_insert(object);
            }
        }
    }
    ui.visuals_mut().selection.bg_fill = theme::CURRENT_ROW;
    ui.visuals_mut().selection.stroke = Stroke::new(1.0, theme::TEXT);
    // Columns wider than the window scroll sideways, header and rows
    // together (#1161). The table's own scroll area is vertical only, so
    // the sideways one wraps it.
    egui::ScrollArea::horizontal()
        .id_salt(("columns", page.key.layout_id))
        .auto_shrink(false)
        .scroll_source(egui::scroll_area::ScrollSource {
            drag: table_drag_scroll(ui),
            ..Default::default()
        })
        .show(ui, |ui| {
            table_grid(ui, session, page, document, calc, &completing, actions);
        });
}

/// [`table_view`]'s grid of records, inside its sideways scroll area.
fn table_grid(
    ui: &mut Ui,
    session: &Session,
    page: &Page,
    document: &mut crate::document::DocumentState,
    calc: Option<&crate::calc::Evaluator<'_>>,
    completing: &std::collections::BTreeMap<&str, &om_layout::LayoutObject>,
    actions: &mut Vec<Action>,
) {
    let fields = &page.data.fields;
    let row_height = 22.0;
    let current_row = usize::try_from(session.current).ok();
    let follow = document.table_current != Some((page.key.layout_id, session.current));
    document.table_current = Some((page.key.layout_id, session.current));
    // The cell that took the keyboard in this paint (a click, Tab or Go to
    // Field): its column's span on screen.
    let mut entered: Option<egui::Rangef> = None;
    // The editor of a field focus the off-screen editor took for a current
    // row scrolled away: its column comes into view when its cell paints.
    // Kept while that editor has the keyboard.
    let waiting_id = ui.make_persistent_id(("table cell to show", page.key.layout_id));
    let mut waiting: Option<egui::Id> = ui
        .data(|data| data.get_temp(waiting_id))
        .filter(|id| ui.memory(|memory| memory.has_focus(*id)));
    let salt = ui
        .scope_id()
        .with(egui::IdSalt::new(("records", page.key.layout_id)))
        .with("__scroll_area");
    let stride = row_height + ui.spacing().item_spacing.y;
    let selected = session.current as f32 * stride;
    let frame = document
        .scroll
        .begin(ui, salt, Some(selected..=selected + row_height));
    // The widths of the columns before the last, which takes the rest: the
    // table's own, so that a divider dragged in the zoom layer measures
    // in the table's points (egui_extras' resizing reads the pointer in
    // window points).
    let widths_id = ui
        .scope_id()
        .with(("table column widths", page.key.layout_id));
    // By field, so that a column keeps its width wherever its field goes.
    let mut stored: Vec<(String, f32)> =
        ui.data(|data| data.get_temp(widths_id)).unwrap_or_default();
    let resizable = fields.len().saturating_sub(1);
    let mut widths: Vec<f32> = fields[..resizable]
        .iter()
        .map(|field| {
            stored
                .iter()
                .find(|(name, _)| *name == field.name)
                .map_or(TABLE_COLUMN_WIDTH, |(_, width)| *width)
        })
        .collect();
    let drag_scroll = table_drag_scroll(ui);
    let mut table = TableBuilder::new(ui)
        .id_salt(("records", page.key.layout_id))
        .striped(false)
        .resizable(false)
        .drag_to_scroll(drag_scroll)
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .column(Column::exact(36.0))
        .min_scrolled_height(0.0);
    for width in &widths {
        table = table.column(Column::exact(*width).clip(true));
    }
    if !fields.is_empty() {
        table = table.column(Column::remainder().at_least(80.0).clip(true));
    }
    if follow && frame.offset().is_none() {
        if let Some(row) = current_row {
            table = table.scroll_to_row(row, None);
        }
    }
    if let Some(offset) = frame.offset() {
        table = table.vertical_scroll_offset(offset.y);
    }
    let table = table.stick_to_bottom(frame.at_end());
    // Each column's header cell, where its divider is dragged.
    let mut header_cells: Vec<Rect> = Vec::with_capacity(fields.len());
    let output = table
        .header(row_height, |mut header| {
            header.col(|ui| table_header_cell(ui, ""));
            for field in fields {
                header.col(|ui| {
                    header_cells.push(ui.max_rect());
                    table_header_cell(ui, &field.name);
                });
            }
        })
        .body(|mut body| {
            let tail = body.ui_mut().new_child(egui::UiBuilder::new());
            document.scroll.origin(&tail, tail.max_rect().min);
            let origin = tail.max_rect().min;
            let mut current_visible = false;
            body.rows(
                row_height,
                usize::try_from(page.data.total).unwrap_or(usize::MAX),
                |mut row| {
                    let index = row.index();
                    current_visible |= Some(index) == current_row;
                    let absolute = index as u64;
                    let Some((_, record)) = session.record_at(absolute) else {
                        row.col(|ui| {
                            ui.label("Loading…");
                        });
                        for _ in fields {
                            row.col(|_| {});
                        }
                        actions.push(Action::LoadRecordRange(absolute));
                        return;
                    };
                    row.set_selected(Some(index) == current_row);
                    row.col(|ui| {
                        table_cell_rule(ui);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            table_label(
                                ui,
                                RichText::new(format!("{}", absolute + 1))
                                    .font(theme::small())
                                    .color(theme::WEAK_TEXT),
                                false,
                            );
                        });
                    });
                    for (column, field) in fields.iter().enumerate() {
                        row.col(|ui| {
                            if calculated_column(field, calc) && !ui.is_rect_visible(ui.max_rect())
                            {
                                return;
                            }
                            table_cell_rule(ui);
                            // The field's flagged drop-down list on the
                            // layout lends the cell its items (#870).
                            let read = completing.get(field.name.as_str()).zip(calc).map(
                                |(object, calc)| {
                                    move || {
                                        calc.option_values(object, record.id as u64, None)
                                            .unwrap_or_default()
                                    }
                                },
                            );
                            let complete_from = read.as_ref().map(|read| {
                                read as &dyn Fn() -> std::sync::Arc<om_data::choice::Choices>
                            });
                            if !calculated_column(field, calc) {
                                let asked = focus_asked(document);
                                if let Some(editor) = document.text_edit(
                                    ui,
                                    session,
                                    absolute,
                                    field,
                                    ui.available_width(),
                                    complete_from,
                                    actions,
                                ) {
                                    // A field focus asked for (Go to Field)
                                    // counts even when the cell had the
                                    // keyboard already, which egui does not
                                    // see as gaining it.
                                    let took = asked && !focus_asked(document);
                                    if editor.gained_focus()
                                        || (took && editor.has_focus())
                                        || waiting == Some(editor.id)
                                    {
                                        entered = Some(editor.rect.x_range());
                                        waiting = None;
                                    }
                                    return;
                                }
                            }
                            let value = column_value(record, column, field, calc)
                                .hidden(session.no_access(absolute, &field.name));
                            let text = RichText::new(value.as_ref()).color(theme::TEXT);
                            if is_numeric(field.kind) {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    table_label(ui, text, true);
                                });
                            } else {
                                table_label(ui, text, true);
                            }
                        });
                    }
                    if row.response().clicked() {
                        if Some(index) == current_row {
                            commit_canvas_click(&row.response(), session, document, actions);
                        } else {
                            actions.push(Action::GoToRecord(absolute));
                        }
                    }
                },
            );
            let mut tail = tail;
            if !current_visible {
                let current = session.current_record().map(|(_, record)| record.id as u64);
                let read = |name: &str| {
                    let (object, calc) = completing.get(name).zip(calc)?;
                    Some(
                        calc.option_values(object, current?, None)
                            .unwrap_or_default(),
                    )
                };
                let asked = focus_asked(document);
                document.offscreen_editor(
                    &mut tail,
                    session,
                    origin + vec2(36.0, session.current as f32 * stride),
                    row_height,
                    Some(&read),
                    actions,
                );
                // Its stand-in sits at no column of the field's, so the
                // column comes into view once the row's own cell paints.
                if asked && !focus_asked(document) {
                    waiting = tail.memory(|memory| memory.focused());
                }
            }
            frame.hold(&tail);
        });
    document.scroll.finish(frame, &output);
    // Over the header, after it: a cell clips what reaches past its edge.
    for (column, (cell, width)) in header_cells.iter().zip(&mut widths).enumerate() {
        column_divider(ui, widths_id.with(column), *cell, width);
    }
    for (field, width) in fields.iter().zip(&widths) {
        match stored.iter_mut().find(|(name, _)| *name == field.name) {
            Some(entry) => entry.1 = *width,
            None => stored.push((field.name.clone(), *width)),
        }
    }
    ui.data_mut(|data| data.insert_temp(widths_id, stored));
    ui.data_mut(|data| {
        if let Some(id) = waiting {
            data.insert_temp(waiting_id, id);
        } else {
            data.remove::<egui::Id>(waiting_id);
        }
    });
    // A cell entered past either side of the window comes into view. The
    // table's vertical scroll area keeps the scroll requests made inside
    // it, the editor's own included, so the sideways one is asked here.
    if let Some(columns) = entered {
        ui.scroll_to_rect(
            Rect::from_x_y_ranges(columns, ui.clip_rect().y_range()),
            None,
        );
    }
}

/// A field focus is asked for (Go to Field, a new record's first field)
/// and no editor has taken it yet.
fn focus_asked(document: &crate::document::DocumentState) -> bool {
    document.focus.is_some() || document.focus_first
}

fn table_header_cell(ui: &mut Ui, text: &str) {
    let rect = ui.max_rect().expand2(vec2(4.0, 2.0));
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::BAR_BG);
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::BORDER),
    );
    ui.painter().vline(
        rect.right() - 0.5,
        rect.y_range(),
        Stroke::new(1.0, theme::HAIRLINE),
    );
    table_label(
        ui,
        RichText::new(text).font(theme::small()).color(theme::TEXT),
        true,
    );
}

/// A Table view column's width before it is dragged.
const TABLE_COLUMN_WIDTH: f32 = 170.0;

/// Dragging a Table view column's width wider or narrower, at the divider
/// after its header `cell`. The drag is in the table's points, the zoom
/// layer's when zoomed: egui gives the pointer in window points.
fn column_divider(ui: &mut Ui, id: egui::Id, cell: Rect, width: &mut f32) {
    let x = cell.right() + ui.spacing().item_spacing.x / 2.0;
    let grab = ui.style().interaction.resize_grab_radius_side;
    let header = cell.expand2(vec2(0.0, 2.0)).y_range();
    let handle = Rect::from_x_y_ranges(x - grab..=x + grab, header);
    let response = ui.interact(handle, id, Sense::drag());
    // The width follows the pointer from where it was pressed, so that
    // movement that came with the press or with the release counts.
    let from_global = ui
        .ctx()
        .layer_transform_from_global(ui.layer_id())
        .unwrap_or(egui::emath::TSTransform::IDENTITY);
    let start = id.with("drag start");
    if response.drag_started() {
        if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
            let origin = (from_global * origin).x;
            ui.data_mut(|data| data.insert_temp(start, (origin, *width)));
        }
    }
    if response.dragged() || response.drag_stopped() {
        let pointer = ui.input(|input| input.pointer.latest_pos());
        if let (Some((origin, from)), Some(pointer)) =
            (ui.data(|data| data.get_temp::<(f32, f32)>(start)), pointer)
        {
            *width = (from + (from_global * pointer).x - origin).clamp(40.0, 4000.0);
        }
    }
    if response.drag_stopped() {
        ui.data_mut(|data| data.remove::<(f32, f32)>(start));
    }
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
        let stroke = if response.dragged() {
            ui.visuals().widgets.active.bg_stroke
        } else {
            ui.visuals().widgets.hovered.bg_stroke
        };
        ui.painter().vline(x - 0.5, header, stroke);
    }
}

/// Table view's scroll areas follow a touch drag at 100% only: zoomed, egui
/// would move them by the drag in window points, the zoom's multiple of
/// the finger's way.
fn table_drag_scroll(ui: &Ui) -> egui::scroll_area::DragScroll {
    if crate::document::layer_scale(ui) == 1.0 {
        egui::scroll_area::DragScroll::OnTouch
    } else {
        egui::scroll_area::DragScroll::Never
    }
}

/// A Table view label, cut off at the cell's edge with `truncate`. Zoomed,
/// its glyphs are rasterized at the zoom (`document::layer_galley`), as the
/// layout's own text is; text cut off keeps egui's renderer, which
/// `scaled_text` does not rebuild.
fn table_label(ui: &mut Ui, text: RichText, truncate: bool) -> egui::Response {
    if crate::document::layer_scale(ui) == 1.0 {
        let label = egui::Label::new(text).selectable(false);
        return ui.add(if truncate { label.truncate() } else { label });
    }
    let mut job = std::sync::Arc::unwrap_or_clone(egui::WidgetText::from(text).into_layout_job(
        ui.style(),
        egui::FontSelection::Default,
        Align::Center,
    ));
    job.wrap = if truncate {
        egui::text::TextWrapping::truncate_at_width(ui.available_width())
    } else {
        egui::text::TextWrapping::no_max_width()
    };
    let galley = crate::document::layer_galley(ui, job);
    ui.add(egui::Label::new(galley).selectable(false))
}

fn table_cell_rule(ui: &Ui) {
    let rect = ui.max_rect().expand2(vec2(4.0, 2.0));
    let stroke = Stroke::new(1.0, theme::SEPARATOR);
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 0.5, stroke);
    ui.painter()
        .vline(rect.right() - 0.5, rect.y_range(), stroke);
}

/// Find mode: the current request on the layout itself, its fields taking
/// criteria (`document::show_find`). Table view, and a layout that cannot
/// be drawn, list the table's fields instead.
pub(crate) fn find(
    ui: &mut Ui,
    session: &Session,
    view: ViewMode,
    find: &mut FindState,
    document: &mut crate::document::DocumentState,
    (schema, calc): (
        Option<&om_data::host::CalcSchema>,
        Option<&crate::calc::Evaluator<'_>>,
    ),
    actions: &mut Vec<Action>,
) {
    document.scroll.begin_paint(session);
    if view != ViewMode::Table {
        if let crate::session::RenderState::Loaded(layout) = &session.render {
            crate::document::show_find(
                ui,
                session,
                layout,
                document,
                find,
                (schema, calc),
                actions,
            );
            return;
        }
    }
    let Some(page) = session.page.as_ref() else {
        loaded_page(ui, session);
        return;
    };
    // Tab goes through the criteria in column order and wraps (#858).
    document.resolve_tab(ui, session, true);
    let layout = u64::from(session.layout.unwrap_or_default());
    let entered: Vec<bool> = (0..page.data.fields.len())
        .map(|column| document.find_cell_tab(layout, column))
        .collect();
    let mut stops = Vec::new();
    let scroll_state = &mut document.scroll;
    let fields = &page.data.fields;
    let request = find.current_mut();
    let mut focused = None;
    let frame = scroll_state.begin(ui, "find_scroll", None);
    let mut scroll = egui::ScrollArea::vertical().id_salt("find_scroll");
    if let Some(offset) = frame.offset() {
        scroll = scroll.scroll_offset(offset);
    }
    let scroll = scroll.stick_to_bottom(frame.at_end());
    let output = scroll.auto_shrink([false, false]).show(ui, |ui| {
        scroll_state.origin(ui, ui.max_rect().min);
        padded(ui, |ui| {
            egui::Grid::new("find_view")
                .num_columns(2)
                .min_col_width(LABEL_WIDTH)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    for (column, field) in fields.iter().enumerate() {
                        let label = ui
                            .with_layout(Layout::right_to_left(Align::Center), |ui| {
                                field_label(ui, &field.name)
                            })
                            .inner;
                        let width = form_field_width(ui);
                        let mut criterion = request.get(&field.name).unwrap_or_default().to_owned();
                        let id = ui.make_persistent_id(("find_criterion", &field.name));
                        // Entered by Tab: the insertion point after the text.
                        if entered[column] {
                            let mut state =
                                egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
                            state
                                .cursor
                                .set_char_range(Some(egui::text::CCursorRange::one(
                                    egui::text::CCursor::new(criterion.chars().count()),
                                )));
                            state.store(ui.ctx(), id);
                        }
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut criterion)
                                .id(id)
                                .desired_width(width)
                                .margin(egui::Margin {
                                    left: 6,
                                    right: 22,
                                    top: 3,
                                    bottom: 3,
                                }),
                        );
                        let response = response.labelled_by(label.id);
                        if response.changed() {
                            request.set(&field.name, criterion);
                        }
                        stops.push((column, response.id));
                        if response.has_focus() {
                            crate::document::keep_tab(ui, response.id);
                        }
                        scroll_state.field(ui, &field.name, response.rect);
                        if response.has_focus() {
                            focused = Some(field.name.clone());
                        }
                        // The small magnifier FileMaker draws in each
                        // find field.
                        let glyph = Rect::from_center_size(
                            pos2(response.rect.right() - 11.0, response.rect.center().y),
                            vec2(11.0, 11.0),
                        );
                        icons::paint(
                            ui.painter(),
                            glyph,
                            Icon::Search,
                            theme::DISABLED,
                            theme::CONTROL_BG,
                        );
                        ui.end_row();
                    }
                });
        });
        frame.hold(ui);
    });
    scroll_state.finish(frame, &output);
    for (column, widget) in stops {
        document.note_find_cell(layout, column, widget);
    }
    if let Some(field) = focused {
        find.set_focused_field(&field);
    }
}

/// Layout authoring chrome wraps the provider's geometry. Selection is local;
/// changing a selection never modifies the file.
#[derive(Default)]
pub(crate) struct LayoutWorkspace {
    canvas: crate::layout_canvas::CanvasState,
    inspector: crate::inspector::Inspector,
    objects: crate::layout_objects::ObjectsPanel,
    /// Button Setup, open over the window for the new button.
    button_setup: Option<crate::script_workspace::button_setup::ButtonSetup>,
    /// Portal Setup or Add Fields to Portal, open over the window for the
    /// new portal.
    portal_setup: Option<crate::portal_setup::PortalSetup>,
    /// Save Layout's placement is with the host.
    pub saving: bool,
    /// Double-click locking applies only to the Field tool.
    pub field_tool_locked: bool,
    /// A change of the layout's parts sent from this window, held until the
    /// layout as the host then stores it is read (`saving` is set).
    parts_hold: Option<PartsHold>,
}

/// A change of the layout's parts sent from a window: by Part Setup
/// (Create, Delete, Change) or a dragged part boundary.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PartsSent {
    Parts(om_data::layout_placement::PartsChange),
    Options(om_data::layout_placement::PartOptionsChange),
}

impl PartsSent {
    fn at(&self) -> (u32, om_data::layout_placement::Revision) {
        match self {
            PartsSent::Parts(change) => (change.layout, change.expected),
            PartsSent::Options(change) => (change.layout, change.expected),
        }
    }
}

/// A parts change with the host, and whether the host has answered it and
/// the layout is being read again.
#[derive(Debug)]
struct PartsHold {
    sent: PartsSent,
    answered: bool,
}

impl LayoutWorkspace {
    pub(crate) fn clear_history(&mut self) {
        self.canvas.clear_history();
    }

    pub(crate) fn draft_edits(&self, session: &Session) -> (bool, bool) {
        if self.saving || self.setting_up_button() {
            return (false, false);
        }
        self.canvas.draft_edits(session)
    }

    pub(crate) fn edit_draft(
        &mut self,
        ctx: &egui::Context,
        session: &Session,
        edit: crate::layout_canvas::DraftEdit,
    ) -> bool {
        if self.saving || self.setting_up_button() {
            return false;
        }
        self.canvas.edit_draft(ctx, session, edit)
    }

    pub(crate) fn revert(&mut self) {
        self.clear_history();
        self.drop_width();
        self.drop_new_object();
        self.drop_moves();
        self.drop_deletions();
        self.drop_controls();
        self.drop_words();
    }

    /// The next field draft to send on explicit Save Layout.
    pub(crate) fn new_field(&self) -> Option<&crate::layout_canvas::NewField> {
        self.canvas.new_field()
    }

    #[cfg(test)]
    pub(crate) fn stage_field(&mut self, session: &Session, field: crate::layout_canvas::NewField) {
        self.canvas.stage_field(session, field);
    }

    pub(crate) fn clear_selection(&mut self) {
        if !self.saving {
            self.canvas.clear_selection();
        }
    }

    pub(crate) fn selection_count(&self, session: &Session) -> usize {
        self.canvas.selection_count(session)
    }

    pub(crate) fn select_all(&mut self, session: &Session) {
        self.canvas.select_all(session);
    }

    pub(crate) fn nudge_selection(
        &mut self,
        session: &Session,
        direction: crate::layout_canvas::NudgeDirection,
    ) {
        let before = self.canvas.checkpoint();
        if self.canvas.nudge_selection(session, direction) {
            self.canvas.record_command(before);
        }
    }

    /// How many selected objects Arrange's Align, Distribute and Resize To
    /// apply to (`CanvasState::arrangeable_count`).
    pub(crate) fn arrangeable_count(&self, session: &Session) -> usize {
        self.canvas.arrangeable_count(session)
    }

    /// Arrange > Align, Distribute or Resize To on the selection, as
    /// changes Save Layout stores.
    pub(crate) fn arrange(
        &mut self,
        session: &Session,
        arrangement: om_data::layout_arrange::Arrangement,
    ) -> Result<bool, om_data::layout_placement::Refusal> {
        let before = self.canvas.checkpoint();
        let changed = self.canvas.arrange(session, arrangement)?;
        if changed {
            self.canvas.record_command(before);
        }
        Ok(changed)
    }

    pub(crate) fn take_field_finished(&mut self) -> bool {
        self.canvas.take_field_finished()
    }

    pub(crate) fn field_placed(&mut self) {
        self.canvas.field_placed();
    }

    /// The button drawn with the Button tool and not yet saved.
    pub(crate) fn new_button(&self) -> Option<&crate::layout_canvas::NewButton> {
        self.canvas.new_button()
    }

    /// The text placed with the Text tool and not yet saved.
    pub(crate) fn new_text(&self) -> Option<&crate::layout_canvas::NewText> {
        self.canvas.new_text()
    }

    /// A part's height dragged to with the Selection tool, to send.
    pub(crate) fn take_part_resize(&mut self) -> Option<crate::layout_canvas::PartResize> {
        self.canvas.take_part_resize()
    }

    /// Whether the Part tool was used on the layout since the last call.
    pub(crate) fn take_part_tool(&mut self) -> bool {
        self.canvas.take_part_tool()
    }

    /// The portal drawn with the Portal tool and not yet saved.
    pub(crate) fn new_portal(&self) -> Option<&crate::layout_canvas::NewPortal> {
        self.canvas.new_portal()
    }

    /// Whether a new field, button, text or portal waits for Save Layout.
    pub(crate) fn has_new_object(&self) -> bool {
        self.new_field().is_some()
            || self.new_button().is_some()
            || self.new_text().is_some()
            || self.new_portal().is_some()
    }

    /// Revert Layout, or the host's answer to Save Layout. Revert waits
    /// while the placement is with the host (`saving`).
    pub(crate) fn drop_new_object(&mut self) {
        self.canvas.drop_new_object();
        self.button_setup = None;
        self.portal_setup = None;
    }

    /// Whether Button Setup, Portal Setup or Add Fields to Portal is open.
    pub(crate) fn setting_up_button(&self) -> bool {
        self.button_setup.is_some() || self.portal_setup.is_some()
    }

    /// Portal Setup, then Add Fields to Portal, opened when the Portal tool
    /// has drawn a portal or the new portal is double clicked. They offer
    /// the occurrences `schema`'s relationship graph joins to the layout's
    /// own (`own`). Done sets the portal's records, rows and fields; Cancel
    /// on a portal not set up yet drops it, as nothing could place it.
    pub(crate) fn show_portal_setup(
        &mut self,
        ctx: &egui::Context,
        schema: Option<std::sync::Arc<om_data::host::CalcSchema>>,
        own: Option<u64>,
    ) {
        use crate::portal_setup::{related_occurrences, PortalSetup, PortalSetupAnswer};
        if self.canvas.take_portal_setup() && !self.saving && self.portal_setup.is_none() {
            if let Some(portal) = self.canvas.new_portal() {
                let occurrences = match (&schema, own) {
                    (Some(schema), Some(own)) => related_occurrences(schema, own),
                    _ => Vec::new(),
                };
                self.portal_setup = Some(PortalSetup::new(
                    occurrences,
                    portal.frame(),
                    portal.setup.as_ref(),
                ));
            }
        }
        if self.canvas.new_portal().is_none() {
            self.portal_setup = None;
        }
        let Some(setup) = &mut self.portal_setup else {
            return;
        };
        match setup.show(ctx) {
            PortalSetupAnswer::Open => {}
            PortalSetupAnswer::Cancel => {
                self.portal_setup = None;
                if self.canvas.new_portal().is_some_and(|p| p.setup.is_none()) {
                    self.canvas.drop_new_object();
                }
            }
            PortalSetupAnswer::Done(choice) => {
                self.portal_setup = None;
                let before = self.canvas.checkpoint();
                if let Some(portal) = self.canvas.new_portal_mut() {
                    portal.setup = Some(choice);
                    self.canvas.record_command(before);
                }
            }
        }
    }

    /// Button Setup, opened when the Button tool has drawn a button, the
    /// new button is double clicked or the Inspector asks for it. OK gives
    /// the button its label and action; Cancel on a button that has no
    /// action yet drops it, as nothing could place it.
    pub(crate) fn show_button_setup(
        &mut self,
        ctx: &egui::Context,
        names: Option<std::sync::Arc<om_data::host::CalcSchema>>,
    ) {
        use crate::script_workspace::button_setup::{ButtonSetup, SetupOutcome};
        if self.canvas.take_button_setup() && !self.saving && self.button_setup.is_none() {
            if let Some(button) = self.canvas.new_button() {
                self.button_setup =
                    Some(ButtonSetup::new(&button.label, button.step.as_ref(), names));
            }
        }
        if self.canvas.new_button().is_none() {
            self.button_setup = None;
        }
        let Some(setup) = &mut self.button_setup else {
            return;
        };
        match setup.show(ctx) {
            None => {}
            Some(SetupOutcome::Cancel) => {
                self.button_setup = None;
                if self.canvas.new_button().is_some_and(|b| b.step.is_none()) {
                    self.canvas.drop_new_object();
                }
            }
            Some(SetupOutcome::Ok {
                label,
                step,
                action,
            }) => {
                self.button_setup = None;
                let before = self.canvas.checkpoint();
                if let Some(button) = self.canvas.new_button_mut() {
                    button.label = label;
                    button.step = Some(step);
                    button.action = action;
                    self.canvas.record_command(before);
                }
            }
        }
    }

    /// The objects moved or resized and not saved yet.
    pub(crate) fn moved(&self) -> &crate::layout_canvas::MovedObjects {
        self.canvas.moved()
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_moves(&mut self) {
        self.canvas.drop_moves();
    }

    /// The layout's width as its right edge was dragged, not saved yet.
    pub(crate) fn new_width(&self) -> Option<crate::layout_canvas::NewWidth> {
        self.canvas.new_width()
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_width(&mut self) {
        self.canvas.drop_width();
    }

    /// The objects deleted and not saved yet.
    pub(crate) fn deleted(&self) -> &crate::layout_canvas::DeletedObjects {
        self.canvas.deleted()
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_deletions(&mut self) {
        self.canvas.drop_deletions();
    }

    /// The field objects given another control and not saved yet.
    pub(crate) fn controls(&self) -> &crate::layout_canvas::ControlChanges {
        self.canvas.controls()
    }

    /// The host's answer for object `key`'s control.
    pub(crate) fn control_sent(&mut self, key: u64) {
        self.canvas.control_sent(key);
    }

    /// The text objects retyped and not saved yet.
    pub(crate) fn words(&self) -> &crate::layout_canvas::WordChanges {
        self.canvas.words()
    }

    /// The host's answer for object `key`'s words.
    pub(crate) fn words_sent(&mut self, key: u64) {
        self.canvas.words_sent(key);
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_words(&mut self) {
        self.canvas.drop_words();
    }

    /// Keep the words of a text being typed, before Save Layout.
    pub(crate) fn finish_retyping(&mut self) {
        self.canvas.finish_retyping();
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_controls(&mut self) {
        self.canvas.drop_controls();
    }

    /// Give the field object `key` placed by Save Layout the control the
    /// Inspector set for the new field: sent next, on the layout the
    /// placement made.
    pub(crate) fn set_placed_control(
        &mut self,
        key: u64,
        control: (u32, Option<u64>),
        revision: om_data::layout_placement::Revision,
    ) {
        self.canvas
            .set_control(key, control, (0, 0), Some(revision));
    }

    /// The host applied a change of the layout at revision `from` and left
    /// it at `to`: the unsaved changes made on `from` are now on `to`.
    pub(crate) fn rebase(
        &mut self,
        from: om_data::layout_placement::Revision,
        to: om_data::layout_placement::Revision,
    ) {
        self.canvas.rebase(from, to);
    }

    /// A parts change goes to the host: the canvas takes no edit until the
    /// host answers it and the layout is read again, since the change moves
    /// objects and gives the layout a new revision. The hold is the
    /// window's, so it lasts when Part Setup closes before the answer (as
    /// another window coming forward closes it).
    pub(crate) fn hold_parts(&mut self, sent: PartsSent) {
        self.clear_history();
        self.saving = true;
        self.parts_hold = Some(PartsHold {
            sent,
            answered: false,
        });
    }

    /// The host answered `sent`, leaving the layout at `revision` when it
    /// saved it: when it is this window's held change, the hold ends now if
    /// the layout cannot have changed (`changed` false: refused without
    /// writing, or the connection is gone) or is where it was, else once it
    /// is read again at another revision. A refusal that wrote nothing is
    /// read again at the revision the hold waits to leave, so it ends now.
    pub(crate) fn parts_answered(
        &mut self,
        sent: &PartsSent,
        changed: bool,
        revision: Option<om_data::layout_placement::Revision>,
    ) {
        let Some(hold) = self.parts_hold.as_mut().filter(|h| h.sent == *sent) else {
            return;
        };
        if changed && revision != Some(sent.at().1) {
            hold.answered = true;
        } else {
            self.parts_hold = None;
            self.saving = false;
        }
    }

    /// The window read layout `layout` again: an answered hold on it ends,
    /// whatever revision the read gave. A refusal (306) whose read could
    /// not reach the host's new catalog still shows the old revision, which
    /// [`Self::release_parts`] would wait on for good.
    pub(crate) fn layout_read(&mut self, layout: u32) {
        if self
            .parts_hold
            .as_ref()
            .is_some_and(|hold| hold.answered && hold.sent.at().0 == layout)
        {
            self.parts_hold = None;
            self.saving = false;
        }
    }

    /// No answer or read will come for a held change (the connection or
    /// the worker is gone): the hold ends.
    pub(crate) fn abandon_parts(&mut self) {
        if self.parts_hold.take().is_some() {
            self.saving = false;
        }
    }

    /// Ends an answered hold once the window no longer shows the layout as
    /// it was when the change was sent: read again at another revision, not
    /// readable, or another layout.
    pub(crate) fn release_parts(&mut self, session: Option<&Session>) {
        let Some(hold) = self.parts_hold.as_ref().filter(|h| h.answered) else {
            return;
        };
        let (layout, expected) = hold.sent.at();
        let stale = session.is_some_and(|s| {
            s.layout == Some(layout)
                && matches!(&s.geometry, Geometry::Loaded(g) if g.revision == Some(expected))
        });
        if !stale {
            self.parts_hold = None;
            self.saving = false;
        }
    }

    /// Delete the entire supported selection as an unsaved change. Validation
    /// precedes mutation, so a mixed unsupported set cannot lose only a subset.
    pub(crate) fn delete_selected(&mut self, session: &Session) -> bool {
        if self.saving {
            return false;
        }
        let before = self.canvas.checkpoint();
        let changed = self.canvas.delete_selection(session);
        if changed {
            self.canvas.record_command(before);
        }
        changed
    }

    /// The new field or button and the moved and deleted objects the
    /// canvas and Inspector hold, before Save Layout.
    pub(crate) fn has_changes(&self) -> bool {
        self.has_new_object()
            || self.canvas.new_width().is_some()
            || !self.canvas.moved().frames.is_empty()
            || !self.canvas.deleted().keys.is_empty()
            || !self.canvas.controls().objects.is_empty()
            || !self.canvas.words().objects.is_empty()
            || self.canvas.retyping_changed()
    }
}

pub(crate) struct LayoutPanels {
    pub inspector: bool,
    pub objects: bool,
}

/// The selected field object's control for the Inspector's Data tab: as
/// the shown layout stores it, its unsaved change, and why it cannot be
/// changed here, if it cannot (the cases operation 18 refuses:
/// docs/authoring.md, "Field controls").
fn control_view<'a>(
    session: &Session,
    schema: Option<&om_data::host::CalcSchema>,
    object: &ObjectGeom,
    (state, revision): (
        &LayoutWorkspace,
        Option<om_data::layout_placement::Revision>,
    ),
    lists: &'a [(u64, String)],
) -> Option<crate::inspector::ControlView<'a>> {
    let crate::session::RenderState::Loaded(document) = &session.render else {
        return None;
    };
    let stored = (object.key != 0)
        .then(|| {
            document
                .layout
                .objects
                .objects
                .iter()
                .find(|o| o.id == object.key)
        })
        .flatten()?;
    let field = stored.field_obj.as_ref()?;
    let value_type = stored.field_spec().and_then(|spec| {
        let occurrence = om_layout::occurrence_id(spec.table_key)?;
        Some(schema?.field(occurrence, spec.field_id()?)?.value_type)
    });
    let known = om_data::layout_objects::FIELD_CONTROLS
        .iter()
        .any(|(display, ..)| *display == field.display_type);
    let fixed = if state.saving {
        Some("Save Layout is with the host.")
    } else if revision.is_none() {
        Some("The window does not know which version of the layout is shown.")
    } else if value_type == Some(om_data::calc::ValueType::Container) {
        Some("A container field shows no other control style.")
    } else if field.num_of_reps > 1 {
        Some("OpenMaker does not change the control of a field showing several repetitions yet.")
    } else if !known {
        Some("OpenMaker does not change this control style yet.")
    } else {
        None
    };
    Some(crate::inspector::ControlView {
        stored: (field.display_type, field.value_list_key),
        pending: state.controls().objects.get(&object.key).copied(),
        fixed,
        date: value_type == Some(om_data::calc::ValueType::Date),
        lists,
    })
}

/// The file's value lists for the Inspector's Values from, by key as
/// layouts store it (`valueListKey`, layout-model.md 5.4).
fn value_lists(schema: Option<&om_data::host::CalcSchema>) -> Vec<(u64, String)> {
    schema
        .map(|schema| {
            schema
                .value_lists()
                .iter()
                .filter_map(|list| {
                    Some((
                        om_data::layout_placement::serial_value(list.id)?,
                        list.name.clone(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn layout(
    ui: &mut Ui,
    session: &Session,
    tool: LayoutTool,
    panels: LayoutPanels,
    (fields, schema): (
        &[crate::layout_canvas::FieldChoice],
        Option<&om_data::host::CalcSchema>,
    ),
    state: &mut LayoutWorkspace,
    document: &mut crate::document::DocumentState,
) {
    let LayoutPanels { inspector, objects } = panels;
    state.canvas.synchronize(session);
    let before = if state.saving {
        None
    } else {
        state.canvas.checkpoint()
    };
    document.scroll.begin_paint(session);
    let full = ui.max_rect();
    let left_width = if objects {
        crate::layout_objects::WIDTH
    } else {
        0.0
    };
    let right_width = if inspector {
        theme::INSPECTOR_WIDTH
    } else {
        0.0
    };
    let canvas = Rect::from_min_max(
        full.min + vec2(left_width, 0.0),
        full.max - vec2(right_width, 0.0),
    );
    if objects {
        let rect = Rect::from_min_max(full.min, pos2(canvas.left(), full.bottom()));
        let mut child = ui.new_child(UiBuilder::new().id_salt("objects_pane").max_rect(rect));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        let table = crate::calc::layout_occurrence(session)
            .and_then(|own| schema?.table_of(own))
            .map(|table| table.name.as_str());
        let frozen = state.saving
            || state.new_button().is_some()
            || state.new_text().is_some()
            || state.new_portal().is_some();
        if let Some(index) = state.objects.show(
            &mut child,
            session,
            &state.canvas,
            crate::layout_objects::FieldList {
                choices: fields,
                table,
                frozen,
            },
        ) {
            state
                .canvas
                .select_object(session, index, ui.input(|i| i.modifiers.shift));
        }
    }
    let mut child = ui.new_child(UiBuilder::new().id_salt("design_canvas").max_rect(canvas));
    child.set_clip_rect(canvas.intersect(ui.clip_rect()));
    crate::layout_canvas::show(
        &mut child,
        session,
        tool,
        fields,
        &mut state.canvas,
        state.saving,
        document,
    );
    if inspector {
        let rect = Rect::from_min_max(pos2(canvas.right(), full.top()), full.max);
        let mut child = ui.new_child(UiBuilder::new().id_salt("inspector_pane").max_rect(rect));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        let lists = value_lists(schema);
        if state.canvas.selection_count(session) > 1 {
            state.inspector.multiple(
                &mut child,
                state.canvas.selection_count(session),
                state.canvas.selection_notice(),
            );
        } else if let Some(new_field) = state.canvas.new_field_mut() {
            state
                .inspector
                .new_field(&mut child, new_field, (fields, &lists), state.saving);
        } else if let Some(new_button) = state.canvas.new_button_mut() {
            if state
                .inspector
                .new_button(&mut child, new_button, state.saving)
            {
                state.canvas.request_button_setup();
            }
        } else if let Some(new_text) = state.canvas.new_text_mut() {
            state.inspector.new_text(&mut child, new_text, state.saving);
        } else {
            // A movable object's frame is set here too, as an unsaved change.
            let selected = state.canvas.selected_object(session).cloned();
            let revision = match &session.geometry {
                crate::session::Geometry::Loaded(geometry) => geometry.revision,
                _ => None,
            };
            let shown = selected
                .as_ref()
                .and_then(|o| state.canvas.moved().frames.get(&o.key).copied());
            let editable = selected
                .as_ref()
                .is_some_and(|o| o.movable && o.key != 0 && !state.saving && revision.is_some());
            let view = selected
                .as_ref()
                .and_then(|o| control_view(session, schema, o, (state, revision), &lists));
            let edit = state.inspector.show(
                &mut child,
                session,
                selected.as_ref(),
                shown,
                (editable, view.as_ref()),
                state.canvas.selection_notice(),
            );
            if let (Some(object), Some(frame)) = (&selected, edit.frame) {
                state
                    .canvas
                    .set_frame(object.key, frame, object.rect, revision);
            }
            if let (Some(object), Some(view), Some(control)) = (&selected, &view, edit.control) {
                state
                    .canvas
                    .set_control(object.key, control, view.stored, revision);
            }
        }
    }
    if !state.saving {
        state.canvas.record_frame(before, ui.ctx());
    }
    ui.allocate_rect(full, Sense::hover());
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::calc::{Calculations, Evaluator};
    use crate::document::DocumentState;
    use crate::session::{RenderState, Target};
    use om_data::calc::eval::Variables;
    use om_data::calc::{Locale, Value, ValueType};
    use om_data::formula::{self, Formula, Key};
    use om_data::host::{CalcSchema, FieldDef, RecordValues, SessionInfo};
    use om_data::{LayoutInfo, RecordPage};

    const TABLE: u64 = 0x8001;
    const OCCURRENCE: u64 = 0xd00001;

    fn session_and_calculations() -> (Session, Calculations) {
        let mut schema = CalcSchema::new("Jobs", Locale::en_us());
        schema.add_table(TABLE, "Jobs");
        schema.add_occurrence(OCCURRENCE, "Jobs", TABLE);
        schema.add_field(TABLE, FieldDef::data(1, "Source", ValueType::Text));
        let mut context = formula::Context::new(schema.names());
        context.occurrence = Some(OCCURRENCE);
        let formula = formula::compile("Source & \" calculated\"", &context).unwrap();
        schema.add_field(
            TABLE,
            FieldDef::calculation(
                0x8000,
                "Calculated",
                ValueType::Text,
                Formula {
                    context: Some(Key::from_id(OCCURRENCE)),
                    formatted: None,
                    polish: formula.expr,
                },
                false,
            ),
        );
        schema.add_field(
            TABLE,
            FieldDef::data(0x80ac, "Global", ValueType::Text).with_global(true),
        );
        schema.add_layout(1, "Jobs", Some(OCCURRENCE));
        let mut calcs = Calculations::default();
        calcs.set_schema(Some(Arc::new(schema)));
        let mut record = RecordValues::default();
        record.set(1, 1, Value::text("Stored"));
        calcs
            .cache
            .get_mut()
            .records
            .insert((TABLE, 1), Some(Arc::new(record)));
        let mut globals = RecordValues::default();
        globals.set(0x80ac, 1, Value::text("Session global"));
        calcs
            .cache
            .get_mut()
            .globals
            .insert(TABLE, Arc::new(globals));
        let mut session = Session::new(
            Target {
                host: "localhost".into(),
                file: "Jobs".into(),
                local: None,
            },
            25,
        );
        let key = session
            .set_layouts(vec![LayoutInfo {
                id: 1,
                name: "Jobs".into(),
            }])
            .unwrap();
        session
            .accept_page(
                key,
                Ok(RecordPage {
                    versions: Default::default(),
                    fields: ["Source", "Calculated", "Global"]
                        .into_iter()
                        .map(|name| Field {
                            name: name.into(),
                            kind: FieldKind::Text,
                        })
                        .collect(),
                    records: vec![Record {
                        id: 1,
                        values: vec![Some("Stored".into()), None, None],
                    }],
                    total: 1,
                }),
            )
            .unwrap();
        // A provider without rich objects still has a schema's occurrence.
        session.render = RenderState::Unavailable;
        session.geometry = Geometry::Unavailable;
        (session, calcs)
    }

    fn text_shapes(shape: &egui::epaint::Shape, output: &mut Vec<String>) {
        match shape {
            egui::epaint::Shape::Text(text) => output.push(text.galley.job.text.clone()),
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    text_shapes(shape, output);
                }
            }
            _ => {}
        }
    }

    /// While Save Layout's placement is with the host, the new field is
    /// what was sent: a Field tool drag on the canvas neither changes it nor
    /// replaces it, so the host's answer drops exactly what it answered.
    /// A failed Save also retains it; a later drag appends another field.
    #[test]
    fn layout_mode_holds_the_new_field_while_save_layout_is_with_the_host() {
        let mut session = Session::new(
            Target {
                host: "test".into(),
                file: "test.fmp12".into(),
                local: None,
            },
            25,
        );
        session.layout = Some(1);
        session.geometry = Geometry::Loaded(LayoutGeometry {
            width: 400.0,
            height: 300.0,
            parts: vec![om_data::PartGeom {
                kind_name: "Body".into(),
                top: 0.0,
                height: 300.0,
            }],
            revision: Some([7; 32]),
            ..LayoutGeometry::default()
        });
        let fields = [crate::layout_canvas::FieldChoice {
            table: 5,
            field: 1,
            name: "Name".into(),
            date: false,
        }];
        let mut state = LayoutWorkspace::default();
        let mut document = DocumentState::default();
        document.zoom.set_percent(200);
        let ctx = egui::Context::default();
        // A drag between layout points; the layout's origin is right of the
        // 22-point part strip.
        let mut drag = |state: &mut LayoutWorkspace, from: egui::Pos2, to: egui::Pos2| {
            let on_screen =
                |point: egui::Pos2| Pos2::ZERO + (point.to_vec2() + vec2(22.0, 0.0)) * 2.0;
            let (from, to) = (on_screen(from), on_screen(to));
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            for events in [
                vec![egui::Event::PointerMoved(from)],
                vec![button(from, true)],
                vec![egui::Event::PointerMoved(from + (to - from) * 0.5)],
                vec![egui::Event::PointerMoved(to)],
                vec![button(to, false)],
                vec![],
            ] {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))),
                    events,
                    ..Default::default()
                };
                let mut rendered = ctx.run_ui(input, |ui| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            layout(
                                ui,
                                &session,
                                LayoutTool::Field,
                                LayoutPanels {
                                    inspector: true,
                                    objects: false,
                                },
                                (&fields, None),
                                state,
                                &mut document,
                            );
                        });
                });
                rendered.textures_delta.clear();
            }
        };
        drag(&mut state, pos2(20.0, 40.0), pos2(120.0, 62.0));
        let drawn = state.new_field().cloned().expect("the Field tool draws");
        assert_eq!(
            (
                drawn.left,
                drawn.top,
                drawn.width,
                drawn.height,
                drawn.revision
            ),
            (20.0, 40.0, 100.0, 22.0, Some([7; 32]))
        );

        state.saving = true;
        drag(&mut state, pos2(200.0, 100.0), pos2(260.0, 130.0));
        assert_eq!(state.new_field(), Some(&drawn));

        state.saving = false;
        drag(&mut state, pos2(200.0, 100.0), pos2(260.0, 130.0));
        assert_eq!(
            state.new_field(),
            Some(&drawn),
            "a second drag never replaces the first"
        );
        assert_eq!(state.canvas.fields().len(), 2);
        let second = &state.canvas.fields()[1];
        assert_eq!((second.left, second.top), (200.0, 100.0));
        assert!(state.edit_draft(&ctx, &session, crate::layout_canvas::DraftEdit::Undo));
        assert_eq!(state.canvas.fields(), std::slice::from_ref(&drawn));
        assert!(state.edit_draft(&ctx, &session, crate::layout_canvas::DraftEdit::Redo));
        assert_eq!(state.canvas.fields().len(), 2);
        state.rebase([7; 32], [8; 32]);
        assert_eq!(state.draft_edits(&session), (false, false));
        assert_eq!(state.new_field().unwrap().revision, Some([8; 32]));
        state.revert();
        assert!(
            state.canvas.fields().is_empty(),
            "Revert discards every addition"
        );
    }

    #[test]
    fn fallback_views_calculate_columns_and_preview_refuses_missing_printable_layout() {
        let (session, mut calcs) = session_and_calculations();
        let mut document = DocumentState::default();
        let evaluator = Evaluator::new(
            &calcs,
            &session,
            &document,
            SessionInfo::default(),
            &Variables::new(),
        )
        .unwrap();
        let ctx = egui::Context::default();
        for view in [ViewMode::Form, ViewMode::List, ViewMode::Table] {
            for previewing in [false, true] {
                let mut output = Vec::new();
                for _ in 0..2 {
                    let mut rendered = ctx.run_ui(Default::default(), |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            if previewing {
                                crate::preview::show(
                                    ui,
                                    &session,
                                    Some(&evaluator),
                                    &document.panels,
                                    document.zoom,
                                    &mut document.scroll,
                                    &mut Vec::new(),
                                );
                            } else {
                                browse(
                                    ui,
                                    &session,
                                    view,
                                    &mut document,
                                    Some(&evaluator),
                                    &mut Vec::new(),
                                );
                            }
                        });
                    });
                    rendered.textures_delta.clear();
                    for shape in rendered.shapes {
                        text_shapes(&shape.shape, &mut output);
                    }
                }
                if previewing {
                    assert!(output
                        .iter()
                        .any(|text| text.contains("Exact Preview is unavailable")));
                    continue;
                }
                for value in ["Stored", "Stored calculated", "Session global"] {
                    assert!(
                        output.iter().any(|text| text == value),
                        "{view:?}, preview={previewing}: {value:?} absent from {output:?}"
                    );
                }
            }
        }
        assert!(calcs.take_wanted().is_empty());
        // Displaying a deferred calculation never fills every page value.
        assert_eq!(session.current_record().unwrap().1.values[1], None);
    }

    #[test]
    fn columns_keep_provider_values_without_a_calculation_source() {
        let field = Field {
            name: "Calculated".into(),
            kind: FieldKind::Text,
        };
        let record = Record {
            id: 1,
            values: vec![Some("Provider value".into())],
        };
        assert_eq!(column_value(&record, 0, &field, None), "Provider value");
        assert!(!calculated_column(&field, None));
    }
}
