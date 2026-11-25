//! OpenMaker's Home window, laid out as FileMaker Pro 26.1.1's Home
//! (docs/re/client-ui.md "Home window"): the app's name in a header band;
//! Create, Open and the page list in a sidebar; and the page itself, with
//! Favorites and Recent tabs over a Name and Location table. Its commands
//! use the same actions as the File menu. Geometry and colors are measured
//! from FileMaker; where FileMaker has Claris's FileMaker Cloud and
//! Resources pages, OpenMaker lists the saved hosts, each a page of the
//! files it lists (docs/hosted-file-list.md).

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, FocusDirection, FontFamily, FontId, Galley,
    Id, Key, Layout, Modifiers, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder,
};
use om_data::file_refs::file_stem;
use om_data::HostedFile;

use crate::app::Action;
use crate::connect::{self, Listing};
use crate::file_history::{Change, History, SavedFile, SavedHost, Tab};
use crate::icons::{self, Icon};
use crate::theme;
use crate::widgets;

// FileMaker's Home, in points from the top-left of the window's content
// (below the title bar). Main-pane x positions are from the sidebar's edge;
// the title's and tabs' allow for Inter's side bearings, so their ink lands
// where FileMaker's does.
const HEADER_HEIGHT: f32 = 51.0;
const WORDMARK_X: f32 = 22.0;
const SIDEBAR_WIDTH: f32 = 198.0;
const CAPSULE: Rect = Rect::from_min_max(pos2(25.0, 84.0), pos2(169.0, 112.0));
const CAPSULE_PITCH: f32 = 40.0;
const SIDEBAR_RULE_Y: f32 = 179.5;
const NAV: Rect = Rect::from_min_max(pos2(20.0, 198.0), pos2(176.0, 228.0));
const NAV_ICON_X: f32 = 40.0;
const NAV_ICON: f32 = 17.0;
const NAV_TEXT_X: f32 = 58.5;
const TITLE_X: f32 = 27.0;
const TITLE_Y: f32 = 103.75;
const TAB_X: f32 = 21.0;
const TAB_GAP: f32 = 12.0;
const TAB_Y: f32 = 140.75;
const TAB_RULE_Y: f32 = 152.0;
const TAB_RULE: f32 = 2.0;
const TABLE_X: f32 = 13.0;
const TABLE_RIGHT_INSET: f32 = 38.0;
const COLUMN_RULE_TOP: f32 = 167.5;
const HEADER_RULE_Y: f32 = 189.5;
const HEADER_TEXT_Y: f32 = 175.5;
const NAME_COLUMN: f32 = 252.0;
const TRAILING_COLUMN: f32 = 13.0;
const HEADER_NAME_X: f32 = 21.0;
const HEADER_LOCATION_X: f32 = 14.0;
const ROWS_TOP: f32 = 194.5;
const ROW_HEIGHT: f32 = 50.0;
const ROW_ICON_X: f32 = 19.0;
const ROW_ICON: f32 = 32.0;
const ROW_NAME_X: f32 = 59.5;
const ROW_LOCATION_X: f32 = 11.0;
const CELL_PADDING: f32 = 11.0;

// OpenMaker's Hosts section, under Home in the sidebar, in the sidebar's
// own measures: a caption with an add button, then the hosts as pages.
const HOSTS_CAPTION: egui::Pos2 = pos2(31.5, 249.0);
const ADD_HOST: Rect = Rect::from_min_max(pos2(153.0, 240.0), pos2(171.0, 258.0));
const HOSTS_TOP: f32 = 266.0;
const HOST_PITCH: f32 = 34.0;
// The host page's refresh button and Search Files, from the table's right
// edge, on the tabs' line.
const REFRESH: f32 = 22.0;
const REFRESH_RIGHT: f32 = 174.0;
const SEARCH_WIDTH: f32 = 160.0;
const SEARCH_HEIGHT: f32 = 26.0;
const PAGE_CAPSULE_HEIGHT: f32 = 28.0;

/// What Home shows and may do this frame.
pub(crate) struct HomeView<'a> {
    pub history: &'a History,
    /// The file list of the host asked last.
    pub listing: &'a Listing,
    /// A file is opening: Home's commands wait for it.
    pub busy: bool,
    /// No dialog or alert is in front of Home: its controls and keys work.
    pub in_front: bool,
    /// Why Recent, Favorites and Hosts could not be read or saved, when
    /// they could not; the lists then last until the app quits.
    pub history_error: Option<&'a str>,
}

/// Which page Home shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Page {
    /// Home: Recent and Favorites.
    #[default]
    Files,
    /// Hosts, while no host is saved.
    Hosts,
    /// A saved host's files.
    Host((String, u16)),
}

/// Home's own state: the page shown, and the selected row of its list.
#[derive(Default)]
pub(crate) struct Home {
    page: Page,
    /// The host shown last, which Show Hosts goes back to.
    last_host: Option<(String, u16)>,
    selected: Option<SavedFile>,
    reveal_selected: bool,
    /// The host page's selected file, by its name on the host.
    host_file: Option<String>,
    reveal_host_file: bool,
    /// The host page's Search Files.
    search: String,
    /// A host row was clicked last: Delete removes that host.
    host_armed: bool,
    /// The host List Files asked of this frame.
    asked: Option<(String, u16)>,
}

