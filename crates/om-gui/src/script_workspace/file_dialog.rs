//! A step's file path list (script-steps.md 11.0, `[00 0, 82]`): FileMaker's
//! Specify Output File dialog for the steps that write a file (Export
//! Records, Save Records as PDF and Excel, Export Field Contents, Save a
//! Copy as, ...), Specify File for those that read one (Insert File, the
//! data file steps, ...). One path per line, each with its kind (`file:`,
//! `filemac:`, `filewin:`, `filelinux:`); the first path that works is
//! used (Claris, "Creating file paths").
//!
//! What OpenMaker's dialog has: the path list and its explanation. The
//! options FileMaker shows in the same dialog for some steps (Automatically
//! open file, Create email with file as attachment, Create folders, the
//! output file type) are rows of the step's own options here, and Add
//! File… (a file chooser) is not offered.

use egui::{vec2, Color32, Sense};
use om_data::scripts::paths;

use super::pickers::{self, dialog, Picked};
use crate::theme;

/// The steps whose path list names the file they write.
const OUTPUT_STEPS: [u16; 9] = [3, 36, 37, 132, 143, 144, 152, 190, 225];

/// The dialog's title for step `id`.
pub(super) fn title(id: u16) -> &'static str {
    if OUTPUT_STEPS.contains(&id) {
        "Specify Output File"
    } else {
        "Specify File"
    }
}

pub(crate) struct SpecifyFile {
    title: &'static str,
    text: String,
    /// Why OK did not take the list.
    error: Option<String>,
    frames: u32,
}

impl SpecifyFile {
    /// The dialog for step `id`'s path list `text` (one path per line).
    pub fn new(id: u16, text: String) -> Self {
        Self {
            title: title(id),
            text,
            error: None,
            frames: 0,
        }
    }

    /// Draw the dialog; `Some` once it is answered. OK answers the list as
    /// typed when every line is a path [`paths::parse`] takes, else it
    /// says why and stays open.
    pub fn show(&mut self, ctx: &egui::Context, on_top: bool) -> Option<Picked<String>> {
        let mut outcome = None;
        let first = self.frames == 0;
        self.frames += 1;
        let shown = dialog(ctx, "specify_file", self.title, 480.0, on_top, |ui| {
            let width = ui.available_width();
            ui.add_space(14.0);
            ui.label(egui::RichText::new("File path list:").font(pickers::body()));
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(vec2(width, 112.0), Sense::hover());
            let edit = ui.put(
                rect,
                egui::TextEdit::multiline(&mut self.text)
                    .font(pickers::body())
                    .desired_width(width)
                    .desired_rows(6),
            );
            crate::widgets::accessible_label(ui, edit.id, "File Path List");
            if first {
                edit.request_focus();
            }
            if edit.changed() {
                self.error = None;
            }
            ui.add_space(8.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "One path per line, each with its kind: file:, filemac:, filewin: or \
                         filelinux: (file:$path for a path in a variable). The first path that \
                         works is used.",
                    )
                    .font(pickers::body())
                    .color(theme::WEAK_TEXT),
                )
                .wrap(),
            );
            if let Some(error) = &self.error {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(error)
                            .font(pickers::body())
                            .color(Color32::from_rgb(0xC0, 0x1C, 0x28)),
                    )
                    .wrap(),
                );
            }
            ui.add_space(16.0);
            let (cancel, ok) = pickers::cancel_ok(ui, true);
            if cancel {
                outcome = Some(Picked::Cancel);
            } else if ok {
                match paths::parse(&self.text) {
                    Ok(_) => outcome = Some(Picked::Ok(self.text.clone())),
                    Err(error) => self.error = Some(error),
                }
            }
        });
        // Return makes a new line in the list, so only Escape answers.
        if outcome.is_none() && shown.escape {
            outcome = Some(Picked::Cancel);
        }
        outcome
    }
}
