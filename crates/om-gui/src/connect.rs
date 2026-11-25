//! Opening a hosted file: the connection a sign-in sends, a host's file
//! listing, and the dialogs that lead there from Home's Hosts
//! (docs/hosted-file-list.md): Add Host and Edit Host, Open Network File,
//! and the sign-in for a listed, Recent or Favorites file.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use egui::{vec2, Align, FontId, Key, Layout, RichText, Ui};
use om_data::file_refs;
use om_data::{Connection, HostListing, ListingError, ListingFailure};
use zeroize::Zeroizing;

use crate::file_history::{self, SavedHost};
use crate::open_file::forget_field;
use crate::theme;
use crate::widgets::dialog_button;
use crate::worker::Listed;

/// The default FileMaker Server port.
pub(crate) const DEFAULT_PORT: u16 = file_refs::DEFAULT_PORT;

const PORT_RANGE: &str = "Port must be a number from 1 to 65535.";

/// Whether a hosted file is opening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectStatus {
    Idle,
    Connecting,
}

/// The hosted file to open, and the account to open it with.
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

impl From<Connection> for ConnectForm {
    fn from(connection: Connection) -> Self {
        Self {
            host: connection.host,
            port: connection.port.to_string(),
            account: connection.account,
            password: connection.password,
            file: connection.file,
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
        port(&self.port)
    }

    /// A connection for the form's file, or why the form is incomplete.
    pub fn connection(&self) -> Result<Connection, String> {
        let host = self.host()?.to_owned();
        let port = self.port()?;
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

    /// The form's file as an Open Network File path, as far as it is filled
    /// in: empty without a host.
    /// The file and port are written as given, so the path opens what the
    /// form names, and says so when its port is wrong.
    fn path(&self) -> String {
        let file = self.file.trim();
        match (self.host.trim(), self.port()) {
            ("", _) => String::new(),
            (host, Ok(port)) => network_path_text(host, port, file),
            (host, Err(_)) => format!("fmnet:/{host}:{}/{file}", self.port.trim()),
        }
    }
}

/// A path's port: digits only, as `file_refs` reads it.
fn path_port(written: &str) -> Result<u16, String> {
    if written.is_empty() || !written.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PORT_RANGE.into());
    }
    port(written)
}

fn port(text: &str) -> Result<u16, String> {
    match text.trim().parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(PORT_RANGE.into()),
    }
}

/// A host's file list, as Home's host page shows it. A listing belongs to
/// the host and port it was asked of: the page shows it for no other, and a
/// reply to a listing no longer waited for (left, or since asked again) is
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

/// The host's name and version as it gave them, for its page.
pub(crate) fn host_identity(listing: &HostListing) -> Option<String> {
    match (listing.host_name.trim(), listing.host_version.trim()) {
        ("", "") => None,
        (name, "") => Some(name.to_owned()),
        ("", version) => Some(version.to_owned()),
        (name, version) => Some(format!("{name} \u{b7} {version}")),
    }
}

/// What a listing that failed says first.
pub(crate) fn failure_headline(failure: ListingFailure) -> &'static str {
    match failure {
        ListingFailure::Unreachable => "Could not connect to this host",
        ListingFailure::Unsupported => "OpenMaker cannot list this host’s files",
        ListingFailure::Malformed => "This host’s file list could not be read",
        ListingFailure::Canceled => "Listing files was canceled",
        ListingFailure::Other => "The files could not be listed",
    }
}

/// `fmnet:/host[:port]/file`: the Open Network File path of `file` on a
/// host, the port written only when it is not FileMaker's, and an IPv6
/// address in brackets.
pub(crate) fn network_path_text(host: &str, port: u16, file: &str) -> String {
    let authority = match (host.contains(':'), port == DEFAULT_PORT) {
        (true, true) => format!("[{host}]"),
        _ => file_history::location(host, port),
    };
    format!("fmnet:/{authority}/{file}")
}

