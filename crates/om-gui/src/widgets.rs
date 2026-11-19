//! Small custom-painted controls shared by the chrome: toolbar buttons with a
//! caption under the icon, the layout bar's compact buttons and popups, view
//! glyph toggles, menu rows, and dialog buttons.
//!
//! Each control reports itself to accessibility with its visible label, which
//! is also how the tests find it.

use egui::{
    pos2, vec2, Align2, Atom, Button, Color32, CornerRadius, Rect, Response, Role, Sense, Stroke,
    StrokeKind, Ui, WidgetInfo, WidgetText,
};

use crate::icons::{self, Icon};
use crate::theme;

/// Raises each dialog and pop-up once more in the pass after it opened.
///
/// egui raises an area in the pass it becomes visible, but the end of the
/// pass keeps the areas raised in one pass in their old order, and a press
/// raises the area under the pointer in that pass. A dialog reopened by a
/// quick click on another dialog, its press and release in one pass, so
/// kept its old place behind that dialog, out of sight: Manage Database's
/// Options… opened Options for Field or Options for Summary Field only the
/// first time. In the next pass nothing else asks to be raised, so raising
/// the dialog again brings it to the front. A layer counts as open in a
/// pass when it drew something in it.
#[derive(Default)]
pub(crate) struct RaiseOpened {
    opened: std::collections::HashMap<egui::ViewportId, Vec<egui::LayerId>>,
}

impl egui::plugin::Plugin for RaiseOpened {
    fn debug_name(&self) -> &'static str {
        "raise opened dialogs"
    }

    fn on_end_pass(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let viewport = ctx.viewport_id();
        let drawn = |layer: egui::LayerId| {
            ctx.graphics(|g| g.get(layer).is_some_and(|list| !list.is_empty()))
        };
        for layer in self.opened.remove(&viewport).unwrap_or_default() {
            if drawn(layer) {
                ctx.move_to_top(layer);
            }
        }
        let fresh: Vec<egui::LayerId> = ctx.memory(|m| {
            m.layer_ids()
                .filter(|layer| {
                    layer.order == egui::Order::Foreground && !m.areas().visible_last_frame(layer)
                })
                .collect()
        });
        let opened: Vec<egui::LayerId> = fresh.into_iter().filter(|l| drawn(*l)).collect();
        if !opened.is_empty() {
            self.opened.insert(viewport, opened);
            ctx.request_repaint();
        }
    }
}

/// The vertical center of toolbar icons, from the top of the toolbar.
pub(crate) const TOOLBAR_ICON_Y: f32 = 18.0;
/// The vertical center of toolbar captions, from the top of the toolbar.
pub(crate) const TOOLBAR_CAPTION_Y: f32 = 43.0;
/// Toolbar icons are drawn in a square this big.
pub(crate) const TOOLBAR_ICON_SIZE: f32 = 20.0;

/// The width a toolbar caption needs.
pub(crate) fn caption_width(ui: &Ui, caption: &str) -> f32 {
    text_width(ui, caption, theme::small())
}

pub(crate) fn text_width(ui: &Ui, text: &str, font: egui::FontId) -> f32 {
    ui.ctx().fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER)
            .size()
            .x
    })
}

fn icon_color(enabled: bool, pressed: bool) -> Color32 {
    if !enabled {
        theme::DISABLED
    } else if pressed {
        Color32::from_gray(0x45)
    } else {
        theme::ICON
    }
}

/// Give a widget without a visible label (a search field, the record
/// number box) its accessible name.
pub(crate) fn accessible_label(ui: &Ui, id: egui::Id, label: &str) {
    ui.ctx()
        .accesskit_node_builder(id, |node| node.set_label(label.to_owned()));
}

/// A text label placed in `rect`, vertically centered and aligned `align`
/// horizontally. Unlike painted text, it is visible to accessibility.
pub(crate) fn put_label(
    ui: &mut Ui,
    rect: Rect,
    text: &str,
    font: egui::FontId,
    color: Color32,
    align: egui::Align,
) {
    let layout = match align {
        egui::Align::Min => egui::Layout::left_to_right(egui::Align::Center),
        egui::Align::Center => egui::Layout::top_down(egui::Align::Center)
            .with_main_align(egui::Align::Center)
            .with_main_justify(true),
        egui::Align::Max => egui::Layout::right_to_left(egui::Align::Center),
    };
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(layout), |ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(text).font(font).color(color))
                .selectable(false)
                .extend(),
        );
    });
}

