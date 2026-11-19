//! The console's routes, at FileMaker Server's URLs
//! (`docs/re/admin-console.md` section 2): sign-in and sign-out, the
//! application's tabs and views, alerts, and the assets. Every response
//! carries the security headers, every state-changing request must come
//! from the console's own origin, and every form of a session carries its
//! CSRF token (`docs/admin-console.md`, "Security").

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use axum::extract::{
    ConnectInfo, DefaultBodyLimit, FromRequestParts, Path, Query, RawQuery, Request, State,
};
use axum::http::header::{
    CACHE_CONTROL, CONNECTION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, HOST, ORIGIN,
    REFERRER_POLICY, SET_COOKIE, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{AppendHeaders, Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use jiff::Zoned;
use log::{info, warn};
use maud::Markup;
use serde::Deserialize;

use crate::api;
use crate::assets;
use crate::auth::{Attempt, Credentials, PasswordDigest, Throttle, LOCKOUT, MAX_FAILURES};
use crate::backend::Backend;
use crate::nav::{self, Tab, View};
use crate::pages::{self, Chrome};
use crate::session::{self, Lookup, Sessions, SignedIn};
use crate::stats::Statistics;
use crate::views;

const SIGN_IN: &str = "/admin-console/signin";
pub(crate) const DASHBOARD: &str = "/admin-console/app/dashboard";

/// The largest request body: the console's forms are a few short fields.
const MAX_BODY: usize = 16 * 1024;

const CSP: &str = "default-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

const INVALID_SIGN_IN: &str = "Enter a valid user name and password.";
const LOCKED_OUT: &str =
    "Too many failed sign-in attempts came from this address. Try again in a few minutes.";
const TIMED_OUT: &str = "Your session has timed out. Sign in again.";

/// What every request shares.
#[derive(Clone)]
pub(crate) struct AppState(Arc<Inner>);

struct Inner {
    backend: Arc<dyn Backend>,
    user: String,
    credentials: Credentials,
    sessions: Mutex<Sessions>,
    throttle: Mutex<Throttle>,
    statistics: Arc<Statistics>,
    api_tokens: Mutex<api::Tokens>,
}

impl AppState {
    pub(crate) fn new(backend: Arc<dyn Backend>, user: &str, password: PasswordDigest) -> Self {
        Self(Arc::new(Inner {
            backend,
            user: user.to_owned(),
            credentials: Credentials::new(user, password),
            sessions: Mutex::default(),
            throttle: Mutex::default(),
            statistics: Arc::default(),
            api_tokens: Mutex::default(),
        }))
    }

    /// The Dashboard's system statistics, which `stats::run` fills.
    pub(crate) fn statistics(&self) -> &Arc<Statistics> {
        &self.0.statistics
    }

    /// The server the console administers.
    pub(crate) fn backend(&self) -> &dyn Backend {
        &*self.0.backend
    }

    // A panic elsewhere cannot leave these maps half-changed (every change
    // is one insert or remove), so a poisoned lock is still usable.
    pub(crate) fn sessions(&self) -> MutexGuard<'_, Sessions> {
        self.0
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn throttle(&self) -> MutexGuard<'_, Throttle> {
        self.0
            .throttle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The Admin API's open tokens.
    pub(crate) fn api_tokens(&self) -> MutexGuard<'_, api::Tokens> {
        self.0
            .api_tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The administrator's user name.
    pub(crate) fn user(&self) -> &str {
        &self.0.user
    }

    /// Check a sign-in from `address`, of the console or the Admin API,
    /// against the administrator's credentials and the address's lockout,
    /// and count a failure. One lock covers the check and its outcome, so
    /// that attempts sent at once cannot all pass the lockout check before
    /// any failure counts.
    pub(crate) fn attempt_sign_in(
        &self,
        address: IpAddr,
        user: &str,
        password: &str,
        now: Instant,
    ) -> Attempt {
        let mut throttle = self.throttle();
        if throttle.locked(address, now) {
            return Attempt::LockedOut;
        }
        if self.0.credentials.check(user, password) {
            throttle.succeeded(address);
            return Attempt::Accepted;
        }
        if throttle.failed(address, now) {
            warn!(
                "{address}: {MAX_FAILURES} failed sign-ins lock the address out for {} s",
                LOCKOUT.as_secs()
            );
            Attempt::LockedOut
        } else {
            Attempt::Refused
        }
    }
}

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(to_dashboard))
        .route("/admin-console", get(to_dashboard))
        .route("/admin-console/", get(to_dashboard))
        .route("/admin-console/app", get(to_dashboard))
        .route("/admin-console/app/", get(to_dashboard))
        .route(SIGN_IN, get(sign_in_page).post(sign_in))
        .route("/admin-console/signout", post(sign_out))
        .route("/admin-console/app/{tab}", get(tab_page))
        .route("/admin-console/app/{tab}/{view}", get(view_page))
        .route(
            "/admin-console/alerts/{id}/acknowledge",
            post(acknowledge_alert),
        )
        .route("/admin-console/fragments/{tab}/{region}", get(fragment))
        .route("/admin-console/assets/{name}", get(asset))
        .merge(views::routes())
        .merge(api::routes(state.clone()))
        .fallback(fallback)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn(same_origin))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}

