//! FileMaker's small dialogs for the step options (capture 35b): the window
//! chrome and controls they share (a title bar with the window buttons and
//! the centered title, FileMaker's push buttons, text fields, pop-up menus,
//! checkboxes, radio buttons and list boxes) and the pickers the options
//! open: Specify Field (Specify Target, which also takes a variable),
//! Specify Layout, Specify Script and Specify Table. Each picker is a modal
//! of its own that answers with [`Picked`], so Specify Calculation's field
//! list can reuse them.

use std::sync::Arc;

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FontId, Id, Key, Rect, Response, Role, Sense,
    Stroke, StrokeKind, Ui, WidgetInfo,
};
use om_data::host::{CalcSchema, LayoutKind};
use om_data::scripts::ScriptKind;

use super::glyphs::{self, Glyph};
use crate::{theme, widgets};

/// The title bar's height (35b: 48 px at 1.74 px per point).
pub(super) const TITLE_BAR: f32 = 28.0;
/// The body's left and right insets.
pub(super) const MARGIN_LEFT: f32 = 22.0;
pub(super) const MARGIN_RIGHT: f32 = 21.0;
pub(super) const MARGIN_BOTTOM: f32 = 19.0;
/// Push buttons: Cancel, OK and Specify… are all this wide in 35b.
pub(super) const BUTTON_WIDTH: f32 = 84.0;
/// Push buttons, text fields and pop-up menus.
pub(super) const CONTROL_HEIGHT: f32 = 20.0;
/// Between Cancel and OK.
pub(super) const BUTTON_GAP: f32 = 14.0;
/// One row of a list box.
pub(super) const LIST_ROW: f32 = 19.0;

const FILL: Color32 = Color32::from_rgb(0xF6, 0xF6, 0xF6);
const TITLE_FILL: Color32 = Color32::from_rgb(0xFC, 0xFC, 0xFC);
const TITLE_RULE: Color32 = Color32::from_rgb(0xDE, 0xDE, 0xDE);
const WINDOW_BORDER: Color32 = Color32::from_rgb(0xC8, 0xC8, 0xC8);
/// The inactive close and minimize buttons, and zoom (35b).
const LIGHT_OFF: Color32 = Color32::from_rgb(0xE1, 0xE1, 0xE1);
const ZOOM: Color32 = Color32::from_rgb(0x28, 0xC8, 0x40);
const FIELD_EDGE: Color32 = Color32::from_rgb(0xE2, 0xE2, 0xE2);
const FIELD_BOTTOM: Color32 = Color32::from_rgb(0xD2, 0xD2, 0xD2);
const BUTTON_EDGE: Color32 = Color32::from_rgb(0xDC, 0xDC, 0xDC);
const LIST_BORDER: Color32 = Color32::from_rgb(0xD0, 0xD0, 0xD0);
const TITLE_TEXT: Color32 = Color32::from_rgb(0x3C, 0x3C, 0x3C);
/// A text field's focus ring and selected text (35b).
const FOCUS_RING: Color32 = Color32::from_rgb(0x89, 0xA2, 0xE8);
const SELECTION: Color32 = Color32::from_rgb(0xC1, 0xCC, 0xF7);
/// The default button's blue in 35b (its gradient runs `#3648D0` to
/// `#1E29C8`), also the checked checkboxes' and radio buttons'. It is
/// deeper than `theme::DEFAULT_BUTTON`, which no capture of an enabled
/// default button backs.
const ACCENT: Color32 = Color32::from_rgb(0x2B, 0x3C, 0xCC);

/// A picker's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Picked<T> {
    Cancel,
    Ok(T),
}

/// Return or Escape pressed while the dialog was the one on top.
pub(super) struct Shown {
    pub enter: bool,
    pub escape: bool,
}

/// A FileMaker dialog window as a modal: the title bar (inactive close
/// and minimize buttons, the green zoom button, the title centered in
/// semibold), a rule, and the body on FileMaker's dialog gray. `on_top`
/// says whether the caller has no dialog open above this one, so only the
/// top dialog answers Return and Escape.
pub(super) fn dialog(
    ctx: &egui::Context,
    id: &'static str,
    title: &str,
    width: f32,
    on_top: bool,
    content: impl FnOnce(&mut Ui),
) -> Shown {
    let frame = egui::Frame::new()
        .fill(FILL)
        .stroke(Stroke::new(1.0, WINDOW_BORDER))
        .corner_radius(CornerRadius::same(10))
        .shadow(ctx.global_style().visuals.window_shadow);
    egui::Modal::new(Id::unique(id))
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(24))
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            title_bar(ui, title, width);
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: MARGIN_LEFT as i8,
                    right: MARGIN_RIGHT as i8,
                    top: 0,
                    bottom: MARGIN_BOTTOM as i8,
                })
                .show(ui, |ui| {
                    ui.set_width(width - MARGIN_LEFT - MARGIN_RIGHT);
                    content(ui);
                });
        });
    let keys = on_top && !egui::Popup::is_any_open(ctx);
    let (enter, escape) = if keys {
        ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)))
    } else {
        (false, false)
    };
    Shown { enter, escape }
}

fn title_bar(ui: &mut Ui, title: &str, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, TITLE_BAR), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        CornerRadius {
            nw: 10,
            ne: 10,
            sw: 0,
            se: 0,
        },
        TITLE_FILL,
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, TITLE_RULE),
    );
    for (n, color) in [LIGHT_OFF, LIGHT_OFF, ZOOM].into_iter().enumerate() {
        let center = pos2(rect.left() + 15.5 + n as f32 * 20.0, rect.center().y);
        painter.circle(
            center,
            6.0,
            color,
            Stroke::new(0.5, color.linear_multiply(0.9)),
        );
    }
    label_in(
        ui,
        Rect::from_center_size(rect.center(), vec2(width - 160.0, TITLE_BAR)),
        title,
        FontId::new(theme::BODY, theme::semibold()),
        TITLE_TEXT,
        Align::Center,
    );
}

