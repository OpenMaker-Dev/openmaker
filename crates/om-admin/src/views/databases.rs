//! Databases (`app/activities`; `docs/re/admin-console.md` 4.2): the hosted
//! files on the left and the connected clients on the right, in FileMaker
//! Server's two columns.
//!
//! The view's state is its query, so that it works without the script and
//! a reload keeps it: `db` (the selected file, which filters the clients),
//! `folder` (the folder filter), `q` and `sort` (the files' search and
//! sort), `cq` and `csort` (the clients'). `close` opens the Close dialog
//! for a file, or `all` for the folder's files. Menus are `<details>`; the
//! client list is one form whose checked rows Disconnect and Send Message
//! act on, with their dialogs inside it.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

use axum::extract::{ConnectInfo, Form, Path, State};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use maud::{html, Markup};
use serde::Deserialize;

use crate::app::{Admin, AppState};
use crate::backend::{
    Backend, BackendError, Client, Database, DatabaseChange, DatabaseStatus, Notice,
};
use crate::components::{modal, modal_actions, modal_button, relative};
use crate::icons::{self, Icon};
use crate::views::{finish, live_query, refuse_stale, Ctx, MAX_QUERY};

const TAB: &str = "activities";
const PAGE: &str = "/admin-console/app/activities";
const CLIENTS_ACTION: &str = "/admin-console/app/activities/clients";
const DATABASES_ACTION: &str = "/admin-console/app/activities/databases";

/// The longest message to clients, in UTF-16 units as a browser's
/// `maxlength` counts (the Admin API's `messageText`).
const MAX_MESSAGE: usize = 200;
/// The longest delay before clients are disconnected, in minutes: the
/// Admin API's `graceTime` of at most 3600 s.
const MAX_DELAY_MINUTES: u64 = 60;
/// The time clients get when a dialog gives a message and no delay: the
/// Admin API's default `graceTime`.
const DEFAULT_GRACE: Duration = Duration::from_secs(90);
/// The longest search text kept, in characters, and in bytes once encoded
/// in the view's query.
const MAX_SEARCH: usize = 100;
const MAX_SEARCH_ENCODED: usize = 480;
/// The longest folder path a filter can name, in bytes once encoded in the
/// view's query: a folder with a longer one cannot be a filter.
const MAX_FOLDER_ENCODED: usize = 960;
/// The rest of the view's query at its longest: the file id, the sorts,
/// and the keys and separators of the texts.
const MAX_FIXED_ENCODED: usize = "db=4294967295&folder=&q=&sort=filename&cq=&csort=session".len();
// Every state the view keeps fits in the address its forms return to.
const _: () = assert!(
    MAX_FIXED_ENCODED + MAX_FOLDER_ENCODED + 2 * MAX_SEARCH_ENCODED <= MAX_QUERY,
    "the view's state must fit in a return address"
);
/// Failures named in a combined outcome; the rest are counted.
const FAILURES_NAMED: usize = 3;

const NOT_AVAILABLE: &str = "Not available in OpenMaker Server";

const DISCONNECT_HELP: &str = "The selected clients are asked to close their files, with \
    your message, and are disconnected when the delay ends: after 90 seconds when you give \
    a message and no delay. With neither, they are disconnected at once.";
const SEND_HELP: &str = "The message appears on the selected clients' screens.";

/// How the files are sorted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DatabaseSort {
    #[default]
    Filename,
    Status,
    Clients,
    Folder,
}

impl DatabaseSort {
    const ALL: [Self; 4] = [Self::Filename, Self::Status, Self::Clients, Self::Folder];

    fn key(self) -> &'static str {
        match self {
            Self::Filename => "filename",
            Self::Status => "status",
            Self::Clients => "clients",
            Self::Folder => "folder",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Filename => "Filename",
            Self::Status => "Status",
            Self::Clients => "Clients",
            Self::Folder => "Folder",
        }
    }
}

/// How the clients are sorted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ClientSort {
    #[default]
    Session,
    Client,
    SignIn,
}

impl ClientSort {
    const ALL: [Self; 3] = [Self::Session, Self::Client, Self::SignIn];

    fn key(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Client => "client",
            Self::SignIn => "signin",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Session => "Session Identifier",
            Self::Client => "Client",
            Self::SignIn => "Sign-in",
        }
    }
}

/// The files a Close dialog closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    One(u32),
    /// Every file in the folder filter that can be closed.
    All,
}

/// The view's selection, filters and sorts, from its query.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ViewState {
    db: Option<u32>,
    folder: Option<String>,
    search: String,
    sort: DatabaseSort,
    client_search: String,
    client_sort: ClientSort,
}

impl ViewState {
    /// The state in `query`, and the Close dialog it opens. Unknown keys
    /// and values are ignored, and texts are cut to their limits.
    fn parse(query: &str) -> (ViewState, Option<Target>) {
        let mut state = ViewState::default();
        let mut close = None;
        for (key, value) in form_urlencoded::parse(query.as_bytes()).take(32) {
            match &*key {
                "db" => state.db = value.parse().ok(),
                "folder" => {
                    state.folder =
                        (!value.is_empty() && fits_folder(&value)).then(|| value.into_owned());
                }
                "q" => state.search = search_text(&value),
                "sort" => {
                    state.sort = DatabaseSort::ALL
                        .into_iter()
                        .find(|s| s.key() == value)
                        .unwrap_or_default();
                }
                "cq" => state.client_search = search_text(&value),
                "csort" => {
                    state.client_sort = ClientSort::ALL
                        .into_iter()
                        .find(|s| s.key() == value)
                        .unwrap_or_default();
                }
                "close" => {
                    close = match &*value {
                        "all" => Some(Target::All),
                        id => id.parse().ok().map(Target::One),
                    };
                }
                _ => {}
            }
        }
        (state, close)
    }