/// A signed-in administrator. Without a session, the request is sent to the
/// sign-in page instead.
pub(crate) struct Admin(pub(crate) SignedIn);

impl FromRequestParts<AppState> for Admin {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let lookup = match session_cookie(&parts.headers) {
            Some(value) => state.sessions().touch(value, Instant::now()),
            None => Lookup::None,
        };
        match lookup {
            Lookup::Valid(session) => Ok(Admin(session)),
            Lookup::TimedOut => Err(to_sign_in(true)),
            Lookup::None => Err(to_sign_in(false)),
        }
    }
}

/// The value of the request's session cookie.
fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| {
            let (name, value) = c.trim().split_once('=')?;
            (name == session::COOKIE).then_some(value)
        })
}

fn set_session_cookie(value: &str) -> String {
    format!(
        "{}={value}; Path=/admin-console; HttpOnly; Secure; SameSite=Strict",
        session::COOKIE
    )
}

fn clear_session_cookie() -> String {
    format!(
        "{}=; Path=/admin-console; HttpOnly; Secure; SameSite=Strict; Max-Age=0",
        session::COOKIE
    )
}

fn to_sign_in(timed_out: bool) -> Response {
    let target = if timed_out {
        "/admin-console/signin?timeout=1"
    } else {
        SIGN_IN
    };
    (
        AppendHeaders([(SET_COOKIE, clear_session_cookie())]),
        Redirect::to(target),
    )
        .into_response()
}

fn html(status: StatusCode, markup: Markup) -> Response {
    (status, Html(markup.into_string())).into_response()
}

async fn to_dashboard() -> Redirect {
    Redirect::to(DASHBOARD)
}

#[derive(Deserialize)]
struct SignInQuery {
    timeout: Option<String>,
}

async fn sign_in_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SignInQuery>,
) -> Response {
    if let Some(value) = session_cookie(&headers) {
        if let Lookup::Valid(_) = state.sessions().touch(value, Instant::now()) {
            return Redirect::to(DASHBOARD).into_response();
        }
    }
    let message = query.timeout.is_some().then_some(TIMED_OUT);
    html(StatusCode::OK, pages::sign_in(message, ""))
}

#[derive(Deserialize)]
struct SignInForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

async fn sign_in(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(form): Form<SignInForm>,
) -> Response {
    let now = Instant::now();
    match state.attempt_sign_in(peer.ip(), &form.username, &form.password, now) {
        Attempt::Accepted => {
            let value = state.sessions().open(&state.0.user, now);
            info!("{peer}: signed in to the Admin Console as {}", state.0.user);
            (
                AppendHeaders([(SET_COOKIE, set_session_cookie(&value))]),
                Redirect::to(DASHBOARD),
            )
                .into_response()
        }
        Attempt::LockedOut => {
            warn!("{peer}: Admin Console sign-in refused: the address is locked out");
            html(
                StatusCode::TOO_MANY_REQUESTS,
                pages::sign_in(Some(LOCKED_OUT), &form.username),
            )
        }
        Attempt::Refused => {
            // The name typed is not logged: it may be a password typed in
            // the wrong field.
            warn!("{peer}: Admin Console sign-in failed");
            html(
                StatusCode::OK,
                pages::sign_in(Some(INVALID_SIGN_IN), &form.username),
            )
        }
    }
}

#[derive(Deserialize)]
struct CsrfForm {
    #[serde(default)]
    csrf: String,
}

async fn sign_out(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    admin: Admin,
    Form(form): Form<CsrfForm>,
) -> Response {
    if !admin.0.csrf_matches(&form.csrf) {
        return stale_form(peer);
    }
    if let Some(value) = session_cookie(&headers) {
        state.sessions().close(value);
    }
    info!("{peer}: {} signed out of the Admin Console", admin.0.user);
    (
        AppendHeaders([(SET_COOKIE, clear_session_cookie())]),
        Redirect::to(SIGN_IN),
    )
        .into_response()
}