impl Home {
    /// Show Home's own page.
    pub fn show_files(&mut self) {
        self.page = Page::Files;
    }

    /// Forget the selection of Home's list, whose tab changed.
    pub fn forget_selection(&mut self) {
        self.selected = None;
        self.reveal_selected = false;
    }

    /// Home's own page with nothing selected, as a file opens. Show Hosts
    /// still goes back to the host shown last.
    pub fn reset(&mut self) {
        *self = Self {
            last_host: self.last_host.take(),
            ..Self::default()
        };
    }

    /// Whether a page of Hosts shows.
    #[cfg(test)]
    pub fn on_hosts(&self) -> bool {
        matches!(self.page, Page::Hosts | Page::Host(_))
    }

    /// Show Hosts: the host shown last, if it is still saved, or else the
    /// first.
    pub fn show_hosts(&mut self, hosts: &[SavedHost]) {
        let last = self
            .last_host
            .as_ref()
            .and_then(|(address, port)| hosts.iter().find(|host| host.is(address, *port)));
        match last.or(hosts.first()) {
            Some(host) => self.show_host(host.server()),
            None => self.page = Page::Hosts,
        }
    }

    /// Show the page of the saved host at `server`.
    pub fn show_host(&mut self, server: (String, u16)) {
        if self.page != Page::Host(server.clone()) {
            self.host_file = None;
            self.search.clear();
        }
        self.last_host = Some(server.clone());
        self.page = Page::Host(server);
    }

    /// The host whose page is shown.
    pub fn shown_host(&self) -> Option<&(String, u16)> {
        match &self.page {
            Page::Host(server) => Some(server),
            _ => None,
        }
    }

    pub fn show(&mut self, ui: &mut Ui, view: HomeView<'_>, actions: &mut Vec<Action>) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::CONTENT_BG))
            .show(ui, |ui| self.contents(ui, &view, actions));
    }

    /// A host page whose host is gone shows another (a removed host's, say).
    /// A host renamed in another instance, the case of its address
    /// included, keeps its page, under the address as saved.
    fn settle_page(&mut self, hosts: &[SavedHost]) {
        let saved = match &self.page {
            Page::Host((address, port)) => Some(
                hosts
                    .iter()
                    .find(|host| host.is(address, *port))
                    .map(SavedHost::server),
            ),
            _ => None,
        };
        match saved {
            Some(Some(server)) if self.page != Page::Host(server.clone()) => {
                self.last_host = Some(server.clone());
                self.page = Page::Host(server);
            }
            Some(None) => {
                self.last_host = None;
                self.show_hosts(hosts);
            }
            _ if self.page == Page::Hosts && !hosts.is_empty() => self.show_hosts(hosts),
            _ => {}
        }
    }

    fn contents(&mut self, ui: &mut Ui, view: &HomeView<'_>, actions: &mut Vec<Action>) {
        let hosts = view.history.hosts();
        self.settle_page(hosts);
        self.asked = None;
        // Delete removes a host only right after a click on it: any other
        // press, or the keyboard focus going elsewhere, disarms it.
        if ui.input(|input| input.pointer.any_pressed()) {
            self.host_armed = false;
        }
        let area = ui.max_rect();
        let at = |x: f32, y: f32| area.min + vec2(x, y);
        let painter = ui.painter().clone();
        let rule = Stroke::new(1.0, theme::SEPARATOR);
        let enabled = !view.busy && view.in_front;

        // The header band, with the app's name where FileMaker has its logo.
        painter.text(
            at(WORDMARK_X, HEADER_HEIGHT / 2.0),
            Align2::LEFT_CENTER,
            crate::APP_TITLE,
            FontId::new(theme::TITLE2, theme::semibold()),
            theme::TEXT,
        );
        painter.hline(area.x_range(), at(0.0, HEADER_HEIGHT + 0.5).y, rule);

        // The sidebar: Create and Open, a rule, then the pages.
        painter.vline(
            at(SIDEBAR_WIDTH, 0.0).x,
            (area.top() + HEADER_HEIGHT + 1.0)..=area.bottom(),
            rule,
        );
        for (index, (label, action)) in [
            ("Create", Action::ShowCreateNew),
            ("Open", Action::ShowOpenFile),
        ]
        .into_iter()
        .enumerate()
        {
            let rect =
                CAPSULE.translate(area.min.to_vec2() + vec2(0.0, CAPSULE_PITCH * index as f32));
            if capsule(ui, rect, label, enabled).clicked() {
                actions.push(action);
            }
        }
        painter.hline(
            area.left()..=at(SIDEBAR_WIDTH - 0.5, 0.0).x,
            at(0.0, SIDEBAR_RULE_Y).y,
            rule,
        );
        let tab = view.history.tab();
        let home = NAV.translate(area.min.to_vec2());
        let response = navigation(
            ui,
            home,
            Id::unique("home_page"),
            "Home",
            Icon::Home,
            self.page == Page::Files,
            enabled,
        );
        if response.clicked() {
            self.host_armed = false;
            actions.push(Action::ShowHome(tab));
        }
        self.hosts_section(ui, area, hosts, enabled, actions);

        // The page: its title, the tabs, and the selected list.
        let pane = Rect::from_min_max(at(SIDEBAR_WIDTH, HEADER_HEIGHT), area.max);
        let table = Rect::from_min_max(
            pos2(pane.left() + TABLE_X, at(0.0, COLUMN_RULE_TOP).y),
            pos2(area.right() - TABLE_RIGHT_INSET, area.bottom()),
        );
        let host = match &self.page {
            Page::Host((address, port)) => hosts.iter().find(|host| host.is(address, *port)),
            _ => None,
        };
        match (&self.page, host) {
            (Page::Host(_), Some(host)) => {
                let host = host.clone();
                self.host_page(ui, area, table, &host, view, actions);
            }
            (Page::Hosts, _) | (Page::Host(_), None) => {
                page_title(ui, area, "Hosts");
                column_header(ui, table, None);
                let mut rows_ui = rows_ui(ui, table);
                empty(
                    &mut rows_ui,
                    "No saved hosts",
                    "Add a host to list the files it shares.",
                );
                if page_capsule(&mut rows_ui, "Add Host...", enabled).clicked() {
                    actions.push(Action::EditHost(None));
                }
            }
            (Page::Files, _) => {
                page_title(ui, area, "Home");
                let mut x = pane.left() + TAB_X;
                for shown in [Tab::Favorites, Tab::Recent] {
                    let (response, text) = tab_button(
                        ui,
                        pos2(x, at(0.0, TAB_Y).y),
                        at(0.0, TAB_RULE_Y).y,
                        shown,
                        shown == tab,
                        enabled,
                    );
                    if response.clicked() && shown != tab {
                        actions.push(Action::ShowHome(shown));
                    }
                    x = text.right() + TAB_GAP;
                }
                status(ui, view, pos2(table.right(), at(0.0, TAB_Y).y));
                self.table(ui, table, view, actions);
            }
        }
        self.host_keys(ui, hosts, enabled, actions);
    }

