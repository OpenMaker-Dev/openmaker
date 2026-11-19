//! Logs (`docs/re/admin-console.md` 4.9). The Log Viewer pages through one
//! of the server's log files (`Backend::log_files`, `Backend::read_log`),
//! newest first, a Block Size of entries at a time, and downloads it as
//! text. Log Settings says which settings OpenMaker Server has; of them,
//! the Block Size is kept for the session (inf: FileMaker Server keeps it
//! on the server). A server without logs says so.

use std::fmt::Write as _;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Form, Query, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use log::{info, warn};
use maud::{html, Markup};
use serde::Deserialize;

use crate::app::{Admin, AppState};
use crate::backend::{Backend, BackendError, LogEntry, LogFile, LogPage};
use crate::components::{form_fields, row, section, title, unavailable_setting};
use crate::icons::Icon;
use crate::pages;
use crate::views::{finish, refuse_stale, Ctx};

const VIEWER: &str = "/admin-console/app/logging/logviewer";
const DOWNLOAD: &str = "/admin-console/app/logging/logviewer/download";
const BLOCK_SIZE: &str = "/admin-console/app/logging/logsettings/blocksize";

/// Entries on a page unless the administrator chose otherwise: FileMaker
/// Server's Block Size as the reference shows it.
const DEFAULT_BLOCK_SIZE: usize = 200;
/// The Block Sizes an administrator can choose (inf).
const BLOCK_SIZES: [usize; 4] = [100, 200, 500, 1000];
/// The furthest from the newest entry a page may start.
const MAX_SKIP: usize = 10_000_000;
/// The longest log file name a page or a download may name, in bytes.
const MAX_FILE_NAME: usize = 255;
/// The most text a download holds: the newest entries that fit (inf).
const MAX_DOWNLOAD: usize = 16 * 1024 * 1024;
/// Entries a download reads at a time.
const DOWNLOAD_PAGE: usize = 1000;

const NOT_YET: &str = "Not available in OpenMaker Server yet";
const INTERACTIVE_HELP: &str = "Interactive View shows the entries of several log files \
    together, in time order. OpenMaker Server does not have it yet.";
const TOP_CALL_HELP: &str = "The Top Call Statistics log lists the client requests that took \
    longest. OpenMaker Server does not write it.";
const STATISTICS_HELP: &str = "The server statistics log records the server's load at \
    intervals. OpenMaker Server does not write it.";
const SCRIPT_LOG_HELP: &str = "Writes the events of server scripts to the event log instead \
    of a script log of their own. OpenMaker Server does not keep a script log yet.";
const DATA_API_HELP: &str = "OpenMaker Server has no FileMaker Data API.";
const WEB_PUBLISHING_HELP: &str = "OpenMaker Server has no Web Publishing Engine.";
const BLOCK_SIZE_HELP: &str = "How many entries the Log Viewer shows on a page. OpenMaker \
    keeps your choice until you sign out.";

/// The Log Viewer's rows per page.
fn block_size(ctx: &Ctx) -> usize {
    ctx.session
        .log_block_size
        .filter(|rows| BLOCK_SIZES.contains(rows))
        .unwrap_or(DEFAULT_BLOCK_SIZE)
}