    /// The query's fields that differ from the defaults, in a fixed order.
    fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(db) = self.db {
            pairs.push(("db", db.to_string()));
        }
        if let Some(folder) = &self.folder {
            pairs.push(("folder", folder.clone()));
        }
        if !self.search.is_empty() {
            pairs.push(("q", self.search.clone()));
        }
        if self.sort != DatabaseSort::default() {
            pairs.push(("sort", self.sort.key().to_owned()));
        }
        if !self.client_search.is_empty() {
            pairs.push(("cq", self.client_search.clone()));
        }
        if self.client_sort != ClientSort::default() {
            pairs.push(("csort", self.client_sort.key().to_owned()));
        }
        pairs
    }

    fn query(&self) -> String {
        let mut query = form_urlencoded::Serializer::new(String::new());
        for (key, value) in self.pairs() {
            query.append_pair(key, &value);
        }
        query.finish()
    }

    /// The page with this state.
    fn href(&self) -> String {
        let query = self.query();
        if query.is_empty() {
            PAGE.to_owned()
        } else {
            format!("{PAGE}?{query}")
        }
    }

    fn with(&self, change: impl FnOnce(&mut ViewState)) -> ViewState {
        let mut state = self.clone();
        change(&mut state);
        state
    }

    /// The page with the Close dialog open for `target`.
    fn close_href(&self, target: Target) -> String {
        let value = match target {
            Target::One(id) => id.to_string(),
            Target::All => "all".to_owned(),
        };
        let mut query = form_urlencoded::Serializer::new(self.query());
        query.append_pair("close", &value);
        format!("{PAGE}?{}#close-database", query.finish())
    }
}

/// The bytes `text` takes in the view's query, as `form_urlencoded` writes
/// it: ASCII letters and digits, `*-._` and spaces (as `+`) take one, every
/// other byte three.
fn encoded_len(text: &str) -> usize {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b" *-._".contains(&b) {
                1
            } else {
                3
            }
        })
        .sum()
}

/// Whether folder path `path` can be the folder filter.
fn fits_folder(path: &str) -> bool {
    encoded_len(path) <= MAX_FOLDER_ENCODED
}

/// A search text as typed, without control characters, cut to its limits
/// in characters and in encoded bytes.
fn search_text(text: &str) -> String {
    let mut kept = String::new();
    let mut encoded = 0;
    for c in text.chars().filter(|c| !c.is_control()).take(MAX_SEARCH) {
        encoded += encoded_len(c.encode_utf8(&mut [0; 4]));
        if encoded > MAX_SEARCH_ENCODED {
            break;
        }
        kept.push(c);
    }
    kept
}

/// Whether `text` contains `search`, ignoring case.
fn matches(text: &str, search: &str) -> bool {
    search.is_empty() || text.to_lowercase().contains(&search.to_lowercase())
}

/// A file's name without its `.fmp12` extension, as the list shows it.
fn display_name(name: &str) -> &str {
    let cut = name.len().saturating_sub(6);
    match name.get(cut..) {
        Some(extension) if cut > 0 && extension.eq_ignore_ascii_case(".fmp12") => &name[..cut],
        _ => name,
    }
}

/// The tag of the Client column for FileMaker application type `app_type`
/// (`docs/re/calc-functions.md` 6), with FileMaker Server's words.
fn client_tag(app_type: u8) -> &'static str {
    match app_type {
        0 | 1 => "Pro",
        2 => "Runtime",
        3 => "Server",
        4 => "CWP",
        5 => "Go",
        6 => "XDBC",
        7 => "FMSE",
        8 => "WebDirect",
        9 => "Data API",
        _ => "Other",
    }
}

fn client_name(client: &Client) -> &str {
    client
        .session_name
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or(&client.machine_name)
}

/// A name for an outcome, which is also logged when an action fails:
/// control characters, such as a line break in the session name a client
/// chose, become spaces so that they cannot forge log lines.
fn printable(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Where in the status sort a file falls: open first, closed last.
fn status_rank(status: DatabaseStatus) -> u8 {
    match status {
        DatabaseStatus::Normal => 0,
        DatabaseStatus::Opening => 1,
        DatabaseStatus::Pausing => 2,
        DatabaseStatus::Paused => 3,
        DatabaseStatus::Resuming => 4,
        DatabaseStatus::Closing => 5,
        DatabaseStatus::Closed => 6,
    }
}

fn in_folder(database: &Database, folder: Option<&str>) -> bool {
    folder.is_none_or(|folder| database.folder.starts_with(folder))
}

/// The files the folder filter shows, before the search.
fn in_filter<'a>(databases: &'a [Database], state: &ViewState) -> Vec<&'a Database> {
    databases
        .iter()
        .filter(|d| in_folder(d, state.folder.as_deref()))
        .collect()
}

