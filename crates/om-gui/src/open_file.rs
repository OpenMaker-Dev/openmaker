//! File > Open: choosing a `.fmp12` file on this computer, and the account
//! dialog a file that needs one shows ("Open “file”", as FileMaker Pro
//! asks). The file opens through the provider's `open_local`; nothing here
//! reads or sends a client license. Also the chooser of a file to save
//! (`SaveChooser`), which a step's save dialog embeds.

use std::path::{Path, PathBuf};

use egui::{Key, RichText};
use zeroize::Zeroizing;

use crate::theme;
use crate::widgets::dialog_button;

/// One folder entry the chooser lists.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    name: String,
    folder: bool,
}

/// What the chooser asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChooserAction {
    Open(PathBuf),
    Cancel,
}

/// The Open dialog: a folder's subfolders and FileMaker files, and a path
/// that can also be typed.
pub(crate) struct OpenFileDialog {
    folder: PathBuf,
    /// The path field: the chosen file, or a folder to show.
    pub path: String,
    entries: Vec<Entry>,
    error: Option<String>,
}

impl OpenFileDialog {
    /// The system's open panel in the chooser's place: FileMaker files, in
    /// the same folder.
    pub fn panel_request() -> crate::file_panel::Request {
        crate::file_panel::Request::open("Open File", "")
            .of_kind("fmp12")
            .in_folder(documents())
            .reporting_modifiers()
    }

    /// The chooser, showing the user's Documents folder (or home).
    pub fn new() -> Self {
        Self::in_folder(documents())
    }

    pub fn in_folder(folder: PathBuf) -> Self {
        let mut dialog = Self {
            folder: PathBuf::new(),
            path: String::new(),
            entries: Vec::new(),
            error: None,
        };
        dialog.show_folder(folder);
        dialog
    }

    fn show_folder(&mut self, folder: PathBuf) {
        match list(&folder, "fmp12") {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(format!("This folder cannot be read: {e}"));
            }
        }
        self.path = folder.display().to_string();
        self.folder = folder;
    }

    /// The path field's choice: a folder is shown, a file opens.
    fn choose(&mut self) -> Option<ChooserAction> {
        let path = PathBuf::from(self.path.trim());
        if path.as_os_str().is_empty() {
            return None;
        }
        let path = if path.is_absolute() {
            path
        } else {
            self.folder.join(path)
        };
        if path.is_dir() {
            self.show_folder(path);
            None
        } else {
            Some(ChooserAction::Open(path))
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<ChooserAction> {
        let mut action = None;
        egui::Modal::new(egui::Id::unique("open_local_file")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.heading("Open File");
            ui.label("Choose a FileMaker Pro file (.fmp12) on this computer.");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = ui.label("Path");
                let field = ui
                    .add(egui::TextEdit::singleline(&mut self.path).desired_width(400.0))
                    .labelled_by(label.id);
                if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    action = self.choose();
                }
                let up = self.folder.parent().map(Path::to_path_buf);
                if ui
                    .add_enabled(up.is_some(), egui::Button::new("Enclosing Folder"))
                    .clicked()
                {
                    if let Some(up) = up {
                        self.show_folder(up);
                    }
                }
            });
            ui.add_space(4.0);
            let mut navigate = None;
            egui::Frame::new()
                .fill(theme::CONTROL_BG)
                .stroke(egui::Stroke::new(1.0, theme::HAIRLINE))
                .inner_margin(4.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .min_scrolled_height(260.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(error) = &self.error {
                                ui.label(RichText::new(error).color(theme::CAPTION));
                            } else if self.entries.is_empty() {
                                ui.label(
                                    RichText::new("No folders or FileMaker files here.")
                                        .color(theme::CAPTION),
                                );
                            }
                            for entry in &self.entries {
                                let path = self.folder.join(&entry.name);
                                let selected = Path::new(self.path.trim()) == path;
                                let text = if entry.folder {
                                    format!("{}/", entry.name)
                                } else {
                                    entry.name.clone()
                                };
                                let row = ui.selectable_label(selected, text);
                                if row.double_clicked() {
                                    if entry.folder {
                                        navigate = Some(path);
                                    } else {
                                        action = Some(ChooserAction::Open(path));
                                    }
                                } else if row.clicked() {
                                    self.path = path.display().to_string();
                                }
                            }
                        });
                });
            if let Some(folder) = navigate {
                self.show_folder(folder);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if dialog_button(ui, "Cancel", false, true).clicked() {
                    action = Some(ChooserAction::Cancel);
                }
                let ready = !self.path.trim().is_empty();
                if dialog_button(ui, "Open", true, ready).clicked() {
                    action = self.choose();
                }
            });
        });
        action
    }
}