    /// The saved hosts, under a Hosts caption with its add button. A
    /// host's context menu lists its files again, opens one by name, edits
    /// the host or removes it.
    fn hosts_section(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        hosts: &[SavedHost],
        enabled: bool,
        actions: &mut Vec<Action>,
    ) {
        let at = |point: egui::Pos2| area.min + point.to_vec2();
        ui.painter().text(
            at(HOSTS_CAPTION),
            Align2::LEFT_CENTER,
            "Hosts",
            FontId::new(theme::SMALL, theme::semibold()),
            theme::CAPTION,
        );
        if add_button(ui, ADD_HOST.translate(area.min.to_vec2()), enabled).clicked() {
            actions.push(Action::EditHost(None));
        }
        if hosts.is_empty() {
            ui.painter().text(
                at(pos2(HOSTS_CAPTION.x, HOSTS_TOP + 10.0)),
                Align2::LEFT_CENTER,
                "No saved hosts",
                theme::small(),
                theme::WEAK_TEXT,
            );
            return;
        }
        let list = Rect::from_min_max(
            at(pos2(NAV.left(), HOSTS_TOP)),
            pos2(at(pos2(NAV.right(), 0.0)).x, area.bottom() - 8.0),
        );
        let mut list_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(list)
                .layout(Layout::top_down(Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt("home_hosts")
            .auto_shrink([false, false])
            .show(&mut list_ui, |ui| {
                ui.spacing_mut().item_spacing.y = HOST_PITCH - NAV.height();
                for host in hosts {
                    let server = host.server();
                    let (rect, _) = ui.allocate_exact_size(NAV.size(), Sense::hover());
                    let selected = self.page == Page::Host(server.clone());
                    let response = navigation(
                        ui,
                        rect,
                        Id::unique(("home_host", &server)),
                        &host.label(),
                        Icon::Server,
                        selected,
                        enabled,
                    )
                    .on_hover_text(host.location());
                    // A right click only opens the host's menu.
                    if response.clicked() {
                        self.host_armed = true;
                        self.show_host(server.clone());
                    }
                    if enabled {
                        response.context_menu(|ui| {
                            // List Files shows the host it lists.
                            if host_menu(ui, host, actions) {
                                self.show_host(server.clone());
                                self.asked = Some(server.clone());
                            }
                        });
                    }
                }
            });
    }

    /// Delete removes the host clicked last, or the one whose row has the
    /// keyboard focus, as Remove Host does.
    fn host_keys(
        &mut self,
        ui: &Ui,
        hosts: &[SavedHost],
        enabled: bool,
        actions: &mut Vec<Action>,
    ) {
        let Some((address, port)) = self.shown_host().cloned() else {
            return;
        };
        let row = Id::unique(("home_host", &(address.clone(), port)));
        let focus = ui.ctx().memory(|memory| memory.focused());
        if focus.is_some_and(|id| id != row) {
            self.host_armed = false;
        }
        let armed = (self.host_armed && focus.is_none()) || focus == Some(row);
        if !enabled
            || !armed
            || egui::Popup::is_any_open(ui.ctx())
            || ui.ctx().memory(|memory| memory.top_modal_layer().is_some())
            || !hosts.iter().any(|host| host.is(&address, port))
        {
            return;
        }
        let remove = ui.input_mut(|input| {
            input.consume_key(Modifiers::NONE, Key::Delete)
                || input.consume_key(Modifiers::NONE, Key::Backspace)
        });
        if remove {
            self.host_armed = false;
            actions.push(Action::EditHistory(Change::RemoveHost(address, port)));
        }
    }

    /// A saved host's page: its name, where it is and what it says it is,
    /// and the files it lists, which it is asked for when the page first
    /// shows.
    fn host_page(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        table: Rect,
        host: &SavedHost,
        view: &HomeView<'_>,
        actions: &mut Vec<Action>,
    ) {
        let server = host.server();
        let enabled = !view.busy && view.in_front;
        let listing = view.listing;
        // Asked once: by the page, or by List Files this frame.
        let asked = listing.server() == Some(&server) || self.asked.as_ref() == Some(&server);
        if !asked && enabled {
            actions.push(Action::ListFiles(server.clone()));
        }
        let listed = listing.files_for(Some(&server));
        page_title(ui, area, &host.label());
        let line_y = area.top() + TAB_Y;
        let caption = match listed.and_then(connect::host_identity) {
            Some(identity) => format!("{}  \u{b7}  {identity}", host.location()),
            None => host.location(),
        };
        let pane_left = area.left() + SIDEBAR_WIDTH;
        let search = Rect::from_min_size(
            pos2(table.right() - SEARCH_WIDTH, line_y - SEARCH_HEIGHT / 2.0),
            vec2(SEARCH_WIDTH, SEARCH_HEIGHT),
        );
        let refresh = Rect::from_center_size(
            pos2(table.right() - REFRESH_RIGHT - REFRESH / 2.0, line_y),
            vec2(REFRESH, REFRESH),
        );
        widgets::put_label(
            ui,
            Rect::from_min_max(
                pos2(pane_left + TAB_X, line_y - 10.0),
                pos2(refresh.left() - 8.0, line_y + 10.0),
            ),
            &caption,
            theme::body(),
            theme::CAPTION,
            Align::Min,
        );
        let again = enabled && listing.server() == Some(&server) && !listing.pending();
        if refresh_button(ui, refresh, again).clicked() {
            actions.push(Action::ListFiles(server.clone()));
        }
        search_field(ui, search, &mut self.search, enabled && listed.is_some());
        if view.busy {
            status(ui, view, pos2(refresh.left() - 10.0, line_y));
        }

        column_header(ui, table, None);
        let mut rows_ui = rows_ui(ui, table);
        let files = match listing {
            Listing::Listed {
                server: of,
                result: Err(error),
            } if *of == server => {
                empty(
                    &mut rows_ui,
                    connect::failure_headline(error.failure),
                    &error.message,
                );
                if page_capsule(&mut rows_ui, "Open Network File...", enabled).clicked() {
                    actions.push(Action::OpenNetworkFile(connect::network_path_text(
                        &server.0, server.1, "",
                    )));
                }
                return;
            }
            _ => match listed {
                Some(listing) => &listing.files,
                None => {
                    listing_files(&mut rows_ui);
                    return;
                }
            },
        };
        let search = self.search.trim();
        let shown: Vec<&HostedFile> = files
            .iter()
            .filter(|file| {
                search.is_empty() || file.name.to_lowercase().contains(&search.to_lowercase())
            })
            .collect();
        if shown.is_empty() {
            let (heading, open, path) = if search.is_empty() {
                (
                    "This host lists no files".to_owned(),
                    "Open Network File...".to_owned(),
                    connect::network_path_text(&server.0, server.1, ""),
                )
            } else {
                (
                    format!("No listed file is named “{search}”"),
                    format!("Open “{search}”..."),
                    connect::network_path_text(&server.0, server.1, search),
                )
            };
            empty(
                &mut rows_ui,
                &heading,
                "A host can leave a file unlisted. It still opens by its name.",
            );
            if page_capsule(&mut rows_ui, &open, enabled).clicked() {
                actions.push(Action::OpenNetworkFile(path));
            }
            return;
        }
        let names: Vec<String> = shown.iter().map(|file| file.name.clone()).collect();
        if self
            .host_file
            .as_ref()
            .is_some_and(|file| !names.contains(file))
        {
            self.host_file = None;
        }
        let rows = rows_ui.max_rect();
        let list = list_focus(ui, rows, "host_files", enabled, "Host files");
        let open = |file: String| {
            Action::OpenSaved(SavedFile::Hosted {
                host: server.0.clone(),
                port: server.1,
                file,
            })
        };
        if enabled {
            let (enter, _) = list_keys(
                ui,
                list.id,
                &names,
                &mut self.host_file,
                &mut self.reveal_host_file,
                false,
            );
            if enter {
                if let Some(file) = &self.host_file {
                    actions.push(open(file.clone()));
                }
            }
        }
        egui::ScrollArea::vertical()
            .id_salt(("host_files", &server))
            .auto_shrink([false, false])
            .show(&mut rows_ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for name in &names {
                    let selected = self.host_file.as_ref() == Some(name);
                    let response = host_file_row(ui, name, selected, enabled);
                    if enabled && response.clicked() {
                        self.host_file = Some(name.clone());
                        self.host_armed = false;
                    }
                    if enabled && response.double_clicked() {
                        actions.push(open(name.clone()));
                    }
                    if selected && std::mem::take(&mut self.reveal_host_file) {
                        response.scroll_to_me(None);
                    }
                }
            });
        if list.has_focus() {
            focus_ring(ui.painter(), rows.shrink(4.0), 0.0);
        }
    }

    fn table(&mut self, ui: &mut Ui, table: Rect, view: &HomeView<'_>, actions: &mut Vec<Action>) {
        let name_rule = table.left() + NAME_COLUMN;
        let trailing_rule = table.right() - TRAILING_COLUMN;
        column_header(ui, table, Some(name_rule));

        let tab = view.history.tab();
        let files = view.history.files(tab);
        let mut rows_ui = rows_ui(ui, table);
        if files.is_empty() {
            let (heading, detail) = match tab {
                Tab::Recent => ("No recent files", "Files you open appear here."),
                Tab::Favorites => (
                    "No favorite files",
                    "Right-click a recent file and choose Add to Favorites.",
                ),
            };
            empty(&mut rows_ui, heading, detail);
            return;
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|file| !files.contains(file))
        {
            self.selected = None;
        }
        let enabled = !view.busy && view.in_front;
        let rows = rows_ui.max_rect();
        let list_label = match tab {
            Tab::Recent => "Recent files",
            Tab::Favorites => "Favorite files",
        };
        let list = list_focus(ui, rows, "home_list", enabled, list_label);
        if enabled {
            let (open, remove) = list_keys(
                ui,
                list.id,
                files,
                &mut self.selected,
                &mut self.reveal_selected,
                true,
            );
            if let Some(selected) = self.selected.clone() {
                if open {
                    actions.push(Action::OpenSaved(selected));
                } else if remove {
                    actions.push(Action::EditHistory(removal(tab, selected)));
                }
            }
        }
        let columns = Columns {
            name: name_rule - table.left(),
            location: trailing_rule - name_rule,
        };
        egui::ScrollArea::vertical()
            .id_salt(("home_files", tab.label()))
            .auto_shrink([false, false])
            .show(&mut rows_ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for file in files {
                    let selected = self.selected.as_ref() == Some(file);
                    let response =
                        row(ui, view.history, file, selected, &columns, enabled, actions);
                    if enabled && (response.clicked() || response.secondary_clicked()) {
                        self.selected = Some(file.clone());
                    }
                    if response.double_clicked() && enabled {
                        actions.push(Action::OpenSaved(file.clone()));
                    }
                    if selected && std::mem::take(&mut self.reveal_selected) {
                        response.scroll_to_me(None);
                    }
                }
            });
        if list.has_focus() {
            focus_ring(ui.painter(), rows.shrink(4.0), 0.0);
        }
    }
}

