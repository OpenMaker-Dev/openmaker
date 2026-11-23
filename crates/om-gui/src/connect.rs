//! The entry flow, styled as FileMaker's Hosts dialog: the server, the
//! credentials, and which hosted file to open.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::{
    pos2, vec2, Align, Align2, CornerRadius, FontId, Key, Layout, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2,
};
use om_data::{Connection, HostListing, HostedFile, ListingError, ListingFailure};

use crate::app::Action;
use crate::icons::{self, Icon};
use crate::theme;
use crate::widgets::{self, dialog_button};
use crate::worker::Listed;

/// The default FileMaker Server port.
pub(crate) const DEFAULT_PORT: u16 = 5003;

/// What the Connect panel is waiting on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectStatus {
    Idle,
    ListingFiles,
    Connecting,
}

/// The editable Connect form.
#[derive(Clone, Debug)]
pub(crate) struct ConnectForm {
    pub host: String,
    pub port: String,
    pub account: String,
    pub password: String,
    pub file: String,
}

impl Default for ConnectForm {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: DEFAULT_PORT.to_string(),
            account: String::new(),
            password: String::new(),
            file: String::new(),
        }
    }
}

impl ConnectForm {
    /// A form prefilled from `OPENMAKER_HOST`, `OPENMAKER_PORT`,
    /// `OPENMAKER_FILE` and `OPENMAKER_ACCOUNT`. The password is always typed.
    pub fn from_env() -> Self {
        let value = |name: &str| std::env::var(name).unwrap_or_default();
        Self {
            host: value("OPENMAKER_HOST"),
            port: std::env::var("OPENMAKER_PORT").unwrap_or_else(|_| DEFAULT_PORT.to_string()),
            file: value("OPENMAKER_FILE"),
            account: value("OPENMAKER_ACCOUNT"),
            password: String::new(),
        }
    }

    pub fn host(&self) -> Result<&str, String> {
        match self.host.trim() {
            "" => Err("Enter a host name or address.".into()),
            host => Ok(host),
        }
    }

    pub fn port(&self) -> Result<u16, String> {
        match self.port.trim().parse::<u16>() {
            Ok(port) if port != 0 => Ok(port),
            _ => Err("Port must be a number from 1 to 65535.".into()),
        }
    }

    /// The server to list files on.
    pub fn server(&self) -> Result<(String, u16), String> {
        Ok((self.host()?.to_owned(), self.port()?))
    }

    /// A connection for the form's file, or why the form is incomplete.
    pub fn connection(&self) -> Result<Connection, String> {
        let (host, port) = self.server()?;
        let file = match self.file.trim() {
            "" => return Err("Enter or choose a file to open.".into()),
            file => file.to_owned(),
        };
        Ok(Connection {
            host,
            port,
            file,
            account: self.account.trim().to_owned(),
            password: self.password.clone(),
        })
    }
}

/// The Hosts dialog's file list. A listing belongs to the host and port it
/// was asked of: the dialog shows it for no other, and a reply to a
/// listing no longer waited for (canceled, or since asked again) is
/// dropped unseen.
#[derive(Debug, Default)]
pub(crate) enum Listing {
    #[default]
    None,
    /// Listing `server`'s files, as listing `token`.
    Pending {
        token: u64,
        server: (String, u16),
        canceled: Arc<AtomicBool>,
    },
    /// What `server` listed, or why it could not.
    Listed {
        server: (String, u16),
        result: Result<HostListing, ListingError>,
    },
}

impl Listing {
    /// The host and port the listing is for.
    pub fn server(&self) -> Option<&(String, u16)> {
        match self {
            Listing::None => None,
            Listing::Pending { server, .. } | Listing::Listed { server, .. } => Some(server),
        }
    }

    /// Whether a listing is under way.
    pub fn pending(&self) -> bool {
        matches!(self, Listing::Pending { .. })
    }

