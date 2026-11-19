//! Drop-down calendars (layout-model.md 5.4, display type 6) in Browse. A
//! drop-down calendar is a date field's edit box, with its typing, draft
//! and triggers (document.rs), and a calendar to pick a day from. Picking
//! a day replaces the field's value with that date in the file's date
//! format (`Date::format`); the change then goes the way a typed date goes.
//!
//! With "Include icon to show and hide calendar" (`HasIcon`), the icon at
//! the right edge shows and hides the calendar. Without it, the calendar
//! opens when the field is entered, as a drop-down list without its arrow
//! does (`drop_down`), and Escape closes it. Claris's help describes both
//! ("Setting up a field control"); neither, nor the calendar's look, is
//! measured on FileMaker Pro.
//!
//! The calendar opens on the month of the field's date, or of today when
//! the field holds no date; its arrows go back and forward a month, its
//! year stepper a year, and "Today: 11/13/26" at its foot picks today.
//! Today is a filled blue cell and the field's own date is outlined.
//! Weeks start on Sunday. The look follows a comparison with FileMaker
//! Pro 26.0.3 (#536): compact rows with hairlines between weeks. This module presents the icon
//! and the calendar and reports what the user did; as for a drop-down
//! list, the document opens the calendar, and applies the day picked in
//! it, only once the field is entered (document.rs, `OptionWait`).

use egui::{vec2, Align2, Color32, FontId, Rect, Sense, Ui};
use om_data::calc::{Ctx, Date};
use om_layout::LayoutObject;

use super::drop_down::{self, Input};
use super::{popup, to_window};
use crate::icons::{self, Icon};
use crate::theme;

/// `FieldLayoutObject.displayType` of a drop-down calendar.
pub(super) const DROP_DOWN_CALENDAR: u32 = 6;

/// A day's cell: its width and the height of a week's row, compact as
/// FileMaker Pro's (#536).
const DAY: f32 = 26.0;
const ROW: f32 = 18.0;
/// The month and year row, and its year stepper's width and half height.
const HEADER: f32 = 22.0;
const STEPPER: f32 = 14.0;
const STEPPER_HALF: f32 = 9.0;
/// The days' font size.
const DAY_FONT: f32 = 12.0;

/// Whether `object` is a drop-down calendar.
pub(super) fn is_calendar(object: &LayoutObject) -> bool {
    object
        .field_obj
        .as_ref()
        .is_some_and(|field| field.display_type == DROP_DOWN_CALENDAR)
}

