//! Checkbox and radio button sets (layout-model.md 5.4, display types 3
//! and 4). Each value-list item is a marker followed by its own
//! left-aligned label; the field's text alignment does not center the
//! labels. Items whose stored value is a line of the field's value are
//! checked. Rows use at least their shaped labels' line height, including
//! saved leading. Marker size and the fallback label gap remain inferred
//! where native component geometry is unmeasured (layout-model.md 12, item 8).
//!
//! An editable set is one focus stop. Clicking an item, or Space on the
//! item the arrow keys or an item's first letter picked, changes the value
//! as FileMaker Pro's help describes (layout-model.md 5.4): this module
//! reports what the user did; the document applies it once the set has
//! been entered (document.rs).

use egui::{pos2, vec2, Color32, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, Vec2};
use om_data::host::ValueListEntry;
use om_layout::{Layout, LayoutObject, ResolvedStyle};

use super::layout_cache::LayoutCache;

/// Components `option_item` (7) and `icon` (4) in the normal (1) and
/// checked (7) states, or with the set's focus the In Focus (4) and
/// checked+focus (16) states (layout-model.md 6.3 and 6.4).
const OPTION_ITEM: i32 = 7;
const ICON: i32 = 4;
const NORMAL: i32 = 1;
const CHECKED: i32 = 7;
const FOCUS: i32 = 4;
const CHECKED_FOCUS: i32 = 16;

pub(super) struct Styles {
    frame: ResolvedStyle,
    text: ResolvedStyle,
    item: ResolvedStyle,
    checked_item: ResolvedStyle,
    icon: ResolvedStyle,
    checked_icon: ResolvedStyle,
    label_left: Option<TextOffset>,
}

/// A text component's offset; keep em relative until labels are shaped.
#[derive(Clone, Copy, Default)]
struct TextOffset {
    points: f64,
    em: f64,
}

impl TextOffset {
    fn of(length: &om_layout::Length) -> Option<Self> {
        if length.unit == 7 && matches!(length.auto_value, 0 | 1) {
            length.value.is_finite().then_some(Self {
                em: length.value,
                ..Self::default()
            })
        } else {
            length.points().map(|points| Self {
                points,
                ..Self::default()
            })
        }
    }

    fn at(self, font_size: f32) -> Option<f32> {
        let left = (self.points + self.em * f64::from(font_size)) as f32;
        // An offset before the item origin would paint into the previous
        // column's sensor. Its overlapping geometry is not measured; keep
        // the existing inferred gap rather than choose the wrong value.
        (left.is_finite() && left >= 0.0).then_some(left)
    }
}

/// The set's styles; `focused` when it has the keyboard focus, which its
/// theme's In Focus state styles.
pub(super) fn styles(
    cache: &LayoutCache,
    layout: &Layout,
    object: &LayoutObject,
    conditions: &[usize],
    focused: bool,
) -> Styles {
    let style =
        |component, state| cache.formatted_style(layout, object, conditions, component, state);
    let (normal, checked) = match focused {
        true => (FOCUS, CHECKED_FOCUS),
        false => (NORMAL, CHECKED),
    };
    let mut text = cache.text_style(layout, object, conditions, NORMAL);
    text.text_align = Some(1);
    // Text components place labels with left + margin-left (properties
    // 120/108). Resolve em against the label's effective font, including
    // conditional font changes, rather than the marker's inferred size.
    let offset = |id| {
        layout
            .object_property(object, 3, NORMAL, id)
            .and_then(|p| p.length.as_ref())
    };
    let label_left = offset(120).and_then(TextOffset::of).and_then(|left| {
        let margin = match offset(108) {
            Some(length) => TextOffset::of(length)?,
            None => TextOffset::default(),
        };
        Some(TextOffset {
            points: left.points + margin.points,
            em: left.em + margin.em,
        })
    });
    Styles {
        frame: style(1, normal),
        text,
        label_left,
        item: style(OPTION_ITEM, normal),
        checked_item: style(OPTION_ITEM, checked),
        icon: style(ICON, normal),
        checked_icon: style(ICON, checked),
    }
}