/// Why `host`, as typed, is no host's address. An account belongs in
/// Account Name: a saved host keeps no account.
fn bad_address(host: &str) -> Option<&'static str> {
    if host.contains('@') {
        Some("Put the account in Account Name, not in the host’s address.")
    } else if host
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '[' | ']'))
    {
        Some("A host’s address can’t contain spaces, /, \\ or brackets.")
    } else {
        None
    }
}

/// What a typed `fmnet:/host[:port]/File` path names: the host, the port
/// (FileMaker's when none is written) and the file, its `.fmp12` added; or
/// what is wrong with it, to show under the field. A path is read as
/// scripts' and related files' paths are ([`file_refs::fmnet_target`]),
/// and checked first for what a typed one gets wrong.
pub(crate) fn network_path(path: &str) -> Result<(String, u16, String), String> {
    const FORM: &str = "fmnet:/host/File";
    let path = path.trim();
    let Some(rest) = path.strip_prefix("fmnet:/") else {
        let bytes = path.as_bytes();
        let drive = bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
        let local = drive
            || path.starts_with("file:")
            || path.starts_with('/')
            || path.starts_with('~')
            || path.starts_with('\\');
        return Err(if local {
            "That is a file on this computer. Use Open.".into()
        } else {
            format!("A network path starts with fmnet:/, as in {FORM}")
        });
    };
    let (authority, name) = rest.split_once('/').unwrap_or((rest, ""));
    // `account:password@host` first: its colons are not a port's either.
    if authority.contains('@') {
        return Err(bad_address(authority).unwrap_or_default().into());
    }
    // An IPv6 address is in brackets, as in a URL: its colons are not a
    // port's. `file_refs` reads no brackets, so this one is read here.
    if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, after) = bracketed
            .split_once(']')
            .ok_or("Close the IPv6 address’s bracket: fmnet:/[::1]/File")?;
        let port = match after {
            "" => DEFAULT_PORT,
            written => path_port(written.strip_prefix(':').ok_or(PORT_RANGE)?)?,
        };
        if host.is_empty() || !host.contains(':') {
            return Err(format!("Add the host: {FORM}"));
        }
        if let Some(reason) = bad_address(host) {
            return Err(reason.into());
        }
        return match name {
            "" => Err(format!("Add the file’s name: {FORM}")),
            name if file_refs::is_plain_name(name) => {
                Ok((host.to_owned(), port, file_refs::hosted_name(name)))
            }
            _ => Err("A file’s name can’t contain /, \\ or :.".into()),
        };
    }
    let host = match authority.rsplit_once(':') {
        Some((host, _)) if host.contains(':') => {
            return Err("Put an IPv6 address in brackets: fmnet:/[::1]/File".into());
        }
        Some((host, written)) => {
            path_port(written)?;
            host
        }
        None => authority,
    };
    if host.is_empty() {
        return Err(format!("Add the host: {FORM}"));
    }
    if let Some(reason) = bad_address(host) {
        return Err(reason.into());
    }
    if name.is_empty() {
        return Err(format!("Add the file’s name: {FORM}"));
    }
    match file_refs::fmnet_target(path) {
        Some((host, port, name)) => Ok((host, port, file_refs::hosted_name(&name))),
        None => Err("A file’s name can’t contain /, \\ or :.".into()),
    }
}

// The dialogs' form: right-aligned labels in a column, the fields after.
const LABEL_WIDTH: f32 = 94.0;
const FIELD_GAP: f32 = 12.0;
const FIELD_HEIGHT: f32 = 22.0;

/// A dialog's frame, as OpenMaker's own dialogs draw it (New Layout's).
fn dialog_frame(ctx: &egui::Context) -> egui::Frame {
    egui::Frame::new()
        .fill(theme::DIALOG_BG)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(theme::WINDOW_RADIUS))
        .shadow(ctx.global_style().visuals.window_shadow)
        .inner_margin(egui::Margin::same(16))
}