/// The icon (when `with_icon`) and the open calendar of drop-down calendar
/// `id` (its editor's id) in `rect`, the editor holding `value`, as
/// `drop_down::show` presents a list: what the user did, a picked day as
/// `Input::Set` of its text in the file's date format. The calendar is
/// shown only when `offered` and takes clicks only when `enabled`;
/// `refusal`, when it is not offered, is shown over the icon.
#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    ui: &Ui,
    id: egui::Id,
    rect: Rect,
    with_icon: bool,
    value: &str,
    cx: &Ctx,
    offered: bool,
    enabled: bool,
    refusal: Option<&str>,
) -> Option<Input> {
    let menu = popup::menu_id(id);
    let month = menu.with("month");
    let open = egui::Popup::is_id_open(ui.ctx(), menu);
    let mut input = None;
    if with_icon {
        // Not focusable: the icon is no stop of its own for Tab.
        let response = ui.interact(drop_down::arrow(rect), id.with("arrow"), Sense::CLICK);
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
    if !(open && offered) {
        // The next opening starts again from the field's date.
        ui.ctx().data_mut(|data| data.remove::<(i64, u32)>(month));
        return input;
    }
    let date = match Date::parse(value.trim(), cx.locale, cx.today) {
        Ok(Some(date)) => Some(date),
        _ => None,
    };
    let shown = ui.ctx().data_mut(|data| {
        *data.get_temp_mut_or_insert_with(month, || {
            let (year, month, _) = date.unwrap_or(cx.today).ymd();
            (year, month)
        })
    });
    let anchor = egui::PopupAnchor::ParentRect(to_window(ui, rect.intersect(ui.clip_rect())));
    // As `popup::list`: not `Popup::from_response`, which would move the
    // calendar, already in window points, by the document's zoom again.
    // A click on its arrows keeps it open; one outside it closes it.
    let popup = egui::Popup::new(menu, ui.ctx().clone(), anchor, ui.layer_id())
        .gap(0.0)
        .open_memory(None)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .style(theme::menu_popup_style)
        .show(|ui| calendar(ui, shown, date, cx, enabled));
    let (step, picked) = popup.map(|popup| popup.inner).unwrap_or_default();
    if let Some(shown) = step {
        ui.ctx().data_mut(|data| data.insert_temp(month, shown));
    }
    if let Some(picked) = picked {
        egui::Popup::close_id(ui.ctx(), menu);
        input = Some(Input::Set(picked.format(cx.locale)));
    }
    input
}

/// The calendar of month `shown` (year, month), `date` (the field's)
/// marked: the month it steps to, and the day picked. Compact, as
/// FileMaker Pro 26 draws it (#536): month arrows and a year stepper over
/// the weekdays, one hairline between weeks, and "Today:" with today's
/// short date at the foot.
fn calendar(
    ui: &mut Ui,
    shown: (i64, u32),
    date: Option<Date>,
    cx: &Ctx,
    enabled: bool,
) -> (Option<(i64, u32)>, Option<Date>) {
    let (year, month) = shown;
    let mut step = None;
    let mut picked = None;
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    ui.set_width(7.0 * DAY);
    ui.horizontal(|ui| {
        if arrow(ui, Icon::ChevronLeft, "Previous Month", enabled).clicked() {
            step = months(shown, -1);
        }
        let name = &cx.locale.month_names[month as usize - 1];
        let (rect, _) = ui.allocate_exact_size(vec2(3.0 * DAY, HEADER), Sense::hover());
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            name,
            FontId::new(theme::BODY, theme::semibold()),
            theme::TEXT,
        );
        if arrow(ui, Icon::ChevronRight, "Next Month", enabled).clicked() {
            step = months(shown, 1);
        }
        let (rect, _) = ui.allocate_exact_size(vec2(2.0 * DAY - STEPPER, HEADER), Sense::hover());
        ui.painter().text(
            rect.right_center() - vec2(2.0, 0.0),
            Align2::RIGHT_CENTER,
            year.to_string(),
            FontId::new(theme::BODY, theme::semibold()),
            theme::TEXT,
        );
        if let Some(by) = year_stepper(ui, enabled) {
            step = months(shown, by * 12);
        }
    });
    ui.horizontal(|ui| {
        for name in &cx.locale.day_abbreviations {
            let (rect, _) = ui.allocate_exact_size(vec2(DAY, ROW), Sense::hover());
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                name,
                FontId::proportional(theme::SMALL),
                theme::CAPTION,
            );
        }
    });
    // Six weeks from the Sunday on or before the first of the month, a
    // hairline above each.
    let first = Date::from_ymd(year, month, 1);
    let start = first.map(|first| first.day_number() - i64::from(first.weekday()) + 1);
    for week in 0..6i64 {
        let top = ui.cursor().top();
        ui.painter().hline(
            ui.max_rect().x_range(),
            top,
            egui::Stroke::new(1.0, theme::HAIRLINE),
        );
        ui.horizontal(|ui| {
            for weekday in 0..7i64 {
                let day = start.and_then(|start| Date::from_day(start + week * 7 + weekday));
                if let Some(day) = day {
                    if cell(ui, day, month, date, cx, enabled).clicked() {
                        picked = Some(day);
                    }
                } else {
                    ui.allocate_exact_size(vec2(DAY, ROW), Sense::hover());
                }
            }
        });
    }
    let top = ui.cursor().top();
    ui.painter().hline(
        ui.max_rect().x_range(),
        top,
        egui::Stroke::new(1.0, theme::HAIRLINE),
    );
    ui.add_space(3.0);
    ui.vertical_centered(|ui| {
        let label = format!("Today: {}", short_date(cx.today, cx));
        let today = ui.add_enabled(
            enabled,
            egui::Button::new(egui::RichText::new(label).size(theme::SMALL)).frame(false),
        );
        if today.clicked() {
            picked = Some(cx.today);
        }
    });
    (step, picked)
}