/// The body's text font (13 pt).
pub(super) fn body() -> FontId {
    theme::body()
}

/// Run `add` in a child of `ui` filling `rect`, without moving `ui`'s
/// cursor: the dialogs lay their rows out in bands they allocate first.
fn place<R>(ui: &mut Ui, rect: Rect, layout: egui::Layout, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout));
    add(&mut child)
}

/// A text label in `rect`, vertically centered and aligned `align`.
fn label_in(ui: &mut Ui, rect: Rect, text: &str, font: FontId, color: Color32, align: Align) {
    let layout = match align {
        Align::Min => egui::Layout::left_to_right(Align::Center),
        Align::Center => egui::Layout::top_down(Align::Center)
            .with_main_align(Align::Center)
            .with_main_justify(true),
        Align::Max => egui::Layout::right_to_left(Align::Center),
    };
    place(ui, rect, layout, |ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(text).font(font).color(color))
                .selectable(false)
                .truncate(),
        );
    });
}

/// A label right-aligned in `rect`, as the options' row labels are.
pub(super) fn row_label(ui: &mut Ui, rect: Rect, text: &str, enabled: bool) {
    let color = if enabled {
        theme::TEXT
    } else {
        theme::DISABLED
    };
    label_in(ui, rect, text, body(), color, Align::Max);
}

/// A label left-aligned in `rect`.
pub(super) fn left_label(ui: &mut Ui, rect: Rect, text: &str, font: FontId, color: Color32) {
    label_in(ui, rect, text, font, color, Align::Min);
}

/// A push button in `rect`: white with a soft edge, or FileMaker's blue
/// for the default button. `accessible` names it for accessibility when
/// several buttons read the same (`Specify…`).
pub(super) fn push_button(
    ui: &mut Ui,
    rect: Rect,
    text: &str,
    accessible: &str,
    default: bool,
    enabled: bool,
) -> Response {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("push", accessible)),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let pressed = response.is_pointer_button_down_on();
    let painter = ui.painter();
    let radius = CornerRadius::same(5);
    // A soft shadow below the button, as macOS draws it.
    painter.rect_filled(
        rect.translate(vec2(0.0, 0.5)).expand(0.5),
        radius,
        Color32::from_black_alpha(18),
    );
    let (fill, stroke, color) = match (default && enabled, pressed) {
        (true, false) => (ACCENT, Stroke::NONE, Color32::WHITE),
        (true, true) => (ACCENT.gamma_multiply(0.85), Stroke::NONE, Color32::WHITE),
        (false, false) => (
            Color32::WHITE,
            Stroke::new(0.5, BUTTON_EDGE),
            if enabled {
                theme::TEXT
            } else {
                theme::DISABLED
            },
        ),
        (false, true) => (
            Color32::from_gray(0xE2),
            Stroke::new(0.5, Color32::from_gray(0xB0)),
            theme::TEXT,
        ),
    };
    painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
    painter.text(rect.center(), Align2::CENTER_CENTER, text, body(), color);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, accessible));
    response
}

/// Cancel and OK at the right of a full-width row; returns (cancel, ok).
pub(super) fn cancel_ok(ui: &mut Ui, ok_enabled: bool) -> (bool, bool) {
    let (row, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), CONTROL_HEIGHT), Sense::hover());
    let ok_rect = Rect::from_min_size(
        pos2(row.right() - BUTTON_WIDTH, row.top()),
        vec2(BUTTON_WIDTH, CONTROL_HEIGHT),
    );
    let cancel_rect = ok_rect.translate(vec2(-(BUTTON_WIDTH + BUTTON_GAP), 0.0));
    let cancel = push_button(ui, cancel_rect, "Cancel", "Cancel", false, true).clicked();
    let ok = push_button(ui, ok_rect, "OK", "OK", true, ok_enabled).clicked();
    (cancel, ok && ok_enabled)
}

/// A single-line text field in `rect`: white, a light edge darker at the
/// bottom, and the blue focus ring.
pub(super) fn text_field(
    ui: &mut Ui,
    rect: Rect,
    text: &mut String,
    accessible: &str,
    enabled: bool,
) -> Response {
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::ZERO,
        if enabled {
            Color32::WHITE
        } else {
            Color32::from_gray(0xF2)
        },
        Stroke::new(1.0, FIELD_EDGE),
        StrokeKind::Inside,
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, FIELD_BOTTOM),
    );
    // Inter sits lower in its line than the system font: the text is
    // raised a little so it shares the row label's baseline (35b).
    let inner = Rect::from_min_max(rect.min + vec2(5.0, 0.0), rect.max - vec2(4.0, 2.5));
    let response = place(
        ui,
        inner,
        egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
        |ui| {
            let visuals = ui.visuals_mut();
            visuals.selection.bg_fill = SELECTION;
            visuals.selection.stroke.color = theme::TEXT;
            ui.add(
                egui::TextEdit::singleline(text)
                    .id_salt(("text_field", accessible))
                    .font(body())
                    .text_color(if enabled {
                        theme::TEXT
                    } else {
                        theme::WEAK_TEXT
                    })
                    .frame(egui::Frame::NONE)
                    .align(Align2::LEFT_CENTER)
                    .desired_width(inner.width())
                    .interactive(enabled),
            )
        },
    );
    widgets::accessible_label(ui, response.id, accessible);
    if response.has_focus() {
        focus_ring(ui, rect, 0);
    }
    response
}

