//! File > Create New...: FileMaker Pro 26's two steps before a new file
//! opens (#593's walkthrough of Pro). First the **Create an App** sheet,
//! with Blank selected; then the **save panel**, which names the file and
//! chooses its folder. The file is made by the `create_file` operation
//! (`om_data::new_file::create`), which never replaces a file; the window
//! then opens it as its Admin account with no password asked, and Manage
//! Database opens on its Fields tab, as in Pro (app.rs).
//!
//! Pro's sheet also offers Convert and its starter solutions. Those are
//! Claris's files, which OpenMaker does not ship, so Blank is the only
//! choice; nor does it say a blank file can be extended with Add-on
//! Tables, which OpenMaker does not offer.

use std::path::PathBuf;

use egui::RichText;

use crate::open_file::SaveChooser;
use crate::theme;
use crate::widgets::dialog_button;

/// What the dialog asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CreateAction {
    Create(PathBuf),
    Cancel,
}

/// Which step is shown.
enum Step {
    /// The Create an App sheet.
    Choose,
    /// The save panel.
    Save(SaveChooser),
}

/// File > Create New's sheet and save panel.
pub(crate) struct CreateNewDialog {
    step: Step,
    /// Why the last attempt made no file.
    pub error: Option<String>,
}

impl CreateNewDialog {
    pub fn new() -> Self {
        Self {
            step: Step::Choose,
            error: None,
        }
    }

    /// The save panel: "Untitled.fmp12" in the user's Documents folder (or
    /// home), as Pro's names it.
    fn save_panel() -> SaveChooser {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let documents = home.join("Documents");
        let folder = if documents.is_dir() { documents } else { home };
        SaveChooser::new(
            folder,
            format!("Untitled.{}", om_data::new_file::EXTENSION),
            om_data::new_file::EXTENSION,
        )
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<CreateAction> {
        let mut action = None;
        let mut next = None;
        egui::Modal::new(egui::Id::unique("create_new_file")).show(ctx, |ui| {
            ui.set_width(520.0);
            match &mut self.step {
                Step::Choose => {
                    ui.heading("Create an App");
                    ui.add_space(8.0);
                    // Blank, selected: the only kind of file OpenMaker makes.
                    egui::Frame::new()
                        .fill(egui::Color32::WHITE)
                        .stroke(egui::Stroke::new(1.0, theme::SELECTED))
                        .inner_margin(10.0)
                        .corner_radius(6.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new("Blank").strong());
                            ui.label(
                                RichText::new(
                                    "A blank app with one table, default fields, and one layout.",
                                )
                                .color(theme::CAPTION),
                            );
                        });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if dialog_button(ui, "Cancel", false, true).clicked() {
                            action = Some(CreateAction::Cancel);
                        }
                        if dialog_button(ui, "Create", true, true).clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            next = Some(Self::save_panel());
                        }
                    });
                }
                Step::Save(chooser) => {
                    let enter = chooser.show(ui);
                    if let Some(error) = &self.error {
                        ui.add_space(4.0);
                        ui.label(RichText::new(error).color(theme::CAPTION));
                    }
                    ui.add_space(8.0);
                    let chosen = chooser.chosen();
                    ui.horizontal(|ui| {
                        if dialog_button(ui, "Cancel", false, true).clicked() {
                            action = Some(CreateAction::Cancel);
                        }
                        let save = dialog_button(ui, "Save", true, chosen.is_some()).clicked();
                        if save || enter {
                            if let Some(path) = chosen {
                                action = Some(CreateAction::Create(path));
                            }
                        }
                    });
                }
            }
        });
        if let Some(chooser) = next {
            self.step = Step::Save(chooser);
        }
        action
    }
}
