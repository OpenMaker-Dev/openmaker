//! The console's views: what each tab and sub-view shows, the live regions
//! its page refreshes, and the actions its forms post.
//!
//! Each area of the console has a module of its own here. A view that
//! OpenMaker Server cannot back yet, or whose backend answers
//! [`BackendError::Unsupported`], says it is not available
//! (`docs/admin-console.md`, "Scope").
//!
//! - [`content`] renders a view inside the application shell, and
//!   [`sidebar`] what a tab's sidebar shows besides its sub-views.
//! - [`fragment`] renders one of a page's live regions, which the script
//!   re-fetches every few seconds from
//!   `/admin-console/fragments/<tab>/<region>` while the page is visible.
//!   Mark a region with `data-refresh` (see [`live`]).
//! - [`routes`] adds the views' form actions. Every action is a POST
//!   carrying the session's CSRF token ([`refuse_stale`]) and ends with
//!   [`finish`], which logs it, leaves the outcome for the next page and
//!   returns to the page it came from.

mod administrator;
mod backups;
mod dashboard;
mod databases;
mod folders;
mod general_settings;
mod logs;
mod schedules;
mod ssl_certificate;

use std::net::SocketAddr;

use axum::response::{IntoResponse, Redirect, Response};
use axum::Router;
use jiff::Zoned;
use log::{info, warn};
use maud::{html, Markup};

use crate::app::{is_app_path, stale_form, AppState, DASHBOARD};
use crate::backend::{Backend, BackendError};
use crate::nav::{Tab, View};
use crate::pages::{self, Sidebar};
use crate::session::{Flash, SignedIn};
use crate::stats::Statistics;

/// What a view renders from.
pub(crate) struct Ctx<'a> {
    pub(crate) backend: &'a dyn Backend,
    pub(crate) session: &'a SignedIn,
    /// The page's own path: forms return here.
    pub(crate) path: &'a str,
    /// The page's query parameters, in order, decoded from [`Ctx::query`].
    pub(crate) params: &'a [(String, String)],
    /// The server's clock.
    pub(crate) now: &'a Zoned,
    /// The page's query string, without the `?`: a view's selection,
    /// search and sort. A fragment gets its page's.
    pub(crate) query: &'a str,
    /// The Dashboard's system statistics.
    pub(crate) statistics: &'a Statistics,
}

/// The parameters of `query`, a page's query string, in order: what
/// [`Ctx::param`] reads.
pub(crate) fn params(query: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

impl Ctx<'_> {
    /// The first value of the page's query parameter `name`.
    pub(crate) fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// The content of view `view` of `tab` (`None` for a tab without views).
/// Each view's module adds its arm here, by the routes of `crate::nav`.
pub(crate) fn content(ctx: &Ctx, tab: &Tab, view: Option<&View>) -> Markup {
    match (tab.route, view.map(|v| v.route)) {
        ("dashboard", None) => dashboard::page(ctx),
        ("activities", None) => databases::page(ctx),
        ("backups", Some("allbackups")) => backups::all(ctx),
        ("backups", Some("preservedbackups")) => backups::preserved(ctx),
        ("backups", Some("backupsettings")) => backups::settings(),
        ("configuration", Some("generalsettings")) => general_settings::page(ctx),
        ("configuration", Some("folders")) => folders::page(ctx),
        ("configuration", Some("schedules")) => schedules::page(ctx),
        ("configuration", Some("sslcertificate")) => ssl_certificate::page(ctx),
        ("administration", Some("administrators")) => administrator::page(ctx),
        ("logging", Some("logviewer")) => logs::viewer(ctx),
        ("logging", Some("logsettings")) => logs::settings(ctx),
        _ => pages::unavailable(view.map_or(tab.title, |v| v.title)),
    }
}

/// What `tab`'s sidebar shows besides its title, introduction and
/// sub-views. A tab whose sidebar shows more adds its arm here.
pub(crate) fn sidebar(ctx: &Ctx, tab: &Tab) -> Sidebar {
    match tab.route {
        "backups" => backups::sidebar(ctx),
        _ => Sidebar::default(),
    }
}

/// Live region `region` of `tab`'s page, or `None` when it has no such
/// region. Each view's module adds its regions here. A region whose state
/// the backend cannot read is an error, not markup saying so: the script
/// then keeps what the region shows, and the boxes checked in it, until a
/// read works again; loading the page shows the error.
pub(crate) fn fragment(ctx: &Ctx, tab: &str, region: &str) -> Option<Result<Markup, BackendError>> {
    match (tab, region) {
        ("dashboard", _) => dashboard::fragment(ctx, region).map(Ok),
        ("activities", _) => databases::fragment(ctx, region).map(Ok),
        ("backups", backups::STATUS_REGION) => Some(Ok(backups::status(ctx))),
        ("configuration", schedules::REGION) => Some(schedules::region(ctx)),
        _ => None,
    }
}

/// The views' form actions.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .merge(general_settings::routes())
        .merge(databases::routes())
        .merge(schedules::routes())
        .merge(backups::routes())
        .merge(logs::routes())
}