/// `date` as the calendar's foot shows it: the file's date order and
/// separator, with a two-digit year (`11/13/26`).
fn short_date(date: Date, cx: &Ctx) -> String {
    use om_data::calc::locale::DateOrder;
    let locale = cx.locale;
    let (year, month, day) = date.ymd();
    let year = format!("{:02}", year.rem_euclid(100));
    let pad = |n: u32, padded: bool| {
        if padded {
            format!("{n:02}")
        } else {
            n.to_string()
        }
    };
    let (month, day) = (pad(month, locale.pad_month), pad(day, locale.pad_day));
    let sep = locale.date_separator;
    match locale.date_order {
        DateOrder::MonthDayYear => format!("{month}{sep}{day}{sep}{year}"),
        DateOrder::DayMonthYear => format!("{day}{sep}{month}{sep}{year}"),
        DateOrder::YearMonthDay => format!("{year}{sep}{month}{sep}{day}"),
    }
}

/// The year stepper beside the year: up for the next year, down for the
/// previous one.
fn year_stepper(ui: &mut Ui, enabled: bool) -> Option<i64> {
    let mut by = None;
    ui.vertical(|ui| {
        ui.add_space((HEADER - 2.0 * STEPPER_HALF) / 2.0);
        for (icon, label, step) in [
            (Icon::ChevronUp, "Next Year", 1),
            (Icon::ChevronDown, "Previous Year", -1),
        ] {
            let sense = if enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let (rect, response) = ui.allocate_exact_size(vec2(STEPPER, STEPPER_HALF), sense);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
            if response.hovered() && enabled {
                ui.painter().rect_filled(rect, 2.0, theme::HAIRLINE);
            }
            icons::paint(
                ui.painter(),
                Rect::from_center_size(rect.center(), vec2(9.0, 9.0)),
                icon,
                if enabled {
                    theme::TEXT
                } else {
                    theme::DISABLED
                },
                Color32::TRANSPARENT,
            );
            if response.clicked() {
                by = Some(step);
            }
        }
    });
    by
}

/// The month `by` months from `shown`, within the years dates hold
/// (1 to 4000).
fn months(shown: (i64, u32), by: i64) -> Option<(i64, u32)> {
    let index = shown.0 * 12 + i64::from(shown.1) - 1 + by;
    let year = index.div_euclid(12);
    let month = u32::try_from(index.rem_euclid(12)).ok()? + 1;
    (1..=4000).contains(&year).then_some((year, month))
}

/// A month arrow of the calendar's title row.
fn arrow(ui: &mut Ui, icon: Icon, label: &str, enabled: bool) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(DAY, HEADER), sense);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if response.hovered() && enabled {
        ui.painter()
            .rect_filled(rect.shrink(3.0), 4.0, theme::HAIRLINE);
    }
    let color = if enabled {
        theme::TEXT
    } else {
        theme::DISABLED
    };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.center(), vec2(12.0, 12.0)),
        icon,
        color,
        Color32::TRANSPARENT,
    );
    response
}

/// The cell of `day` in the calendar of `month`: today a filled blue
/// cell, the field's `date` outlined, days of other months weaker. Its
/// accessible name is the whole date, in the file's format.
fn cell(
    ui: &mut Ui,
    day: Date,
    month: u32,
    date: Option<Date>,
    cx: &Ctx,
    enabled: bool,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(DAY, ROW), sense);
    let text = day.format(cx.locale);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &text));
    let (_, day_month, number) = day.ymd();
    let same = |other: Date| other.ymd() == day.ymd();
    let inner = rect.shrink2(vec2(1.0, 1.0));
    let color = if same(cx.today) {
        ui.painter().rect_filled(inner, 3.0, theme::SELECTED);
        Color32::WHITE
    } else {
        if response.hovered() && enabled {
            ui.painter().rect_filled(inner, 3.0, theme::HAIRLINE);
        }
        if day_month != month {
            theme::WEAK_TEXT
        } else {
            theme::TEXT
        }
    };
    if date.is_some_and(same) && !same(cx.today) {
        ui.painter().rect_stroke(
            inner,
            3.0,
            egui::Stroke::new(1.0, theme::SELECTED),
            egui::StrokeKind::Inside,
        );
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        number.to_string(),
        FontId::proportional(DAY_FONT),
        color,
    );
    response
}