/// A status-toolbar button: a glyph above a small caption, with no frame,
/// occupying `column` (the full toolbar height). `chevron` adds the small
/// options arrow FileMaker shows on split and menu buttons.
pub(crate) fn toolbar_button(
    ui: &mut Ui,
    column: Rect,
    icon: Icon,
    caption: &str,
    enabled: bool,
    chevron: bool,
) -> Response {
    // A disabled bar (a set's reduced menus) disables its buttons too.
    let enabled = enabled && ui.is_enabled();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.allocate_rect(column, sense);
    let pressed = response.is_pointer_button_down_on();
    let painter = ui.painter_at(column);
    let color = icon_color(enabled, pressed);
    let icon_center = pos2(column.center().x, column.top() + TOOLBAR_ICON_Y);
    let glyph_center = if chevron {
        icon_center - vec2(6.0, 0.0)
    } else {
        icon_center
    };
    icons::paint(
        &painter,
        Rect::from_center_size(glyph_center, egui::Vec2::splat(TOOLBAR_ICON_SIZE)),
        icon,
        color,
        theme::TOOLBAR_BG,
    );
    if chevron {
        icons::paint(
            &painter,
            Rect::from_center_size(glyph_center + vec2(16.0, 1.0), egui::Vec2::splat(9.0)),
            Icon::ChevronDown,
            color,
            theme::TOOLBAR_BG,
        );
    }
    toolbar_caption(ui, column, caption, enabled);
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, caption));
    response
}

/// A caption centered under a toolbar column.
pub(crate) fn toolbar_caption(ui: &Ui, column: Rect, caption: &str, enabled: bool) {
    let enabled = enabled && ui.is_enabled();
    ui.painter().text(
        pos2(column.center().x, column.top() + TOOLBAR_CAPTION_Y),
        Align2::CENTER_CENTER,
        caption,
        theme::small(),
        if enabled {
            theme::CAPTION
        } else {
            theme::DISABLED
        },
    );
}

/// The options half of a split toolbar button: a hairline then a small
/// chevron, sized to `rect` (icon row only).
pub(crate) fn split_chevron(ui: &mut Ui, rect: Rect, label: &str, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.allocate_rect(rect, sense);
    let painter = ui.painter_at(rect.expand(1.0));
    painter.vline(
        rect.left(),
        rect.center().y - 7.0..=rect.center().y + 7.0,
        Stroke::new(1.0, theme::HAIRLINE),
    );
    icons::paint(
        &painter,
        Rect::from_center_size(rect.center() + vec2(4.0, 1.0), egui::Vec2::splat(9.0)),
        Icon::ChevronDown,
        icon_color(enabled, response.is_pointer_button_down_on()),
        theme::TOOLBAR_BG,
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    response
}

/// A compact rounded push button for the layout and formatting bars
/// ("Preview", "Edit Layout", ...).
pub(crate) fn bar_button(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    let enabled = enabled && ui.is_enabled();
    let width = text_width(ui, label, theme::small()) + 18.0;
    let (rect, response) = ui.allocate_exact_size(
        vec2(width, 16.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let pressed = response.is_pointer_button_down_on();
    let painter = ui.painter();
    let fill = if pressed {
        Color32::from_gray(0xDA)
    } else {
        Color32::from_gray(0xF7)
    };
    painter.rect(
        rect,
        CornerRadius::same(4),
        fill,
        Stroke::new(1.0, Color32::from_gray(0xA6)),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::small(),
        if enabled {
            theme::TEXT
        } else {
            theme::DISABLED
        },
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    response
}

/// A small text label in the layout bar.
pub(crate) fn bar_label(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::small())
                .color(theme::TEXT),
        )
        .selectable(false),
    );
}

/// A hairline vertical divider between layout-bar groups.
pub(crate) fn bar_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 15.0), Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0, theme::DIVIDER),
    );
}