/// Log Viewer: the chosen file (the page's `file`) from `skip` (the page's
/// `skip`, counted from the newest entry); before a file is chosen, "No
/// data to display".
pub(crate) fn viewer(ctx: &Ctx) -> Markup {
    let files = match ctx.backend.log_files() {
        Ok(files) => files,
        Err(BackendError::Unsupported) => {
            return html! {
                (title("Log Viewer"))
                p.unavailable { "Logs are not available in OpenMaker Server yet." }
            }
        }
        Err(e) => {
            return html! {
                (title("Log Viewer"))
                p.read-failed { "The logs could not be listed: " (e) }
            }
        }
    };
    let requested = ctx.param("file");
    let chosen = requested.and_then(|name| files.iter().find(|f| f.name == name));
    let (interactive, interactive_toggle) = unavailable_setting("Interactive View");
    html! {
        (title("Log Viewer"))
        div.toolbar.log-toolbar {
            @if let Some(file) = chosen {
                a.btn href=(download_url(&file.name)) download=(safe_file_name(&file.name)) {
                    "Download"
                }
            } @else {
                button.btn type="button" disabled { "Download" }
            }
            button.btn type="button" disabled title=(NOT_YET) { "Download All Logs" }
            button.btn type="button" disabled title=(NOT_YET) { "Reset All Filters" }
            // A checkbox the style sheet reads, so that collapsing works
            // without the script.
            input.visually-hidden id="collapse-sidebar" type="checkbox";
            label.btn.collapse-sidebar for="collapse-sidebar" {
                span.when-expanded { "Collapse Side Bar" }
                span.when-collapsed { "Expand Side Bar" }
            }
        }
        (row("Interactive View", Some(INTERACTIVE_HELP), interactive, interactive_toggle))
        (row(
            "Log Files",
            None,
            html! { @if let Some(file) = chosen { (file.name) } },
            file_menu(&files, chosen),
        ))
        @match chosen {
            Some(file) => (entries(ctx, file)),
            None => div.log-empty {
                p {
                    @if requested.is_some() {
                        "The server has no log file of that name."
                    } @else {
                        "No data to display"
                    }
                }
            },
        }
    }
}

/// "Select" and the menu of log files.
fn file_menu(files: &[LogFile], chosen: Option<&LogFile>) -> Markup {
    html! {
        details.menu.log-menu {
            summary.link-button.menu-button { "Select" (Icon::Chevron.render(12)) }
            ul.menu-list {
                @for file in files {
                    @let current = chosen.is_some_and(|c| c.name == file.name);
                    li {
                        a.menu-item href=(viewer_url(&file.name, 0))
                            aria-current=[current.then_some("true")] { (file.name) }
                    }
                }
                @if files.is_empty() {
                    li.menu-empty { "The server has no log files." }
                }
            }
        }
    }
}

/// A page of `file`'s entries and the links to the pages beside it.
fn entries(ctx: &Ctx, file: &LogFile) -> Markup {
    let block = block_size(ctx);
    let skip = ctx
        .param("skip")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_SKIP);
    let page = match ctx
        .backend
        .read_log(&file.name, LogPage { skip, limit: block })
    {
        Ok(page) => page,
        Err(e) => return html! { p.read-failed.log-error { "The log could not be read: " (e) } },
    };
    let rows = &page.entries[..page.entries.len().min(block)];
    let next = skip + rows.len();
    html! {
        div.table-scroll {
            table.log-table {
                colgroup {
                    col.log-col-time;
                    col.log-col-level;
                    col.log-col-error;
                    col.log-col-host;
                    col;
                }
                thead {
                    tr {
                        th scope="col" { "Timestamp" }
                        th scope="col" { "Level" }
                        th scope="col" { "Error" }
                        th scope="col" { "Host" }
                        th scope="col" { "Message" }
                    }
                }
                tbody {
                    @for entry in rows {
                        tr {
                            td title=(entry.timestamp) { (shown_time(&entry.timestamp)) }
                            td title=(entry.level) { (entry.level) }
                            td { @if let Some(code) = entry.code { (code) } }
                            td title=(entry.host) { (entry.host) }
                            td.log-message { (entry.message) }
                        }
                    }
                    @if rows.is_empty() {
                        tr.empty-row { td colspan="5" { "No data to display" } }
                    }
                }
            }
        }
        nav.log-pager aria-label="Pages of the log" {
            @if !rows.is_empty() {
                span.log-range { (thousands(skip + 1)) " to " (thousands(next)) }
            }
            @if skip > 0 {
                a.link-button href=(viewer_url(&file.name, skip.saturating_sub(block))) rel="prev" {
                    "Previous"
                }
            } @else {
                button.link-button type="button" disabled { "Previous" }
            }
            span.log-page { "Page " (thousands(skip / block + 1)) }
            @if page.more && !rows.is_empty() && next <= MAX_SKIP {
                a.link-button href=(viewer_url(&file.name, next)) rel="next" { "Next" }
            } @else {
                button.link-button type="button" disabled { "Next" }
            }
        }
    }
}

