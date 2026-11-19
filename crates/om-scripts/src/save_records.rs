//! Save Records as Excel (143) and Save Records as PDF (144) as commands
//! (AGENTS.md, "Everything is a command"): a stable id, a label and menu
//! path, typed parameters, an availability check, and the script step that
//! performs them. Every caller runs that step with a host's runtime: the
//! File menu, the Share menu and the Preview toolbar, a script, and an
//! agent, so each gets the same output and errors. The host writes the
//! file (the desktop client: `om-gui`'s report builder for PDF and
//! `om_transfer` for Excel) and gathers what the step leaves open with the
//! step's own dialog.
//!
//! The result is the step's error code: 0 when the file was written, 1
//! when the user cancelled the dialog, 3 for what the host cannot do, 401
//! when there are no records, 800 when the file could not be saved, and
//! the codes of calculation and data errors (portable-pdf.md,
//! script-steps.md "Implementation notes (import/export)").

use crate::runtime::Mode;
use crate::step::{flag, ScriptStep, StepData};
use crate::value::text_node;

/// The file a command writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// Save Records as Excel (143): an `.xlsx` workbook.
    Excel,
    /// Save Records as PDF (144).
    Pdf,
}

impl Format {
    pub const ALL: [Format; 2] = [Format::Excel, Format::Pdf];

    /// The stable id agents and logs name the command by.
    pub fn id(self) -> &'static str {
        match self {
            Format::Excel => "save_records_as_excel",
            Format::Pdf => "save_records_as_pdf",
        }
    }

    /// The command's name, as the script step is named.
    pub fn label(self) -> &'static str {
        match self {
            Format::Excel => "Save Records as Excel",
            Format::Pdf => "Save Records as PDF",
        }
    }

    /// The File menu path. Claris names it "File > Save/Send Records As >
    /// Excel" (and PDF); the items' ellipses are inferred, not measured.
    pub fn menu_path(self) -> [&'static str; 3] {
        match self {
            Format::Excel => ["File", "Save/Send Records As", "Excel..."],
            Format::Pdf => ["File", "Save/Send Records As", "PDF..."],
        }
    }

    /// The script step that performs the command.
    pub fn step_id(self) -> u16 {
        match self {
            Format::Excel => 143,
            Format::Pdf => 144,
        }
    }
}

/// Whether the option calculation in `slot` of a Save Records as PDF step
/// goes unused, so the step neither evaluates nor checks it: the page-from
/// calculation a restored step keeps in slot 4, its XML position, beside
/// the print permission while All pages (slot 1 = 1) is chosen
/// (script-steps.md 11.7) [I: FileMaker keeps From's default with All
/// pages; its use then is unmeasured].
pub fn unused_pdf_calculation(step: &ScriptStep, slot: u16) -> bool {
    step.id() == Format::Pdf.step_id()
        && slot == 4
        && step.has_flag(flag::RESTORE)
        && step.number(1) == Some(1)
}

/// Which records a command writes (script-steps.md 11.7): 1 records being
/// browsed, 2 current record, in `mStepValue` for Save Records as PDF and in
/// numeric slot 1 (u16) for Save Records as Excel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Records {
    /// The window's found set, in its order.
    Browsed,
    /// The window's current record.
    Current,
}

impl Records {
    /// The records a stored Save Records as PDF or as Excel step writes.
    /// Excel's choice is one of its stored options, which apply only with
    /// Restore on. FileMaker Pro 26, with dialog off, writes the found set
    /// when Restore is off (script-steps.md 11.7).
    pub fn of(step: &ScriptStep) -> Records {
        let current = match step.id() {
            144 => step.value == 2,
            143 => step.has_flag(flag::RESTORE) && step.number(1) == Some(2),
            _ => false,
        };
        if current {
            Records::Current
        } else {
            Records::Browsed
        }
    }
}

/// A command's parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveRecords {
    pub format: Format,
    pub records: Records,
    /// The output file as one line of a path list (`file:Report.pdf`;
    /// relative paths are in the user's Documents folder); `None` leaves it
    /// to the step's dialog.
    pub path: Option<String>,
    /// Show the step's dialog even with a path.
    pub with_dialog: bool,
}

