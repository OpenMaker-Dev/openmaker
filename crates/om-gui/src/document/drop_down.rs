//! Drop-down lists (layout-model.md 5.4, display type 1) in Browse. A
//! drop-down list is the field's edit box, with its typing, draft and
//! triggers (document.rs), and a list of its value list's items
//! (`om_data::choice`). Choosing an item replaces the field's value with
//! the item's stored value.
//!
//! With "Include arrow" (`HasIcon`), an arrow at the right edge shows and
//! hides the list. Without it, the list opens when the field is entered,
//! as Claris's help describes a drop-down list ("Setting up a field
//! control"; not measured on FileMaker Pro), and Escape closes it.
//!
//! This module only presents the arrow and the list and reports what the
//! user did. As for a pop-up menu (`popup`), the document opens the list,
//! and applies the item chosen in it, only once the field is entered
//! (document.rs, `OptionWait`).

use egui::{pos2, EventFilter, Rect, Sense, Ui};
use om_data::choice::{Choices, DROP_DOWN_LIST};
use om_layout::LayoutObject;

use super::{popup, to_window};

/// The width of the arrow at the control's right edge, which its text
/// leaves free.
pub(super) const ARROW_WIDTH: f32 = 20.0;

/// Whether `object` is a drop-down list, which offers its list: display
/// type 1.
pub(super) fn offers_list(object: &LayoutObject) -> bool {
    object
        .field_obj
        .as_ref()
        .is_some_and(|field| field.display_type == DROP_DOWN_LIST)
}

/// Whether drop-down list `object` shows its arrow ("Include arrow").
pub(super) fn has_arrow(object: &LayoutObject) -> bool {
    object
        .field_obj
        .as_ref()
        .is_some_and(|field| field.has_icon())
}

/// Remember that arrowless drop-down list `id` (its editor's id) was
/// entered while its items were still being read: its list opens once
/// they are, while the field keeps the focus (`opens_once_read`).
pub(super) fn open_once_read(ui: &Ui, id: egui::Id) {
    ui.ctx()
        .data_mut(|data| data.insert_temp(id.with("open once read"), true));
}

/// Whether list `id`, entered while its items were being read, opens now:
/// they are read (`offered`) and its editor still has the focus. The wait
/// ends as the list opens or the field is left.
pub(super) fn opens_once_read(ui: &Ui, id: egui::Id, offered: bool) -> bool {
    let key = id.with("open once read");
    if !ui
        .ctx()
        .data(|data| data.get_temp::<bool>(key))
        .unwrap_or(false)
    {
        return false;
    }
    let focused = ui.memory(|memory| memory.has_focus(id));
    if offered || !focused {
        ui.ctx().data_mut(|data| data.remove::<bool>(key));
    }
    offered && focused
}

/// The arrow of the control in `rect`.
pub(super) fn arrow(rect: Rect) -> Rect {
    Rect::from_min_max(
        pos2((rect.right() - ARROW_WIDTH).max(rect.left()), rect.top()),
        rect.max,
    )
}

/// The keys the editor of a drop-down list keeps from egui's keyboard
/// focus: a text editor's, and Escape while the list is open, so that
/// Escape closes the list and leaves the field, its focus and the record's
/// draft as they were.
pub(super) fn editor_keys(open: bool) -> EventFilter {
    EventFilter {
        escape: open,
        ..egui::TextEdit::singleline(&mut String::new()).get_event_filter()
    }
}

/// [`editor_keys`] for a field of the current record in Browse, which also
/// keeps Tab: the document moves the focus through the layout's tab order
/// instead of egui's widget order (#848).
pub(super) fn browse_keys(open: bool) -> EventFilter {
    EventFilter {
        tab: true,
        ..editor_keys(open)
    }
}

