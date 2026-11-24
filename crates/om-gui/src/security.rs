//! File > Manage > Security: the file's accounts, privilege sets and
//! extended privileges, read through the provider's security catalog
//! (`om_data::security_catalog`), which the host decodes and sends without
//! any password or other credential. It changes nothing: accounts and
//! privilege sets cannot be edited yet ([`EDITING_UNSUPPORTED`]), so the
//! dialog shows no editing controls. The layout is OpenMaker's; FileMaker
//! Pro's dialog was not compared.

use egui::{Align, CornerRadius, Id, Layout, RichText, Stroke};
use om_data::security_catalog::{
    AccountKind, RecordAccess, SecurityAccount, SecurityCatalog, EDITING_UNSUPPORTED,
};
use om_data::DataError;

use crate::{theme, widgets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecurityAction {
    Reload,
    Close,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Accounts,
    PrivilegeSets,
    ExtendedPrivileges,
    FileAccess,
}

pub(crate) struct ManageSecurity {
    file: String,
    owner: u64,
    token: u64,
    catalog: Option<SecurityCatalog>,
    error: Option<String>,
    busy: bool,
    tab: Tab,
}

impl ManageSecurity {
    pub(crate) fn new(owner: u64, file: String) -> Self {
        Self {
            file,
            owner,
            token: 0,
            catalog: None,
            error: None,
            busy: true,
            tab: Tab::Accounts,
        }
    }

    pub(crate) fn owns(&self, file: u64) -> bool {
        self.owner == file
    }

    pub(crate) fn requested(&mut self, token: u64) {
        self.token = token;
        self.busy = true;
        self.error = None;
    }

    pub(crate) fn accepts(&self, file: u64, token: u64) -> bool {
        self.owns(file) && self.token == token
    }

    pub(crate) fn accept(&mut self, result: Result<SecurityCatalog, DataError>) {
        self.busy = false;
        match result {
            Ok(catalog) => {
                self.catalog = Some(catalog);
                self.error = None;
            }
            Err(error) => {
                self.catalog = None;
                self.error = Some(error.message);
            }
        }
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<SecurityAction> {
        let width = (ctx.content_rect().width() - 64.0).clamp(560.0, 780.0);
        let height = (ctx.content_rect().height() - 260.0).clamp(160.0, 340.0);
        let mut action = None;
        let response = egui::Modal::new(Id::unique("manage_security"))
            .frame(
                egui::Frame::new()
                    .fill(theme::DIALOG_BG)
                    .stroke(Stroke::new(1.0, theme::DIVIDER))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(18)
                    .shadow(ctx.global_style().visuals.window_shadow),
            )
            .backdrop_color(theme::SCRIM)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.label(
                    RichText::new(format!("Manage Security for “{}”", self.file))
                        .font(egui::FontId::new(15.0, theme::semibold())),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space((width - 420.0).max(0.0) / 2.0);
                    for (tab, title) in [
                        (Tab::Accounts, "Accounts"),
                        (Tab::PrivilegeSets, "Privilege Sets"),
                        (Tab::ExtendedPrivileges, "Extended Privileges"),
                        (Tab::FileAccess, "File Access"),
                    ] {
                        if ui.selectable_label(self.tab == tab, title).clicked() {
                            self.tab = tab;
                        }
                    }
                });
                ui.add_space(10.0);
                match (&self.catalog, self.tab) {
                    (Some(catalog), Tab::Accounts) => accounts(ui, height, catalog),
                    (Some(catalog), Tab::PrivilegeSets) => privilege_sets(ui, height, catalog),
                    (Some(catalog), Tab::ExtendedPrivileges) => extended(ui, height, catalog),
                    (Some(_), Tab::FileAccess) => list_frame(ui, height, |ui| {
                        ui.label(
                            "Which other files may use this file's tables is not decoded \
                             (issue #137), so it is not shown.",
                        );
                    }),
                    (None, _) => {
                        ui.allocate_ui(egui::vec2(width, height), |ui| {
                            ui.set_min_height(height);
                            ui.centered_and_justified(|ui| match (&self.error, self.busy) {
                                (_, true) => {
                                    ui.spinner();
                                }
                                (Some(error), false) => {
                                    ui.colored_label(theme::RED_INK, error);
                                }
                                (None, false) => {
                                    ui.label("The file's security could not be read.");
                                }
                            });
                        });
                    }
                }
                if let (Some(error), Some(_)) = (&self.error, &self.catalog) {
                    ui.add_space(6.0);
                    ui.colored_label(theme::RED_INK, error);
                }
                if self.catalog.is_some() {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("Read only. {EDITING_UNSUPPORTED}"))
                            .small()
                            .color(theme::CAPTION),
                    );
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Refresh"))
                        .clicked()
                    {
                        action = Some(SecurityAction::Reload);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::dialog_button(ui, "Done", true, true).clicked() {
                            action = Some(SecurityAction::Close);
                        }
                    });
                });
            });
        if response.should_close() {
            action = Some(SecurityAction::Close);
        }
        if action == Some(SecurityAction::Reload) {
            self.busy = true;
            self.error = None;
        }
        action
    }
}