    /// Forget the listing, stopping one under way (its connection closes).
    pub fn forget(&mut self) {
        if let Listing::Pending { canceled, .. } = self {
            canceled.store(true, Ordering::SeqCst);
        }
        *self = Listing::None;
    }

    /// Cancel an explicit user request and retain its host-bound outcome.
    /// Changing away from Pending also rejects any late reply to its token.
    pub fn cancel(&mut self) {
        if let Listing::Pending {
            server, canceled, ..
        } = self
        {
            canceled.store(true, Ordering::SeqCst);
            *self = Listing::Listed {
                server: std::mem::take(server),
                result: Err(ListingError::new(
                    ListingFailure::Canceled,
                    "Listing files was canceled.",
                )),
            };
        }
    }

    /// Take `listed` if it answers the listing under way; `false` drops it.
    pub fn accept(&mut self, listed: Listed) -> bool {
        match self {
            Listing::Pending { token, server, .. } if *token == listed.token => {
                *self = Listing::Listed {
                    server: std::mem::take(server),
                    result: listed.result,
                };
                true
            }
            _ => false,
        }
    }

    /// The files listed, when they were listed for `server`.
    pub fn files_for(&self, server: Option<&(String, u16)>) -> Option<&HostListing> {
        match self {
            Listing::Listed {
                server: listed,
                result: Ok(listing),
            } if Some(listed) == server => Some(listing),
            _ => None,
        }
    }
}

/// Hosts-dialog view state that is not part of the connection.
#[derive(Clone, Debug)]
pub(crate) struct HostsView {
    /// The Search Files filter.
    pub search: String,
    /// Show the file list as tiles instead of rows.
    pub tiles: bool,
    pub use_saved_license: bool,
}

impl Default for HostsView {
    fn default() -> Self {
        Self {
            search: String::new(),
            tiles: false,
            use_saved_license: true,
        }
    }
}

const DIALOG_SIZE: Vec2 = vec2(828.0, 540.0);
const SIDEBAR_WIDTH: f32 = 224.0;
const TITLE_HEIGHT: f32 = 28.0;

/// The Hosts dialog (capture 28), centered over a plain backdrop: hosts at
/// the left, the selected host's files at the right, and the account,
/// password and file to open along the bottom. With `closable`, Cancel also
/// closes the dialog when nothing is under way (it was opened from Home).
#[allow(clippy::too_many_arguments)]
pub(crate) fn hosts_dialog(
    ui: &mut Ui,
    form: &mut ConnectForm,
    view: &mut HostsView,
    status: ConnectStatus,
    client_license: bool,
    listing: &Listing,
    closable: bool,
    actions: &mut Vec<Action>,
) {
    let area = ui.max_rect();
    ui.painter()
        .rect_filled(area, CornerRadius::ZERO, theme::PASTEBOARD);
    let size = vec2(
        DIALOG_SIZE.x.min(area.width() - 48.0),
        DIALOG_SIZE.y.min(area.height() - 48.0),
    )
    .max(vec2(560.0, 360.0));
    let dialog = Rect::from_center_size(area.center(), size);
    let idle = status == ConnectStatus::Idle;

    let shadow = ui.visuals().window_shadow;
    ui.painter()
        .add(shadow.as_shape(dialog, CornerRadius::same(10)));
    ui.painter().rect(
        dialog,
        CornerRadius::same(10),
        theme::DIALOG_BG,
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Outside,
    );
    let title = Rect::from_min_size(dialog.min, vec2(dialog.width(), TITLE_HEIGHT));
    widgets::put_label(
        ui,
        title,
        "Hosts",
        FontId::new(theme::BODY, theme::semibold()),
        theme::TEXT,
        Align::Center,
    );
    ui.painter().hline(
        title.x_range(),
        title.bottom(),
        Stroke::new(1.0, theme::HAIRLINE),
    );

    let sidebar = Rect::from_min_max(
        pos2(dialog.left(), title.bottom()),
        pos2(dialog.left() + SIDEBAR_WIDTH, dialog.bottom()),
    );
    ui.painter().rect_filled(
        sidebar,
        CornerRadius {
            sw: 10,
            ..CornerRadius::ZERO
        },
        theme::SIDEBAR_BG,
    );
    ui.painter().vline(
        sidebar.right(),
        sidebar.y_range(),
        Stroke::new(1.0, theme::BORDER),
    );
    let mut sidebar_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(sidebar.shrink2(vec2(12.0, 12.0)))
            .layout(Layout::top_down(Align::Min)),
    );
    ui_sidebar(&mut sidebar_ui, form, view, idle, client_license, actions);

    let pane = Rect::from_min_max(
        pos2(sidebar.right() + 1.0, title.bottom() + 1.0),
        dialog.max,
    );
    let mut pane_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(pane)
            .layout(Layout::top_down(Align::Min)),
    );
    files_pane(&mut pane_ui, form, view, status, listing, closable, actions);
    ui.allocate_rect(area, Sense::hover());
}

