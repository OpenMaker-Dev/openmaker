//! Pop-up menus (layout-model.md 5.4, display type 2). A pop-up menu shows
//! the field's value, takes no typing, and offers its value list's items
//! in a menu (`om_data::choice`). Choosing one replaces the value with the
//! item's stored value.
//!
//! This module only presents the control and reports what the user did.
//! Like a checkbox or radio button set (`options`), a click first enters
//! the field, after the field the focus leaves has run its exit triggers;
//! the document opens the menu, and applies the item chosen in it, only
//! once the field is entered (document.rs, `OptionWait`).

use egui::{Rect, Sense, Ui, UiBuilder};
use om_data::choice::{self, Choices, MenuCommand};
use om_layout::ResolvedStyle;

use super::options::{Input, Shown};
use super::{color, paint_drop_down_arrow, paint_rich_text};
use crate::{theme, widgets};

/// The id of the menu of the pop-up `id`.
pub(super) fn menu_id(id: egui::Id) -> egui::Id {
    id.with("menu")
}

/// The text the closed menu shows for `value`: the label of the item that
/// stores it, else the value itself (a value no item stores is shown as
/// stored; what FileMaker shows is unknown, layout-model.md 5.4).
fn shown<'c>(value: &'c str, choices: &'c Choices) -> &'c str {
    choices
        .items
        .iter()
        .find(|item| item.value == value && !choices.is_divider(item))
        .map_or(value, |item| &item.label)
}

/// Paint pop-up menu `id` in `rect`, showing `value` (or, when it is
/// empty, the object's `placeholder`) in `text`'s style, with its arrow
/// when `arrow` (the field object's `HasIcon`, `FieldObject::has_icon`:
/// "Include arrow" can be off), and its open menu.
/// `editable`: the control is a focus stop that takes clicks; `enabled`:
/// it takes them now. A click on the closed control is
/// [`Input::Open`]; choosing an item in the open menu is
/// [`Input::Click`] with the item's index, and one of `commands` after the
/// items (“Other…”, “Edit…”) is [`Input::Command`].
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    value: &str,
    placeholder: Option<&str>,
    (choices, commands): (&Choices, &[MenuCommand]),
    text: &ResolvedStyle,
    arrow: bool,
    editable: bool,
    enabled: bool,
    label: &str,
) -> Shown {
    let mut out = Shown {
        set: None,
        input: None,
    };
    if !rect.is_finite() || !rect.intersects(ui.clip_rect()) {
        return out;
    }
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    child.set_clip_rect(ui.clip_rect().intersect(rect));
    let shown_text = shown(value, choices);
    let mut text_rect = rect;
    if arrow {
        text_rect.max.x -= 20.0;
        paint_drop_down_arrow(&child, rect, text.color.map(color));
    }
    let mut style = text.clone();
    style.vertical_align.get_or_insert(1);
    let painted = placeholder.unwrap_or(shown_text);
    let _ = paint_rich_text(&mut child, text_rect, painted, &style, None, None);
    if !editable {
        return out;
    }
    let menu = menu_id(id);
    let open = egui::Popup::is_id_open(ui.ctx(), menu);
    let keep = keeps_focus(ui, id, None);
    let response = child.interact(rect, id, Sense::click());
    if keep {
        response.request_focus();
    }
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(egui::Role::ComboBox, enabled, label);
        info.current_text_value = Some(shown_text.to_owned());
        info
    });
    if enabled && response.clicked() {
        out.input = Some(Input::Open);
    }
    if open {
        let anchor = egui::PopupAnchor::from(&response);
        out.input = match list_with(
            ui,
            menu,
            (anchor, response.layer_id, rect),
            value,
            (choices, commands),
            enabled,
        ) {
            Some(Picked::Item(index)) => Some(Input::Click {
                index,
                shift: false,
            }),
            Some(Picked::Command(command)) => Some(Input::Command(command)),
            None => out.input,
        };
    }
    out.set = Some(response);
    out
}