/// A modal dialog in OpenMaker's frame, over the window's scrim.
fn modal(id: &str, ctx: &egui::Context) -> egui::Modal {
    egui::Modal::new(egui::Id::unique(id))
        .frame(dialog_frame(ctx))
        .backdrop_color(theme::SCRIM)
}

fn title(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(FontId::new(theme::TITLE3, theme::semibold()))
            .color(theme::TEXT),
    );
}

/// A form row: `label` at the end of the label column, then the field
/// `add` puts there, which the label names.
fn labelled(
    ui: &mut Ui,
    label: &str,
    add: impl FnOnce(&mut Ui) -> egui::Response,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = FIELD_GAP;
        let label = ui
            .allocate_ui_with_layout(
                vec2(LABEL_WIDTH, FIELD_HEIGHT),
                Layout::right_to_left(Align::Center),
                |ui| ui.label(label),
            )
            .inner;
        add(ui).labelled_by(label.id)
    })
    .inner
}

/// Under the fields, lined up with them.
fn indented<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.add_space(LABEL_WIDTH + FIELD_GAP);
        add(ui)
    })
    .inner
}

fn note(text: &str, color: egui::Color32) -> RichText {
    RichText::new(text).font(theme::small()).color(color)
}

/// The dialog's buttons at its trailing edge, from the right: `add` puts
/// the default one first.
fn buttons(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 24.0),
        Layout::right_to_left(Align::Center),
        add,
    );
}

/// Escape, for a dialog with nothing in front of it (`top`): an alert over
/// it takes the key itself.
fn escape(ctx: &egui::Context, top: bool) -> bool {
    top && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
}

/// Forget what egui keeps of Open Network File's or the sign-in's password
/// field, as the dialog goes.
pub(crate) fn forget_password(ctx: &egui::Context) {
    forget_field(ctx, egui::Id::unique(PASSWORD_ID));
}

/// What a host dialog did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostAction {
    Save {
        replacing: Option<(String, u16)>,
        host: SavedHost,
    },
    Cancel,
}

/// Add Host and Edit Host: a host's address, port, and the optional name
/// the sidebar shows for it.
pub(crate) struct HostDialog {
    /// The host edited, at the address and port it had; `None` adds one.
    editing: Option<(String, u16)>,
    pub address: String,
    pub port: String,
    pub name: String,
    focus: bool,
}

const HOST_DIALOG_WIDTH: f32 = 400.0;

impl HostDialog {
    pub fn add() -> Self {
        Self {
            editing: None,
            address: String::new(),
            port: DEFAULT_PORT.to_string(),
            name: String::new(),
            focus: true,
        }
    }

    pub fn edit(host: &SavedHost) -> Self {
        Self {
            editing: Some(host.server()),
            address: host.address.clone(),
            port: host.port.to_string(),
            name: host.name.clone(),
            focus: true,
        }
    }