/// A saved host's context menu.
/// A saved host's context menu. Returns whether List Files was chosen.
fn host_menu(ui: &mut Ui, host: &SavedHost, actions: &mut Vec<Action>) -> bool {
    let list = ui.button("List Files").clicked();
    if list {
        actions.push(Action::ListFiles(host.server()));
        ui.close();
    }
    if ui.button("Open Network File...").clicked() {
        actions.push(Action::OpenNetworkFile(connect::network_path_text(
            &host.address,
            host.port,
            "",
        )));
        ui.close();
    }
    ui.separator();
    if ui.button("Edit Host...").clicked() {
        actions.push(Action::EditHost(Some(host.clone())));
        ui.close();
    }
    if ui.button("Remove Host").clicked() {
        actions.push(Action::EditHistory(Change::RemoveHost(
            host.address.clone(),
            host.port,
        )));
        ui.close();
    }
    list
}

/// The page's title.
fn page_title(ui: &Ui, area: Rect, title: &str) {
    ui.painter().text(
        pos2(area.left() + SIDEBAR_WIDTH + TITLE_X, area.top() + TITLE_Y),
        Align2::LEFT_CENTER,
        title,
        FontId::new(theme::PAGE_TITLE, theme::semibold()),
        theme::TEXT,
    );
}