/// The files listed: in the folder filter, matching the search, sorted.
fn listed<'a>(databases: &'a [Database], state: &ViewState) -> Vec<&'a Database> {
    let mut list: Vec<&Database> = in_filter(databases, state)
        .into_iter()
        .filter(|d| matches(display_name(&d.name), &state.search))
        .collect();
    let name = |d: &Database| display_name(&d.name).to_lowercase();
    match state.sort {
        DatabaseSort::Filename => list.sort_by_cached_key(|d| (name(d), d.id)),
        DatabaseSort::Status => list.sort_by_cached_key(|d| (status_rank(d.status), name(d), d.id)),
        DatabaseSort::Clients => {
            list.sort_by_cached_key(|d| (std::cmp::Reverse(d.clients), name(d), d.id));
        }
        DatabaseSort::Folder => list.sort_by_cached_key(|d| (d.folder.clone(), name(d), d.id)),
    }
    list
}

/// The selected file, when it is listed.
fn selected<'a>(listed: &[&'a Database], state: &ViewState) -> Option<&'a Database> {
    let id = state.db?;
    listed.iter().copied().find(|d| d.id == id)
}

/// The clients listed: those of the selected file, or with no file
/// selected those of the folder filter's files (every client without a
/// filter), matching the search, sorted.
fn listed_clients<'a>(
    clients: &'a [Client],
    databases: &[Database],
    selected: Option<&Database>,
    state: &ViewState,
) -> Vec<&'a Client> {
    let search = &state.client_search;
    let folder_files: Option<HashSet<u32>> = state
        .folder
        .as_ref()
        .map(|_| in_filter(databases, state).iter().map(|d| d.id).collect());
    let mut list: Vec<&Client> = clients
        .iter()
        .filter(|c| match (selected, &folder_files) {
            (Some(d), _) => c.files.iter().any(|f| f.id == d.id),
            (None, Some(ids)) => c.files.iter().any(|f| ids.contains(&f.id)),
            (None, None) => true,
        })
        .filter(|c| {
            search.is_empty()
                || matches(client_name(c), search)
                || matches(&c.machine_name, search)
                || matches(&c.address.ip().to_string(), search)
                || matches(&c.version, search)
                || c.files.iter().any(|f| {
                    matches(display_name(&f.name), search)
                        || f.account.as_deref().is_some_and(|a| matches(a, search))
                })
        })
        .collect();
    match state.client_sort {
        ClientSort::Session => {
            list.sort_by_cached_key(|c| (client_name(c).to_lowercase(), c.connected_at, c.id));
        }
        ClientSort::Client => {
            list.sort_by_cached_key(|c| (client_tag(c.app_type), c.version.clone(), c.id));
        }
        ClientSort::SignIn => list.sort_by_key(|c| (c.connected_at, c.id)),
    }
    list
}

/// A folder in the folder filter's list.
struct Folder {
    path: String,
    name: String,
    /// Below the database folder it is in: 0 for a database folder.
    depth: usize,
}