    /// The host the fields describe, or why they do not.
    fn host(&self) -> Result<SavedHost, String> {
        // An IPv6 address as a path writes it, in brackets: the brackets
        // are the path's, not the address's.
        let address = self.address.trim();
        let address = address
            .strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(address);
        if address.is_empty() {
            return Err("Enter a host name or address.".into());
        }
        if let Some(reason) = bad_address(address) {
            return Err(reason.into());
        }
        Ok(SavedHost {
            address: address.to_owned(),
            port: port(&self.port)?,
            name: self.name.trim().to_owned(),
        })
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<HostAction> {
        let mut action = None;
        let (heading, save) = match self.editing {
            None => ("Add Host", "Add"),
            Some(_) => ("Edit Host", "Save"),
        };
        let shown = modal("om_host_dialog", ctx).show(ctx, |ui| {
            ui.set_width(HOST_DIALOG_WIDTH);
            ui.spacing_mut().item_spacing.y = 8.0;
            title(ui, heading);
            ui.add_space(6.0);
            let mut entered = false;
            let mut field = |ui: &mut Ui, label: &str, text: &mut String, width: f32| {
                let response = labelled(ui, label, |ui| {
                    ui.add(egui::TextEdit::singleline(text).desired_width(width))
                });
                entered |= response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                response
            };
            let address = field(ui, "Address", &mut self.address, 260.0);
            if std::mem::take(&mut self.focus) {
                address.request_focus();
            }
            field(ui, "Port", &mut self.port, 56.0);
            field(ui, "Name", &mut self.name, 260.0);
            // Read after this frame's typing, so the dialog's height does
            // not change a frame late.
            let host = self.host();
            indented(ui, |ui| {
                ui.label(note(
                    "Optional. Shown in the sidebar instead of the address.",
                    theme::CAPTION,
                ));
            });
            // Why Add waits, once there is something to say it of.
            if let Err(reason) = &host {
                if !self.address.trim().is_empty() || port(&self.port).is_err() {
                    indented(ui, |ui| ui.label(note(reason, theme::RED_INK)));
                }
            }
            ui.add_space(4.0);
            buttons(ui, |ui| {
                let ready = host.is_ok();
                if dialog_button(ui, save, true, ready).clicked() || (entered && ready) {
                    if let Ok(host) = &host {
                        action = Some(HostAction::Save {
                            replacing: self.editing.clone(),
                            host: host.clone(),
                        });
                    }
                }
                if dialog_button(ui, "Cancel", false, true).clicked() {
                    action = Some(HostAction::Cancel);
                }
            });
        });
        if escape(ctx, shown.is_top_modal) {
            action = Some(HostAction::Cancel);
        }
        action
    }
}

/// What a hosted file's dialog opens.
#[derive(Clone, Debug)]
enum NetworkKind {
    /// Open Network File: the file a typed path names.
    Path,
    /// A listed, Recent or Favorites file: its sign-in.
    SignIn {
        host: String,
        port: u16,
        file: String,
    },
}

/// What a hosted file's dialog did.
#[derive(Clone, Debug)]
pub(crate) enum NetworkAction {
    Open {
        connection: Connection,
        /// Add the file's host to Hosts.
        save_host: Option<SavedHost>,
    },
    Cancel,
}

/// What the hosted file's dialog shows this frame.
pub(crate) struct NetworkView<'a> {
    /// The provider can use a saved FileMaker license.
    pub client_license: bool,
    /// The file is opening: the dialog waits, and Cancel stops it.
    pub connecting: bool,
    /// Home's saved hosts; `None` without Home, which has no Hosts to add
    /// the file's host to.
    pub saved: Option<&'a [SavedHost]>,
    /// An alert is up, or comes up this frame, over the dialog: Escape is
    /// the alert's. egui knows a modal is in front only from the frame
    /// after it is first drawn.
    pub covered: bool,
}

/// Open Network File, or the sign-in for a hosted file: the account and
/// password to open it with, and, for Open Network File, its
/// `fmnet:/host/File` path.
pub(crate) struct NetworkDialog {
    kind: NetworkKind,
    pub path: String,
    pub account: String,
    pub password: Zeroizing<String>,
    pub use_saved_license: bool,
    /// The host and port Add to Hosts was ticked for: another path's host
    /// starts unticked.
    save_host: Option<(String, u16)>,
    /// Cancel closes it. Without Home it is the window's launcher, and
    /// Cancel only stops a file opening.
    closable: bool,
    focus: bool,
}

const NETWORK_DIALOG_WIDTH: f32 = 440.0;
const PATH_WIDTH: f32 = 300.0;
const PASSWORD_ID: &str = "om_network_password";

impl NetworkDialog {
    /// Open Network File, its path filled in with `path`.
    pub fn path(path: String, use_saved_license: bool) -> Self {
        Self {
            kind: NetworkKind::Path,
            path,
            account: String::new(),
            password: Zeroizing::new(String::new()),
            use_saved_license,
            save_host: None,
            closable: true,
            focus: true,
        }
    }