/// A time stamp as FileMaker Server writes it, `2026-10-29 18:05:00.064
/// -0500`, as its Log Viewer shows it, `10/29/2026 18:05:00`; any other
/// text as it is.
fn shown_time(written: &str) -> String {
    let b = written.as_bytes();
    let digits = |from: usize, to: usize| {
        b.get(from..to)
            .is_some_and(|d| d.iter().all(u8::is_ascii_digit))
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    let shaped = digits(0, 4)
        && at(4, b'-')
        && digits(5, 7)
        && at(7, b'-')
        && digits(8, 10)
        && at(10, b' ')
        && digits(11, 13)
        && at(13, b':')
        && digits(14, 16)
        && at(16, b':')
        && digits(17, 19);
    if !shaped {
        return written.to_owned();
    }
    // The first 19 bytes are ASCII, so these are character boundaries.
    format!(
        "{}/{}/{} {}",
        &written[5..7],
        &written[8..10],
        &written[..4],
        &written[11..19]
    )
}

/// `n` with thousands separators: `1,234`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `value` for a URL's query: every byte but the unreserved ones
/// percent-encoded.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn viewer_url(file: &str, skip: usize) -> String {
    if skip == 0 {
        format!("{VIEWER}?file={}", encode(file))
    } else {
        format!("{VIEWER}?file={}&skip={skip}", encode(file))
    }
}

fn download_url(file: &str) -> String {
    format!("{DOWNLOAD}?file={}", encode(file))
}

/// Log Settings: the settings OpenMaker Server does not have say so; the
/// Block Size is the session's.
pub(crate) fn settings(ctx: &Ctx) -> Markup {
    let setting = |label: &str, help_text: Option<&str>| {
        let (value, toggle) = unavailable_setting(label);
        row(label, help_text, value, toggle)
    };
    let block = block_size(ctx);
    html! {
        (title("Log Settings"))
        (section("Logging Options", None))
        (setting("Top Call Statistics", Some(TOP_CALL_HELP)))
        (setting("Server Statistics", Some(STATISTICS_HELP)))
        (setting("Script Log to Event Log", Some(SCRIPT_LOG_HELP)))
        (section("FileMaker Script Log Options", None))
        (setting("Allow FileMaker Script Engine Log", None))
        (setting("Allow FileMaker Data API Script Log", Some(DATA_API_HELP)))
        (setting("Allow Web Publishing Script Log", Some(WEB_PUBLISHING_HELP)))
        (section("Log Viewer Settings", Some(BLOCK_SIZE_HELP)))
        (row("Block Size", None, html! { (block) " rows" }, block_menu(ctx, block)))
    }
}

/// "Change" and the menu of Block Sizes, each a button that saves it.
fn block_menu(ctx: &Ctx, current: usize) -> Markup {
    html! {
        details.menu.log-menu {
            summary.link-button.menu-button { "Change" (Icon::Chevron.render(12)) }
            form.menu-list method="post" action=(BLOCK_SIZE) {
                (form_fields(ctx))
                @for rows in BLOCK_SIZES {
                    button.menu-item type="submit" name="rows" value=(rows)
                        aria-current=[(rows == current).then_some("true")] { (rows) " rows" }
                }
            }
        }
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(DOWNLOAD, get(download))
        .route(BLOCK_SIZE, post(set_block_size))
}

#[derive(Deserialize)]
struct BlockSizeForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    back: String,
    #[serde(default)]
    rows: String,
}

async fn set_block_size(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Admin(admin): Admin,
    Form(form): Form<BlockSizeForm>,
) -> Response {
    if let Some(refused) = refuse_stale(&admin, &form.csrf, peer) {
        return refused;
    }
    let outcome = form
        .rows
        .parse::<usize>()
        .ok()
        .filter(|rows| BLOCK_SIZES.contains(rows))
        .ok_or_else(|| BackendError::Invalid("Choose one of the block sizes offered.".into()))
        .map(|rows| {
            state.sessions().set_log_block_size(&admin.cookie, rows);
            format!("The Log Viewer shows {rows} rows at a time.")
        });
    finish(
        &state,
        &admin,
        peer,
        &form.back,
        "change the Log Viewer's block size",
        outcome,
    )
}