/// The user's Documents folder, or home without one: where the choosers
/// and panels of Open and Create New start.
pub(crate) fn documents() -> PathBuf {
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

/// The subfolders and the files with `extension` of `folder`, folders
/// first, each by name in any case; hidden entries are left out.
fn list(folder: &Path, extension: &str) -> std::io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(folder)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let folder = entry.path().is_dir();
            let kind = Path::new(&name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(extension));
            (folder || kind).then_some(Entry { name, folder })
        })
        .collect();
    entries.sort_by(|a, b| {
        b.folder
            .cmp(&a.folder)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// The chooser of a file to save, as a save panel offers it: the file's
/// name ("Save As") and its folder ("Where", which also takes a typed
/// folder), with the folder's subfolders and its files of the same kind.
/// A name may also be a whole path; the chosen file is a native path.
#[derive(Clone, Debug)]
pub(crate) struct SaveChooser {
    folder: PathBuf,
    /// The Where field: the folder shown, or one typed to go to.
    place: String,
    /// The Save As field.
    pub name: String,
    extension: &'static str,
    entries: Vec<Entry>,
    error: Option<String>,
}

impl SaveChooser {
    /// File `name` in `folder`, listing the folder's files with
    /// `extension`. A folder that does not exist (a home without
    /// Documents) opens on its nearest existing enclosing folder.
    pub fn new(folder: PathBuf, name: String, extension: &'static str) -> Self {
        let folder = folder
            .ancestors()
            .find(|ancestor| ancestor.is_dir())
            .map_or(folder.clone(), Path::to_path_buf);
        let mut chooser = Self {
            folder: PathBuf::new(),
            place: String::new(),
            name,
            extension,
            entries: Vec::new(),
            error: None,
        };
        chooser.show_folder(folder);
        chooser
    }

    fn show_folder(&mut self, folder: PathBuf) {
        match list(&folder, self.extension) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(format!("This folder cannot be read: {e}"));
            }
        }
        self.place = folder.display().to_string();
        self.folder = folder;
    }

    /// The file chosen: the name in the folder the Where field names, or
    /// the name itself when it is an absolute path. `None` without a name,
    /// or while Where names no existing folder: Save never writes to a
    /// folder other than the one the field shows.
    pub fn chosen(&self) -> Option<PathBuf> {
        let name = self.name.trim();
        if name.is_empty() {
            return None;
        }
        Some(self.place_folder()?.join(home_relative(name)))
    }

    /// The folder the Where field names: the one shown, or one typed there
    /// (Enter not pressed yet) that exists.
    fn place_folder(&self) -> Option<PathBuf> {
        let typed = self.place.trim();
        if Path::new(typed) == self.folder {
            return Some(self.folder.clone());
        }
        let folder = self.folder.join(home_relative(typed));
        folder.is_dir().then_some(folder)
    }

    /// The chooser's fields and the folder's list. Enter in the Save As
    /// field asks to save (`true`).
    pub fn show(&mut self, ui: &mut egui::Ui) -> bool {
        let mut save = false;
        let mut go = None;
        egui::Grid::new("save_chooser")
            .num_columns(2)
            .show(ui, |ui| {
                let label = ui.label("Save As:");
                let field = ui
                    .add(egui::TextEdit::singleline(&mut self.name).desired_width(380.0))
                    .labelled_by(label.id);
                if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    save = true;
                }
                ui.end_row();
                let label = ui.label("Where:");
                ui.horizontal(|ui| {
                    let field = ui
                        .add(egui::TextEdit::singleline(&mut self.place).desired_width(260.0))
                        .labelled_by(label.id);
                    if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        let typed = self.folder.join(home_relative(self.place.trim()));
                        if typed.is_dir() {
                            go = Some(typed);
                        } else {
                            self.error = Some(format!("“{}” is not a folder.", typed.display()));
                        }
                    }
                    let up = self.folder.parent().map(Path::to_path_buf);
                    if ui
                        .add_enabled(up.is_some(), egui::Button::new("Enclosing Folder"))
                        .clicked()
                    {
                        go = up;
                    }
                });
                ui.end_row();
            });
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme::CONTROL_BG)
            .stroke(egui::Stroke::new(1.0, theme::HAIRLINE))
            .inner_margin(4.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .min_scrolled_height(180.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(error) = &self.error {
                            ui.label(RichText::new(error).color(theme::CAPTION));
                        }
                        for entry in &self.entries {
                            let text = if entry.folder {
                                format!("{}/", entry.name)
                            } else {
                                entry.name.clone()
                            };
                            let selected = !entry.folder && self.name.trim() == entry.name;
                            let row = ui.selectable_label(selected, text);
                            if entry.folder && row.double_clicked() {
                                go = Some(self.folder.join(&entry.name));
                            } else if !entry.folder && row.clicked() {
                                self.name = entry.name.clone();
                            }
                        }
                    });
            });
        if let Some(folder) = go {
            self.show_folder(folder);
        }
        save
    }
}

