//! The console's shared building blocks, so that every view has
//! FileMaker Server's structure (`docs/re/admin-console.md` sections 5 and
//! 6): settings sections and rows, toggles, help popovers, buttons, tables,
//! modals, and the formats of times and sizes. Each works without the
//! script: toggles and actions are form posts, popovers are `<details>`,
//! and a modal is shown by its address's fragment (`#id`).
//!
//! Some are not used by the first views yet: the Dashboard, Databases,
//! Schedules and Backups views are built on them (`#[allow(dead_code)]`
//! until then).

use std::time::SystemTime;

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use maud::{html, Markup};

use crate::session::Flash;
use crate::views::Ctx;

/// The hidden fields every console form carries: the session's CSRF token
/// and the page to return to.
pub(crate) fn form_fields(ctx: &Ctx) -> Markup {
    html! {
        input type="hidden" name="csrf" value=(ctx.session.csrf);
        input type="hidden" name="back" value=(ctx.path);
    }
}

/// A view's title.
pub(crate) fn title(text: &str) -> Markup {
    html! { h1.m-title { (text) } }
}

/// A settings section's header, underlined, with an optional help popover.
pub(crate) fn section(text: &str, help_text: Option<&str>) -> Markup {
    html! {
        h2.module-header {
            (text)
            @if let Some(help_text) = help_text { (help(help_text)) }
        }
    }
}

/// A settings row: the gray label (with an optional help popover), the
/// value, and an action such as a link button or a toggle.
pub(crate) fn row(label: &str, help_text: Option<&str>, value: Markup, action: Markup) -> Markup {
    html! {
        div.settings-row {
            div.settings-label {
                (label)
                @if let Some(help_text) = help_text { (help(help_text)) }
            }
            div.settings-value { (value) }
            div.settings-action { (action) }
        }
    }
}

/// A toggle that saves at once: a form posting `on` (the new state) to
/// `action`. A disabled one is drawn faded and posts nothing.
pub(crate) fn toggle(ctx: &Ctx, action: &str, on: bool, enabled: bool, label: &str) -> Markup {
    html! {
        form.toggle-form method="post" action=(action) {
            (form_fields(ctx))
            input type="hidden" name="on" value=(if on { "false" } else { "true" });
            button.toggle type="submit" role="switch" aria-checked=(on) aria-label=(label)
                disabled[!enabled] {
                span.toggle-knob {}
            }
        }
    }
}

/// The value and the faded toggle of a setting OpenMaker Server does not
/// have: it says so, and the toggle posts nothing.
pub(crate) fn unavailable_setting(label: &str) -> (Markup, Markup) {
    (
        html! { span.unavailable { "Not available yet" } },
        html! {
            button.toggle type="button" role="switch" aria-checked="false" aria-label=(label)
                disabled { span.toggle-knob {} }
        },
    )
}

/// "Enabled" or "Disabled", the value column beside a toggle.
pub(crate) fn enabled(on: bool) -> &'static str {
    if on {
        "Enabled"
    } else {
        "Disabled"
    }
}