/// The table's column header: Name, and Location after `name_rule` when
/// the table has one.
fn column_header(ui: &Ui, table: Rect, name_rule: Option<f32>) {
    let painter = ui.painter();
    let rule = Stroke::new(1.0, theme::SEPARATOR);
    let header_rule = table.top() + (HEADER_RULE_Y - COLUMN_RULE_TOP);
    let trailing_rule = table.right() - TRAILING_COLUMN;
    for x in name_rule.into_iter().chain([trailing_rule]) {
        painter.vline(x, table.top()..=header_rule, rule);
    }
    painter.hline(table.x_range(), header_rule, rule);
    let header_y = table.top() + (HEADER_TEXT_Y - COLUMN_RULE_TOP);
    let mut labels = vec![(table.left() + HEADER_NAME_X, "Name")];
    if let Some(name_rule) = name_rule {
        labels.push((name_rule + HEADER_LOCATION_X, "Location"));
    }
    for (x, label) in labels {
        painter.text(
            pos2(x, header_y),
            Align2::LEFT_CENTER,
            label,
            theme::small(),
            theme::TEXT,
        );
    }
}

/// The table's rows, below its header.
fn rows_ui(ui: &mut Ui, table: Rect) -> Ui {
    let rows = Rect::from_min_max(
        pos2(table.left(), table.top() + (ROWS_TOP - COLUMN_RULE_TOP)),
        table.max,
    );
    ui.new_child(
        UiBuilder::new()
            .max_rect(rows)
            .layout(Layout::top_down(Align::Min)),
    )
}

