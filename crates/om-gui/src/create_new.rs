//! File > Create New...: FileMaker Pro 26's two steps before a new file
//! opens (#593's walkthrough of Pro). First the **Create an App** sheet,
//! with Blank selected; then the **save panel**, which names the file and
//! chooses its folder: the system's own (`file_panel`), or where there is
//! none, this dialog's chooser. The file is made by the `create_file` operation
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
    /// The sheet's Create: the save panel comes next, the system's where
    /// there is one (app.rs), else [`CreateNewDialog::choose_location`].
    ChooseLocation,
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

    /// The dialog at its save panel, past the sheet: where the system's
    /// panel could not be shown after all.
    pub fn saving() -> Self {
        let mut dialog = Self::new();
        dialog.choose_location();
        dialog
    }

    /// Go on from the sheet to the dialog's own save panel.
    pub fn choose_location(&mut self) {
        self.step = Step::Save(SaveChooser::new(
            Self::folder(),
            Self::name(),
            om_data::new_file::EXTENSION,
        ));
    }

    /// The system's save panel in the dialog's: FileMaker files, proposing
    /// the same name and folder.
    pub fn panel_request() -> crate::file_panel::Request {
        crate::file_panel::Request::save("Create New", Self::folder(), Self::name())
            .of_kind(om_data::new_file::EXTENSION)
    }

    /// The save panel's folder: the user's Documents folder (or home), as
    /// Pro's.
    fn folder() -> PathBuf {
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let documents = home.join("Documents");
        if documents.is_dir() {
            documents
        } else {
            home
        }
    }

    /// "Untitled.fmp12", as Pro's save panel names a new file.
    fn name() -> String {
        format!("Untitled.{}", om_data::new_file::EXTENSION)
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<CreateAction> {
        let mut action = None;
        egui::Modal::new(egui::Id::unique("create_new_file")).show(ctx, |ui| {
            ui.set_width(520.0);
            match &mut self.step {
                Step::Choose => {
                    ui.heading("Create an App");
                    ui.add_space(8.0);
                    // Blank, selected: the only kind of file OpenMaker makes.
                    egui::Frame::new()
                        .fill(theme::CONTROL_BG)
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
                            action = Some(CreateAction::ChooseLocation);
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
        action
    }
}