    /// Open Network File filled in from `form`: the window's launcher
    /// without Home, and a startup's `OPENMAKER_*` prefill.
    pub fn from_form(form: &ConnectForm, use_saved_license: bool) -> Self {
        Self {
            account: form.account.clone(),
            ..Self::path(form.path(), use_saved_license)
        }
    }

    /// The sign-in for `file` on `host`, as a listed or saved file opens.
    pub fn sign_in(host: String, port: u16, file: String, use_saved_license: bool) -> Self {
        Self {
            kind: NetworkKind::SignIn { host, port, file },
            ..Self::path(String::new(), use_saved_license)
        }
    }

    /// Whether Cancel closes it: not as the window's launcher.
    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    /// The host, port and file the dialog opens, or what is wrong with
    /// the path.
    fn target(&self) -> Result<(String, u16, String), String> {
        match &self.kind {
            NetworkKind::Path => network_path(&self.path),
            NetworkKind::SignIn { host, port, file } => Ok((host.clone(), *port, file.clone())),
        }
    }

    /// Open Network File or the sign-in over the window, as a modal dialog.
    pub fn show(&mut self, ctx: &egui::Context, view: NetworkView<'_>) -> Option<NetworkAction> {
        let mut action = None;
        let shown = modal("om_network_dialog", ctx).show(ctx, |ui| {
            self.contents(ui, &view, &mut action);
        });
        self.finish(ctx, &view, action, shown.is_top_modal)
    }