#[derive(Clone, Copy, Debug)]
struct Placement {
    mark: Rect,
    label: Pos2,
    label_fits: bool,
    bounds: Rect,
}

/// Items run down the field, then continue in a new column as wide as the
/// widest item of the previous one.
fn placements(
    rect: Rect,
    text_sizes: &[Vec2],
    style: &ResolvedStyle,
    label_left: Option<f32>,
) -> Vec<Placement> {
    if !rect.is_finite() || !rect.is_positive() {
        return Vec::new();
    }
    let padding = style.padding.map(|p| p.unwrap_or(0.0).max(0.0) as f32);
    let rect = Rect::from_min_max(
        rect.min + vec2(padding[3], padding[0]),
        rect.max - vec2(padding[1], padding[2]),
    );
    if !rect.is_positive() {
        return Vec::new();
    }
    let size = style
        .font_size
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(12.0) as f32;
    let side = (size - 2.0)
        .clamp(6.0, 13.0)
        .min(rect.width())
        .min(rect.height());
    // Labels already carry the resolved font metrics and saved leading.
    // Advancing by the font size alone compresses tall line boxes and
    // places the click sensors above the later labels.
    let line_height = text_sizes
        .iter()
        .map(|text| text.y)
        .filter(|height| height.is_finite())
        .fold(size + 2.0, f32::max);
    let mut pitch = line_height.min(rect.height());
    // A one-row control centers its markers in the field's height.
    if rect.height() < pitch * 2.0 {
        pitch = rect.height();
    }
    let label_left = label_left.unwrap_or(side + 2.0);
    let mut cursor = rect.min;
    let mut column_width = 0.0_f32;
    text_sizes
        .iter()
        .map(|text| {
            if cursor.y > rect.top() && cursor.y + pitch > rect.bottom() + 0.001 {
                cursor = pos2(cursor.x + column_width, rect.top());
                column_width = 0.0;
            }
            let width = side.max(label_left + text.x) + 4.0;
            column_width = column_width.max(width);
            let mark =
                Rect::from_min_size(cursor + vec2(0.0, (pitch - side) / 2.0), Vec2::splat(side));
            let label = cursor + vec2(label_left, (pitch - text.y) / 2.0);
            let placement = Placement {
                mark,
                label,
                // A field as narrow as its marker usually sits beside a
                // separate layout label: a clipped fragment of its own
                // label would only add noise.
                label_fits: label.x + text.x <= rect.right() + 0.001,
                bounds: Rect::from_min_size(cursor, vec2(width, pitch)).intersect(rect),
            };
            cursor.y += pitch;
            placement
        })
        .collect()
}

/// The marker's fill style, border and mark ink. The ink is the icon
/// color, which defaults to a border color (layout-model.md 6.7, rule 4).
fn marker_style(styles: &Styles, checked: bool) -> (ResolvedStyle, Stroke, Color32) {
    let mut item = styles.item.clone();
    let mut icon = styles.icon.clone();
    if checked {
        item.overlay(&styles.checked_item);
        icon.overlay(&styles.checked_icon);
    }
    // Themes style a checked checkbox's border on the icon component,
    // independently of its mark ink (layout-model.md 6.4). The option item
    // and field frame supply only the properties that icon leaves unset.
    item.overlay(&icon);
    let border = item
        .border_color
        .iter()
        .flatten()
        .next()
        .copied()
        .or_else(|| styles.frame.border_color.iter().flatten().next().copied())
        .or(styles.text.color)
        .map(super::color)
        .unwrap_or(Color32::BLACK);
    let width = item
        .border_width
        .iter()
        .flatten()
        .next()
        .copied()
        .or_else(|| styles.frame.border_width.iter().flatten().next().copied())
        .filter(|w| w.is_finite() && *w > 0.0)
        .unwrap_or(1.0) as f32;
    let ink = icon
        .icon_color
        .or(icon.color)
        .map(super::color)
        .unwrap_or(border);
    (item, Stroke::new(width, border), ink)
}