/// `~/...` in the user's home folder; other text as typed.
fn home_relative(text: &str) -> PathBuf {
    let home = || std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    match text.strip_prefix("~/").or((text == "~").then_some("")) {
        Some(rest) => home().map_or_else(
            || PathBuf::from(text),
            |home| PathBuf::from(home).join(rest),
        ),
        None => PathBuf::from(text),
    }
}

/// Forget what egui keeps of the text field `id`: its undo history holds
/// earlier text in the clear, a password field's too.
fn forget_field(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| d.remove::<egui::text_edit::TextEditState>(id));
}

/// What the account dialog asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoginAction {
    Open,
    Cancel,
}

/// "Open “file”": the account and password a local file asks for, or a
/// related file that refused the account its file was opened with (#214).
pub(crate) struct LoginDialog {
    pub path: PathBuf,
    /// A related file's: FileMaker Pro's text for it, reported by a
    /// FileMaker Pro 26 run on a local pair (#214).
    pub related: bool,
    pub account: String,
    pub password: Zeroizing<String>,
    /// Why the last attempt failed.
    pub error: Option<String>,
    /// Waiting for the provider's answer.
    pub pending: bool,
}

impl LoginDialog {
    pub fn new(path: PathBuf, account: String) -> Self {
        Self {
            path,
            related: false,
            account,
            password: Zeroizing::new(String::new()),
            error: None,
            pending: false,
        }
    }

    /// For related file `name`, suggesting `account`.
    pub fn related(name: &str, account: String) -> Self {
        Self {
            related: true,
            ..Self::new(PathBuf::from(name), account)
        }
    }