/// The list as one keyboard focus stop, as a macOS table is: Tab reaches
/// it, and while it has the focus the arrows move its selection rather
/// than the focus. Its rows are not focus stops, so Return and Delete
/// always act on the selection.
fn list_focus(ui: &Ui, rows: Rect, salt: &str, enabled: bool, label: &str) -> Response {
    let list = ui.interact(
        rows,
        ui.scope_id().with(salt),
        if enabled {
            Sense::focusable_noninteractive()
        } else {
            Sense::hover()
        },
    );
    list.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ListBox, enabled, label));
    list
}

/// A list's keys, as a macOS table takes them: arrows move the selection,
/// Return opens the selected item and, with `delete`, Delete removes it.
/// They work while the list or nothing has the keyboard focus. Returns
/// whether Return and Delete were pressed.
fn list_keys<T: Clone + PartialEq>(
    ui: &Ui,
    list: Id,
    items: &[T],
    selected: &mut Option<T>,
    reveal: &mut bool,
    delete: bool,
) -> (bool, bool) {
    // An open menu (a row's context menu, say) takes the keys first.
    let free = !egui::Popup::is_any_open(ui.ctx())
        && ui.ctx().memory(|memory| {
            memory.top_modal_layer().is_none() && memory.focused().is_none_or(|id| id == list)
        });
    if !free || items.is_empty() {
        return (false, false);
    }
    let index = selected
        .as_ref()
        .and_then(|selected| items.iter().position(|item| item == selected));
    let (down, up, open, remove) = ui.input_mut(|input| {
        (
            input.consume_key(Modifiers::NONE, Key::ArrowDown),
            input.consume_key(Modifiers::NONE, Key::ArrowUp),
            input.consume_key(Modifiers::NONE, Key::Enter),
            delete
                && (input.consume_key(Modifiers::NONE, Key::Delete)
                    || input.consume_key(Modifiers::NONE, Key::Backspace)),
        )
    });
    if down || up {
        // egui moves the focus with the arrows at the end of the pass;
        // the list keeps it. (A focus lock filter would take effect only
        // from the second pass after Tab.)
        ui.ctx()
            .memory_mut(|memory| memory.move_focus(FocusDirection::None));
    }
    let moved = match (index, down, up) {
        (None, true, _) => Some(0),
        (None, _, true) => Some(items.len() - 1),
        (Some(index), true, _) => Some((index + 1).min(items.len() - 1)),
        (Some(index), _, true) => Some(index.saturating_sub(1)),
        _ => None,
    };
    if let Some(moved) = moved {
        *selected = Some(items[moved].clone());
        *reveal = true;
    }
    (open && selected.is_some(), remove && selected.is_some())
}

/// What Remove does in each list: Recent forgets the file, Favorites
/// unpins it. Neither touches the file.
fn removal(tab: Tab, file: SavedFile) -> Change {
    match tab {
        Tab::Recent => Change::RemoveRecent(file),
        Tab::Favorites => Change::SetFavorite(file, false),
    }
}

/// A capsule button: Home's Create and Open.
fn capsule(ui: &mut Ui, rect: Rect, label: &str, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, ui.scope_id().with(("home_capsule", label)), sense);
    let fill = if enabled && response.is_pointer_button_down_on() {
        theme::HOVER_FILL
    } else {
        theme::CAPSULE_FILL
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), fill);
    if response.has_focus() {
        focus_ring(painter, rect, rect.height() / 2.0);
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::body(),
        if enabled {
            theme::TEXT
        } else {
            theme::DISABLED
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    response
}

/// A capsule under a page's empty-state text, centered.
fn page_capsule(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    let width = widgets::text_width(ui, label, theme::body()) + 40.0;
    ui.add_space(12.0);
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), PAGE_CAPSULE_HEIGHT),
        Sense::hover(),
    );
    capsule(
        ui,
        Rect::from_center_size(rect.center(), vec2(width, PAGE_CAPSULE_HEIGHT)),
        label,
        enabled,
    )
}

/// Hosts' add button: a plus on the sidebar, a gray square under the
/// pointer.
fn add_button(ui: &mut Ui, rect: Rect, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, Id::unique("home_add_host"), sense);
    let painter = ui.painter();
    let fill = if enabled && response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(4), theme::HOVER_FILL);
        theme::HOVER_FILL
    } else {
        theme::CONTENT_BG
    };
    if response.has_focus() {
        focus_ring(painter, rect, 4.0);
    }
    icons::paint(
        painter,
        rect.shrink(2.0),
        Icon::Plus,
        if enabled {
            theme::ICON
        } else {
            theme::DISABLED
        },
        fill,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, "Add Host"));
    response.on_hover_text("Add Host")
}