fn caption(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Label::new(
            RichText::new(text)
                .font(FontId::new(theme::SMALL, theme::semibold()))
                .color(theme::CAPTION),
        )
        .selectable(false),
    )
}

fn ui_sidebar(
    ui: &mut Ui,
    form: &mut ConnectForm,
    view: &mut HostsView,
    idle: bool,
    client_license: bool,
    actions: &mut Vec<Action>,
) {
    ui.add_enabled_ui(idle, |ui| {
        let label = caption(ui, "Host");
        ui.add(
            egui::TextEdit::singleline(&mut form.host)
                .hint_text("Host address")
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
        ui.add_space(4.0);
        let label = caption(ui, "Port");
        ui.add(
            egui::TextEdit::singleline(&mut form.port)
                .hint_text("5003")
                .desired_width(72.0),
        )
        .labelled_by(label.id);
    });
    ui.add_space(14.0);
    caption(ui, "Favorites");
    ui.add_space(2.0);
    match form.host.trim() {
        "" => {
            ui.label(
                RichText::new("Enter a host address")
                    .font(theme::small())
                    .color(theme::WEAK_TEXT),
            );
        }
        host => {
            // The host being connected to, as FileMaker highlights the
            // selected favorite.
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(5), theme::DEFAULT_BUTTON);
            ui.painter().text(
                rect.left_center() + vec2(10.0, 0.0),
                Align2::LEFT_CENTER,
                host,
                theme::body(),
                theme::ON_ACCENT,
            );
        }
    }
    ui.add_space(14.0);
    caption(ui, "Local");
    ui.label(
        RichText::new("No local hosts")
            .font(theme::small())
            .color(theme::WEAK_TEXT),
    );
    ui.add_space(4.0);
    ui.add_enabled_ui(idle, |ui| {
        if ui
            .button("Browse...")
            .on_hover_text("Open a FileMaker Pro file on this computer")
            .clicked()
        {
            actions.push(Action::ShowOpenFile);
        }
    });
    if client_license {
        ui.add_space(18.0);
        ui.add_enabled_ui(idle, |ui| {
            if ui.button("FileMaker License...").clicked() {
                actions.push(Action::ManageLicense);
            }
            ui.checkbox(&mut view.use_saved_license, "Use saved license");
        });
    }
}