    fn file_name(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        om_data::file_refs::file_stem(&name).to_owned()
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<LoginAction> {
        let mut action = None;
        let name = self.file_name();
        egui::Modal::new(egui::Id::unique("open_local_login")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(format!("Open “{name}”"));
            ui.label(if self.related {
                format!("Sign in to open “{name}”.")
            } else {
                format!("Sign in to “{name}” with:")
            });
            ui.add_space(6.0);
            ui.add_enabled_ui(!self.pending, |ui| {
                egui::Grid::new("open_local_login_fields")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        let label = ui.label("Account Name");
                        ui.add(egui::TextEdit::singleline(&mut self.account).desired_width(220.0))
                            .labelled_by(label.id);
                        ui.end_row();
                        let label = ui.label("Password");
                        let field = ui
                            .add(
                                egui::TextEdit::singleline(&mut *self.password)
                                    .id(egui::Id::unique("open_local_login_password"))
                                    .password(true)
                                    .desired_width(220.0),
                            )
                            .labelled_by(label.id);
                        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            action = Some(LoginAction::Open);
                        }
                        ui.end_row();
                    });
            });
            if let Some(error) = &self.error {
                ui.add_space(4.0);
                ui.label(RichText::new(error).color(theme::TEXT));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if self.pending {
                    ui.spinner();
                }
                if dialog_button(ui, "Cancel", false, true).clicked() {
                    action = Some(LoginAction::Cancel);
                }
                let ok = if self.related { "Sign in" } else { "OK" };
                if dialog_button(ui, ok, true, !self.pending).clicked() {
                    action = Some(LoginAction::Open);
                }
            });
        });
        if action.is_some() {
            forget_field(ctx, egui::Id::unique("open_local_login_password"));
        }
        action
    }
}

/// What the change-password dialog asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChangeAction {
    Change,
    Cancel,
}

/// A new password for the account a local file's Login refused because its
/// password must be changed first (211, #157), or, with the old password
/// asked too, for the account a file is open with (File > Change
/// Password…, #830). Its wording is OpenMaker's [I: FileMaker Pro's dialogs
/// for these are not measured yet].
pub(crate) struct ChangePasswordDialog {
    pub path: PathBuf,
    pub account: String,
    /// The old password, asked for File > Change Password….
    pub old: Option<Zeroizing<String>>,
    pub new: Zeroizing<String>,
    pub confirm: Zeroizing<String>,
    /// Why the last attempt failed.
    pub error: Option<String>,
    /// Waiting for the provider's answer.
    pub pending: bool,
}

impl ChangePasswordDialog {
    pub fn new(path: PathBuf, account: String) -> Self {
        Self {
            path,
            account,
            old: None,
            new: password_buffer(),
            confirm: password_buffer(),
            error: None,
            pending: false,
        }
    }

    /// For File > Change Password… of the account `account` the file at
    /// `path` is open with: the old password is asked too.
    pub fn own(path: PathBuf, account: String) -> Self {
        Self {
            old: Some(password_buffer()),
            ..Self::new(path, account)
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<ChangeAction> {
        let mut action = None;
        let name = self
            .path
            .file_name()
            .map(|n| om_data::file_refs::file_stem(&n.to_string_lossy()).to_owned())
            .unwrap_or_default();
        egui::Modal::new(egui::Id::unique("change_due_password")).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("Change Password");
            ui.label(match self.old {
                Some(_) => format!("Change the password of “{}” in “{name}”.", self.account),
                None => format!(
                    "The password of “{}” must be changed before “{name}” opens.",
                    self.account
                ),
            });
            ui.add_space(6.0);
            ui.add_enabled_ui(!self.pending, |ui| {
                egui::Grid::new("change_due_password_fields")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        let mut fields = Vec::new();
                        if let Some(old) = &mut self.old {
                            fields.push(("Old Password", old, CHANGE_OLD));
                        }
                        fields.push(("New Password", &mut self.new, CHANGE_NEW));
                        fields.push(("Confirm New Password", &mut self.confirm, CHANGE_CONFIRM));
                        for (label, text, id) in fields {
                            let label = ui.label(label);
                            let field = ui
                                .add(
                                    egui::TextEdit::singleline(&mut **text)
                                        .id(egui::Id::unique(id))
                                        .password(true)
                                        .desired_width(220.0),
                                )
                                .labelled_by(label.id);
                            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                                action = Some(ChangeAction::Change);
                            }
                            ui.end_row();
                        }
                    });
            });
            if let Some(error) = &self.error {
                ui.add_space(4.0);
                ui.label(RichText::new(error).color(theme::TEXT));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if self.pending {
                    ui.spinner();
                }
                if dialog_button(ui, "Cancel", false, true).clicked() {
                    action = Some(ChangeAction::Cancel);
                }
                if dialog_button(ui, "OK", true, !self.pending).clicked() {
                    action = Some(ChangeAction::Change);
                }
            });
        });
        // No undo history is kept for these fields at all, and none once
        // the dialog is answered.
        for id in [CHANGE_OLD, CHANGE_NEW, CHANGE_CONFIRM] {
            if action.is_some() {
                forget_field(ctx, egui::Id::unique(id));
            } else {
                clear_undo(ctx, egui::Id::unique(id));
            }
        }
        action
    }
}