/// The host page's refresh button: list the host's files again.
fn refresh_button(ui: &mut Ui, rect: Rect, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, Id::unique("home_refresh_host"), sense);
    let painter = ui.painter();
    let fill = if enabled && response.hovered() {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::CONTROL_RADIUS),
            theme::HOVER_FILL,
        );
        theme::HOVER_FILL
    } else {
        theme::CONTENT_BG
    };
    if response.has_focus() {
        focus_ring(painter, rect, f32::from(theme::CONTROL_RADIUS));
    }
    icons::paint(
        painter,
        rect.shrink(2.0),
        Icon::Refresh,
        if enabled {
            theme::ICON
        } else {
            theme::DISABLED
        },
        fill,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, "List Files"));
    response.on_hover_text("List Files")
}

/// The host page's Search Files: a gray rounded field with a magnifier,
/// white with the focus color while typed in.
fn search_field(ui: &mut Ui, rect: Rect, text: &mut String, enabled: bool) {
    let id = Id::unique("home_search_files");
    let focused = ui.memory(|memory| memory.has_focus(id));
    let (fill, border) = if focused && enabled {
        (theme::CONTROL_BG, theme::FOCUS)
    } else {
        (theme::SEARCH_FILL, theme::SEARCH_BORDER)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(6),
        fill,
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    let glyph = if enabled {
        theme::ICON
    } else {
        theme::DISABLED
    };
    icons::paint(
        ui.painter(),
        Rect::from_center_size(rect.left_center() + vec2(13.0, 0.0), vec2(14.0, 14.0)),
        Icon::Search,
        glyph,
        fill,
    );
    let text_rect = Rect::from_min_max(rect.min + vec2(26.0, 0.0), rect.max - vec2(6.0, 0.0));
    let hint = if enabled {
        theme::WEAK_TEXT
    } else {
        theme::DISABLED
    };
    let response = ui
        .add_enabled_ui(enabled, |ui| {
            ui.put(
                text_rect,
                egui::TextEdit::singleline(text)
                    .id(id)
                    .hint_text(egui::RichText::new("Search Files").color(hint))
                    .font(theme::body())
                    .frame(egui::Frame::NONE)
                    .align(Align2::LEFT_CENTER),
            )
        })
        .inner;
    widgets::accessible_label(ui, response.id, "Search Files");
}

/// A page in the sidebar, as FileMaker draws it: a glyph and gray text, the
/// selected page on a light gray fill.
fn navigation(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    label: &str,
    icon: Icon,
    selected: bool,
    enabled: bool,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    let painter = ui.painter();
    if selected {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::CONTROL_RADIUS),
            theme::NAV_SELECTION,
        );
    }
    if response.has_focus() {
        focus_ring(painter, rect, f32::from(theme::CONTROL_RADIUS));
    }
    let color = if enabled {
        theme::CAPTION
    } else {
        theme::DISABLED
    };
    icons::paint(
        painter,
        Rect::from_center_size(
            pos2(rect.left() + (NAV_ICON_X - NAV.left()), rect.center().y),
            vec2(NAV_ICON, NAV_ICON),
        ),
        icon,
        if enabled {
            theme::ICON
        } else {
            theme::DISABLED
        },
        if selected {
            theme::NAV_SELECTION
        } else {
            theme::CONTENT_BG
        },
    );
    let text_x = rect.left() + (NAV_TEXT_X - NAV.left());
    let galley = truncated(
        painter,
        label,
        theme::body(),
        color,
        rect.right() - CELL_PADDING - text_x,
    );
    painter.galley(
        pos2(text_x, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, label));
    response
}

/// One of the page's tabs: gray text, the selected one underlined in the
/// accent from `rule_top`. Also returns where its text is.
fn tab_button(
    ui: &mut Ui,
    left_center: egui::Pos2,
    rule_top: f32,
    tab: Tab,
    selected: bool,
    enabled: bool,
) -> (Response, Rect) {
    let label = tab.label();
    let galley = ui
        .painter()
        .layout_no_wrap(label.into(), theme::body(), theme::CAPTION);
    let text = Rect::from_min_size(
        left_center - vec2(0.0, galley.size().y / 2.0),
        galley.size(),
    );
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let target = text.expand2(vec2(4.0, 6.0));
    let response = ui.interact(target, ui.scope_id().with(("home_tab", label)), sense);
    let painter = ui.painter();
    if response.has_focus() {
        focus_ring(painter, target, f32::from(theme::CONTROL_RADIUS));
    }
    painter.galley(text.min, galley, theme::CAPTION);
    if selected {
        painter.rect_filled(
            Rect::from_min_max(
                pos2(text.left() - 2.0, rule_top),
                pos2(text.right(), rule_top + TAB_RULE),
            ),
            CornerRadius::ZERO,
            theme::ACCENT,
        );
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, enabled, selected, label));
    (response, text)
}

/// At the tabs' right end: a file opening, or why the lists can't be saved.
fn status(ui: &mut Ui, view: &HomeView<'_>, right_center: egui::Pos2) {
    let rect = Rect::from_min_max(
        right_center - vec2(320.0, 10.0),
        right_center + vec2(0.0, 10.0),
    );
    let mut status_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    if view.busy {
        status_ui.label(
            egui::RichText::new("Opening…")
                .font(theme::small())
                .color(theme::CAPTION),
        );
        status_ui.spinner();
    } else if let Some(error) = view.history_error {
        status_ui
            .label(
                egui::RichText::new("Recent, Favorites and Hosts can't be saved")
                    .font(theme::small())
                    .color(theme::YELLOW),
            )
            .on_hover_text(error);
    }
}