/// macOS's focus ring around a control.
pub(super) fn focus_ring(ui: &Ui, rect: Rect, radius: u8) {
    ui.painter().rect_stroke(
        rect.expand(0.5),
        CornerRadius::same(radius.saturating_add(2)),
        Stroke::new(3.0, FOCUS_RING),
        StrokeKind::Outside,
    );
}

/// Select all of a text field's text and give it the keyboard, as a
/// dialog opens with its first field (35b).
pub(super) fn select_all(ui: &Ui, response: &Response, text: &str) {
    let mut state = egui::TextEdit::load_state(ui.ctx(), response.id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::select_all_str(text)));
    state.store(ui.ctx(), response.id);
    response.request_focus();
}

/// A read-only value where a text field would be (a field or layout
/// name): the field's look, the text truncated.
pub(super) fn value_box(ui: &mut Ui, rect: Rect, text: &str, accessible: &str, enabled: bool) {
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::ZERO,
        Color32::from_gray(0xFB),
        Stroke::new(1.0, FIELD_EDGE),
        StrokeKind::Inside,
    );
    let inner = Rect::from_min_max(rect.min + vec2(6.0, 0.0), rect.max - vec2(4.0, 0.0));
    let color = if enabled {
        theme::TEXT
    } else {
        theme::WEAK_TEXT
    };
    let galley = ui.fonts_mut(|f| {
        let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), body(), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(inner.width());
        f.layout_job(job)
    });
    let response = ui.interact(
        rect,
        ui.scope_id().with(("value", accessible)),
        Sense::hover(),
    );
    ui.painter().galley(
        pos2(inner.left(), inner.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::TextInput, false, accessible);
        info.current_text_value = Some(text.to_owned());
        info
    });
}

/// A pop-up menu in `rect`: the current item and the blue up-down box;
/// returns the index of a newly chosen item.
pub(super) fn popup_menu(
    ui: &mut Ui,
    rect: Rect,
    accessible: &str,
    current: &str,
    items: &[String],
    enabled: bool,
) -> Option<usize> {
    let response = ui.interact(
        rect,
        ui.scope_id().with(("popup", accessible)),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let painter = ui.painter();
    let radius = CornerRadius::same(5);
    painter.rect_filled(
        rect.translate(vec2(0.0, 0.5)).expand(0.5),
        radius,
        Color32::from_black_alpha(18),
    );
    painter.rect(
        rect,
        radius,
        Color32::WHITE,
        Stroke::new(0.5, BUTTON_EDGE),
        StrokeKind::Inside,
    );
    let arrows =
        Rect::from_center_size(pos2(rect.right() - 10.5, rect.center().y), vec2(16.0, 16.0));
    painter.rect_filled(
        arrows,
        CornerRadius::same(4),
        if enabled { ACCENT } else { theme::DISABLED },
    );
    let stroke = Stroke::new(1.3, Color32::WHITE);
    let c = arrows.center();
    painter.line(
        vec![
            c + vec2(-3.0, -1.5),
            c + vec2(0.0, -4.5),
            c + vec2(3.0, -1.5),
        ],
        stroke,
    );
    painter.line(
        vec![c + vec2(-3.0, 1.5), c + vec2(0.0, 4.5), c + vec2(3.0, 1.5)],
        stroke,
    );
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 8.0, rect.top()),
        pos2(arrows.left() - 4.0, rect.bottom()),
    );
    let color = if enabled {
        theme::TEXT
    } else {
        theme::DISABLED
    };
    let galley = ui.fonts_mut(|f| {
        let mut job = egui::text::LayoutJob::simple_singleline(current.to_owned(), body(), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width());
        f.layout_job(job)
    });
    ui.painter().galley(
        pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::ComboBox, enabled, accessible);
        info.current_text_value = Some(current.to_owned());
        info
    });
    let mut chosen = None;
    let popup = egui::Popup::menu(&response).width(rect.width());
    let layer = egui::LayerId::new(egui::Order::Foreground, popup.get_id());
    let shown = popup.show(|ui| {
        for (i, item) in items.iter().enumerate() {
            if ui.selectable_label(item == current, item).clicked() {
                chosen = Some(i);
                ui.close();
            }
        }
    });
    // Keep the menu directly above the dialog it opens from. Both are
    // foreground areas, and a dialog raised after the menu last showed
    // (by a nested dialog closing over it, or a press on it) would
    // otherwise cover it, leaving a menu that cannot be used (#794).
    if shown.is_some() {
        ui.ctx().set_sublayer(ui.layer_id(), layer);
    }
    chosen
}

/// A checkbox at the left of `rect` with its label; toggles `on` when
/// clicked and says whether it was.
pub(super) fn checkbox(ui: &mut Ui, rect: Rect, on: &mut bool, label: &str, enabled: bool) -> bool {
    let text_width = widgets::text_width(ui, label, body());
    let hit = Rect::from_min_size(rect.min, vec2(20.0 + text_width, rect.height()));
    let response = ui.interact(
        hit,
        ui.scope_id().with(("checkbox", label)),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let clicked = response.clicked() && enabled;
    if clicked {
        *on = !*on;
    }
    let boxed = Rect::from_min_size(pos2(rect.left(), rect.center().y - 7.0), vec2(14.0, 14.0));
    let painter = ui.painter();
    if *on {
        painter.rect_filled(
            boxed,
            CornerRadius::same(3),
            if enabled { ACCENT } else { theme::DISABLED },
        );
        let c = boxed.center();
        painter.line(
            vec![
                c + vec2(-3.5, 0.2),
                c + vec2(-1.0, 2.8),
                c + vec2(3.8, -2.8),
            ],
            Stroke::new(1.8, Color32::WHITE),
        );
    } else {
        painter.rect(
            boxed,
            CornerRadius::same(3),
            Color32::WHITE,
            Stroke::new(1.0, Color32::from_gray(0xB8)),
            StrokeKind::Inside,
        );
    }
    let color = if enabled {
        theme::TEXT
    } else {
        theme::DISABLED
    };
    painter.text(
        pos2(boxed.right() + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        body(),
        color,
    );
    let on = *on;
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, enabled, on, label));
    clicked
}

