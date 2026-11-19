//! Layout mode's Button Setup, which opens when the Button tool has drawn
//! a button, as FileMaker Pro opens its own: the button's label and its
//! action. The action is Single Step (one script step, chosen in Specify
//! Step from the Script Workspace's steps pane, the steps FileMaker Pro
//! allows on a button) or Perform Script. Either step is edited in its own
//! options dialog, as the Script Workspace edits it.
//!
//! FileMaker Pro's Button Setup also offers icons, popover buttons and Do
//! Nothing, and takes the label typed in the button itself; here the label
//! is a field of the dialog (docs/desktop-client.md, "Placing a button").

use std::sync::Arc;

use egui::{pos2, vec2, Rect, Sense};
use om_data::host::CalcSchema;
use om_data::layout_placement::{
    check_button_step, check_label, ButtonAction, PERFORM_SCRIPT_BUTTON_VALUE,
};
use om_data::scripts::display::{step_line, NoStepNames, StepNames};
use om_data::scripts::StepData;
use om_data::ScriptStep;

use super::options::{OptionsOutcome, StepOptions};
use super::pickers::{
    self, popup_menu, push_button, row_label, text_field, value_box, Picked, BUTTON_WIDTH,
    CONTROL_HEIGHT,
};
use super::steps_pane::StepsPane;

/// Perform Script's step id.
const PERFORM_SCRIPT: u16 = 1;
/// The dialog's width, its label column and the rows' height (OpenMaker's
/// measures: FileMaker Pro 26's dialog was not captured).
const WIDTH: f32 = 520.0;
const LABEL_COLUMN: f32 = 52.0;
const LABEL_GAP: f32 = 8.0;
const ROW: f32 = 34.0;
const ACTION_WIDTH: f32 = 208.0;
const SPECIFY_GAP: f32 = 8.0;
/// Specify Step's width and its steps pane's height (OpenMaker's).
const STEP_PICKER_WIDTH: f32 = 380.0;
const STEP_PANE_HEIGHT: f32 = 400.0;

/// The Action pop-up's items, in this order (FileMaker Pro's order is not
/// captured).
const SINGLE_STEP: usize = 0;
const PERFORM_SCRIPT_ITEM: usize = 1;
const ACTIONS: [&str; 2] = ["Single Step", "Perform Script"];

fn perform_script_step() -> ScriptStep {
    let mut step = ScriptStep::new(PERFORM_SCRIPT);
    step.value = PERFORM_SCRIPT_BUTTON_VALUE;
    step
}

/// Button Setup's answer.
pub(crate) enum SetupOutcome {
    Cancel,
    /// The label, the step (Perform Script or a single step), and the step
    /// as the Script Workspace shows it.
    Ok {
        label: String,
        step: ScriptStep,
        action: String,
    },
}

pub(crate) struct ButtonSetup {
    label: String,
    step: ScriptStep,
    names: Option<Arc<CalcSchema>>,
    options: Option<Box<StepOptions>>,
    /// Specify Step, open for Single Step.
    picker: Option<StepPicker>,
    /// Why OK was refused: FileMaker shows such messages as an alert.
    alert: Option<String>,
    /// Frames shown, to give the label the keyboard once the modal's
    /// sizing pass is over.
    frames: u32,
}

impl ButtonSetup {
    /// Set up a button with `label` and `step`, or a new Perform Script
    /// step when it has none yet.
    pub(crate) fn new(
        label: &str,
        step: Option<&ScriptStep>,
        names: Option<Arc<CalcSchema>>,
    ) -> Self {
        let step = step.cloned().unwrap_or_else(perform_script_step);
        Self {
            label: label.to_owned(),
            step,
            names,
            options: None,
            picker: None,
            alert: None,
            frames: 0,
        }
    }