/// The folder filter's list: each database folder, then the folders below
/// it that hold files, indented; folders holding files outside every
/// database folder come last. When the server's folders cannot be read
/// (other than not being available), the list holds the files' folders and
/// the error says why.
fn folders(backend: &dyn Backend, databases: &[Database]) -> (Vec<Folder>, Option<String>) {
    let mut holders: Vec<&FsPath> = databases.iter().map(|d| d.folder.as_path()).collect();
    holders.sort();
    holders.dedup();
    let (mut roots, error): (Vec<PathBuf>, _) = match backend.folders() {
        Ok(folders) => (folders.databases, None),
        Err(BackendError::Unsupported) => (Vec::new(), None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    for holder in &holders {
        if !roots.iter().any(|root| holder.starts_with(root)) {
            roots.push(holder.to_path_buf());
        }
    }
    let mut list = Vec::new();
    let mut seen = HashSet::new();
    for root in &roots {
        if !seen.insert(root.clone()) {
            continue;
        }
        list.push(folder(root, 0));
        for holder in &holders {
            if *holder != root.as_path()
                && holder.starts_with(root)
                && seen.insert(holder.to_path_buf())
            {
                let depth = holder.components().count() - root.components().count();
                list.push(folder(holder, depth.min(3)));
            }
        }
    }
    (list, error)
}

fn folder(path: &FsPath, depth: usize) -> Folder {
    Folder {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name().map_or_else(
            || path.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        ),
        depth,
    }
}

pub(crate) fn page(ctx: &Ctx) -> Markup {
    let (state, close) = ViewState::parse(ctx.query);
    let query = state.query();
    html! {
        div.activities {
            aside.activities-databases aria-labelledby="databases-title" {
                h1.m-title id="databases-title" { "Databases" }
                div.list-tools {
                    (search_form(&state, "q", &state.search, "Search databases"))
                    (sort_menu(
                        state.sort.label(),
                        DatabaseSort::ALL.into_iter().map(|s| {
                            (s.label(), state.with(|v| v.sort = s).href(), s == state.sort)
                        }),
                    ))
                }
                (live_query(TAB, "databases", &query, database_list(ctx, &state)))
                form id="db-action" method="post" action=(PAGE) {
                    (fields(ctx, &state))
                    @if let Some(folder) = &state.folder {
                        input type="hidden" name="folder" value=(folder);
                    }
                }
            }
            section.activities-clients aria-labelledby="clients-title" {
                h2.m-title id="clients-title" { "Clients" }
                div.toolbar {
                    (modal_button("Disconnect", "disconnect-client", Some("client")))
                    (modal_button("Send Message", "send-message", Some("client")))
                    div.toolbar-end {
                        (search_form(&state, "cq", &state.client_search, "Search clients"))
                        (sort_menu(
                            state.client_sort.label(),
                            ClientSort::ALL.into_iter().map(|s| {
                                (s.label(), state.with(|v| v.client_sort = s).href(), s == state.client_sort)
                            }),
                        ))
                    }
                }
                form id="clients-form" method="post" action=(CLIENTS_ACTION) {
                    (fields(ctx, &state))
                    (live_query(TAB, "clients", &query, client_table(ctx, &state)))
                    (disconnect_modal())
                    (send_modal())
                }
            }
            @if let Some(target) = close { (close_modal(ctx, &state, target)) }
        }
    }
}

/// Live region `region` of the view.
pub(crate) fn fragment(ctx: &Ctx, region: &str) -> Option<Markup> {
    let (state, _) = ViewState::parse(ctx.query);
    match region {
        "databases" => Some(database_list(ctx, &state)),
        "clients" => Some(client_table(ctx, &state)),
        _ => None,
    }
}

/// The hidden fields of the view's action forms: the session's CSRF token
/// and the page with its state, to return to.
fn fields(ctx: &Ctx, state: &ViewState) -> Markup {
    html! {
        input type="hidden" name="csrf" value=(ctx.session.csrf);
        input type="hidden" name="back" value=(state.href());
    }
}

/// A search box: a form that keeps the rest of the state.
fn search_form(state: &ViewState, name: &str, value: &str, label: &str) -> Markup {
    html! {
        form.search-form method="get" action=(PAGE) role="search" {
            @for (key, value) in state.pairs() {
                @if key != name { input type="hidden" name=(key) value=(value); }
            }
            input.input.search-input type="search" name=(name) value=(value) placeholder="Search"
                aria-label=(label) maxlength=(MAX_SEARCH) autocomplete="off";
        }
    }
}

/// "Sort by" and a menu of `options`: label, address, whether current.
fn sort_menu<'a>(current: &str, options: impl Iterator<Item = (&'a str, String, bool)>) -> Markup {
    html! {
        div.sort {
            span.sort-label { "Sort by" }
            details.menu.sort-menu {
                summary.link-button.dropdown { (current) (icons::dropdown_chevron()) }
                div.menu-panel.sort-panel {
                    @for (label, href, current) in options {
                        a.menu-item.current[current] href=(href)
                            aria-current=[current.then_some("true")] { (label) }
                    }
                }
            }
        }
    }
}

/// "Showing n results", with a link that clears the search.
fn search_note(count: usize, cleared: &ViewState) -> Markup {
    html! {
        p.search-note {
            "Showing " (count) @if count == 1 { " result" } @else { " results" }
            " "
            a href=(cleared.href()) { "Clear search results" }
        }
    }
}

/// The left column's list: the folder row, then one row per file.
fn database_list(ctx: &Ctx, state: &ViewState) -> Markup {
    let databases = ctx.backend.databases();
    let in_filter = in_filter(&databases, state);
    let listed = listed(&databases, state);
    let selected = selected(&listed, state);
    let (folders, folders_error) = folders(ctx.backend, &databases);
    let folder_label = state
        .folder
        .as_deref()
        .and_then(|path| folders.iter().find(|f| f.path == path))
        .map_or("All Databases", |f| f.name.as_str());
    html! {
        @if !state.search.is_empty() {
            (search_note(listed.len(), &state.with(|v| v.search.clear())))
        }
        ul.db-list {
            li.db-row.db-all.selected[selected.is_none()] {
                a.db-select href=(state.with(|v| v.db = None).href())
                    aria-current=[selected.is_none().then_some("true")]
                    aria-label="Show the clients of all databases" {
                    (Icon::Folder.render(20))
                }
                details.menu.folder-menu {
                    summary.folder-summary aria-label={"Folder: " (folder_label)} {
                        span.folder-label { (folder_label) }
                        (icons::select_arrow())
                    }
                    div.menu-panel.folder-panel {
                        a.menu-item.current[state.folder.is_none()]
                            href=(state.with(|v| { v.folder = None; v.db = None; }).href()) {
                            "All Databases"
                        }
                        @for folder in &folders {
                            @let current = state.folder.as_deref() == Some(folder.path.as_str());
                            @if fits_folder(&folder.path) {
                                a class={
                                    "menu-item folder-item depth-" (folder.depth)
                                    @if current { " current" }
                                } title=(folder.path)
                                    href=(state.with(|v| { v.folder = Some(folder.path.clone()); v.db = None; }).href()) {
                                    (Icon::Folder.render(18))
                                    span { (folder.name) }
                                }
                            } @else {
                                button class={"menu-item folder-item depth-" (folder.depth)}
                                    type="button" disabled
                                    title={(folder.path) ": its path is too long to filter by"} {
                                    (Icon::Folder.render(18))
                                    span { (folder.name) }
                                }
                            }
                        }
                        @if let Some(why) = &folders_error {
                            p.read-failed.menu-note {
                                "The server's folders could not be read: " (why)
                            }
                        }
                    }
                }
                span.db-count { "(" (in_filter.len()) ")" }
                (all_menu(&in_filter, state))
            }
            @for database in &listed {
                @let current = selected.is_some_and(|s| s.id == database.id);
                li.db-row.selected[current] {
                    a.db-select href=(state.with(|v| v.db = Some(database.id)).href())
                        aria-current=[current.then_some("true")] {
                        (icons::database_status(database.status))
                        (icons::encryption(database.encrypted))
                        span.db-name title=(database.name) { (display_name(&database.name)) }
                    }
                    span.db-clients {
                        (database.clients) @if database.clients == 1 { " Client" } @else { " Clients" }
                    }
                    (database_menu(database, state))
                }
            }
            @if listed.is_empty() {
                li.db-empty {
                    @if databases.is_empty() { "No databases are hosted." }
                    @else { "No databases match." }
                }
            }
        }
    }
}

