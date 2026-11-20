//! Relookup Field Contents (40) as a command (AGENTS.md, "Everything is a
//! command"): a stable id, its label and menu path, an availability
//! check, and the script step that performs it, as [`crate::revert_record`]
//! has them. Records > Relookup Field Contents runs that step with its
//! dialog on the field the user is in, a script runs it with or without,
//! and an agent runs it through a script, so each gets the same
//! confirmation and the same result.
//!
//! In Browse mode the host copies every lookup that matches through the
//! field again, for each record of the window's found set
//! (script-runtime.md, Relookup Field Contents).
//!
//! The result is the step's error code: 0 when the records were written,
//! 1 when the user cancelled the dialog, and the codes script-runtime.md
//! lists for a field it refuses.

use crate::runtime::Mode;
use crate::step::{flag, ScriptStep};

/// The stable id agents and logs name the command by.
pub const ID: &str = "relookup_field_contents";

/// The command's name, as the script step is named.
pub const LABEL: &str = "Relookup Field Contents";

/// The menu path: FileMaker Pro 26's Records menu (client-ui.md, Menus),
/// without an ellipsis although the command asks before it writes.
pub const MENU_PATH: [&str; 2] = ["Records", "Relookup Field Contents"];

/// The script step that performs the command.
pub const STEP_ID: u16 = 40;

/// The confirmation the command asks before it writes. FileMaker Pro's
/// wording is unmeasured; this text and its OK and Cancel buttons are
/// OpenMaker's.
pub fn message(field: &str, records: usize) -> String {
    format!(
        "Look up the values again in all {records} records of the current found set, \
         through the field \u{201c}{field}\u{201d}?"
    )
}

/// Whether the command can run: in Browse mode with a field entered, the
/// field the command looks up through (Claris's help: "In Browse mode
/// ... select the match field, then choose Records menu > Relookup Field
/// Contents"). Whether FileMaker Pro enables the item for a field that is
/// no match field is unmeasured; the step answers that one (3).
pub fn available(mode: Mode, field_entered: bool) -> bool {
    mode == Mode::Browse && field_entered
}

/// The step the command runs on the field the user is in: with its dialog
/// when `dialog` (the menu item's way), else as Relookup Field Contents
/// [With dialog: Off].
pub fn step(dialog: bool) -> ScriptStep {
    let mut step = ScriptStep::new(STEP_ID);
    if !dialog {
        step.flags |= flag::NO_INTERACT;
    }
    step
}