    fn names(&self) -> &dyn StepNames {
        match &self.names {
            Some(schema) => schema.as_ref(),
            None => &NoStepNames,
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<SetupOutcome> {
        let on_top = self.options.is_none() && self.picker.is_none() && self.alert.is_none();
        let first_frame = self.frames == 1;
        self.frames += 1;
        let action = step_line(&self.step, self.names()).text;
        let mut outcome = None;
        let mut ok = false;
        let mut specify = false;
        let mut chosen = None;
        let current = if self.step.id() == PERFORM_SCRIPT {
            PERFORM_SCRIPT_ITEM
        } else {
            SINGLE_STEP
        };
        let step_name = om_data::script_step_name(self.step.id()).unwrap_or("Step");
        let shown = pickers::dialog(ctx, "button_setup", "Button Setup", WIDTH, on_top, |ui| {
            let full = ui.available_width();
            ui.add_space(14.0);
            let left = ui.cursor().left();
            let right = left + full;
            let field_x = left + LABEL_COLUMN + LABEL_GAP;
            let row = |ui: &mut egui::Ui, label: &str| {
                let (band, _) = ui.allocate_exact_size(vec2(full, ROW), Sense::hover());
                if !label.is_empty() {
                    let label_rect = Rect::from_min_max(
                        pos2(left, band.top()),
                        pos2(field_x - LABEL_GAP, band.bottom()),
                    );
                    row_label(ui, label_rect, label, true);
                }
                Rect::from_min_max(
                    pos2(field_x, band.center().y - CONTROL_HEIGHT / 2.0),
                    pos2(right, band.center().y + CONTROL_HEIGHT / 2.0),
                )
            };

            let control = row(ui, "Label:");
            let edit = text_field(ui, control, &mut self.label, "Label", true);
            if first_frame {
                pickers::select_all(ui, &edit, &self.label);
            }

            let control = row(ui, "Action:");
            let popup = Rect::from_min_size(control.min, vec2(ACTION_WIDTH, CONTROL_HEIGHT));
            let actions = ACTIONS.map(str::to_owned);
            chosen = popup_menu(ui, popup, "Action", &actions[current], &actions, true);

            let control = row(ui, "");
            let specify_rect =
                Rect::from_min_max(pos2(right - BUTTON_WIDTH, control.top()), control.max);
            let step_rect = Rect::from_min_max(
                control.min,
                pos2(specify_rect.left() - SPECIFY_GAP, control.bottom()),
            );
            value_box(ui, step_rect, &action, "Button Action", true);
            specify = push_button(
                ui,
                specify_rect,
                "Specify…",
                &format!("Specify {step_name}…"),
                false,
                true,
            )
            .clicked();

            ui.add_space(18.0);
            let (cancel, ok_clicked) = pickers::cancel_ok(ui, true);
            if cancel {
                outcome = Some(SetupOutcome::Cancel);
            }
            ok = ok_clicked;
        });
        if shown.escape {
            outcome = Some(SetupOutcome::Cancel);
        }
        if (ok || shown.enter) && outcome.is_none() {
            ctx.input_mut(|input| {
                input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            });
            let checked = check_label(&self.label)
                .map_err(|refusal| refusal.to_string())
                .and_then(|()| button_action(&self.step).map(|_| ()));
            match checked {
                Ok(()) => {
                    outcome = Some(SetupOutcome::Ok {
                        label: self.label.clone(),
                        step: self.step.clone(),
                        action,
                    })
                }
                Err(message) => self.alert = Some(message),
            }
        }
        match chosen {
            // Single Step, chosen again or anew, opens Specify Step.
            Some(SINGLE_STEP) => {
                let id = (current == SINGLE_STEP).then(|| self.step.id());
                self.picker = Some(StepPicker::new(id));
            }
            Some(PERFORM_SCRIPT_ITEM) if current != PERFORM_SCRIPT_ITEM => {
                self.step = perform_script_step();
            }
            _ => {}
        }
        if let Some(picker) = &mut self.picker {
            match picker.show(ctx) {
                Some(Picked::Ok(id)) => {
                    self.picker = None;
                    if id == PERFORM_SCRIPT {
                        if self.step.id() != PERFORM_SCRIPT {
                            self.step = perform_script_step();
                        }
                    } else if id != self.step.id() {
                        // A new step has its defaults, as the Script
                        // Workspace inserts one.
                        self.step = super::editor::new_step(id);
                    }
                }
                Some(Picked::Cancel) => self.picker = None,
                None => {}
            }
        }
        if specify {
            self.options = Some(Box::new(StepOptions::new(
                self.step.clone(),
                self.names.clone(),
            )));
        }
        if let Some(options) = &mut self.options {
            match options.show(ctx) {
                Some(OptionsOutcome::Ok(step)) => {
                    self.step = step;
                    self.options = None;
                }
                Some(OptionsOutcome::Cancel) => self.options = None,
                None => {}
            }
        }
        if let Some(message) = self.alert.clone() {
            if super::alert(ctx, &message, &["OK"]).is_some() {
                self.alert = None;
            }
        }
        outcome
    }
}

/// Specify Step: the Script Workspace's steps pane, listing the steps
/// FileMaker Pro allows on a button. A double click or Return chooses.
struct StepPicker {
    pane: StepsPane,
}

impl StepPicker {
    fn new(current: Option<u16>) -> Self {
        let mut pane = StepsPane::default();
        pane.buttons = true;
        pane.selected = current;
        Self { pane }
    }

    fn show(&mut self, ctx: &egui::Context) -> Option<Picked<u16>> {
        let mut outcome = None;
        let shown = pickers::dialog(
            ctx,
            "button_step",
            "Specify Step",
            STEP_PICKER_WIDTH,
            true,
            |ui| {
                let full = ui.available_width();
                ui.add_space(10.0);
                let (rect, _) =
                    ui.allocate_exact_size(vec2(full, STEP_PANE_HEIGHT), Sense::hover());
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                child.set_clip_rect(rect.intersect(ui.clip_rect()));
                if let Some(id) = self.pane.ui(&mut child, None, true) {
                    outcome = Some(Picked::Ok(id));
                }
                ui.add_space(16.0);
                let (cancel, ok) = pickers::cancel_ok(ui, self.pane.selected.is_some());
                if cancel {
                    outcome = Some(Picked::Cancel);
                } else if ok {
                    outcome = self.pane.selected.map(Picked::Ok);
                }
            },
        );
        if outcome.is_none() && shown.escape {
            outcome = Some(Picked::Cancel);
        }
        // Return chooses the selected step, also while the search field
        // has the keyboard (the steps pane then leaves Return alone).
        if outcome.is_none() && shown.enter {
            if let Some(id) = self.pane.selected {
                ctx.input_mut(|input| {
                    input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                });
                outcome = Some(Picked::Ok(id));
            }
        }
        outcome
    }
}

/// The action Save Layout sends for `step` (`ButtonPlacement`), or why this
/// version cannot place it: Perform Script with a script chosen from the
/// list, or a single step `check_button_step` allows. A script chosen by
/// name or from another file is not placed.
pub(crate) fn button_action(step: &ScriptStep) -> Result<ButtonAction, String> {
    if step.id() != PERFORM_SCRIPT {
        check_button_step(step).map_err(|refusal| refusal.to_string())?;
        return Ok(ButtonAction::Step(step.clone()));
    }
    // `Specified: By name` is list value 1 in slot 1 (script-steps.md).
    if step.list_value(1) == Some(1) {
        return Err(
            "Choose the button's script from the list: OpenMaker does not place a button that \
             performs a script by name."
                .into(),
        );
    }
    if step.data_leaf(StepData::FILE_REFERENCES).is_some() {
        return Err(
            "OpenMaker does not place a button that performs a script of another file.".into(),
        );
    }
    let script = step
        .script_key()
        .and_then(|key| key.serial_id())
        .ok_or_else(|| "Specify the script the button performs.".to_owned())?;
    Ok(ButtonAction::PerformScript {
        script,
        parameter: step.calculation(0),
    })
}
