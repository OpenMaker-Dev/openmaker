//! Revert Record/Request (51) as a command (AGENTS.md, "Everything is a
//! command"): a stable id, its labels and menu paths, an availability
//! check, and the script step that performs it, as [`crate::save_records`]
//! has them. Records > Revert Record... in Browse mode and Requests >
//! Revert Request... in Find mode run that step with its dialog, a script
//! runs it with or without, and an agent runs it through a script, so each
//! gets the same confirmation and the same result.
//!
//! In Browse mode the host reverts the window's record: the user's unsaved
//! typing and what scripts left on it go, its related records' included,
//! and a record New Record made that no commit has kept is deleted
//! (script-runtime.md 6.3 O3). In Find mode it clears the current find
//! request's criteria.
//!
//! The result is the step's error code: 0 when the record or request was
//! reverted (or had nothing to revert), 1 when the user cancelled the
//! dialog.

use crate::runtime::Mode;
use crate::step::{flag, ScriptStep};

/// The stable id agents and logs name the command by.
pub const ID: &str = "revert_record";

/// The command's name in Browse mode, as the script step is named.
pub const LABEL: &str = "Revert Record";

/// The menu path in Browse mode: FileMaker Pro 26's Records menu
/// (client-ui.md, Menus).
pub const MENU_PATH: [&str; 2] = ["Records", "Revert Record..."];

/// The command's name in Find mode.
pub const REQUEST_LABEL: &str = "Revert Request";

/// The menu path in Find mode: FileMaker Pro 26's Requests menu.
pub const REQUEST_MENU_PATH: [&str; 2] = ["Requests", "Revert Request..."];

/// The script step that performs the command: Revert Record/Request.
pub const STEP_ID: u16 = 51;

/// The confirmation Revert Record... asks, with Cancel then Revert and
/// neither the default (FileMaker Pro 26, observed on #871). The step's
/// "With dialog" asks the same; that is inferred, not measured.
pub const MESSAGE: &str = "Revert all changes to this record since it was last entered?";

/// The confirmation Revert Request... asks, with the same buttons
/// (observed on #871).
pub const REQUEST_MESSAGE: &str =
    "Revert all changes to this find request since it was last entered?";

/// Whether the command can run. In Browse mode `changed` is whether the
/// record has changes no commit has kept (the user's typing, what scripts
/// left on it, or its being new), and `saving` whether a save of it is in
/// flight. In Find mode `changed` is whether the current request has a
/// criterion typed. As FileMaker Pro 26 enables the menu items (#871).
pub fn available(mode: Mode, changed: bool, saving: bool) -> bool {
    match mode {
        Mode::Browse => changed && !saving,
        Mode::Find => changed,
        Mode::Preview | Mode::Layout => false,
    }
}

/// The step the command runs: with its dialog when `dialog` (the menu
/// items' way), else as Revert Record/Request [With dialog: Off].
pub fn step(dialog: bool) -> ScriptStep {
    let mut step = ScriptStep::new(STEP_ID);
    if !dialog {
        step.flags |= flag::NO_INTERACT;
    }
    step
}