/// A menu item that posts the hidden `db-action` form to `action`.
fn menu_post(label: &str, action: &str, enabled: bool) -> Markup {
    html! {
        button.menu-item type="submit" form="db-action" formaction=(action) disabled[!enabled] {
            (label)
        }
    }
}

/// A menu item that opens a dialog at `href`.
fn menu_link(label: &str, href: &str, enabled: bool) -> Markup {
    html! {
        @if enabled {
            a.menu-item href=(href) { (label) }
        } @else {
            button.menu-item type="button" disabled { (label) }
        }
    }
}

/// One of FileMaker Server's items that OpenMaker Server does not have.
fn menu_unavailable(label: &str) -> Markup {
    html! {
        button.menu-item type="button" disabled title=(NOT_AVAILABLE) { (label) }
    }
}

fn action_url(target: &str, change: &str) -> String {
    format!("{DATABASES_ACTION}/{target}/{change}")
}

fn closable(status: DatabaseStatus) -> bool {
    matches!(status, DatabaseStatus::Normal | DatabaseStatus::Paused)
}

/// The folder row's menu: its items act on the files of the folder filter.
fn all_menu(files: &[&Database], state: &ViewState) -> Markup {
    let any = |status: DatabaseStatus| files.iter().any(|d| d.status == status);
    html! {
        details.menu.row-menu {
            summary.row-menu-toggle aria-label="Actions for all databases" { (icons::row_menu()) }
            div.menu-panel {
                (menu_post("Open All", &action_url("all", "open"), any(DatabaseStatus::Closed)))
                (menu_post("Pause All", &action_url("all", "pause"), any(DatabaseStatus::Normal)))
                (menu_post("Resume All", &action_url("all", "resume"), any(DatabaseStatus::Paused)))
                (menu_link(
                    "Close All",
                    &state.close_href(Target::All),
                    files.iter().any(|d| closable(d.status)),
                ))
                (menu_unavailable("Remove All"))
                (menu_unavailable("Verify All"))
                (menu_unavailable("Flush All"))
                (menu_unavailable("Clear All Encryption Passwords"))
                (menu_unavailable("Show Extended Privileges"))
            }
        }
    }
}

/// A file's menu, its items enabled as its status allows.
fn database_menu(database: &Database, state: &ViewState) -> Markup {
    let id = database.id.to_string();
    let status = database.status;
    html! {
        details.menu.row-menu {
            summary.row-menu-toggle aria-label={"Actions for " (display_name(&database.name))} {
                (icons::row_menu())
            }
            div.menu-panel {
                (menu_post("Open", &action_url(&id, "open"), status == DatabaseStatus::Closed))
                @if status == DatabaseStatus::Paused {
                    (menu_post("Resume", &action_url(&id, "resume"), true))
                } @else {
                    (menu_post("Pause", &action_url(&id, "pause"), status == DatabaseStatus::Normal))
                }
                (menu_link("Close", &state.close_href(Target::One(database.id)), closable(status)))
                (menu_unavailable("Download"))
                (menu_unavailable("Remove"))
                (menu_unavailable("Verify"))
                (menu_unavailable("Flush"))
                (menu_unavailable("Clear Encryption Password"))
            }
        }
    }
}

/// The right column's table of clients.
fn client_table(ctx: &Ctx, state: &ViewState) -> Markup {
    let databases = ctx.backend.databases();
    let clients = ctx.backend.clients();
    let listed_files = listed(&databases, state);
    let selected = selected(&listed_files, state);
    let rows = listed_clients(&clients, &databases, selected, state);
    html! {
        @if !state.client_search.is_empty() {
            (search_note(rows.len(), &state.with(|v| v.client_search.clear())))
        }
        table.data-table.clients-table {
            thead {
                tr {
                    th.check-cell scope="col" {
                        input type="checkbox" data-select-all="client" aria-label="Select all clients";
                    }
                    th.col-session scope="col" { "Session Identifier" }
                    @if selected.is_some() {
                        th.col-databases scope="col" { "Account Name" }
                    } @else {
                        th.col-databases scope="col" { "Databases" }
                    }
                    th.col-host scope="col" { "Host Name / IP Address" }
                    th.col-signin scope="col" { "Sign-in" }
                    th.col-client scope="col" { "Client" }
                }
            }
            tbody {
                @for client in &rows {
                    tr.disconnecting[client.disconnecting] {
                        td.check-cell {
                            input type="checkbox" name="client" value=(client.id)
                                disabled[client.disconnecting]
                                aria-label={"Select " (client_name(client))};
                        }
                        td.col-session {
                            @match client.session_name.as_deref().filter(|n| !n.is_empty()) {
                                Some(name) => span.cell-text { (name) },
                                None => span.cell-text.muted { "Not signed in" },
                            }
                            @if client.disconnecting { span.client-state { "Disconnecting" } }
                        }
                        td.col-databases {
                            @if let Some(database) = selected {
                                @let account = client.files.iter()
                                    .find(|f| f.id == database.id)
                                    .and_then(|f| f.account.as_deref());
                                span.cell-text { (account.unwrap_or_default()) }
                            } @else {
                                @let names: Vec<&str> = client.files.iter()
                                    .map(|f| display_name(&f.name)).collect();
                                span.cell-text title=(names.join("\n")) { (client.files.len()) }
                            }
                        }
                        td.col-host {
                            span.cell-text title=(client.machine_name) { (client.address.ip()) }
                        }
                        td.col-signin { (relative(client.connected_at, ctx.now)) }
                        td.col-client {
                            span.client-tag title=(client.version) { (client_tag(client.app_type)) }
                        }
                    }
                }
                @if rows.is_empty() {
                    tr.empty-row { td colspan="6" { "No clients are connected." } }
                }
            }
        }
    }
}