/// A layout-bar popup button: `text` with a chevron, in a rounded outline.
pub(crate) fn popup_button(
    ui: &mut Ui,
    label: &str,
    text: &str,
    width: f32,
    enabled: bool,
) -> Response {
    let enabled = enabled && ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(
        vec2(width, 16.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let painter = ui.painter();
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    let fill = if open || response.is_pointer_button_down_on() {
        Color32::from_gray(0xD0)
    } else {
        theme::LAYOUT_BAR_BG
    };
    painter.rect(
        rect,
        CornerRadius::same(4),
        fill,
        Stroke::new(1.0, theme::POPUP_BORDER),
        StrokeKind::Inside,
    );
    let text_rect = Rect::from_min_max(rect.min + vec2(7.0, 0.0), rect.max - vec2(18.0, 0.0));
    let color = if enabled {
        theme::TEXT
    } else {
        theme::DISABLED
    };
    let galley = ui.ctx().fonts_mut(|fonts| {
        let mut job =
            egui::text::LayoutJob::simple_singleline(text.to_owned(), theme::small(), color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(text_rect.width());
        fonts.layout_job(job)
    });
    painter.galley(
        pos2(text_rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    icons::paint(
        painter,
        Rect::from_center_size(
            pos2(rect.right() - 10.0, rect.center().y),
            egui::Vec2::splat(10.0),
        ),
        Icon::ChevronDown,
        if enabled {
            Color32::from_gray(0x40)
        } else {
            theme::DISABLED
        },
        fill,
    );
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::ComboBox, enabled, label);
        info.current_text_value = Some(text.to_owned());
        info
    });
    response
}

/// One of the View As (or similar) glyph toggles: blue when selected, gray
/// when available, pale when unavailable.
pub(crate) fn glyph_toggle(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    enabled: bool,
) -> Response {
    let enabled = enabled && ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(
        vec2(18.0, 16.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let color = match (enabled, selected) {
        (false, _) => theme::DISABLED,
        (true, true) => theme::SELECTED,
        (true, false) if response.is_pointer_button_down_on() => Color32::from_gray(0x60),
        (true, false) => theme::VIEW_GLYPH,
    };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), egui::Vec2::splat(15.0)),
        icon,
        color,
        theme::LAYOUT_BAR_BG,
    );
    response.widget_info(|| WidgetInfo::selected(Role::Button, enabled, selected, label));
    response
}

/// A two-or-more-segment control (Find mode's Include / Omit). Returns the
/// index of a newly clicked segment.
pub(crate) fn segmented(ui: &mut Ui, segments: &[&str], selected: usize) -> Option<usize> {
    let widths: Vec<f32> = segments
        .iter()
        .map(|segment| text_width(ui, segment, theme::small()) + 14.0)
        .collect();
    let total: f32 = widths.iter().sum();
    let (rect, _) = ui.allocate_exact_size(vec2(total, 16.0), Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(4),
        Color32::from_gray(0xF7),
        Stroke::new(1.0, Color32::from_gray(0xA6)),
        StrokeKind::Inside,
    );
    let mut clicked = None;
    let mut left = rect.left();
    for (index, (segment, width)) in segments.iter().zip(&widths).enumerate() {
        let segment_rect = Rect::from_min_size(pos2(left, rect.top()), vec2(*width, rect.height()));
        left += width;
        let id = ui.scope_id().with(("segment", segment));
        let response = ui.interact(segment_rect, id, Sense::click());
        let is_selected = index == selected;
        if is_selected {
            painter.rect_filled(segment_rect.shrink(1.0), CornerRadius::same(3), theme::ICON);
        } else if index > 0 && index != selected + 1 {
            painter.vline(
                segment_rect.left(),
                segment_rect.y_range().shrink(3.0),
                Stroke::new(1.0, theme::DIVIDER),
            );
        }
        painter.text(
            segment_rect.center(),
            Align2::CENTER_CENTER,
            *segment,
            theme::small(),
            if is_selected {
                Color32::WHITE
            } else {
                theme::TEXT
            },
        );
        response.widget_info(|| WidgetInfo::selected(Role::Button, true, is_selected, *segment));
        if response.clicked() && !is_selected {
            clicked = Some(index);
        }
    }
    clicked
}

/// A FileMaker menu row: a checkmark column, the label, and a right-aligned
/// shortcut, highlighted blue with white text while hovered. Clicking closes
/// the menu.
pub(crate) fn menu_item(
    ui: &mut Ui,
    label: &str,
    shortcut: Option<&str>,
    checked: bool,
    enabled: bool,
) -> Response {
    let font = theme::body();
    let label_width = text_width(ui, label, font.clone());
    let shortcut_width = shortcut.map_or(0.0, |text| text_width(ui, text, font.clone()) + 28.0);
    let desired = vec2(
        CHECK_COLUMN + label_width + shortcut_width + 24.0,
        theme::MENU_ROW_HEIGHT,
    );
    let (rect, response) = ui.allocate_at_least(
        desired,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let highlighted = enabled && (response.hovered() || response.has_focus());
    let painter = ui.painter();
    if highlighted {
        painter.rect_filled(rect, CornerRadius::same(5), theme::HIGHLIGHT);
    }
    let (text, weak) = match (enabled, highlighted) {
        (false, _) => (theme::DISABLED, theme::DISABLED),
        (true, true) => (Color32::WHITE, Color32::WHITE),
        (true, false) => (theme::TEXT, theme::WEAK_TEXT),
    };
    if checked {
        paint_check(ui, rect, text);
    }
    painter.text(
        pos2(rect.left() + CHECK_COLUMN, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font.clone(),
        text,
    );
    if let Some(shortcut) = shortcut {
        painter.text(
            pos2(rect.right() - 8.0, rect.center().y),
            Align2::RIGHT_CENTER,
            shortcut,
            font,
            weak,
        );
    }
    response.widget_info(|| {
        if checked {
            WidgetInfo::selected(Role::Button, enabled, true, label)
        } else {
            WidgetInfo::labeled(Role::Button, enabled, label)
        }
    });
    if response.clicked() {
        ui.close();
    }
    response
}

/// Width of the menu checkmark column.
const CHECK_COLUMN: f32 = 20.0;

fn paint_check(ui: &Ui, row: Rect, color: Color32) {
    let center = pos2(row.left() + 10.0, row.center().y);
    let points = vec![
        center + vec2(-4.0, 0.0),
        center + vec2(-1.2, 3.0),
        center + vec2(4.0, -4.0),
    ];
    ui.painter().add(egui::epaint::PathShape::line(
        points,
        egui::epaint::PathStroke::new(1.6, color),
    ));
}

/// A submenu row with the label aligned to [`menu_item`]s and a chevron.
/// A disabled submenu is drawn like a disabled item and cannot open.
pub(crate) fn submenu<R>(
    ui: &mut Ui,
    label: &str,
    enabled: bool,
    add_contents: impl FnOnce(&mut Ui) -> R,
) {
    if !enabled {
        let width = CHECK_COLUMN + text_width(ui, label, theme::body()) + 48.0;
        let (rect, response) =
            ui.allocate_at_least(vec2(width, theme::MENU_ROW_HEIGHT), Sense::hover());
        ui.painter().text(
            pos2(rect.left() + CHECK_COLUMN, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            theme::body(),
            theme::DISABLED,
        );
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(rect.right() - 12.0, rect.center().y), vec2(12.0, 12.0)),
            Icon::ChevronRight,
            theme::DISABLED,
            theme::MENU_FILL,
        );
        response.widget_info(|| WidgetInfo::labeled(Role::Button, false, label));
        return;
    }
    let chevron = Atom::paint(vec2(12.0, 12.0), |ui: &Ui, args: egui::AtomPaintArgs| {
        icons::paint(
            ui.painter(),
            args.rect,
            Icon::ChevronRight,
            args.fallback_text_color,
            theme::MENU_FILL,
        );
    });
    // Button padding plus the atom gap make up the rest of the check column.
    let spacer = Atom::custom(egui::IdSalt::new("check"), vec2(CHECK_COLUMN - 8.0, 1.0));
    let button = Button::new((spacer, WidgetText::from(label)))
        .right_text(chevron)
        .min_size(vec2(0.0, theme::MENU_ROW_HEIGHT));
    egui::containers::menu::SubMenuButton::from_button(button).ui(ui, add_contents);
}

/// A menu separator with the inset FileMaker uses. It takes no width of its
/// own, so it never widens the menu.
pub(crate) fn menu_separator(ui: &mut Ui) {
    let (rect, _) = ui.allocate_at_least(vec2(0.0, 9.0), Sense::hover());
    ui.painter().hline(
        rect.x_range().shrink(8.0),
        rect.center().y,
        Stroke::new(1.0, Color32::from_gray(0xDA)),
    );
}

/// A dialog push button; `default` buttons are filled blue.
pub(crate) fn dialog_button(ui: &mut Ui, label: &str, default: bool, enabled: bool) -> Response {
    let width = (text_width(ui, label, theme::body()) + 28.0).max(72.0);
    let (rect, response) = ui.allocate_exact_size(
        vec2(width, 22.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let pressed = response.is_pointer_button_down_on();
    let (fill, stroke, text) = match (default && enabled, pressed) {
        (true, false) => (theme::DEFAULT_BUTTON, Stroke::NONE, Color32::WHITE),
        (true, true) => (
            theme::DEFAULT_BUTTON.gamma_multiply(0.85),
            Stroke::NONE,
            Color32::WHITE,
        ),
        (false, false) => (
            Color32::WHITE,
            Stroke::new(1.0, theme::BUTTON_BORDER),
            if enabled {
                theme::TEXT
            } else {
                theme::DISABLED
            },
        ),
        (false, true) => (
            Color32::from_gray(0xE2),
            Stroke::new(1.0, Color32::from_gray(0xA0)),
            theme::TEXT,
        ),
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(5),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::body(),
        text,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(7),
            Stroke::new(2.0, theme::FOCUS.gamma_multiply(0.6)),
            StrokeKind::Outside,
        );
    }
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    response
}