/// Auto-complete (#870): when this frame's typing (a text input, the
/// insertion point then at the end with nothing selected) left `text` the
/// beginning of an item of `choices` (`choice::completion`), the rest of
/// the item follows what was typed, selected, as FileMaker Pro 26.0.3
/// completes a drop-down list with "Auto-complete using value list"
/// (layout-model.md 5.4): the typed letters keep their case ("a" shows
/// "acme"). The list stays closed; the editor's own keys do the rest
/// (Backspace removes the selection, Tab and Return keep the text). `id`
/// is the editor's. The item completed to, whose own text the field keeps
/// when it is left (`FieldBuffer::accept_completion`).
pub(super) fn complete(
    ui: &Ui,
    id: egui::Id,
    text: &mut String,
    choices: &Choices,
) -> Option<String> {
    let typed = ui.input(|input| {
        input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Text(text) if !text.is_empty()))
    });
    if !typed {
        return None;
    }
    let mut state = egui::TextEdit::load_state(ui.ctx(), id)?;
    let length = text.chars().count();
    let at_end = state
        .cursor
        .char_range()
        .is_some_and(|range| range.is_empty() && range.primary.index.0 == length);
    if !at_end {
        return None;
    }
    let item = om_data::choice::completion(choices, text)?;
    text.extend(item.value.chars().skip(length));
    let whole = text.chars().count();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(length),
            egui::text::CCursor::new(whole),
        )));
    state.store(ui.ctx(), id);
    // No list while completing, as in Pro: one open closes, and one
    // waiting to open as the field was entered does not (Tab then moves
    // on, which an open list would keep).
    egui::Popup::close_id(ui.ctx(), popup::menu_id(id));
    ui.ctx()
        .data_mut(|data| data.remove::<bool>(id.with("open once read")));
    ui.ctx().request_repaint();
    Some(item.value.clone())
}

/// Whether drop-down list `id`'s list is open.
pub(super) fn is_open(ui: &Ui, id: egui::Id) -> bool {
    egui::Popup::is_id_open(ui.ctx(), popup::menu_id(id))
}

/// Whether a press on the arrow of the control in `rect` (when it has
/// one), or in its open list, keeps the focus its editor `id` has
/// (`popup::keeps_focus`). Checked before the editor is shown, which gives
/// up the focus on a press off it.
pub(super) fn keeps_focus(ui: &Ui, id: egui::Id, rect: Rect, with_arrow: bool) -> bool {
    let arrow = with_arrow.then(|| to_window(ui, arrow(rect).intersect(ui.clip_rect())));
    popup::keeps_focus(ui, id, arrow)
}

/// What the user did to a drop-down list's arrow or list (or to a
/// drop-down calendar's icon or calendar, `calendar`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Input {
    /// A click on the arrow while the list is closed.
    Open,
    /// A click on the arrow while the list is open.
    Close,
    /// The item at this index was chosen in the open list.
    Choose(usize),
    /// This text is stored: a day picked in a calendar.
    Set(String),
}

/// The arrow (when `with_arrow`) and the open list of drop-down list `id`
/// (its editor's id) in `rect`, the editor holding `value`. The list is
/// shown only when `offered` (the user may choose in the control and it
/// has items) and takes clicks only when `enabled`; `refusal`, when it is
/// not offered, is shown over the arrow.
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    ui: &Ui,
    id: egui::Id,
    rect: Rect,
    with_arrow: bool,
    value: &str,
    choices: &Choices,
    offered: bool,
    enabled: bool,
    refusal: Option<&str>,
) -> Option<Input> {
    let menu = popup::menu_id(id);
    let open = is_open(ui, id);
    let mut input = None;
    if with_arrow {
        // Not focusable: the arrow is no stop of its own for Tab.
        let response = ui.interact(arrow(rect), id.with("arrow"), Sense::CLICK);
        let clicked = response.clicked();
        if let Some(refusal) = refusal {
            response.on_hover_text(refusal);
        }
        input = (enabled && offered && clicked).then_some(if open {
            Input::Close
        } else {
            Input::Open
        });
    }
    if open && offered {
        let anchor = egui::PopupAnchor::ParentRect(to_window(ui, rect.intersect(ui.clip_rect())));
        if let Some(index) = popup::list(
            ui,
            menu,
            anchor,
            ui.layer_id(),
            rect,
            value,
            choices,
            enabled,
        ) {
            input = Some(Input::Choose(index));
        }
    }
    input
}