/// Whether `item` is checked: one of the lines of the field's `value`.
/// Lines that are not items are not drawn.
fn checked(value: &str, item: &str) -> bool {
    value.split(['\r', '\n']).any(|line| line == item)
}

/// The value after the user picks the item storing `item` in a set
/// holding `value` (`shift`: a Shift-click); `None` when it stays as it is.
/// A checkbox turned on adds its value at the end and one turned off
/// removes its lines; a radio button replaces the value, and Shift
/// clears a selected one by removing its lines (layout-model.md 5.4).
/// Other lines stay.
pub(super) fn picked(value: &str, item: &str, radio: bool, shift: bool) -> Option<String> {
    if item.is_empty() {
        return None;
    }
    let on = checked(value, item);
    let without = || {
        value
            .split(['\r', '\n'])
            .filter(|line| *line != item)
            .collect::<Vec<_>>()
            .join("\r")
    };
    let changed = if on && (!radio || shift) {
        without()
    } else if radio {
        item.to_owned()
    } else {
        let mut value = value.trim_end_matches(['\r', '\n']).to_owned();
        if !value.is_empty() {
            value.push('\r');
        }
        value.push_str(item);
        value
    };
    (changed != value).then_some(changed)
}

/// What the user did to an editable set this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Input {
    /// Clicked item `index`, with Shift held or not.
    Click { index: usize, shift: bool },
    /// Space on the item the keyboard picked.
    Space(usize),
    /// A click on a closed pop-up menu, which opens its menu once the
    /// field is entered (`popup`).
    Open,
    /// A menu's command after its items: “Other…” or “Edit…”
    /// (`choice::menu_commands`).
    Command(om_data::choice::MenuCommand),
}

/// A painted set: its focus stop (an editable set's) and the user's input.
pub(super) struct Shown {
    pub set: Option<egui::Response>,
    pub input: Option<Input>,
}

/// The item the keyboard picks in the set `id`: the one it last picked,
/// else the first checked item, else the first.
fn cursor(ui: &Ui, id: egui::Id, value: &str, items: &[ValueListEntry]) -> usize {
    ui.data(|d| d.get_temp::<usize>(id))
        .filter(|index| *index < items.len())
        .or_else(|| items.iter().position(|item| checked(value, &item.value)))
        .unwrap_or(0)
}

/// A key typed into the focused set `id`: an item's first letter picks
/// the next item starting with it; Space returns the picked item, whose
/// value it enters.
pub(super) fn key(
    ui: &Ui,
    id: egui::Id,
    value: &str,
    items: &[ValueListEntry],
    text: &str,
) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    let at = cursor(ui, id, value, items);
    if text == " " {
        return Some(at);
    }
    let first = text.chars().next()?.to_lowercase().collect::<String>();
    let next = (1..=items.len())
        .map(|step| (at + step) % items.len())
        .find(|&index| items[index].label.to_lowercase().starts_with(&first))?;
    ui.data_mut(|d| d.insert_temp(id, next));
    None
}

/// The arrow keys move the focused set's picked item; Space and typed
/// letters are taken from the frame's input for the set (`key`).
fn keyboard(ui: &Ui, id: egui::Id, value: &str, items: &[ValueListEntry]) -> Option<usize> {
    let (step, typed) = ui.input_mut(|input| {
        let forward = input.consume_key(Modifiers::NONE, Key::ArrowDown)
            | input.consume_key(Modifiers::NONE, Key::ArrowRight);
        let back = input.consume_key(Modifiers::NONE, Key::ArrowUp)
            | input.consume_key(Modifiers::NONE, Key::ArrowLeft);
        let space = input.consume_key(Modifiers::NONE, Key::Space);
        let mut typed = Vec::new();
        input.events.retain(|event| match event {
            egui::Event::Text(text) => {
                if text != " " {
                    typed.push(text.clone());
                }
                false
            }
            _ => true,
        });
        if space {
            typed.push(" ".to_owned());
        }
        (i32::from(forward) - i32::from(back), typed)
    });
    if items.is_empty() {
        return None;
    }
    if step != 0 {
        let at = cursor(ui, id, value, items) as i32;
        let next = (at + step).rem_euclid(items.len() as i32) as usize;
        ui.data_mut(|d| d.insert_temp(id, next));
    }
    // In typing order: a letter picks an item before a later Space.
    let mut entered = None;
    for text in &typed {
        if let Some(index) = key(ui, id, value, items, text) {
            entered = Some(index);
        }
    }
    entered
}