fn message_field(id: &str, name: &str, required: bool, placeholder: &str) -> Markup {
    html! {
        div.modal-field {
            label.required[required] for=(id) { "Message" }
            textarea.input id=(id) name=(name) maxlength=(MAX_MESSAGE) rows="3"
                placeholder=(placeholder) data-required[required] {}
        }
    }
}

fn delay_field(id: &str, name: &str) -> Markup {
    html! {
        div.modal-field {
            label for=(id) { "Delay Time (minutes)" }
            input.input.delay-input id=(id) name=(name) inputmode="numeric" maxlength="2"
                placeholder="Minutes" autocomplete="off" data-delay;
        }
    }
}

fn disconnect_modal() -> Markup {
    modal(
        "disconnect-client",
        "Disconnect Client",
        html! {
            p.modal-intro { (DISCONNECT_HELP) }
            (message_field("disconnect-message", "disconnect_message", false, "Up to 200 characters, optional"))
            (delay_field("disconnect-delay", "disconnect_delay"))
            (modal_actions("OK", "disconnect"))
        },
    )
}

fn send_modal() -> Markup {
    modal(
        "send-message",
        "Send Message",
        html! {
            p.modal-intro { (SEND_HELP) }
            (message_field("send-message-text", "send_message", true, "Up to 200 characters"))
            (modal_actions("OK", "send"))
        },
    )
}

/// The Close dialog for `target`, in a form of its own; nothing when no
/// file of it can be closed.
fn close_modal(ctx: &Ctx, state: &ViewState, target: Target) -> Markup {
    let databases = ctx.backend.databases();
    let (heading, intro, action) = match target {
        Target::One(id) => {
            let Some(database) = databases.iter().find(|d| d.id == id && closable(d.status)) else {
                return html! {};
            };
            (
                "Close Database",
                format!(
                    "Clients that have \u{201c}{}\u{201d} open are asked to close it, with \
                     your message, and are disconnected from it when the delay ends: after 90 \
                     seconds when you give none.",
                    display_name(&database.name)
                ),
                action_url(&id.to_string(), "close"),
            )
        }
        Target::All => {
            if !in_filter(&databases, state)
                .iter()
                .any(|d| closable(d.status))
            {
                return html! {};
            }
            (
                "Close All Databases",
                "Clients are asked to close the open databases, with your message, and are \
                 disconnected from them when the delay ends: after 90 seconds when you give \
                 none."
                    .to_owned(),
                action_url("all", "close"),
            )
        }
    };
    html! {
        form.close-form method="post" action=(action) {
            (fields(ctx, state))
            @if let Some(folder) = &state.folder {
                input type="hidden" name="folder" value=(folder);
            }
            (modal("close-database", heading, html! {
                p.modal-intro { (intro) }
                (message_field("close-message", "message", false, "Up to 200 characters, optional"))
                (delay_field("close-delay", "delay"))
                (modal_actions("OK", "close"))
            }))
        }
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(CLIENTS_ACTION, post(act_on_clients))
        .route(
            "/admin-console/app/activities/databases/{target}/{change}",
            post(change_databases),
        )
}

fn invalid(why: &str) -> BackendError {
    BackendError::Invalid(why.to_owned())
}

/// A message from a dialog, with the browser's line breaks (CR LF) made
/// LF, or why it is refused: longer than [`MAX_MESSAGE`], with control
/// characters other than line breaks and tabs, or, when `required`, empty
/// or only spaces.
fn message(text: &str, required: bool) -> Result<String, BackendError> {
    let text = text.replace("\r\n", "\n");
    if required && text.trim().is_empty() {
        return Err(invalid("Enter a message."));
    }
    if text.encode_utf16().count() > MAX_MESSAGE {
        return Err(invalid("The message is longer than 200 characters."));
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(invalid("The message cannot contain control characters."));
    }
    Ok(text)
}

/// A dialog's delay in minutes, as a grace time: `None` when it is empty.
fn delay(text: &str) -> Result<Option<Duration>, BackendError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    match text.parse::<u64>() {
        Ok(minutes) if minutes <= MAX_DELAY_MINUTES => Ok(Some(Duration::from_secs(minutes * 60))),
        _ => Err(invalid("Enter a delay from 0 to 60 minutes.")),
    }
}