    /// Open Network File as the window's launcher, without Home: a dialog
    /// drawn in `ui` over the pasteboard, which leaves the window's menus
    /// working, as Home does.
    pub fn show_inline(&mut self, ui: &mut Ui, view: NetworkView<'_>) -> Option<NetworkAction> {
        let mut action = None;
        let area = ui.max_rect();
        let frame = dialog_frame(ui.ctx());
        let width = NETWORK_DIALOG_WIDTH + frame.total_margin().sum().x;
        let top = area.top() + (area.height() / 2.0 - 160.0).max(24.0);
        let rect = egui::Rect::from_min_max(
            egui::pos2(area.center().x - width / 2.0, top),
            egui::pos2(area.center().x + width / 2.0, area.bottom()),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            frame.show(ui, |ui| self.contents(ui, &view, &mut action));
        });
        let top = ui.ctx().memory(|m| m.top_modal_layer().is_none());
        self.finish(ui.ctx(), &view, action, top)
    }

    pub fn is_closable(&self) -> bool {
        self.closable
    }

    /// Escape cancels while nothing is in front of the dialog (`top`); and
    /// once the dialog is done, egui forgets the password field's text.
    fn finish(
        &self,
        ctx: &egui::Context,
        view: &NetworkView<'_>,
        mut action: Option<NetworkAction>,
        top: bool,
    ) -> Option<NetworkAction> {
        if (view.connecting || self.closable) && !view.covered && escape(ctx, top) {
            action = Some(NetworkAction::Cancel);
        }
        if action.is_some() {
            forget_password(ctx);
        }
        action
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        view: &NetworkView<'_>,
        action: &mut Option<NetworkAction>,
    ) {
        let idle = !view.connecting;
        ui.set_width(NETWORK_DIALOG_WIDTH);
        ui.spacing_mut().item_spacing.y = 8.0;
        let mut entered = false;
        let mut first = None;
        match &self.kind {
            NetworkKind::Path => {
                title(ui, "Open Network File");
                ui.add_space(6.0);
                let path = labelled(ui, "Path", |ui| {
                    ui.add_enabled(
                        idle,
                        egui::TextEdit::singleline(&mut self.path)
                            .hint_text("fmnet:/host/File")
                            .desired_width(PATH_WIDTH),
                    )
                });
                entered |= path.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                first = Some(path);
                // Read after this frame's typing.
                let target = self.target();
                let line = match (&target, self.path.trim()) {
                    (_, "") => note("A file on a host: fmnet:/host/File", theme::CAPTION),
                    (Ok((host, port, file)), _) => note(
                        &format!(
                            "File “{}” on {host}, port {port}",
                            file_refs::file_stem(file)
                        ),
                        theme::CAPTION,
                    ),
                    (Err(reason), _) => note(reason, theme::RED_INK),
                };
                indented(ui, |ui| ui.label(line));
            }
            NetworkKind::SignIn { host, port, file } => {
                let name = file_refs::file_stem(file);
                title(ui, &format!("Open “{name}”"));
                ui.label(format!(
                    "Sign in to “{name}” on {} with:",
                    file_history::location(host, *port)
                ));
                ui.add_space(2.0);
            }
        }
        let target = self.target();
        // Offered for a host Home does not have yet.
        let unsaved = match (&self.kind, view.saved, &target) {
            (NetworkKind::Path, Some(saved), Ok((host, port, _))) => {
                (!saved.iter().any(|saved| saved.is(host, *port))).then(|| SavedHost {
                    address: host.clone(),
                    port: *port,
                    name: String::new(),
                })
            }
            _ => None,
        };
        let account = labelled(ui, "Account Name", |ui| {
            ui.add_enabled(
                idle,
                egui::TextEdit::singleline(&mut self.account).desired_width(PATH_WIDTH),
            )
        });
        entered |= account.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let password = labelled(ui, "Password", |ui| {
            ui.add_enabled(
                idle,
                egui::TextEdit::singleline(&mut *self.password)
                    .id(egui::Id::unique(PASSWORD_ID))
                    .password(true)
                    .desired_width(PATH_WIDTH),
            )
        });
        entered |= password.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if std::mem::take(&mut self.focus) {
            first.unwrap_or(account).request_focus();
        }
        if view.client_license {
            indented(ui, |ui| {
                ui.add_enabled(
                    idle,
                    egui::Checkbox::new(&mut self.use_saved_license, "Use saved FileMaker license"),
                );
            });
        }
        let mut save_host = false;
        if let Some(host) = &unsaved {
            save_host = self.save_host.as_ref() == Some(&host.server());
            indented(ui, |ui| {
                ui.add_enabled(
                    idle,
                    egui::Checkbox::new(
                        &mut save_host,
                        format!("Add {} to Hosts", host.location()),
                    ),
                );
            });
            self.save_host = save_host.then(|| host.server());
        }
        ui.add_space(4.0);
        buttons(ui, |ui| {
            let ready = idle && target.is_ok();
            let open = match self.kind {
                NetworkKind::Path => "Open",
                NetworkKind::SignIn { .. } => "Sign in",
            };
            let reason = target.as_ref().err().cloned().unwrap_or_default();
            if dialog_button(ui, open, true, ready)
                .on_disabled_hover_text(reason)
                .clicked()
                || (entered && ready)
            {
                if let Ok((host, port, file)) = &target {
                    *action = Some(NetworkAction::Open {
                        connection: Connection {
                            host: host.clone(),
                            port: *port,
                            file: file.clone(),
                            account: self.account.trim().to_owned(),
                            password: self.password.to_string(),
                        },
                        save_host: unsaved.clone().filter(|_| save_host),
                    });
                }
            }
            if dialog_button(ui, "Cancel", false, view.connecting || self.closable).clicked() {
                *action = Some(NetworkAction::Cancel);
            }
            if view.connecting {
                ui.label(RichText::new("Opening…").color(theme::CAPTION));
                ui.spinner();
            }
        });
    }
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
        for port in ["", "0", "70000", "abc", "-1"] {
            let form = ConnectForm {
                port: port.into(),
                ..filled()
            };
            assert!(form.connection().is_err(), "port {port:?} accepted");
        }
    }

    /// What Open Network File reads a typed path as, and what it says of
    /// one it cannot open.
    #[test]
    fn network_paths_name_a_host_port_and_file() {
        let opens = |host: &str, port: u16, file: &str| Ok((host.into(), port, file.into()));
        for (path, read) in [
            (
                "fmnet:/fms.example.com/Payroll",
                opens("fms.example.com", 5003, "Payroll.fmp12"),
            ),
            (
                " fmnet:/db.example.net:5004/Archive.fmp12 ",
                opens("db.example.net", 5004, "Archive.fmp12"),
            ),
            (
                "fmnet:/fms.example.com",
                Err("Add the file’s name: fmnet:/host/File".into()),
            ),
            (
                "fmnet:/fms.example.com/",
                Err("Add the file’s name: fmnet:/host/File".into()),
            ),
            (
                "fmnet://Payroll",
                Err("Add the host: fmnet:/host/File".into()),
            ),
            ("fmnet:/fms.example.com:0/Payroll", Err(PORT_RANGE.into())),
            (
                "fmnet:/fms.example.com:70000/Payroll",
                Err(PORT_RANGE.into()),
            ),
            ("fmnet:/fms.example.com:x/Payroll", Err(PORT_RANGE.into())),
            (
                "fmnet:/fms.example.com/Folder/Payroll",
                Err("A file’s name can’t contain /, \\ or :.".into()),
            ),
            (
                "/Users/me/Payroll.fmp12",
                Err("That is a file on this computer. Use Open.".into()),
            ),
            (
                "file:Payroll.fmp12",
                Err("That is a file on this computer. Use Open.".into()),
            ),
            ("fmnet:/[::1]/Payroll", opens("::1", 5003, "Payroll.fmp12")),
            (
                "fmnet:/[fe80::1]:5004/Payroll",
                opens("fe80::1", 5004, "Payroll.fmp12"),
            ),
            (
                "fmnet:/::1/Payroll",
                Err("Put an IPv6 address in brackets: fmnet:/[::1]/File".into()),
            ),
            ("fmnet:/localhost: 5004/Payroll", Err(PORT_RANGE.into())),
            ("fmnet:/[::1]: 5004/Payroll", Err(PORT_RANGE.into())),
            (
                "fmnet:/demo:secret@db.example.test:5004/Payroll",
                Err("Put the account in Account Name, not in the host’s address.".into()),
            ),
            (
                "fms.example.com/Payroll",
                Err("A network path starts with fmnet:/, as in fmnet:/host/File".into()),
            ),
        ] {
            assert_eq!(network_path(path), read, "{path:?}");
        }
        assert_eq!(
            network_path_text("db.example.net", 5004, "Archive"),
            "fmnet:/db.example.net:5004/Archive"
        );
        assert_eq!(
            network_path_text("fms.example.com", DEFAULT_PORT, ""),
            "fmnet:/fms.example.com/"
        );
        assert_eq!(
            network_path_text("::1", DEFAULT_PORT, "A"),
            "fmnet:/[::1]/A"
        );
        assert_eq!(network_path_text("::1", 5004, "A"), "fmnet:/[::1]:5004/A");
        // Add Host takes an IPv6 address as a path writes it, and saves it
        // without the path's brackets, so its paths open it.
        let added = HostDialog {
            address: " [::1] ".into(),
            ..HostDialog::add()
        };
        let saved = added.host().unwrap();
        assert_eq!(saved.address, "::1");
        assert_eq!(
            network_path(&network_path_text(&saved.address, saved.port, "A")),
            opens("::1", 5003, "A.fmp12")
        );
        // A launch's file and port, as given: one extension is not taken
        // off a name that has two, and a bad port is shown, not replaced.
        let form = |file: &str, port: &str| ConnectForm {
            file: file.into(),
            port: port.into(),
            ..filled()
        };
        let path = |form: ConnectForm| network_path(&form.path());
        assert_eq!(
            path(form("Archive.fmp12.fmp12", "5003")),
            opens("fms.local", 5003, "Archive.fmp12.fmp12")
        );
        assert_eq!(path(form("Contacts.fmp12", "0")), Err(PORT_RANGE.into()));
    }
}