/// A radio button at the left of `rect` with its label; true when clicked.
pub(super) fn radio(ui: &mut Ui, rect: Rect, on: bool, label: &str, enabled: bool) -> bool {
    let text_width = widgets::text_width(ui, label, body());
    let hit = Rect::from_min_size(rect.min, vec2(20.0 + text_width, rect.height()));
    let response = ui.interact(
        hit,
        ui.scope_id().with(("radio", label)),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let center = pos2(rect.left() + 7.0, rect.center().y);
    let painter = ui.painter();
    if on {
        painter.circle_filled(center, 7.0, if enabled { ACCENT } else { theme::DISABLED });
        painter.circle_filled(center, 2.6, Color32::WHITE);
    } else {
        painter.circle(
            center,
            6.5,
            Color32::WHITE,
            Stroke::new(1.0, Color32::from_gray(0xB8)),
        );
    }
    painter.text(
        pos2(center.x + 13.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        body(),
        if enabled {
            theme::TEXT
        } else {
            theme::DISABLED
        },
    );
    response.widget_info(|| WidgetInfo::selected(Role::RadioButton, enabled, on, label));
    response.clicked() && enabled
}

/// Paints a mark at the right of a list row (a sort direction).
pub(super) type Mark = fn(&egui::Painter, Rect, Color32);

/// A row of a list box.
#[derive(Clone, Debug)]
pub(super) struct ListRow {
    pub text: String,
    /// Gray text at the right (a layout's occurrence).
    pub detail: String,
    /// A second column at this offset from the row's left (a find
    /// request's criteria after its action).
    pub second: Option<(f32, String)>,
    /// A mark painted at the right.
    pub mark: Option<Mark>,
    /// Folder nesting.
    pub depth: usize,
    pub icon: Option<Glyph>,
    /// A folder or a separator: shown, not chosen.
    pub selectable: bool,
    pub separator: bool,
}

impl ListRow {
    pub fn item(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            detail: String::new(),
            second: None,
            mark: None,
            depth: 0,
            icon: None,
            selectable: true,
            separator: false,
        }
    }

    /// The row's accessible name: its text and second column.
    fn label(&self) -> String {
        match &self.second {
            Some((_, second)) => format!("{} {second}", self.text),
            None => self.text.clone(),
        }
    }
}

/// What a click or drag in a list box did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ListClick {
    pub clicked: Option<usize>,
    pub double_clicked: Option<usize>,
    /// A row dragged to before another position: (from, to).
    pub moved: Option<(usize, usize)>,
}

/// A white list box with a thin border filling `rect`, its rows scrolled
/// inside; the selected row is FileMaker's blue with white text.
pub(super) fn list_box(
    ui: &mut Ui,
    rect: Rect,
    id_salt: &str,
    rows: &[ListRow],
    selected: Option<usize>,
    enabled: bool,
) -> ListClick {
    list_box_with(ui, rect, id_salt, rows, selected, enabled, false)
}