/// Whether a click on control `id`, or in its open menu (or on `also`, in
/// window points), keeps the focus `id` has. egui takes the focus from a
/// control on a press or a click off it; a press in the control's own menu
/// (an item, a divider) or on `also` does not leave the field. Only a
/// pointer press or release keeps it, so Tab and scripts still move the
/// focus with the pointer resting there.
pub(super) fn keeps_focus(ui: &Ui, id: egui::Id, also: Option<Rect>) -> bool {
    let menu = menu_id(id);
    ui.memory(|memory| memory.has_focus(id))
        && ui.input(|input| input.pointer.any_pressed() || input.pointer.any_released())
        && ui
            .input(|input| input.pointer.interact_pos())
            .is_some_and(|pointer| {
                also.is_some_and(|rect| rect.contains(pointer))
                    || (egui::Popup::is_id_open(ui.ctx(), menu)
                        && ui
                            .memory(|memory| memory.area_rect(menu))
                            .is_some_and(|area| area.contains(pointer)))
            })
}

/// Show the open menu `menu` of the control in `rect` (of `layer`, at
/// `anchor`) offering `choices`, with the item that stores `value`
/// checked, and close it once an item is chosen: that item's index.
/// Shared by pop-up menus and drop-down lists.
#[allow(clippy::too_many_arguments)]
pub(super) fn list(
    ui: &Ui,
    menu: egui::Id,
    anchor: egui::PopupAnchor,
    layer: egui::LayerId,
    rect: Rect,
    value: &str,
    choices: &Choices,
    enabled: bool,
) -> Option<usize> {
    match list_with(
        ui,
        menu,
        (anchor, layer, rect),
        value,
        (choices, &[]),
        enabled,
    )? {
        Picked::Item(index) => Some(index),
        Picked::Command(_) => None,
    }
}

/// What the open menu's user chose: an item, by index, or a command.
enum Picked {
    Item(usize),
    Command(MenuCommand),
}

/// [`list`], with `commands` after the items: “Other…” checked when the
/// value is no item's (`choice::other_checked`), “Edit…” directly after it,
/// as FileMaker Pro 26.0.3 draws them (#869).
fn list_with(
    ui: &Ui,
    menu: egui::Id,
    (anchor, layer, rect): (egui::PopupAnchor, egui::LayerId, Rect),
    value: &str,
    (choices, commands): (&Choices, &[MenuCommand]),
    enabled: bool,
) -> Option<Picked> {
    // At least as wide as the control on screen (the document's zoom).
    let width = ui
        .ctx()
        .layer_transform_to_global(layer)
        .map_or(rect, |transform| transform * rect)
        .width();
    // Not `Popup::from_response`: that makes the menu a child of the
    // control in the accessibility tree, and the control is in the
    // document's zoom layer, whose transform (zoom.rs) would then move
    // the menu's items, already in window points, a second time.
    // Laid out as `Popup::menu` lays out OpenMaker's menus, so items
    // and dividers span the menu; not of its kind, whose arrow keys
    // would move the focus off the field into the menu.
    let popup = egui::Popup::new(menu, ui.ctx().clone(), anchor, layer)
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .gap(0.0)
        .open_memory(None)
        .width(width)
        .style(theme::menu_popup_style)
        .show(|ui| {
            ui.set_min_width((width - ui.spacing().menu_margin.sum().x).max(0.0));
            let mut chosen = None;
            for (index, item) in choices.items.iter().enumerate() {
                if choices.is_divider(item) {
                    widgets::menu_separator(ui);
                } else if widgets::menu_item(ui, &item.label, None, item.value == value, enabled)
                    .clicked()
                {
                    chosen = Some(Picked::Item(index));
                }
            }
            for &command in commands {
                let checked =
                    command == MenuCommand::Other && choice::other_checked(choices, value);
                if widgets::menu_item(ui, command.label(), None, checked, enabled).clicked() {
                    chosen = Some(Picked::Command(command));
                }
            }
            chosen
        });
    let picked = popup.and_then(|popup| popup.inner)?;
    egui::Popup::close_id(ui.ctx(), menu);
    Some(picked)
}