async fn tab_page(
    State(state): State<AppState>,
    admin: Admin,
    Path(tab): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let Some(tab) = nav::tab(&tab) else {
        return not_found().await;
    };
    match tab.views.first() {
        Some(first) => Redirect::to(&tab.view_url(first)).into_response(),
        None => app_page(&state, &admin, tab, None, query.as_deref()),
    }
}

async fn view_page(
    State(state): State<AppState>,
    admin: Admin,
    Path((tab, view)): Path<(String, String)>,
    RawQuery(query): RawQuery,
) -> Response {
    let Some((tab, view)) = nav::tab(&tab).and_then(|t| Some((t, t.view(&view)?))) else {
        return not_found().await;
    };
    app_page(&state, &admin, tab, Some(view), query.as_deref())
}

fn app_page(
    state: &AppState,
    admin: &Admin,
    tab: &Tab,
    view: Option<&View>,
    query: Option<&str>,
) -> Response {
    let alerts = state.0.backend.alerts();
    let now = Zoned::now();
    let path = match view {
        Some(view) => tab.view_url(view),
        None => tab.url(),
    };
    let query = query.unwrap_or_default();
    // Acknowledging an alert returns to the page as it is, with its view's
    // selection, search and sort, when they make a valid return address.
    let page = format!("{path}?{query}");
    let back = if !query.is_empty() && views::is_return_path(&page) {
        page.as_str()
    } else {
        path.as_str()
    };
    let params = views::params(query);
    let flash = state.sessions().take_flash(&admin.0.cookie);
    let ctx = views::Ctx {
        backend: state.backend(),
        session: &admin.0,
        path: &path,
        params: &params,
        now: &now,
        query,
        statistics: state.statistics(),
    };
    let content = views::content(&ctx, tab, view);
    let sidebar = views::sidebar(&ctx, tab);
    let chrome = Chrome {
        session: &admin.0,
        path: back,
        alerts: &alerts,
        now: &now,
        flash: flash.as_ref(),
        sidebar: &sidebar,
    };
    html(StatusCode::OK, pages::app(&chrome, tab, view, content))
}