#[derive(Deserialize)]
struct DownloadQuery {
    #[serde(default)]
    file: String,
}

/// Download: the chosen log file as text, as an attachment.
async fn download(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Admin(admin): Admin,
    Query(query): Query<DownloadQuery>,
) -> Response {
    let name = query.file;
    if name.is_empty() || name.len() > MAX_FILE_NAME {
        return not_found();
    }
    // Reading a large log takes many backend calls: off the console's
    // request threads.
    let reader = state.clone();
    let read = {
        let name = name.clone();
        tokio::task::spawn_blocking(move || log_text(reader.backend(), &name)).await
    };
    match read {
        Ok(Ok(text)) => {
            info!("{peer}: {} downloaded the log {name:?}", admin.user);
            let disposition = HeaderValue::from_str(&format!(
                "attachment; filename=\"{}\"",
                safe_file_name(&name)
            ))
            .unwrap_or(HeaderValue::from_static("attachment"));
            (
                [
                    (
                        CONTENT_TYPE,
                        HeaderValue::from_static("text/plain; charset=utf-8"),
                    ),
                    (CONTENT_DISPOSITION, disposition),
                ],
                text,
            )
                .into_response()
        }
        Ok(Err(BackendError::NotFound)) => not_found(),
        Ok(Err(e)) => {
            warn!(
                "{peer}: {} could not download the log {name:?}: {e}",
                admin.user
            );
            message(
                StatusCode::SERVICE_UNAVAILABLE,
                "Log Not Available",
                &format!("The log could not be read: {e}"),
            )
        }
        Err(e) => {
            warn!("{peer}: reading the log {name:?} failed: {e}");
            message(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Log Not Available",
                "The log could not be read.",
            )
        }
    }
}

fn message(status: StatusCode, title: &str, text: &str) -> Response {
    (status, Html(pages::message(title, text).into_string())).into_response()
}

fn not_found() -> Response {
    message(
        StatusCode::NOT_FOUND,
        "Log Not Found",
        "The server has no log file of that name.",
    )
}

/// Log file `name` as text, in the file's order (oldest first): one line
/// per entry, its fields in the Log Viewer's column order separated by
/// tabs. A log larger than [`MAX_DOWNLOAD`] gives its newest entries that
/// fit. Only a file the server lists is read.
fn log_text(backend: &dyn Backend, name: &str) -> Result<String, BackendError> {
    if !backend.log_files()?.iter().any(|f| f.name == name) {
        return Err(BackendError::NotFound);
    }
    let mut lines = Vec::new();
    let mut size = 0;
    let mut skip = 0;
    'read: loop {
        let page = backend.read_log(
            name,
            LogPage {
                skip,
                limit: DOWNLOAD_PAGE,
            },
        )?;
        let entries = &page.entries[..page.entries.len().min(DOWNLOAD_PAGE)];
        for entry in entries {
            let line = line(entry);
            if size + line.len() > MAX_DOWNLOAD {
                break 'read;
            }
            size += line.len();
            lines.push(line);
        }
        if !page.more || entries.is_empty() {
            break;
        }
        skip += entries.len();
    }
    lines.reverse();
    Ok(lines.concat())
}

/// An entry as a line of the log: tabs and line breaks inside a field
/// become spaces, so each entry stays one line of five fields.
fn line(entry: &LogEntry) -> String {
    let clean = |text: &str| text.replace(['\t', '\r', '\n'], " ");
    let code = entry.code.map(|c| c.to_string()).unwrap_or_default();
    format!(
        "{}\t{}\t{code}\t{}\t{}\n",
        clean(&entry.timestamp),
        clean(&entry.level),
        clean(&entry.host),
        clean(&entry.message)
    )
}

/// A download's file name: the log's, with anything but ASCII letters,
/// digits, `.`, `-` and `_` replaced, so that it can neither break the
/// header nor name a place outside the browser's download folder.
fn safe_file_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .take(100)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = safe.trim_start_matches('.');
    if safe.is_empty() {
        "log.txt".to_owned()
    } else {
        safe.to_owned()
    }
}