/// An empty password buffer large enough that typing a password (at most
/// 100 characters) does not reallocate it, leaving the text it held in
/// freed memory.
fn password_buffer() -> Zeroizing<String> {
    Zeroizing::new(String::with_capacity(512))
}

/// Drop the undo history egui keeps for the text field `id`, which holds
/// earlier text in the clear.
fn clear_undo(ctx: &egui::Context, id: egui::Id) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) {
        state.clear_undoer();
        state.store(ctx, id);
    }
}

/// The change-password dialog's fields.
const CHANGE_OLD: &str = "change_password_old";
const CHANGE_NEW: &str = "change_due_password_new";
const CHANGE_CONFIRM: &str = "change_due_password_confirm";

#[cfg(test)]
mod tests {
    use super::*;

    /// Save takes the folder Where names even before Enter, and nothing
    /// while it names no folder (review of #570: the folder shown before
    /// was used, replacing a file there).
    #[test]
    fn the_save_chooser_saves_where_its_where_field_says() {
        let dir = std::env::temp_dir().join(format!("om-gui-save-chooser-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Reports")).unwrap();
        let mut chooser = SaveChooser::new(dir.clone(), "report.pdf".into(), "pdf");
        assert_eq!(chooser.chosen(), Some(dir.join("report.pdf")));
        chooser.place = dir.join("Reports").display().to_string();
        assert_eq!(
            chooser.chosen(),
            Some(dir.join("Reports").join("report.pdf"))
        );
        chooser.place = "Reports".into();
        assert_eq!(
            chooser.chosen(),
            Some(dir.join("Reports").join("report.pdf"))
        );
        chooser.place = dir.join("Missing").display().to_string();
        assert_eq!(chooser.chosen(), None);
        // A folder that does not exist opens on its nearest existing one.
        let missing = SaveChooser::new(dir.join("No").join("Such"), "a.pdf".into(), "pdf");
        assert_eq!(missing.chosen(), Some(dir.join("a.pdf")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_chooser_lists_folders_then_filemaker_files() {
        let dir = std::env::temp_dir().join(format!("om-gui-open-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Sub")).unwrap();
        for name in ["b.fmp12", "A.FMP12", "notes.txt", ".hidden.fmp12"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let names: Vec<(String, bool)> = list(&dir, "fmp12")
            .unwrap()
            .into_iter()
            .map(|e| (e.name, e.folder))
            .collect();
        assert_eq!(
            names,
            [
                ("Sub".to_owned(), true),
                ("A.FMP12".to_owned(), false),
                ("b.fmp12".to_owned(), false)
            ]
        );
        let mut dialog = OpenFileDialog::in_folder(dir.clone());
        dialog.path = "Sub".into();
        assert_eq!(dialog.choose(), None);
        assert_eq!(dialog.folder, dir.join("Sub"));
        dialog.path = dir.join("b.fmp12").display().to_string();
        assert_eq!(
            dialog.choose(),
            Some(ChooserAction::Open(dir.join("b.fmp12")))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