fn accounts(ui: &mut egui::Ui, height: f32, catalog: &SecurityCatalog) {
    ui.label(count_label(catalog.accounts.len(), "account"));
    list_frame(ui, height, |ui| {
        egui::Grid::new("security_accounts")
            .num_columns(5)
            .spacing([28.0, 7.0])
            .show(ui, |ui| {
                for title in ["Account", "Status", "Type", "Privilege Set", "Password"] {
                    ui.label(RichText::new(title).strong());
                }
                ui.end_row();
                for account in &catalog.accounts {
                    ui.label(account_name(account));
                    ui.label(match account.active {
                        Some(true) => "Active",
                        Some(false) => "Inactive",
                        None => "Not readable",
                    });
                    ui.label(match account.kind {
                        Some(AccountKind::FileMakerFile) => "FileMaker File".to_owned(),
                        Some(AccountKind::Other(kind)) => format!("Other ({kind})"),
                        None => "Not readable".to_owned(),
                    });
                    ui.label(
                        account
                            .privilege_set
                            .as_deref()
                            .and_then(|key| catalog.set(key))
                            .and_then(|set| set.name.as_deref())
                            .unwrap_or("Not readable"),
                    );
                    ui.label(match account.must_change_password {
                        Some(true) => "Must change at next login",
                        Some(false) => "",
                        None => "Not readable",
                    });
                    ui.end_row();
                }
            });
    });
}

fn privilege_sets(ui: &mut egui::Ui, height: f32, catalog: &SecurityCatalog) {
    ui.label(count_label(catalog.privilege_sets.len(), "privilege set"));
    list_frame(ui, height, |ui| {
        egui::Grid::new("security_privilege_sets")
            .num_columns(5)
            .spacing([24.0, 7.0])
            .show(ui, |ui| {
                for title in [
                    "Privilege Set",
                    "Records",
                    "Password Lifetime",
                    "Accounts",
                    "Keywords",
                ] {
                    ui.label(RichText::new(title).strong());
                }
                ui.end_row();
                for set in &catalog.privilege_sets {
                    ui.label(set.name.as_deref().unwrap_or("Not readable"));
                    ui.label(match (set.record_access(), &set.record_flags) {
                        (RecordAccess::Unknown, Some(flags)) => {
                            format!("Not decoded ({})", hex(flags))
                        }
                        (access, _) => access.label().to_owned(),
                    });
                    ui.label(match set.password_days {
                        Some(0) => "No limit".to_owned(),
                        Some(days) => format!("{days} days"),
                        None => "Not readable".to_owned(),
                    });
                    ui.label(
                        catalog
                            .accounts_in(&set.key)
                            .map(account_name)
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                    ui.label(
                        catalog
                            .extended_of(&set.key)
                            .map(|privilege| privilege.keyword.as_deref().unwrap_or("?"))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                    ui.end_row();
                }
            });
    });
}

fn extended(ui: &mut egui::Ui, height: f32, catalog: &SecurityCatalog) {
    ui.label(count_label(
        catalog.extended_privileges.len(),
        "extended privilege",
    ));
    list_frame(ui, height, |ui| {
        egui::Grid::new("security_extended_privileges")
            .num_columns(2)
            .spacing([40.0, 7.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Keyword").strong());
                ui.label(RichText::new("Granted To").strong());
                ui.end_row();
                for privilege in &catalog.extended_privileges {
                    ui.label(privilege.keyword.as_deref().unwrap_or("Not readable"));
                    ui.label(
                        privilege
                            .sets
                            .iter()
                            .map(|key| {
                                catalog
                                    .set(key)
                                    .and_then(|set| set.name.clone())
                                    .unwrap_or_else(|| format!("set {}", hex(key)))
                            })
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                    ui.end_row();
                }
            });
    });
}

fn account_name(account: &SecurityAccount) -> String {
    account
        .display_name()
        .map_or_else(|| format!("Account {}", hex(&account.key)), str::to_owned)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn list_frame(ui: &mut egui::Ui, height: f32, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::CONTROL_BG)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .inner_margin(8)
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt("security_list")
                .min_scrolled_height(height)
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, contents);
        });
}

fn count_label(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}