/// [`list_box`], with rows that can be dragged into a new order when
/// `draggable` (the sort order).
pub(super) fn list_box_with(
    ui: &mut Ui,
    rect: Rect,
    id_salt: &str,
    rows: &[ListRow],
    selected: Option<usize>,
    enabled: bool,
    draggable: bool,
) -> ListClick {
    ui.painter().rect(
        rect,
        CornerRadius::ZERO,
        Color32::WHITE,
        Stroke::new(1.0, LIST_BORDER),
        StrokeKind::Inside,
    );
    let mut click = ListClick::default();
    let inner = rect.shrink(1.0);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .id_salt(("list_box", id_salt))
            .layout(egui::Layout::top_down(Align::Min)),
    );
    child.set_clip_rect(inner.intersect(ui.clip_rect()));
    egui::ScrollArea::vertical()
        .id_salt(("list_scroll", id_salt))
        .max_height(inner.height())
        .auto_shrink([false, false])
        .show(&mut child, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let top = ui.cursor().top();
            let dragging_id = ui.scope_id().with("dragging");
            let sense = if draggable {
                Sense::click_and_drag()
            } else {
                Sense::click()
            };
            for (i, row) in rows.iter().enumerate() {
                let (row_rect, _) =
                    ui.allocate_exact_size(vec2(inner.width(), LIST_ROW), Sense::hover());
                if row.separator {
                    ui.painter().hline(
                        row_rect.x_range().shrink(6.0),
                        row_rect.center().y,
                        Stroke::new(1.0, theme::HAIRLINE),
                    );
                    continue;
                }
                let response = ui.interact(
                    row_rect,
                    ui.scope_id().with(("row", i)),
                    if enabled && row.selectable {
                        sense
                    } else {
                        Sense::hover()
                    },
                );
                if draggable {
                    if response.drag_started() {
                        ui.data_mut(|d| d.insert_temp(dragging_id, i));
                    }
                    let target = |ui: &Ui| {
                        ui.ctx().pointer_interact_pos().map(|pos| {
                            (((pos.y - top) / LIST_ROW).round().max(0.0) as usize).min(rows.len())
                        })
                    };
                    if response.dragged() {
                        if let Some(to) = target(ui) {
                            let y = top + to as f32 * LIST_ROW;
                            ui.painter().hline(
                                inner.x_range().shrink(4.0),
                                y,
                                Stroke::new(2.0, theme::SELECTED),
                            );
                        }
                    }
                    if response.drag_stopped() {
                        let from = ui.data_mut(|d| d.remove_temp::<usize>(dragging_id));
                        if let (Some(from), Some(to)) = (from, target(ui)) {
                            click.moved = Some((from, to));
                        }
                    }
                }
                let is_selected = selected == Some(i);
                let color = if !enabled {
                    theme::WEAK_TEXT
                } else if is_selected {
                    Color32::WHITE
                } else {
                    theme::TEXT
                };
                if is_selected {
                    ui.painter()
                        .rect_filled(row_rect, CornerRadius::ZERO, theme::SELECTED);
                }
                let mut x = row_rect.left() + 8.0 + row.depth as f32 * 16.0;
                if let Some(icon) = row.icon {
                    let icon_rect = Rect::from_center_size(
                        pos2(x + 7.0, row_rect.center().y),
                        vec2(15.0, 15.0),
                    );
                    glyphs::paint(ui.painter(), icon_rect, icon, color);
                    x += 20.0;
                }
                let detail_width = if row.detail.is_empty() {
                    0.0
                } else {
                    widgets::text_width(ui, &row.detail, body()) + 16.0
                };
                let mark_width = if row.mark.is_some() { 22.0 } else { 0.0 };
                let text_right = match &row.second {
                    Some((offset, _)) => row_rect.left() + offset - 8.0,
                    None => row_rect.right() - 8.0 - detail_width - mark_width,
                };
                let text_rect = Rect::from_min_max(
                    pos2(x, row_rect.top()),
                    pos2(text_right, row_rect.bottom()),
                );
                if let Some((offset, second)) = &row.second {
                    let second_rect = Rect::from_min_max(
                        pos2(row_rect.left() + offset, row_rect.top()),
                        pos2(row_rect.right() - 8.0, row_rect.bottom()),
                    );
                    let galley = ui.fonts_mut(|f| {
                        let mut job =
                            egui::text::LayoutJob::simple_singleline(second.clone(), body(), color);
                        job.wrap = egui::text::TextWrapping::truncate_at_width(second_rect.width());
                        f.layout_job(job)
                    });
                    ui.painter().galley(
                        pos2(
                            second_rect.left(),
                            row_rect.center().y - galley.size().y / 2.0,
                        ),
                        galley,
                        color,
                    );
                }
                if let Some(mark) = row.mark {
                    let mark_rect = Rect::from_center_size(
                        pos2(row_rect.right() - 16.0, row_rect.center().y),
                        vec2(14.0, 14.0),
                    );
                    mark(ui.painter(), mark_rect, color);
                }
                let font = if row.selectable {
                    body()
                } else {
                    FontId::new(theme::BODY, theme::semibold())
                };
                let galley = ui.fonts_mut(|f| {
                    let mut job =
                        egui::text::LayoutJob::simple_singleline(row.text.clone(), font, color);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width());
                    f.layout_job(job)
                });
                ui.painter().galley(
                    pos2(
                        text_rect.left(),
                        row_rect.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    color,
                );
                if !row.detail.is_empty() {
                    ui.painter().text(
                        pos2(row_rect.right() - 8.0, row_rect.center().y),
                        Align2::RIGHT_CENTER,
                        &row.detail,
                        body(),
                        if is_selected {
                            Color32::WHITE
                        } else {
                            theme::WEAK_TEXT
                        },
                    );
                }
                let label = row.label();
                response.widget_info(|| {
                    WidgetInfo::selected(
                        Role::ListItem,
                        enabled && row.selectable,
                        is_selected,
                        &label,
                    )
                });
                if response.double_clicked() {
                    click.double_clicked = Some(i);
                } else if response.clicked() {
                    click.clicked = Some(i);
                }
            }
        });
    click
}

/// A field or a variable, as Specify Field and Specify Target answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FieldTarget {
    /// A field of a table occurrence (the ids step field specs use) and
    /// its repetition.
    Field {
        occurrence: u64,
        field: u64,
        repetition: i16,
    },
    /// A variable target (`$name`, `$$name`).
    Variable(String),
}

/// Specify Field: the table occurrence pop-up and its table's fields,
/// with the repetition below. As Specify Target it also takes a variable
/// name, as FileMaker's target options do.
pub(super) struct SpecifyField {
    names: Option<Arc<CalcSchema>>,
    target: bool,
    variable: bool,
    variable_name: String,
    occurrence: Option<u64>,
    field: Option<u64>,
    repetition: String,
    /// A spec's repetition 0 (the repetition is a calculation, slot 10),
    /// answered again while the repetition shown (1) is not edited.
    calculated_repetition: bool,
    /// Give the first field the keyboard once.
    focus: bool,
}