fn files_pane(
    ui: &mut Ui,
    form: &mut ConnectForm,
    view: &mut HostsView,
    status: ConnectStatus,
    listing: &Listing,
    closable: bool,
    actions: &mut Vec<Action>,
) {
    let idle = status == ConnectStatus::Idle;
    let pane = ui.max_rect();

    // Heading: the host (with what it says it is, once listed), and
    // listing its files.
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        let heading = match form.host.trim() {
            "" => "No Host Selected",
            host => host,
        };
        ui.label(
            RichText::new(heading)
                .font(FontId::new(theme::BODY, theme::semibold()))
                .color(theme::TEXT),
        );
        let server = form.server().ok();
        if let Some(about) = listing.files_for(server.as_ref()).and_then(host_identity) {
            ui.label(
                RichText::new(about)
                    .font(theme::small())
                    .color(theme::CAPTION),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(14.0);
            let can_list = idle && form.server().is_ok();
            let reason = form.server().err().unwrap_or_default();
            if dialog_button(ui, "List files", false, can_list)
                .on_hover_text("List the files hosted on this server")
                .on_disabled_hover_text(reason)
                .clicked()
            {
                actions.push(Action::ListFiles);
            }
        });
    });
    ui.add_space(6.0);

    // Tiles/list toggles and Search Files.
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.spacing_mut().item_spacing.x = 4.0;
        for (tiles, icon, label) in [(true, Icon::Tiles, "Tiles"), (false, Icon::Lines, "List")] {
            let selected = view.tiles == tiles;
            let (rect, response) = ui.allocate_exact_size(vec2(24.0, 18.0), Sense::click());
            if selected {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(4), theme::HOVER_FILL);
            }
            icons::paint(
                ui.painter(),
                rect.shrink(3.0),
                icon,
                if selected { theme::TEXT } else { theme::ICON },
                theme::DIALOG_BG,
            );
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::Button, true, selected, label)
            });
            if response.clicked() {
                view.tiles = tiles;
            }
        }
        ui.add_space(10.0);
        let search = ui.add(
            egui::TextEdit::singleline(&mut view.search)
                .hint_text("Search Files")
                .desired_width(220.0),
        );
        widgets::accessible_label(ui, search.id, "Search Files");
    });
    ui.add_space(8.0);

    // The white file area fills what the bottom bar leaves.
    let bottom_height = 86.0;
    let top = ui.cursor().top();
    let files_rect = Rect::from_min_max(
        pos2(pane.left(), top),
        pos2(pane.right(), pane.bottom() - bottom_height),
    );
    ui.painter()
        .rect_filled(files_rect, CornerRadius::ZERO, theme::CONTROL_BG);
    ui.painter().hline(
        files_rect.x_range(),
        files_rect.top(),
        Stroke::new(1.0, theme::HAIRLINE),
    );
    ui.painter().hline(
        files_rect.x_range(),
        files_rect.bottom(),
        Stroke::new(1.0, theme::HAIRLINE),
    );
    let mut files_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(files_rect.shrink(10.0))
            .layout(Layout::top_down(Align::Min)),
    );
    file_area(&mut files_ui, form, view, status, listing, actions);

    let bottom = Rect::from_min_max(
        pos2(pane.left() + 14.0, files_rect.bottom() + 10.0),
        pane.max - vec2(14.0, 10.0),
    );
    let mut bottom_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(bottom)
            .layout(Layout::top_down(Align::Min)),
    );
    credentials(&mut bottom_ui, form, status, closable, actions);
}

/// The host's name and version as it gave them, for the heading.
fn host_identity(listing: &HostListing) -> Option<String> {
    match (listing.host_name.trim(), listing.host_version.trim()) {
        ("", "") => None,
        (name, "") => Some(name.to_owned()),
        ("", version) => Some(version.to_owned()),
        (name, version) => Some(format!("{name} \u{b7} {version}")),
    }
}

/// What the user should know about a listing that failed.
fn listing_failure(ui: &mut Ui, error: &ListingError) {
    let headline = match error.failure {
        ListingFailure::Unreachable => "Could not connect to this host.",
        ListingFailure::Unsupported => "OpenMaker cannot list this host's files.",
        ListingFailure::Malformed => "This host's file list could not be read.",
        ListingFailure::Canceled => "Listing files was canceled.",
        ListingFailure::Other => "The files could not be listed.",
    };
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() / 2.0 - 30.0).max(0.0));
        ui.label(RichText::new(headline).strong().color(theme::TEXT));
        if error.message != headline {
            ui.add_space(4.0);
            ui.label(RichText::new(&error.message).color(theme::CAPTION));
        }
    });
}