/// One of a page's live regions, for the script's refresh. Reading it does
/// not count as using the session, so an unattended console still times
/// out; without a session it answers 401, and the script reloads the page.
async fn fragment(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((tab, region)): Path<(String, String)>,
    RawQuery(query): RawQuery,
) -> Response {
    let lookup = match session_cookie(&headers) {
        Some(value) => state.sessions().peek(value, Instant::now()),
        None => Lookup::None,
    };
    let Lookup::Valid(session) = lookup else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let now = Zoned::now();
    let path = format!("/admin-console/app/{tab}");
    let query = query.as_deref().unwrap_or_default();
    let params = views::params(query);
    let ctx = views::Ctx {
        backend: state.backend(),
        session: &session,
        path: &path,
        params: &params,
        now: &now,
        query,
        statistics: state.statistics(),
    };
    match views::fragment(&ctx, &tab, &region) {
        Some(Ok(markup)) => html(StatusCode::OK, markup),
        // Any status but 200 and 401 leaves the region as it is.
        Some(Err(e)) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct AcknowledgeForm {
    #[serde(default)]
    csrf: String,
    /// The page to return to.
    #[serde(default)]
    back: String,
}

async fn acknowledge_alert(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    admin: Admin,
    Path(id): Path<u64>,
    Form(form): Form<AcknowledgeForm>,
) -> Response {
    if !admin.0.csrf_matches(&form.csrf) {
        return stale_form(peer);
    }
    if state.0.backend.acknowledge_alert(id) {
        info!("{peer}: {} acknowledged alert {id}", admin.0.user);
    }
    let back = if views::is_return_path(&form.back) {
        form.back.as_str()
    } else {
        DASHBOARD
    };
    Redirect::to(back).into_response()
}

/// Whether `path` is a page of the console's application, so that a form
/// cannot send the browser elsewhere.
pub(crate) fn is_app_path(path: &str) -> bool {
    path.starts_with("/admin-console/app/")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'/' || b == b'-')
        && !path.contains("//")
}

async fn asset(Path(name): Path<String>) -> Response {
    let Some(asset) = assets::find(&name) else {
        return not_found().await;
    };
    (
        [
            (CONTENT_TYPE, asset.content_type),
            (CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        asset.body,
    )
        .into_response()
}

/// A path no route serves: the Admin API's 404 in its envelope under its
/// prefix, the console's page elsewhere.
async fn fallback(uri: Uri) -> Response {
    if api::is_api_path(uri.path()) {
        return api::no_such_resource();
    }
    not_found().await
}

async fn not_found() -> Response {
    html(
        StatusCode::NOT_FOUND,
        pages::message(
            "Page Not Found",
            "The Admin Console has no page at this address.",
        ),
    )
}

pub(crate) fn stale_form(peer: SocketAddr) -> Response {
    warn!("{peer}: refused a console form without its session's token");
    html(
        StatusCode::FORBIDDEN,
        pages::message(
            "Request Refused",
            "The form was not sent from this session's Admin Console. Reload the page and try again.",
        ),
    )
}

/// The headers every response carries. Pages and API replies are never
/// cached; the assets set their own caching.
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // Not `no-referrer`: under it a browser sends `Origin: null` even with
    // the console's own forms, which `same_origin` must then refuse.
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers
        .entry(CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    response
}

/// Refuse a state-changing request that another site's page sent.
async fn same_origin(request: Request, next: Next) -> Response {
    let safe = matches!(*request.method(), Method::GET | Method::HEAD);
    if !safe && !from_same_origin(request.headers()) {
        let peer = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.to_string())
            .unwrap_or_default();
        warn!(
            "{peer}: refused a cross-site {} {}",
            request.method(),
            api::loggable_path(request.uri().path())
        );
        let mut refused = if api::is_api_path(request.uri().path()) {
            api::cross_site_refusal()
        } else {
            html(
                StatusCode::FORBIDDEN,
                pages::message(
                    "Request Refused",
                    "The request came from another site, so the Admin Console refused it.",
                ),
            )
        };
        // Its body is left unread, so the connection cannot carry another
        // request: say so, rather than close it after the reply only when
        // the body has not arrived yet, which a client reusing the
        // connection finds out by losing its next request.
        refused
            .headers_mut()
            .insert(CONNECTION, HeaderValue::from_static("close"));
        return refused;
    }
    next.run(request).await
}

/// Whether a request came from the console's own pages: its `Origin` is this
/// server's (as the request addressed it). A browser that hides the origin
/// (`Origin: null`, as under a `no-referrer` policy) or sends none must say
/// `Sec-Fetch-Site: same-origin`, which pages cannot set; a request with
/// neither header is not from a browser's page, so it carries no victim's
/// cookie to misuse.
fn from_same_origin(headers: &HeaderMap) -> bool {
    let fetch_site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    match headers.get(ORIGIN).map(|o| o.to_str()) {
        Some(Ok("null")) => fetch_site == Some("same-origin"),
        Some(Ok(origin)) => {
            let host = headers.get(HOST).and_then(|h| h.to_str().ok());
            host.is_some_and(|host| {
                origin
                    .strip_prefix("https://")
                    .is_some_and(|o| o.eq_ignore_ascii_case(host))
            })
        }
        Some(Err(_)) => false,
        None => matches!(fetch_site, None | Some("same-origin" | "none")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[test]
    fn only_the_consoles_own_origin_may_change_state() {
        let host = ("host", "server.example:16000");
        assert!(from_same_origin(&headers(&[
            host,
            ("origin", "https://server.example:16000")
        ])));
        for origin in [
            "https://evil.example",
            "http://server.example:16000",
            "https://server.example:16001",
            "https://server.example:16000.evil.example",
            "null",
        ] {
            let mut map = headers(&[host]);
            map.insert(ORIGIN, HeaderValue::from_str(origin).unwrap());
            assert!(!from_same_origin(&map), "{origin}");
        }
        assert!(from_same_origin(&headers(&[host])));
        assert!(from_same_origin(&headers(&[
            host,
            ("sec-fetch-site", "same-origin")
        ])));
        assert!(!from_same_origin(&headers(&[
            host,
            ("sec-fetch-site", "cross-site")
        ])));
        // A hidden origin counts only with the browser's word for it.
        assert!(from_same_origin(&headers(&[
            host,
            ("origin", "null"),
            ("sec-fetch-site", "same-origin")
        ])));
        assert!(!from_same_origin(&headers(&[
            host,
            ("origin", "null"),
            ("sec-fetch-site", "cross-site")
        ])));
    }

    #[test]
    fn forms_return_only_to_console_pages() {
        assert!(is_app_path(
            "/admin-console/app/configuration/generalsettings"
        ));
        for path in [
            "",
            "https://evil.example/admin-console/app/",
            "//evil.example/admin-console/app/",
            "/admin-console/app//evil.example",
            "/admin-console/app/../signin",
            "/admin-console/app/x?y=1",
            "/admin-console/signout",
        ] {
            assert!(!is_app_path(path), "{path}");
        }
    }
}