impl SpecifyField {
    pub fn new(names: Option<Arc<CalcSchema>>, current: Option<FieldTarget>, target: bool) -> Self {
        let first_occurrence = names
            .as_ref()
            .and_then(|n| n.occurrences_in_order().first().map(|o| o.id));
        let mut picker = Self {
            names,
            target,
            variable: false,
            variable_name: String::new(),
            occurrence: first_occurrence,
            field: None,
            repetition: "1".into(),
            calculated_repetition: false,
            focus: true,
        };
        match current {
            Some(FieldTarget::Field {
                occurrence,
                field,
                repetition,
            }) => {
                picker.occurrence = Some(occurrence);
                picker.field = Some(field);
                picker.repetition = repetition.max(1).to_string();
                picker.calculated_repetition = repetition == 0;
            }
            Some(FieldTarget::Variable(name)) if target => {
                picker.variable = true;
                picker.variable_name = name;
            }
            _ => {}
        }
        picker
    }

    pub fn title(&self) -> &'static str {
        if self.target {
            "Specify Target"
        } else {
            "Specify Field"
        }
    }

    /// The occurrences in display order: (id, name).
    fn occurrences(&self) -> Vec<(u64, String)> {
        self.names
            .as_ref()
            .map(|n| {
                n.occurrences_in_order()
                    .into_iter()
                    .map(|o| (o.id, o.name.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The fields of the chosen occurrence's table, in creation order.
    fn fields(&self) -> Vec<(u64, String)> {
        let (Some(names), Some(occurrence)) = (&self.names, self.occurrence) else {
            return Vec::new();
        };
        names
            .table_of(occurrence)
            .map(|t| t.fields.values().map(|f| (f.id, f.name.clone())).collect())
            .unwrap_or_default()
    }

    fn answer(&self) -> Option<FieldTarget> {
        if self.variable {
            let name = self.variable_name.trim();
            return (name.starts_with('$') && name.len() > 1 && name != "$$")
                .then(|| FieldTarget::Variable(name.to_owned()));
        }
        let repetition = match self.repetition.trim() {
            "1" if self.calculated_repetition => 0,
            text => text.parse::<i16>().ok().filter(|r| *r >= 1)?,
        };
        Some(FieldTarget::Field {
            occurrence: self.occurrence?,
            field: self.field?,
            repetition,
        })
    }

    pub fn show(&mut self, ctx: &egui::Context, on_top: bool) -> Option<Picked<FieldTarget>> {
        let width = 360.0;
        let title = self.title();
        let mut outcome = None;
        let shown = dialog(ctx, "specify_field", title, width, on_top, |ui| {
            let full = ui.available_width();
            let indent = if self.target { 22.0 } else { 0.0 };
            ui.add_space(14.0);
            if self.target {
                let (rect, _) = ui.allocate_exact_size(vec2(full, 20.0), Sense::hover());
                if radio(ui, rect, !self.variable, "Field", true) {
                    self.variable = false;
                }
                ui.add_space(6.0);
            }
            let field_enabled = !self.variable;
            let occurrences = self.occurrences();
            let current = occurrences
                .iter()
                .find(|(id, _)| Some(*id) == self.occurrence)
                .map_or_else(String::new, |(_, name)| name.clone());
            let (row, _) = ui.allocate_exact_size(vec2(full, CONTROL_HEIGHT), Sense::hover());
            let popup = Rect::from_min_max(row.min + vec2(indent, 0.0), row.max);
            let items: Vec<String> = occurrences.iter().map(|(_, n)| n.clone()).collect();
            if let Some(i) = popup_menu(ui, popup, "Table", &current, &items, field_enabled) {
                if self.occurrence != Some(occurrences[i].0) {
                    self.occurrence = Some(occurrences[i].0);
                    self.field = None;
                }
            }
            ui.add_space(8.0);
            let fields = self.fields();
            let rows: Vec<ListRow> = fields.iter().map(|(_, n)| ListRow::item(n)).collect();
            let selected = fields.iter().position(|(id, _)| Some(*id) == self.field);
            let (area, _) = ui.allocate_exact_size(vec2(full, 220.0), Sense::hover());
            let list = Rect::from_min_max(area.min + vec2(indent, 0.0), area.max);
            let click = list_box(ui, list, "fields", &rows, selected, field_enabled);
            if let Some(i) = click.clicked.or(click.double_clicked) {
                self.field = Some(fields[i].0);
            }
            ui.add_space(10.0);
            let (row, _) = ui.allocate_exact_size(vec2(full, CONTROL_HEIGHT), Sense::hover());
            let label_rect =
                Rect::from_min_size(row.min + vec2(indent, 0.0), vec2(80.0, CONTROL_HEIGHT));
            left_label(
                ui,
                label_rect,
                "Repetition:",
                body(),
                if field_enabled {
                    theme::TEXT
                } else {
                    theme::DISABLED
                },
            );
            let rep_rect = Rect::from_min_size(
                pos2(label_rect.left() + 80.0, row.top()),
                vec2(60.0, CONTROL_HEIGHT),
            );
            text_field(
                ui,
                rep_rect,
                &mut self.repetition,
                "Field Repetition",
                field_enabled,
            );
            if self.target {
                ui.add_space(14.0);
                let (row, _) = ui.allocate_exact_size(vec2(full, CONTROL_HEIGHT), Sense::hover());
                let radio_rect = Rect::from_min_size(row.min, vec2(90.0, CONTROL_HEIGHT));
                if radio(ui, radio_rect, self.variable, "Variable", true) {
                    self.variable = true;
                }
                let name_rect = Rect::from_min_max(pos2(row.left() + 90.0, row.top()), row.max);
                let edit = text_field(
                    ui,
                    name_rect,
                    &mut self.variable_name,
                    "Variable Name",
                    self.variable,
                );
                if self.focus && self.variable {
                    select_all(ui, &edit, &self.variable_name.clone());
                }
                if edit.gained_focus() || edit.clicked() {
                    self.variable = true;
                }
            }
            self.focus = false;
            ui.add_space(20.0);
            let answer = self.answer();
            let (cancel, ok) = cancel_ok(ui, answer.is_some());
            if cancel {
                outcome = Some(Picked::Cancel);
            } else if ok || click.double_clicked.is_some() {
                outcome = answer.map(Picked::Ok);
            }
        });
        if outcome.is_none() {
            if shown.escape {
                outcome = Some(Picked::Cancel);
            } else if shown.enter {
                outcome = self.answer().map(Picked::Ok);
            }
        }
        outcome
    }
}

/// One choice of a list picker.
#[derive(Clone, Debug)]
struct Choice {
    row: ListRow,
    /// `None` for folders and separators.
    id: Option<u64>,
}

/// The Specify Layout answer for Go to Related Record's `<Current Layout>`.
/// Layout ids are 32-bit, so no layout has this one.
pub(super) const CURRENT_LAYOUT: u64 = u64::MAX;

/// A list to pick one item from: Specify Layout, Specify Script and
/// Specify Table.
pub(super) struct ListPicker {
    title: String,
    heading: String,
    choices: Vec<Choice>,
    selected: Option<usize>,
}

impl ListPicker {
    fn new(title: &str, heading: &str, choices: Vec<Choice>, current: Option<u64>) -> Self {
        let selected = current.and_then(|id| choices.iter().position(|c| c.id == Some(id)));
        Self {
            title: title.into(),
            heading: heading.into(),
            choices,
            selected,
        }
    }

    /// The file's layouts in display order, folders and separators shown,
    /// each with its table occurrence.
    pub fn layouts(names: Option<&CalcSchema>, current: Option<u64>) -> Self {
        Self::new(
            "Specify Layout",
            "Layouts",
            Self::layout_choices(names),
            current,
        )
    }

    /// Go to Related Record's layouts: `<Current Layout>` (answered as
    /// [`CURRENT_LAYOUT`], selected when `current` is `None`), then the
    /// file's layouts.
    pub fn related_layouts(names: Option<&CalcSchema>, current: Option<u64>) -> Self {
        let mut choices = vec![Choice {
            row: ListRow::item("<Current Layout>"),
            id: Some(CURRENT_LAYOUT),
        }];
        choices.extend(Self::layout_choices(names));
        Self::new(
            "Specify Layout",
            "Layouts",
            choices,
            Some(current.unwrap_or(CURRENT_LAYOUT)),
        )
    }

    /// The file's layouts as list rows: folders and separators shown, each
    /// layout with its table occurrence.
    fn layout_choices(names: Option<&CalcSchema>) -> Vec<Choice> {
        let mut choices = Vec::new();
        let mut depth = 0usize;
        for layout in names.map(|n| n.layouts()).unwrap_or_default() {
            match layout.kind {
                LayoutKind::Folder => {
                    choices.push(Choice {
                        row: ListRow {
                            depth,
                            icon: Some(Glyph::Folder),
                            selectable: false,
                            ..ListRow::item(&layout.name)
                        },
                        id: None,
                    });
                    depth += 1;
                }
                LayoutKind::FolderEnd => depth = depth.saturating_sub(1),
                LayoutKind::Layout if layout.name == "-" => choices.push(Choice {
                    row: ListRow {
                        separator: true,
                        selectable: false,
                        ..ListRow::item("-")
                    },
                    id: None,
                }),
                LayoutKind::Layout => {
                    let detail = layout
                        .occurrence
                        .and_then(|o| names.and_then(|n| n.occurrence(o)))
                        .map_or_else(String::new, |o| o.name.clone());
                    choices.push(Choice {
                        row: ListRow {
                            depth,
                            detail,
                            ..ListRow::item(&layout.name)
                        },
                        id: Some(u64::from(layout.id)),
                    });
                }
            }
        }
        choices
    }

    /// The file's scripts in display order, in their folders.
    pub fn scripts(names: Option<&CalcSchema>, current: Option<u64>) -> Self {
        let mut choices = Vec::new();
        let mut depth = 0usize;
        for script in names.map(|n| n.scripts()).unwrap_or_default() {
            match script.kind {
                ScriptKind::Folder => {
                    choices.push(Choice {
                        row: ListRow {
                            depth,
                            icon: Some(Glyph::Folder),
                            selectable: false,
                            ..ListRow::item(&script.name)
                        },
                        id: None,
                    });
                    depth += 1;
                }
                ScriptKind::FolderEnd => depth = depth.saturating_sub(1),
                ScriptKind::Separator => choices.push(Choice {
                    row: ListRow {
                        separator: true,
                        selectable: false,
                        ..ListRow::item("-")
                    },
                    id: None,
                }),
                ScriptKind::Script => choices.push(Choice {
                    row: ListRow {
                        depth,
                        icon: Some(Glyph::Script),
                        ..ListRow::item(&script.name)
                    },
                    id: Some(script.id),
                }),
            }
        }
        Self::new("Specify Script", "Scripts", choices, current)
    }

    /// The file's table occurrences in display order.
    pub fn occurrences(names: Option<&CalcSchema>, current: Option<u64>) -> Self {
        let choices = names
            .map(|n| n.occurrences_in_order())
            .unwrap_or_default()
            .into_iter()
            .map(|o| Choice {
                row: ListRow::item(&o.name),
                id: Some(o.id),
            })
            .collect();
        Self::new("Specify Table", "Tables", choices, current)
    }

    /// The file's base tables in display order (Truncate Table).
    pub fn base_tables(names: Option<&CalcSchema>, current: Option<u64>) -> Self {
        let choices = names
            .map(|n| n.tables_in_order())
            .unwrap_or_default()
            .into_iter()
            .map(|t| Choice {
                row: ListRow::item(&t.name),
                id: Some(t.id),
            })
            .collect();
        Self::new("Specify Table", "Tables", choices, current)
    }

    fn answer(&self) -> Option<u64> {
        self.choices.get(self.selected?)?.id
    }

    pub fn show(&mut self, ctx: &egui::Context, on_top: bool) -> Option<Picked<u64>> {
        let mut outcome = None;
        let title = self.title.clone();
        let shown = dialog(ctx, "list_picker", &title, 380.0, on_top, |ui| {
            let full = ui.available_width();
            ui.add_space(12.0);
            let (row, _) = ui.allocate_exact_size(vec2(full, 18.0), Sense::hover());
            left_label(
                ui,
                row,
                &self.heading,
                FontId::new(theme::BODY, theme::semibold()),
                theme::TEXT,
            );
            ui.add_space(4.0);
            let (list, _) = ui.allocate_exact_size(vec2(full, 260.0), Sense::hover());
            let rows: Vec<ListRow> = self.choices.iter().map(|c| c.row.clone()).collect();
            let click = list_box(ui, list, &title, &rows, self.selected, true);
            if let Some(i) = click.clicked.or(click.double_clicked) {
                self.selected = Some(i);
            }
            ui.add_space(20.0);
            let answer = self.answer();
            let (cancel, ok) = cancel_ok(ui, answer.is_some());
            if cancel {
                outcome = Some(Picked::Cancel);
            } else if ok || click.double_clicked.is_some() {
                outcome = answer.map(Picked::Ok);
            }
        });
        if outcome.is_none() {
            if shown.escape {
                outcome = Some(Picked::Cancel);
            } else if shown.enter {
                outcome = self.answer().map(Picked::Ok);
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_data::calc::{Locale, ValueType};
    use om_data::host::FieldDef;
    use om_data::meta::ScriptEntry;

    fn schema() -> CalcSchema {
        let mut schema = CalcSchema::new("Contacts", Locale::en_us());
        schema.add_table(0x8002, "Contacts");
        schema.add_field(0x8002, FieldDef::data(1, "First Name", ValueType::Text));
        schema.add_field(0x8002, FieldDef::data(2, "Last Name", ValueType::Text));
        schema.add_occurrence(0xd00002, "Contacts", 0x8002);
        schema.add_layout(3, "Contact Details", Some(0xd00002));
        schema.set_scripts(vec![
            ScriptEntry {
                id: 1,
                name: "Contacts".into(),
                kind: ScriptKind::Folder,
            },
            ScriptEntry {
                id: 2,
                name: "Go to Entry Field".into(),
                kind: ScriptKind::Script,
            },
            ScriptEntry {
                id: 4,
                name: "--".into(),
                kind: ScriptKind::FolderEnd,
            },
            ScriptEntry {
                id: 5,
                name: "-".into(),
                kind: ScriptKind::Separator,
            },
        ]);
        schema
    }

    #[test]
    fn list_pickers_show_folders_and_pick_items_only() {
        let schema = schema();
        let scripts = ListPicker::scripts(Some(&schema), Some(2));
        let rows: Vec<(&str, usize, Option<u64>)> = scripts
            .choices
            .iter()
            .map(|c| (c.row.text.as_str(), c.row.depth, c.id))
            .collect();
        assert_eq!(
            rows,
            [
                ("Contacts", 0, None),
                ("Go to Entry Field", 1, Some(2)),
                ("-", 0, None)
            ]
        );
        assert_eq!(scripts.answer(), Some(2));
        let layouts = ListPicker::layouts(Some(&schema), None);
        assert_eq!(layouts.choices[0].row.detail, "Contacts");
        assert_eq!(layouts.answer(), None);
        // Go to Related Record's list: <Current Layout> first, chosen for
        // no layout.
        let related = ListPicker::related_layouts(Some(&schema), None);
        assert_eq!(related.choices[0].row.text, "<Current Layout>");
        assert_eq!(related.answer(), Some(CURRENT_LAYOUT));
        let related = ListPicker::related_layouts(Some(&schema), Some(3));
        assert_eq!(related.answer(), Some(3));
    }

    #[test]
    fn specify_field_answers_fields_and_variables() {
        let names = Some(Arc::new(schema()));
        let mut picker = SpecifyField::new(names.clone(), None, true);
        assert_eq!(picker.occurrence, Some(0xd00002));
        assert_eq!(picker.answer(), None);
        picker.field = Some(2);
        assert_eq!(
            picker.answer(),
            Some(FieldTarget::Field {
                occurrence: 0xd00002,
                field: 2,
                repetition: 1
            })
        );
        picker.variable = true;
        picker.variable_name = "count".into();
        assert_eq!(picker.answer(), None);
        picker.variable_name = "$$count".into();
        assert_eq!(
            picker.answer(),
            Some(FieldTarget::Variable("$$count".into()))
        );
        let fields = SpecifyField::new(
            names.clone(),
            Some(FieldTarget::Variable("$x".into())),
            false,
        );
        assert!(!fields.variable);
        // A calculated repetition (spec repetition 0) shows 1 and stays 0
        // until the repetition is edited.
        let calculated = FieldTarget::Field {
            occurrence: 0xd00002,
            field: 1,
            repetition: 0,
        };
        let mut picker = SpecifyField::new(names, Some(calculated.clone()), false);
        assert_eq!(picker.repetition, "1");
        assert_eq!(picker.answer(), Some(calculated));
        picker.repetition = "2".into();
        assert!(matches!(
            picker.answer(),
            Some(FieldTarget::Field { repetition: 2, .. })
        ));
    }
}