/// A blue circled "?" that opens a popover with `text`.
pub(crate) fn help(text: &str) -> Markup {
    html! {
        details.help {
            summary.help-button aria-label="Help" { "?" }
            div.help-popover role="note" { p { (text) } }
        }
    }
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// An outlined button that submits its form. `selection` names the
/// checkboxes one of which must be checked for it to be enabled; the
/// script enforces it, the server checks it again.
pub(crate) fn button(label: &str, name: &str, selection: Option<&str>) -> Markup {
    html! {
        button.btn type="submit" name=(name) value="1" data-needs-selection=[selection] {
            (label)
        }
    }
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// An outlined button that opens modal `id`.
pub(crate) fn modal_button(label: &str, id: &str, selection: Option<&str>) -> Markup {
    html! {
        a.btn href={"#" (id)} role="button" data-needs-selection=[selection] { (label) }
    }
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// A blue text link styled as a button, such as "Change" or "Details".
pub(crate) fn link_button(label: &str, href: &str) -> Markup {
    html! { a.link-button href=(href) { (label) } }
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// A table with FileMaker Server's header and row style. `body` holds the
/// rows; with none, it shows `empty` in a centered gray row.
pub(crate) fn table(columns: &[&str], rows: usize, body: Markup, empty: &str) -> Markup {
    html! {
        table.data-table {
            thead { tr { @for column in columns { th scope="col" { (column) } } } }
            tbody {
                (body)
                @if rows == 0 {
                    tr.empty-row { td colspan=(columns.len()) { (empty) } }
                }
            }
        }
    }
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// "Showing n of m" under a long list.
pub(crate) fn showing(shown: usize, total: usize) -> Markup {
    html! { p.showing { "Showing " (shown) " of " (total) } }
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// A modal: hidden until the address's fragment is `#id`, closed by its X
/// (which clears the fragment) or Escape. Put it inside the form whose
/// fields it completes, so that its submit button sends them all.
pub(crate) fn modal(id: &str, heading: &str, body: Markup) -> Markup {
    html! {
        div.modal id=(id) role="dialog" aria-modal="true" aria-labelledby={(id) "-title"} {
            div.modal-content {
                div.modal-header {
                    h2.modal-title id={(id) "-title"} { (heading) }
                    a.modal-close href="#" aria-label="Close" { "\u{00d7}" }
                }
                (body)
            }
        }
    }
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// The modal footer: the primary action, then Cancel.
pub(crate) fn modal_actions(submit: &str, name: &str) -> Markup {
    html! {
        div.modal-actions {
            button.btn.btn-wide type="submit" name=(name) value="1" { (submit) }
            a.btn.btn-wide href="#" { "Cancel" }
        }
    }
}

/// The outcome of the latest action, above the view.
pub(crate) fn flash(flash: &Flash) -> Markup {
    let class = if flash.ok { "flash ok" } else { "flash error" };
    html! { p class=(class) role="status" { (flash.text) } }
}

fn zoned(time: SystemTime) -> Option<Zoned> {
    Timestamp::try_from(time)
        .ok()
        .map(|t| t.to_zoned(TimeZone::system()))
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// `11/11/2026, 18:05`, in the server's time zone.
pub(crate) fn datetime(time: SystemTime) -> String {
    zoned(time).map_or_else(String::new, |z| z.strftime("%m/%d/%Y, %H:%M").to_string())
}

/// `Wednesday, November 11, 2026 at 18:05`, in the server's time zone.
pub(crate) fn long_datetime(time: SystemTime) -> String {
    zoned(time).map_or_else(String::new, |z| {
        z.strftime("%A, %B %-d, %Y at %H:%M").to_string()
    })
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// A recent time as "3 hours ago", with the absolute time as its tooltip.
pub(crate) fn relative(time: SystemTime, now: &Zoned) -> Markup {
    let now = SystemTime::from(now.timestamp());
    let text = match now.duration_since(time) {
        Ok(ago) => ago_text(ago.as_secs()),
        Err(_) => "just now".to_owned(),
    };
    html! { span.relative-time title=(datetime(time)) { (text) } }
}

#[allow(dead_code)] // For the views still to come; see the module doc.
fn ago_text(seconds: u64) -> String {
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match seconds {
        0..=44 => "a few seconds ago".to_owned(),
        45..=89 => "a minute ago".to_owned(),
        90..=2_699 => plural(seconds.div_ceil(60).max(2), "minute"),
        2_700..=5_399 => "an hour ago".to_owned(),
        5_400..=79_199 => plural((seconds + 1_800) / 3_600, "hour"),
        79_200..=129_599 => "a day ago".to_owned(),
        _ => plural((seconds + 43_200) / 86_400, "day"),
    }
}

#[allow(dead_code)] // For the views still to come; see the module doc.
/// A size in bytes as FileMaker Server shows one: `12.3 MB`.
pub(crate) fn bytes(size: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if size < 1024 {
        return format!("{size} B");
    }
    let mut value = size as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_ages_read_as_in_the_console() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(3_276_800), "3.1 MB");
        assert_eq!(bytes(52_428_800), "50.0 MB");
        assert_eq!(bytes(5 * 1024 * 1024 * 1024 * 1024), "5.0 TB");
        assert_eq!(ago_text(10), "a few seconds ago");
        assert_eq!(ago_text(25 * 60), "25 minutes ago");
        assert_eq!(ago_text(3 * 3600), "3 hours ago");
        assert_eq!(ago_text(3 * 86_400), "3 days ago");
    }
}