/// When a grace time ends: "at once", "in 90 seconds", "in 5 minutes".
fn when(grace: Duration) -> String {
    match grace.as_secs() {
        0 => "at once".to_owned(),
        1 => "in 1 second".to_owned(),
        s if s % 60 != 0 || s < 120 => format!("in {s} seconds"),
        s => format!("in {} minutes", s / 60),
    }
}

/// Who or what an outcome is about: a quoted name, or a count.
struct Subject {
    text: String,
    plural: bool,
}

/// The outcome of acting on several items one by one: `done` describes
/// what succeeded, and the failures are named after it. When the server
/// lacks the feature, the outcome is that error alone.
fn combine(
    results: Vec<(String, Result<(), BackendError>)>,
    noun: &str,
    done: impl Fn(&Subject) -> String,
) -> Result<String, BackendError> {
    let total = results.len();
    let failures: Vec<(&str, &BackendError)> = results
        .iter()
        .filter_map(|(name, result)| result.as_ref().err().map(|e| (name.as_str(), e)))
        .collect();
    let succeeded = total - failures.len();
    let subject = match (succeeded, total) {
        (1, 1) => Subject {
            text: format!("\u{201c}{}\u{201d}", results[0].0),
            plural: false,
        },
        (s, t) if s == t => Subject {
            text: format!("{t} {noun}s"),
            plural: true,
        },
        (s, t) => Subject {
            text: format!("{s} of {t} {noun}s"),
            plural: true,
        },
    };
    let Some((_, first)) = failures.first() else {
        return Ok(done(&subject));
    };
    if succeeded == 0
        && failures
            .iter()
            .all(|(_, e)| **e == BackendError::Unsupported)
    {
        return Err((*first).clone());
    }
    let mut text = if succeeded == 0 {
        String::new()
    } else {
        format!("{} ", done(&subject))
    };
    for (name, error) in failures.iter().take(FAILURES_NAMED) {
        text.push_str(&format!("\u{201c}{name}\u{201d}: {error} "));
    }
    if failures.len() > FAILURES_NAMED {
        text.push_str(&format!("And {} more.", failures.len() - FAILURES_NAMED));
    }
    Err(BackendError::Failed(text.trim_end().to_owned()))
}

/// Ids for the log: "11, 12".
fn id_list(ids: &[u32]) -> String {
    ids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

async fn act_on_clients(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Admin(admin): Admin,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let field = |name: &str| {
        form.iter()
            .find(|(key, _)| key == name)
            .map_or("", |(_, value)| value.as_str())
    };
    if let Some(refused) = refuse_stale(&admin, field("csrf"), peer) {
        return refused;
    }
    let mut seen = HashSet::new();
    let ids: Vec<u32> = form
        .iter()
        .filter(|(key, _)| key == "client")
        .filter_map(|(_, value)| value.parse().ok())
        .filter(|id| seen.insert(*id))
        .collect();
    let backend = state.backend();
    let (what, outcome) = if field("send") == "1" {
        (
            format!("send a message to clients {}", id_list(&ids)),
            send_message(backend, &ids, field("send_message")),
        )
    } else if field("disconnect") == "1" {
        (
            format!("disconnect clients {}", id_list(&ids)),
            disconnect(
                backend,
                &ids,
                field("disconnect_message"),
                field("disconnect_delay"),
            ),
        )
    } else {
        (
            "act on clients".to_owned(),
            Err(invalid("The form is not complete.")),
        )
    };
    finish(&state, &admin, peer, field("back"), &what, outcome)
}

/// Each client's name for an outcome, by id.
fn client_names(backend: &dyn Backend, ids: &[u32]) -> HashMap<u32, String> {
    let clients = backend.clients();
    ids.iter()
        .map(|id| {
            let name = clients
                .iter()
                .find(|c| c.id == *id)
                .map_or_else(|| format!("client {id}"), |c| printable(client_name(c)));
            (*id, name)
        })
        .collect()
}

fn disconnect(
    backend: &dyn Backend,
    ids: &[u32],
    text: &str,
    delay_text: &str,
) -> Result<String, BackendError> {
    if ids.is_empty() {
        return Err(invalid("Select the clients to disconnect."));
    }
    let text = message(text, false)?;
    let delay = delay(delay_text)?;
    let notice = (!text.is_empty() || delay.is_some()).then(|| Notice {
        message: text,
        grace: delay.unwrap_or(DEFAULT_GRACE),
    });
    let names = client_names(backend, ids);
    let results = ids
        .iter()
        .map(|id| {
            (
                names[id].clone(),
                backend.disconnect_client(*id, notice.as_ref()),
            )
        })
        .collect();
    combine(results, "client", |s| match &notice {
        None => format!("Disconnected {}.", s.text),
        Some(notice) => format!(
            "Asked {} to close {} files; {} disconnected {}.",
            s.text,
            if s.plural { "their" } else { "its" },
            if s.plural { "they are" } else { "it is" },
            when(notice.grace)
        ),
    })
}

fn send_message(backend: &dyn Backend, ids: &[u32], text: &str) -> Result<String, BackendError> {
    if ids.is_empty() {
        return Err(invalid("Select the clients to send the message to."));
    }
    let text = message(text, true)?;
    let names = client_names(backend, ids);
    let results = ids
        .iter()
        .map(|id| (names[id].clone(), backend.send_message(*id, &text)))
        .collect();
    combine(results, "client", |s| {
        format!("Sent the message to {}.", s.text)
    })
}

#[derive(Deserialize)]
struct DatabaseForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    back: String,
    /// The folder filter, for `all`.
    #[serde(default)]
    folder: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    delay: String,
}