/// A live region: `body` in an element the script replaces every few
/// seconds with `/admin-console/fragments/<tab>/<region>`, which
/// [`fragment`] renders with the same markup.
pub(crate) fn live(tab: &str, region: &str, body: Markup) -> Markup {
    live_query(tab, region, "", body)
}

/// A live region of a page whose content depends on its query (`query`,
/// without the `?`): the script fetches the region with the same query,
/// which [`fragment`] finds in [`Ctx::query`].
pub(crate) fn live_query(tab: &str, region: &str, query: &str, body: Markup) -> Markup {
    html! {
        div.live-region data-refresh={
            "/admin-console/fragments/" (tab) "/" (region)
            @if !query.is_empty() { "?" (query) }
        } {
            (body)
        }
    }
}

/// What a view shows in place of what the backend could not give it:
/// `unavailable` when the server lacks the feature, else why it failed.
pub(crate) fn not_shown(unavailable: &str, error: &BackendError) -> Markup {
    match error {
        BackendError::Unsupported => html! { p.unavailable { (unavailable) } },
        error => {
            html! { p.unavailable.read-failed role="status" { "Could not be read: " (error) } }
        }
    }
}

/// The refusal of a form without the session's CSRF token, or `None` when
/// the form carries it.
pub(crate) fn refuse_stale(admin: &SignedIn, csrf: &str, peer: SocketAddr) -> Option<Response> {
    (!admin.csrf_matches(csrf)).then(|| stale_form(peer))
}

/// End an action: log it with who asked and from where, leave its outcome
/// (`Ok` with a sentence saying what was done, or the backend's error) for
/// the page it returns to, and return there (`back`, when it is a console
/// page, optionally with a query: see [`is_return_path`]). `what` names the
/// action for the log, such as "change Block New Users", which is logged
/// followed by "done" or "failed" and the reason.
pub(crate) fn finish(
    state: &AppState,
    admin: &SignedIn,
    peer: SocketAddr,
    back: &str,
    what: &str,
    outcome: Result<String, BackendError>,
) -> Response {
    let flash = match outcome {
        Ok(done) => {
            info!("{peer}: {}: {what}: done", admin.user);
            Flash {
                ok: true,
                text: done,
            }
        }
        Err(e) => {
            warn!("{peer}: {}: {what}: failed: {e}", admin.user);
            Flash {
                ok: false,
                text: e.to_string(),
            }
        }
    };
    state.sessions().set_flash(&admin.cookie, flash);
    let back = if is_return_path(back) {
        back
    } else {
        DASHBOARD
    };
    Redirect::to(back).into_response()
}

/// The longest view query, encoded, that a page keeps: a view's state
/// must fit in it, as every form returns to its page with that query.
pub(crate) const MAX_QUERY: usize = 2048;

/// Whether a form may return to `back`: a page of the console's
/// application, with an optional query of URL-safe characters (a view's
/// selection, search and sort, percent-encoded) of at most [`MAX_QUERY`]
/// bytes, so that a form cannot send the browser elsewhere or put anything
/// but a query in the address.
pub(crate) fn is_return_path(back: &str) -> bool {
    let (path, query) = back.split_once('?').unwrap_or((back, ""));
    is_app_path(path)
        && query.len() <= MAX_QUERY
        && query
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"%&=+-._~*".contains(&b))
}