/// Why a command cannot run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The window shows no layout.
    NoLayout,
    /// Only Browse and Preview mode write records.
    Mode(Mode),
}

impl SaveRecords {
    /// The command as the File menu and the toolbars run it: the records
    /// being browsed, the output file chosen in the step's dialog.
    pub fn interactive(format: Format) -> SaveRecords {
        SaveRecords {
            format,
            records: Records::Browsed,
            path: None,
            with_dialog: true,
        }
    }

    /// Whether the command can run in a window in `mode` that shows a
    /// layout (`layout`). Find mode has no records to write; Layout mode
    /// shows the design.
    pub fn check(&self, mode: Mode, layout: bool) -> Result<(), Unavailable> {
        if !layout {
            return Err(Unavailable::NoLayout);
        }
        if !matches!(mode, Mode::Browse | Mode::Preview) {
            return Err(Unavailable::Mode(mode));
        }
        Ok(())
    }

    /// The step that performs the command, as FileMaker stores it: the
    /// records ([`Records`]; Excel's current record as a restored option),
    /// "With dialog" off as `kNoInteract`, and the path list as text (the
    /// runtime's path list reader also takes it at step level, where
    /// FileMaker keeps it in slot 0).
    pub fn step(&self) -> ScriptStep {
        let mut step = ScriptStep::new(self.format.step_id());
        let current = self.records == Records::Current;
        match self.format {
            Format::Pdf => step.value = if current { 2 } else { 1 },
            Format::Excel if current => {
                step.flags |= flag::RESTORE;
                step.set_number_u16(1, Some(2));
            }
            Format::Excel => {}
        }
        if !self.with_dialog {
            step.flags |= flag::NO_INTERACT;
        }
        if let Some(path) = &self.path {
            step.flags |= flag::FILE_PATH;
            step.set_data_leaf(
                StepData::PATH_STRING,
                Some(text_node(&[StepData::PATH_STRING], path)),
            );
        }
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_run_in_browse_and_preview_mode_with_a_layout() {
        let pdf = SaveRecords::interactive(Format::Pdf);
        assert_eq!(pdf.check(Mode::Browse, true), Ok(()));
        assert_eq!(pdf.check(Mode::Preview, true), Ok(()));
        assert_eq!(
            pdf.check(Mode::Find, true),
            Err(Unavailable::Mode(Mode::Find))
        );
        assert_eq!(pdf.check(Mode::Browse, false), Err(Unavailable::NoLayout));
        let excel = SaveRecords {
            records: Records::Current,
            ..SaveRecords::interactive(Format::Excel)
        };
        assert_eq!(excel.check(Mode::Browse, true), Ok(()));
    }

    #[test]
    fn the_step_carries_the_records_the_dialog_and_the_path() {
        let step = SaveRecords::interactive(Format::Pdf).step();
        assert_eq!((step.opcode, step.value), (144, 1));
        assert!(!step.has_flag(flag::NO_INTERACT));
        assert!(crate::runtime::path_list(&step).is_none());
        let step = SaveRecords {
            format: Format::Excel,
            records: Records::Browsed,
            path: Some("file:Report.xlsx".into()),
            with_dialog: false,
        }
        .step();
        assert_eq!((step.opcode, step.value), (143, 0));
        assert!(step.has_flag(flag::NO_INTERACT));
        assert_eq!(
            crate::runtime::path_list(&step).as_deref(),
            Some("file:Report.xlsx")
        );
        assert_eq!(Records::of(&step), Records::Browsed);
    }

    /// Both formats' current record reads back as it is stored; Excel's
    /// slot 1 counts only with Restore (script-steps.md 11.7).
    #[test]
    fn the_records_choice_reads_back_from_each_format() {
        for format in Format::ALL {
            let current = SaveRecords {
                records: Records::Current,
                ..SaveRecords::interactive(format)
            }
            .step();
            assert_eq!(Records::of(&current), Records::Current, "{format:?}");
        }
        let excel = SaveRecords {
            records: Records::Current,
            ..SaveRecords::interactive(Format::Excel)
        }
        .step();
        assert_eq!(excel.number(1), Some(2));
        let mut stale = excel.clone();
        stale.flags &= !flag::RESTORE;
        assert_eq!(Records::of(&stale), Records::Browsed);
    }
}