/// A list with nothing in it: what it is, and how something gets there.
fn empty(ui: &mut Ui, heading: &str, detail: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.3).max(24.0));
        ui.label(
            egui::RichText::new(heading)
                .font(theme::body())
                .color(theme::CAPTION),
        );
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(detail)
                .font(theme::small())
                .color(theme::CAPTION),
        );
    });
}

/// A host's files on their way.
fn listing_files(ui: &mut Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.3).max(24.0));
        ui.spinner();
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("Listing files…")
                .font(theme::small())
                .color(theme::CAPTION),
        );
    });
}

/// The keyboard focus ring just outside `rect`: `FOCUS`, opaque, which
/// keeps 3:1 on Home's white.
fn focus_ring(painter: &egui::Painter, rect: Rect, radius: f32) {
    painter.rect_stroke(
        rect.expand(2.0),
        CornerRadius::same((radius + 2.0) as u8),
        Stroke::new(2.0, theme::FOCUS),
        StrokeKind::Outside,
    );
}

/// The widths of the Name and Location columns.
struct Columns {
    name: f32,
    location: f32,
}

/// A row's fill and its text and glyph colors.
fn row_colors(painter: &egui::Painter, rect: Rect, selected: bool) -> (Color32, Color32, Color32) {
    if selected {
        painter.rect_filled(
            rect,
            CornerRadius::same(theme::CONTROL_RADIUS),
            theme::SELECTED,
        );
        (theme::ON_ACCENT, theme::ON_ACCENT, theme::SELECTED)
    } else {
        (theme::TEXT, theme::ICON, theme::CONTENT_BG)
    }
}

/// `icon` at a row's start.
fn row_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32, fill: Color32) {
    icons::paint(
        painter,
        Rect::from_min_size(
            pos2(rect.left() + ROW_ICON_X, rect.center().y - ROW_ICON / 2.0),
            vec2(ROW_ICON, ROW_ICON),
        ),
        icon,
        color,
        fill,
    );
}

/// One file: its glyph and name, and where it is. A click selects it, a
/// double click opens it, and its context menu pins or removes it.
fn row(
    ui: &mut Ui,
    history: &History,
    file: &SavedFile,
    selected: bool,
    columns: &Columns,
    enabled: bool,
    actions: &mut Vec<Action>,
) -> Response {
    // Clicks only: the list, not each row, takes the keyboard focus.
    let sense = if enabled {
        Sense::CLICK
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), sense);
    let name = file.name();
    let location = file.location();
    let painter = ui.painter();
    let (ink, glyph, fill) = row_colors(painter, rect, selected);
    let icon = match file {
        SavedFile::Local { .. } => Icon::File,
        SavedFile::Hosted { .. } => Icon::Server,
    };
    row_icon(painter, rect, icon, glyph, fill);
    let name_x = rect.left() + ROW_NAME_X;
    let location_x = rect.left() + columns.name + ROW_LOCATION_X;
    for (x, text, width) in [
        (name_x, &name, columns.name - ROW_NAME_X - CELL_PADDING),
        (
            location_x,
            &location,
            columns.location - ROW_LOCATION_X - CELL_PADDING,
        ),
    ] {
        let galley = truncated(painter, text, body_font(), ink, width);
        let top = rect.center().y - galley.size().y / 2.0;
        painter.galley(pos2(x, top), galley, ink);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::ListBoxOption, enabled, selected, &name)
    });
    let response = response.on_hover_text(&location);
    if enabled {
        let favorite = history.files(Tab::Favorites).contains(file);
        response.context_menu(|ui| {
            if history.tab() == Tab::Recent {
                let pin = if favorite {
                    "Remove from Favorites"
                } else {
                    "Add to Favorites"
                };
                if ui.button(pin).clicked() {
                    actions.push(Action::EditHistory(Change::SetFavorite(
                        file.clone(),
                        !favorite,
                    )));
                    ui.close();
                }
            }
            if ui.button("Remove").clicked() {
                actions.push(Action::EditHistory(removal(history.tab(), file.clone())));
                ui.close();
            }
        });
    }
    response
}

/// One file a host lists: a document glyph and its name. A click selects
/// it, and a double click signs in to open it.
fn host_file_row(ui: &mut Ui, file: &str, selected: bool, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::CLICK
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), sense);
    let name = file_stem(file);
    let painter = ui.painter();
    let (ink, glyph, fill) = row_colors(painter, rect, selected);
    row_icon(painter, rect, Icon::File, glyph, fill);
    let x = rect.left() + ROW_NAME_X;
    let galley = truncated(
        painter,
        name,
        body_font(),
        ink,
        rect.right() - x - CELL_PADDING,
    );
    painter.galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        ink,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::ListBoxOption, enabled, selected, name)
    });
    response
}

fn body_font() -> FontId {
    FontId::new(theme::BODY, FontFamily::Proportional)
}

/// `text` on one line, cut with an ellipsis at `width`.
fn truncated(
    painter: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), TextFormat::simple(font, color));
    job.wrap = TextWrapping::truncate_at_width(width.max(0.0));
    painter.layout_job(job)
}