/// `id` identifies the set (its object, record and portal row): each
/// item's sensor derives its own id from it. `editable`: the set is a focus
/// stop whose items take clicks (the caller has checked that the user may
/// change the field here); otherwise it only shows the value. `enabled`:
/// an editable set takes input now (not while its record saves). `label`
/// names the set for accessibility.
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    ui: &mut Ui,
    id: egui::Id,
    rect: Rect,
    value: &str,
    items: &[ValueListEntry],
    radio: bool,
    styles: &Styles,
    editable: bool,
    enabled: bool,
    label: &str,
) -> Shown {
    let mut shown = Shown {
        set: None,
        input: None,
    };
    if !rect.is_finite() || !rect.intersects(ui.clip_rect()) {
        return shown;
    }
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    child.set_clip_rect(ui.clip_rect().intersect(rect));
    // The set takes the focus and its keys: arrows pick items rather
    // than move the focus.
    if editable {
        let set = child.interact(rect, id, Sense::click());
        set.widget_info(|| {
            let role = if radio {
                egui::Role::RadioGroup
            } else {
                egui::Role::Group
            };
            egui::WidgetInfo::labeled(role, enabled, label)
        });
        if set.has_focus() && enabled {
            child.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
            shown.input = keyboard(&child, id, value, items).map(Input::Space);
        }
        shown.set = Some(set);
    }
    let scale = child
        .ctx()
        .layer_transform_to_global(child.layer_id())
        .map_or(1.0, |t| t.scaling);
    let labels: Vec<_> = items
        .iter()
        .map(|item| {
            child.fonts_mut(|fonts| {
                let job = super::rich_text::layout_job(
                    fonts,
                    &item.label,
                    &styles.text,
                    None,
                    f32::INFINITY,
                );
                super::scaled_text::layout_galley(fonts, job, scale)
            })
        })
        .collect();
    let sizes: Vec<_> = labels.iter().map(|label| label.size()).collect();
    // Use the shaper's effective font size, including its fallback/clamp;
    // the marker's inferred dimensions do not define a CSS em.
    let label_left = labels
        .iter()
        .find_map(|label| label.job.sections.first())
        .and_then(|section| styles.label_left?.at(section.format.font_id.size));
    let text_color = styles
        .text
        .color
        .map(super::color)
        .unwrap_or(Color32::BLACK);
    let focused = shown.set.as_ref().is_some_and(egui::Response::has_focus);
    let picked = focused.then(|| cursor(&child, id, value, items));
    for (index, ((item, galley), placement)) in items
        .iter()
        .zip(labels)
        .zip(placements(rect, &sizes, &styles.text, label_left))
        .enumerate()
    {
        if !placement.bounds.is_positive() {
            continue;
        }
        let selected = checked(value, &item.value);
        let (item_style, border, ink) = marker_style(styles, selected);
        let mark = placement.mark;
        let painter = child.painter();
        if radio {
            if let Some(fill) = item_style.background.map(super::color) {
                painter.circle_filled(mark.center(), mark.width() / 2.0, fill);
            }
            painter.circle_stroke(mark.center(), mark.width() / 2.0, border);
            if selected {
                painter.circle_filled(mark.center(), (mark.width() / 2.0 - 2.0).max(0.5), ink);
            }
        } else {
            if let Some(fill) = item_style.background.map(super::color) {
                painter.rect_filled(mark, 0, fill);
            }
            painter.rect_stroke(mark, 0, border, egui::StrokeKind::Inside);
            if selected {
                let inner = mark.shrink(1.0);
                let stroke = Stroke::new(0.8, ink);
                painter.line_segment([inner.left_top(), inner.right_bottom()], stroke);
                painter.line_segment([inner.left_bottom(), inner.right_top()], stroke);
            }
        }
        if placement.label_fits {
            painter.galley(placement.label, galley, text_color);
        }
        if picked == Some(index) {
            painter.rect_stroke(
                placement.bounds.shrink(0.5),
                2,
                Stroke::new(1.5, crate::theme::FOCUS),
                egui::StrokeKind::Inside,
            );
        }
        // An item takes clicks without being a focus stop of its own: the
        // set is.
        let sense = if editable {
            Sense::CLICK
        } else {
            Sense::hover()
        };
        let response = child.interact(placement.bounds, id.with(index), sense);
        response.widget_info(|| {
            let role = if radio {
                egui::Role::RadioButton
            } else {
                egui::Role::CheckBox
            };
            egui::WidgetInfo::selected(role, editable && enabled, selected, &item.label)
        });
        if editable && enabled && response.clicked() {
            let shift = child.input(|input| input.modifiers.shift);
            child.data_mut(|d| d.insert_temp(id, index));
            shown.input = Some(Input::Click { index, shift });
        }
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> ResolvedStyle {
        ResolvedStyle {
            font_size: Some(9.0),
            ..Default::default()
        }
    }

    #[test]
    fn one_row_items_keep_labels_after_their_markers() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(196.0, 13.0));
        for offset in [
            None,
            TextOffset {
                points: -4.0,
                ..Default::default()
            }
            .at(9.0),
            TextOffset {
                em: -0.5,
                ..Default::default()
            }
            .at(9.0),
        ] {
            let placed = placements(
                rect,
                &[vec2(72.0, 9.0), vec2(23.0, 9.0), vec2(43.0, 9.0)],
                &text(),
                offset,
            );
            assert_eq!(placed.len(), 3);
            assert!(placed
                .iter()
                .all(|p| p.label_fits && rect.contains_rect(p.mark)));
            assert_eq!(placed[0].mark.center().y, rect.center().y);
            assert!(placed[0].label.x >= placed[0].mark.right());
            assert!(placed[1].mark.left() > placed[0].label.x + 72.0);
            assert!(placed[2].mark.left() > placed[1].label.x + 23.0);
        }
    }

    #[test]
    fn marker_only_fields_do_not_draw_clipped_labels() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(12.0, 11.0));
        let placed = placements(rect, &[vec2(6.0, 9.0)], &text(), None);
        assert!(rect.contains_rect(placed[0].mark));
        assert!(!placed[0].label_fits);
        assert_eq!(placed[0].mark.left(), 0.0);
    }

    #[test]
    fn wrapped_columns_start_after_the_widest_previous_label() {
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(200.0, 25.0));
        let placed = placements(
            rect,
            &[vec2(70.0, 9.0), vec2(10.0, 9.0), vec2(20.0, 9.0)],
            &text(),
            None,
        );
        assert_eq!(placed[1].mark.left(), placed[0].mark.left());
        assert!(placed[2].mark.left() > placed[0].label.x + 70.0);
        assert_eq!(placed[2].mark.top(), placed[0].mark.top());
    }

    #[test]
    fn checkboxes_add_at_the_end_and_remove_only_their_own_lines() {
        // Turned on: added after what the field holds, in click order.
        assert_eq!(picked("", "Red", false, false).as_deref(), Some("Red"));
        assert_eq!(
            picked("Blue\rMauve", "Red", false, false).as_deref(),
            Some("Blue\rMauve\rRed")
        );
        assert_eq!(
            picked("Blue\r", "Red", false, false).as_deref(),
            Some("Blue\rRed")
        );
        // Turned off: its lines go; a line outside the list stays.
        assert_eq!(
            picked("Red\rMauve\rRed", "Red", false, false).as_deref(),
            Some("Mauve")
        );
        assert_eq!(picked("Red", "Red", false, true).as_deref(), Some(""));
        // A line that only contains the value is not the value.
        assert_eq!(
            picked("Red light", "Red", false, false).as_deref(),
            Some("Red light\rRed")
        );
        assert_eq!(picked("", "", false, false), None);
    }

    #[test]
    fn radio_buttons_replace_and_shift_clears_the_selected_one() {
        assert_eq!(
            picked("Small", "Large", true, false).as_deref(),
            Some("Large")
        );
        assert_eq!(picked("Large", "Large", true, false), None);
        // Several lines become the one chosen.
        assert_eq!(
            picked("Large\rOdd", "Large", true, false).as_deref(),
            Some("Large")
        );
        // Shift-click clears a selected button; a value outside the list
        // stays (FileMaker Pro Help: it "may not clear the field").
        assert_eq!(picked("Large", "Large", true, true).as_deref(), Some(""));
        assert_eq!(
            picked("Large\rOdd", "Large", true, true).as_deref(),
            Some("Odd")
        );
        assert_eq!(
            picked("Small", "Large", true, true).as_deref(),
            Some("Large")
        );
    }

    #[test]
    fn several_lines_check_several_items_and_partial_text_none() {
        let value = "Red\rBlue\nGreen light";
        assert!(checked(value, "Red"));
        assert!(checked(value, "Blue"));
        assert!(checked(value, "Green light"));
        assert!(!checked(value, "Green"));
        assert!(!checked(value, "Re"));
        assert!(!checked("", "Red"));
    }

    #[test]
    fn marker_border_and_checked_icon_keep_distinct_file_colors() {
        let purple = om_layout::Color {
            red: 0.3,
            green: 0.0,
            blue: 0.9,
            alpha: 1.0,
        };
        let black = om_layout::Color {
            red: 0.0,
            green: 0.0,
            blue: 0.0,
            alpha: 1.0,
        };
        let styles = Styles {
            frame: ResolvedStyle {
                border_color: [Some(purple); 4],
                border_width: [Some(1.0); 4],
                ..Default::default()
            },
            text: text(),
            label_left: None,
            item: Default::default(),
            checked_item: Default::default(),
            icon: Default::default(),
            checked_icon: ResolvedStyle {
                icon_color: Some(black),
                ..Default::default()
            },
        };
        let (_, border, ink) = marker_style(&styles, true);
        assert_eq!(border.color, super::super::color(purple));
        assert_eq!(ink, Color32::BLACK);
        // Unchecked, the icon color defaults to the border's.
        let (_, _, ink) = marker_style(&styles, false);
        assert_eq!(ink, super::super::color(purple));
        let mut styles = styles;
        styles.checked_icon.border_color = [Some(black); 4];
        styles.checked_icon.border_width = [Some(2.0); 4];
        let (_, border, ink) = marker_style(&styles, true);
        assert_eq!(border.color, Color32::BLACK);
        assert_eq!(border.width, 2.0);
        assert_eq!(ink, Color32::BLACK);
        let (_, normal_border, _) = marker_style(&styles, false);
        assert_eq!(normal_border.color, super::super::color(purple));
    }

    #[test]
    fn item_labels_ignore_the_fields_center_alignment() {
        let layout = Layout::default();
        let object = LayoutObject {
            style: Some(om_layout::style::ObjectStyle {
                local_styles: Some(om_layout::style::StyleSet {
                    components: vec![om_layout::style::ComponentStyle {
                        component: 1,
                        state: 1,
                        properties: vec![om_layout::Property {
                            id: 74,
                            int: Some(2),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(layout.object_style(&object, 1, 1).text_align, Some(2));
        let styles = styles(&LayoutCache::default(), &layout, &object, &[], false);
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.fonts_mut(|fonts| {
                let job = super::super::rich_text::layout_job(
                    fonts,
                    "Second option",
                    &styles.text,
                    None,
                    f32::INFINITY,
                );
                assert_eq!(job.halign, egui::Align::Min);
            });
        })
        .drop_without_applying_deltas();
    }
}