fn file_area(
    ui: &mut Ui,
    form: &ConnectForm,
    view: &HostsView,
    status: ConnectStatus,
    listing: &Listing,
    actions: &mut Vec<Action>,
) {
    let message = |ui: &mut Ui, text: &str| {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new(text).color(theme::CAPTION));
        });
    };
    // Only what was listed for the host and port in the form: never the
    // files of a host the form no longer names.
    let server = form.server().ok();
    let files = match listing {
        Listing::Pending {
            server: pending, ..
        } if Some(pending) == server.as_ref() => {
            ui.centered_and_justified(|ui| {
                ui.spinner();
            });
            return;
        }
        Listing::Listed {
            server: listed,
            result,
        } if Some(listed) == server.as_ref() => match result {
            Ok(listing) => &listing.files,
            Err(error) => {
                listing_failure(ui, error);
                return;
            }
        },
        _ => {
            message(
                ui,
                "Enter a host, then choose List files to see its hosted files.",
            );
            return;
        }
    };
    if files.is_empty() {
        message(
            ui,
            "This host lists no files. Enter a file name below to open one.",
        );
        return;
    }
    let search = view.search.trim().to_lowercase();
    let shown: Vec<&HostedFile> = files
        .iter()
        .filter(|file| search.is_empty() || file.name.to_lowercase().contains(&search))
        .collect();
    if shown.is_empty() {
        message(ui, "No files match the search.");
        return;
    }
    let idle = status == ConnectStatus::Idle;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_enabled_ui(idle, |ui| {
                let mut file_row = |ui: &mut Ui, file: &HostedFile, size: Vec2| {
                    let selected = form.file.trim() == file.name;
                    let response = file_entry(ui, &file.name, selected, view.tiles, size)
                        .on_hover_text("Double-click to open");
                    if response.double_clicked() {
                        actions.push(Action::PickFile(file.name.clone()));
                        actions.push(Action::Connect);
                    } else if response.clicked() {
                        actions.push(Action::PickFile(file.name.clone()));
                    }
                };
                if view.tiles {
                    ui.horizontal_wrapped(|ui| {
                        for file in &shown {
                            file_row(ui, file, vec2(120.0, 96.0));
                        }
                    });
                } else {
                    for file in &shown {
                        file_row(ui, file, vec2(ui.available_width(), 24.0));
                    }
                }
            });
        });
}

/// One hosted file, as a list row or a tile with a document glyph.
fn file_entry(ui: &mut Ui, name: &str, selected: bool, tile: bool, size: Vec2) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    let text = if selected {
        theme::ON_ACCENT
    } else {
        theme::TEXT
    };
    if tile {
        let glyph = Rect::from_center_size(rect.center_top() + vec2(0.0, 32.0), vec2(40.0, 44.0));
        if selected {
            painter.rect_filled(glyph.expand(4.0), CornerRadius::same(6), theme::HOVER_FILL);
        }
        icons::paint(
            painter,
            glyph,
            Icon::NewLayout,
            theme::ICON,
            theme::CONTROL_BG,
        );
        let label = Rect::from_center_size(
            rect.center_bottom() - vec2(0.0, 14.0),
            vec2(size.x - 8.0, 18.0),
        );
        if selected {
            painter.rect_filled(label, CornerRadius::same(4), theme::DEFAULT_BUTTON);
        }
        painter.text(
            label.center(),
            Align2::CENTER_CENTER,
            name,
            theme::small(),
            text,
        );
    } else {
        if selected {
            painter.rect_filled(rect, CornerRadius::same(4), theme::DEFAULT_BUTTON);
        }
        icons::paint(
            painter,
            Rect::from_center_size(rect.left_center() + vec2(14.0, 0.0), vec2(16.0, 16.0)),
            Icon::NewLayout,
            if selected {
                theme::ON_ACCENT
            } else {
                theme::ICON
            },
            theme::CONTROL_BG,
        );
        painter.text(
            rect.left_center() + vec2(30.0, 0.0),
            Align2::LEFT_CENTER,
            name,
            theme::body(),
            text,
        );
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, name));
    response
}