async fn change_databases(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Admin(admin): Admin,
    Path((target, change)): Path<(String, String)>,
    Form(form): Form<DatabaseForm>,
) -> Response {
    if let Some(refused) = refuse_stale(&admin, &form.csrf, peer) {
        return refused;
    }
    let target = match target.as_str() {
        "all" => Some(Target::All),
        id => id.parse().ok().map(Target::One),
    };
    // Only known words reach the log: the path is the client's.
    let action = match change.as_str() {
        known @ ("open" | "pause" | "resume" | "close") => known,
        _ => "change",
    };
    let what = format!(
        "{action} databases {}",
        match target {
            Some(Target::One(id)) => id.to_string(),
            Some(Target::All) => "all".to_owned(),
            None => "?".to_owned(),
        }
    );
    let outcome = match target {
        Some(target) => change_files(state.backend(), target, &change, &form),
        None => Err(BackendError::NotFound),
    };
    finish(&state, &admin, peer, &form.back, &what, outcome)
}

/// Apply `change` (`open`, `pause`, `resume` or `close`) to `target`'s
/// files one by one; `all` takes those of the folder filter that the change
/// applies to.
fn change_files(
    backend: &dyn Backend,
    target: Target,
    change: &str,
    form: &DatabaseForm,
) -> Result<String, BackendError> {
    let (change, applies, verb): (DatabaseChange, fn(DatabaseStatus) -> bool, &str) = match change {
        "open" => (
            DatabaseChange::Open,
            |s| s == DatabaseStatus::Closed,
            "Opened",
        ),
        "pause" => (
            DatabaseChange::Pause,
            |s| s == DatabaseStatus::Normal,
            "Paused",
        ),
        "resume" => (
            DatabaseChange::Resume,
            |s| s == DatabaseStatus::Paused,
            "Resumed",
        ),
        "close" => {
            let notice = Notice {
                message: message(&form.message, false)?,
                grace: delay(&form.delay)?.unwrap_or(DEFAULT_GRACE),
            };
            (DatabaseChange::Close(notice), closable, "Closing")
        }
        _ => return Err(invalid("The Admin Console has no such action.")),
    };
    let databases = backend.databases();
    let files: Vec<&Database> = match target {
        Target::One(id) => vec![databases
            .iter()
            .find(|d| d.id == id)
            .ok_or(BackendError::NotFound)?],
        Target::All => {
            let folder = (!form.folder.is_empty()).then_some(form.folder.as_str());
            databases
                .iter()
                .filter(|d| in_folder(d, folder) && applies(d.status))
                .collect()
        }
    };
    if files.is_empty() {
        return Err(BackendError::InvalidState(
            "No database is in a state this applies to.".into(),
        ));
    }
    let results = files
        .iter()
        .map(|d| {
            (
                printable(display_name(&d.name)),
                backend.change_database(d.id, &change),
            )
        })
        .collect();
    combine(results, "database", |s| format!("{verb} {}.", s.text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_query_round_trips_and_ignores_what_it_does_not_know() {
        let state = ViewState {
            db: Some(3),
            folder: Some("/srv/Data bases/Ware&house".into()),
            search: "inv é".into(),
            sort: DatabaseSort::Clients,
            client_search: String::new(),
            client_sort: ClientSort::SignIn,
        };
        let query = state.query();
        assert!(
            crate::views::is_return_path(&format!("{PAGE}?{query}")),
            "{query}"
        );
        assert_eq!(ViewState::parse(&query), (state.clone(), None));
        let (parsed, close) = ViewState::parse(&format!("{query}&close=all&sort=bogus&x=1"));
        assert_eq!(close, Some(Target::All));
        assert_eq!(parsed.sort, DatabaseSort::Filename);
        let long = format!("q={}", "a".repeat(500));
        assert_eq!(ViewState::parse(&long).0.search.len(), MAX_SEARCH);

        // The longest state the view keeps still fits in the address its
        // forms return to: searches are cut by their encoded size, and a
        // folder too long to fit is not a filter.
        let worst = |folder: &str| {
            let wide = "\u{1F600}".repeat(MAX_SEARCH);
            let mut query = form_urlencoded::Serializer::new(String::new());
            query
                .append_pair("db", "4294967295")
                .append_pair("folder", folder)
                .append_pair("q", &wide)
                .append_pair("sort", "clients")
                .append_pair("cq", &wide)
                .append_pair("csort", "signin");
            ViewState::parse(&query.finish()).0
        };
        let kept = worst(&format!("/{}", "\u{e9}".repeat(159)));
        assert!(kept.folder.is_some());
        assert_eq!(kept.search.chars().count(), MAX_SEARCH_ENCODED / 12);
        let back = kept.href();
        assert!(crate::views::is_return_path(&back), "{}", back.len());
        assert_eq!(worst(&format!("/{}", "a".repeat(1440))).folder, None);
    }
}
