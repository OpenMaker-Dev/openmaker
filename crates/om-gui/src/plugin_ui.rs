//! The desktop side of the plug-in subset (`om_scripts::runtime::plugins`,
//! docs/plugins.md): the clock an `FM.RunScriptIdle` script waits on, and
//! the alert `Dialog.Run` shows. Presentation only; what the commands mean
//! is the script engine's.

use std::time::{Duration, Instant};

use egui::{Event, Pos2, Shape, Stroke, Ui};
use om_data::runtime::plugins::{AlertMode, PluginDialog, PluginDialogAnswer};

use crate::{theme, widgets};

/// An idle script's clock: due once nobody has used the app for `delay`.
/// The script runs once: it is scheduled again by calling the command again
/// [I: docs/re/mbs-plugin.md 2].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct IdleClock {
    delay: Duration,
    since: Instant,
}

impl IdleClock {
    pub(crate) fn new(delay: f64, now: Instant) -> Self {
        Self {
            // The engine bounds the delay; one past `Duration` is never due.
            delay: Duration::try_from_secs_f64(delay).unwrap_or(Duration::MAX),
            since: now,
        }
    }

    /// The user moved the pointer, typed or scrolled: the count starts
    /// again ("Moving the mouse or using keyboard will reset the counter",
    /// the plug-in's documentation).
    pub(crate) fn note_activity(&mut self, now: Instant) {
        self.since = now;
    }

    /// Whether the script is due.
    pub(crate) fn due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) >= self.delay
    }

    /// How long until it is due, for the next repaint.
    pub(crate) fn until_due(&self, now: Instant) -> Duration {
        self.delay
            .saturating_sub(now.saturating_duration_since(self.since))
    }
}

/// Whether this frame's input is the user's: pointer, keys, text, scroll
/// or touch. Focus and window events are not.
pub(crate) fn user_active(ctx: &egui::Context) -> bool {
    ctx.input(|input| {
        input.events.iter().any(|event| {
            matches!(
                event,
                Event::PointerMoved(_)
                    | Event::MouseMoved(_)
                    | Event::PointerButton { .. }
                    | Event::Key { .. }
                    | Event::Text(_)
                    | Event::Paste(_)
                    | Event::MouseWheel { .. }
                    | Event::Zoom(_)
                    | Event::Touch { .. }
            )
        })
    })
}

/// `Dialog.Run`'s alert: its icon and message, and its buttons with the
/// default last (rightmost), as a Mac alert lays them out. Return chooses
/// the default. The answer is `Timeout` once `deadline` passes.
pub(crate) fn alert(
    ui: &mut Ui,
    dialog: &PluginDialog,
    deadline: Option<Instant>,
    enter: bool,
) -> Option<PluginDialogAnswer> {
    if let Some(deadline) = deadline {
        let now = Instant::now();
        if now >= deadline {
            return Some(PluginDialogAnswer::Timeout);
        }
        ui.ctx().request_repaint_after(deadline - now);
    }
    ui.horizontal_top(|ui| {
        if matches!(dialog.mode, AlertMode::Caution | AlertMode::Stop) {
            caution_icon(ui, dialog.mode);
            ui.add_space(10.0);
        }
        ui.add(egui::Label::new(&dialog.message).wrap());
    });
    ui.add_space(12.0);
    let mut chosen = None;
    ui.horizontal(|ui| {
        for (i, label) in dialog.buttons.iter().enumerate().rev() {
            if widgets::dialog_button(ui, label, i == 0, true).clicked() {
                chosen = Some(i);
            }
        }
    });
    if enter && chosen.is_none() {
        chosen = Some(0);
    }
    chosen.map(PluginDialogAnswer::Button)
}

/// A 32-point triangle with an exclamation mark: dark on yellow for a
/// warning, white on red for a stop. OpenMaker's own drawing; the plug-in
/// shows the system's alert icon, which is not measured.
fn caution_icon(ui: &mut Ui, mode: AlertMode) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
    let (fill, ink) = match mode {
        AlertMode::Stop => (theme::RED, theme::ON_ACCENT),
        _ => (theme::CAUTION, theme::TEXT),
    };
    let top = Pos2::new(rect.center().x, rect.top() + 2.0);
    let left = Pos2::new(rect.left() + 1.0, rect.bottom() - 2.0);
    let right = Pos2::new(rect.right() - 1.0, rect.bottom() - 2.0);
    let painter = ui.painter();
    painter.add(Shape::convex_polygon(
        vec![top, right, left],
        fill,
        Stroke::NONE,
    ));
    let x = rect.center().x;
    painter.line_segment(
        [
            Pos2::new(x, rect.top() + 11.0),
            Pos2::new(x, rect.top() + 21.0),
        ],
        Stroke::new(3.0, ink),
    );
    painter.circle_filled(Pos2::new(x, rect.top() + 25.5), 1.8, ink);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_starts_the_idle_count_again() {
        let start = Instant::now();
        let at = |s: f64| start + Duration::from_secs_f64(s);
        let mut clock = IdleClock::new(7.0, start);
        assert!(!clock.due(at(6.9)));
        assert_eq!(clock.until_due(at(6.0)), Duration::from_secs(1));
        clock.note_activity(at(5.0));
        assert!(!clock.due(at(11.9)));
        assert!(clock.due(at(12.0)));
    }
}