/// A right-aligned label in a fixed column, so the fields below line up.
fn form_label(ui: &mut Ui, text: &str) -> egui::Response {
    ui.allocate_ui_with_layout(
        vec2(56.0, 22.0),
        Layout::right_to_left(Align::Center),
        |ui| ui.label(text),
    )
    .inner
}

fn credentials(
    ui: &mut Ui,
    form: &mut ConnectForm,
    status: ConnectStatus,
    closable: bool,
    actions: &mut Vec<Action>,
) {
    let idle = status == ConnectStatus::Idle;
    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
    ui.add_enabled_ui(idle, |ui| {
        ui.horizontal(|ui| {
            let label = form_label(ui, "Account");
            ui.add(
                egui::TextEdit::singleline(&mut form.account)
                    .hint_text("Account name")
                    .desired_width(180.0),
            )
            .labelled_by(label.id);
            ui.add_space(8.0);
            let label = ui.label("Password");
            let response = ui
                .add(
                    egui::TextEdit::singleline(&mut form.password)
                        .password(true)
                        .desired_width(180.0),
                )
                .labelled_by(label.id);
            if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                actions.push(Action::Connect);
            }
        });
    });
    ui.horizontal(|ui| {
        ui.add_enabled_ui(idle, |ui| {
            let label = form_label(ui, "File");
            let response = ui
                .add(
                    egui::TextEdit::singleline(&mut form.file)
                        .hint_text("File name")
                        .desired_width(180.0),
                )
                .labelled_by(label.id);
            if response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
                actions.push(Action::Connect);
            }
        });
        match status {
            ConnectStatus::Idle => {}
            ConnectStatus::Connecting => {
                ui.spinner();
                ui.label(RichText::new("Opening...").color(theme::CAPTION));
            }
            ConnectStatus::ListingFiles => {
                ui.spinner();
                ui.label(RichText::new("Listing files...").color(theme::CAPTION));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let ready = form.connection();
            let reason = ready.as_ref().err().cloned().unwrap_or_default();
            if dialog_button(ui, "Open", true, idle && ready.is_ok())
                .on_disabled_hover_text(reason)
                .clicked()
            {
                actions.push(Action::Connect);
            }
            let busy = status != ConnectStatus::Idle;
            if dialog_button(ui, "Cancel", false, busy || closable).clicked() {
                actions.push(Action::CancelConnect);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> ConnectForm {
        ConnectForm {
            host: " fms.local ".into(),
            port: "5003".into(),
            account: "admin".into(),
            password: "secret".into(),
            file: "Contacts.fmp12".into(),
        }
    }

    #[test]
    fn a_complete_form_builds_a_trimmed_connection() {
        let connection = filled().connection().unwrap();
        assert_eq!(connection.host, "fms.local");
        assert_eq!(connection.port, 5003);
        assert_eq!(connection.file, "Contacts.fmp12");
        assert_eq!(connection.password, "secret");
    }

    #[test]
    fn bad_ports_are_rejected() {
        for port in ["", "0", "70000", "abc", "-1"] {
            let form = ConnectForm {
                port: port.into(),
                ..filled()
            };
            assert!(form.connection().is_err(), "port {port:?} accepted");
        }
    }

    #[test]
    fn host_and_file_are_required() {
        let no_host = ConnectForm {
            host: "  ".into(),
            ..filled()
        };
        assert!(no_host.server().is_err());
        let no_file = ConnectForm {
            file: String::new(),
            ..filled()
        };
        assert!(no_file.server().is_ok());
        assert!(no_file.connection().is_err());
    }
}
