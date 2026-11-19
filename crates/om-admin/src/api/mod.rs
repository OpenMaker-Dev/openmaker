//! The Admin API v2: FileMaker Server's documented REST interface
//! (`docs/re/admin-console.md` 8) under `/fmi/admin/api/v2`, with its paths,
//! JSON shapes, HTTP statuses and error codes, over the same [`Backend`] as
//! the console's views (`docs/admin-console.md`, "Admin API").
//!
//! - `auth`: tokens. `POST /user/auth` with the administrator's HTTP Basic
//!   credentials opens one; every other call sends it as a Bearer token,
//!   or Basic credentials, which open a new token returned in
//!   `X-FM-Access-Token`.
//! - `server`, `databases`, `clients`, `schedules`: the endpoints.
//!
//! Every reply is FileMaker's envelope, `{"response": {...}, "messages":
//! [{"code": "0", "text": "OK"}]}`; an error has an empty `response` and
//! FileMaker's code ([`ApiError`]). A feature the backend answers
//! [`BackendError::Unsupported`] for is a resource that does not exist
//! (404, 1700) when it is read, and an operation refused in the resource's
//! current state (409, 1709) when it is asked to change.
//!
//! The API takes no cookies, and sends no CORS headers and no
//! `WWW-Authenticate` challenge, so a browser never holds credentials for
//! it: another site's page can neither read its replies nor send it a
//! token or a JSON body.
//!
//! [`Backend`]: crate::backend::Backend

mod auth;
mod clients;
mod databases;
mod schedules;
mod server;

use std::ops::RangeInclusive;
use std::path::Path;
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::Path as PathParam;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, MethodRouter};
use axum::{Json, Router};
use jiff::tz::TimeZone;
use jiff::Timestamp;
use log::warn;
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::app::AppState;
use crate::backend::BackendError;

pub(crate) use auth::{Caller, Tokens};

/// Where the API is served.
const PREFIX: &str = "/fmi/admin/api/v2";

/// The longest `messageText` (`DELETE /clients/{id}`, `PATCH /databases/{id}`,
/// `POST /clients/{id}/message`, `PATCH /server/status`), in characters.
const MAX_MESSAGE: usize = 200;

/// FileMaker's time for "never": a schedule that has not run.
const NEVER: &str = "0000-00-00T00:00:00";

/// The API's routes. Every route but sign-in and sign-out needs a token.
/// Other paths under the prefix reach the console's fallback, which answers
/// them with [`no_such_resource`].
pub(crate) fn routes(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route(&at("/server/metadata"), only(get(server::metadata)))
        .route(
            &at("/server/status"),
            only(get(server::status).patch(server::set_status)),
        )
        .route(
            &at("/server/config/blocknewusers"),
            only(get(server::block_new_users).patch(server::set_block_new_users)),
        )
        .route(
            &at("/server/currentfoldersettings"),
            only(get(server::folders)),
        )
        .route(
            &at("/server/certificate/info"),
            only(get(server::certificate)),
        )
        .route(
            &at("/databases"),
            only(get(databases::list).patch(databases::change_all)),
        )
        .route(&at("/databases/{id}"), only(patch(databases::change)))
        .route(&at("/clients"), only(get(clients::list)))
        .route(&at("/clients/{id}"), only(delete(clients::disconnect)))
        .route(&at("/clients/{id}/message"), only(post(clients::message)))
        .route(&at("/schedules"), only(get(schedules::list)))
        .route(
            &at("/schedules/{id}"),
            only(get(schedules::get).patch(schedules::change)),
        )
        .route_layer(middleware::from_fn_with_state(state, auth::authenticate));
    Router::new()
        .route(&at("/user/auth"), only(post(auth::sign_in)))
        .route(&at("/user/auth/{token}"), only(delete(auth::sign_out)))
        .merge(protected)
}

fn at(path: &str) -> String {
    format!("{PREFIX}{path}")
}

/// Whether `path` is the API's, so that a refusal made before routing
/// answers with the API's envelope.
pub(crate) fn is_api_path(path: &str) -> bool {
    path.strip_prefix(PREFIX)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The answer to an API request another site's page sent (the console's
/// `Origin` check): 403 in the API's envelope. FileMaker has no such
/// refusal; the code and text are OpenMaker's own.
pub(crate) fn cross_site_refusal() -> Response {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "-1",
        "The request came from another site's page, so OpenMaker Server refused it.",
    )
    .into_response()
}

/// `path` as it may be logged: `DELETE /user/auth/{token}` carries a token
/// in its path, which is cut.
pub(crate) fn loggable_path(path: &str) -> &str {
    const SIGN_OUT: &str = "/fmi/admin/api/v2/user/auth/";
    match path.get(..SIGN_OUT.len()) {
        Some(start) if start.eq_ignore_ascii_case(SIGN_OUT) => start,
        _ => path,
    }
}

/// A path's methods, answering any other method with the API's envelope.
fn only(methods: MethodRouter<AppState>) -> MethodRouter<AppState> {
    methods.fallback(|| async {
        ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "1700",
            "Resource doesn't exist",
        )
    })
}

/// The answer to a path under the API's prefix that no route serves, for
/// the console's fallback: 404 and 1700 in the API's envelope.
pub(crate) fn no_such_resource() -> Response {
    ApiError::not_found().into_response()
}

/// FileMaker's envelope, `response` first as FileMaker Server writes it.
#[derive(Serialize)]
struct Envelope<'a> {
    response: Value,
    messages: [Message<'a>; 1],
}

#[derive(Serialize)]
struct Message<'a> {
    code: &'a str,
    text: &'a str,
}

/// A successful reply: `response` in FileMaker's envelope.
pub(crate) fn ok(response: Value) -> Response {
    Json(Envelope {
        response,
        messages: [Message {
            code: "0",
            text: "OK",
        }],
    })
    .into_response()
}

/// A refusal: an HTTP status, FileMaker's error code and its text, sent as
/// FileMaker's envelope with an empty `response`.
#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    text: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, text: impl Into<String>) -> Self {
        Self {
            status,
            code,
            text: text.into(),
        }
    }

    /// 404, 1700: no such client, file or schedule, or a feature the server
    /// does not have.
    pub(crate) fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "1700", "Resource doesn't exist")
    }

    /// 401, 1703: no credentials or token, or wrong ones.
    pub(crate) fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "1703",
            "Invalid username or password, or JSON Web Token",
        )
    }

    /// 400, 1707: a required parameter is missing.
    pub(crate) fn missing(name: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "1707",
            format!("{name}: required parameter wasn't specified for this request"),
        )
    }

    /// 400, 1708: a parameter's value is not one the API takes.
    pub(crate) fn invalid(name: &str) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "1708",
            format!("{name}: parameter value is invalid."),
        )
    }

    /// 400, 1708, without naming the parameter.
    pub(crate) fn invalid_value() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "1708",
            "Parameter value is invalid.",
        )
    }

    /// 409, 1709: the resource's state does not allow the operation, or the
    /// server cannot do it.
    pub(crate) fn invalid_state() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "1709",
            "Operation is invalid for the resource's current status",
        )
    }

    /// 500, -1: the server tried and failed.
    pub(crate) fn unknown() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "-1", "Unknown error")
    }

    /// The answer to a backend's error when reading a resource.
    pub(crate) fn reading(error: BackendError) -> Self {
        match error {
            BackendError::Unsupported | BackendError::NotFound => Self::not_found(),
            other => Self::changing(other),
        }
    }

    /// The answer to a backend's error when changing a resource.
    pub(crate) fn changing(error: BackendError) -> Self {
        match error {
            BackendError::NotFound => Self::not_found(),
            BackendError::Unsupported | BackendError::InvalidState(_) => Self::invalid_state(),
            BackendError::Invalid(_) => Self::invalid_value(),
            BackendError::Failed(_) => Self::unknown(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(Envelope {
            response: json!({}),
            messages: [Message {
                code: self.code,
                text: &self.text,
            }],
        });
        (self.status, body).into_response()
    }
}

/// Log a change the caller asked for, with its address and account (never
/// its token), as the console logs its actions: `what` names it ("close
/// database 2"), followed by "done", or "failed" and the reason. Passes the
/// outcome on.
pub(crate) fn logged<T>(
    caller: &Caller,
    what: &str,
    outcome: Result<T, BackendError>,
) -> Result<T, ApiError> {
    match outcome {
        Ok(value) => {
            log::info!("{}: {} (Admin API): {what}: done", caller.peer, caller.user);
            Ok(value)
        }
        Err(e) => {
            refused(caller, what, &e);
            Err(ApiError::changing(e))
        }
    }
}

/// Log a change the API refused before asking the backend, or the backend's
/// refusal.
pub(crate) fn refused(caller: &Caller, what: &str, why: &dyn std::fmt::Display) {
    warn!(
        "{}: {} (Admin API): {what}: failed: {why}",
        caller.peer, caller.user
    );
}

/// A request's JSON body: an object, from a request whose `Content-Type` is
/// `application/json`, as every documented body is. Fields the API does not
/// document are ignored; a `null` field counts as absent.
pub(crate) struct Body(Map<String, Value>);

impl Body {
    pub(crate) fn parse(
        headers: &HeaderMap,
        body: Result<Bytes, BytesRejection>,
    ) -> Result<Self, ApiError> {
        let body = body.map_err(|e| {
            if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "1708",
                    "Parameter value is invalid.",
                )
            } else {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "1710",
                    "JSON input isn't syntactically valid",
                )
            }
        })?;
        if !is_json(headers) {
            return Err(ApiError::invalid("Content-Type"));
        }
        match serde_json::from_slice(&body) {
            Ok(Value::Object(fields)) => Ok(Self(fields)),
            Ok(_) => Err(ApiError::invalid_value()),
            Err(_) => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "1710",
                "JSON input isn't syntactically valid",
            )),
        }
    }

    fn field(&self, name: &str) -> Option<&Value> {
        self.0.get(name).filter(|v| !v.is_null())
    }

    /// Whether field `name` is given.
    pub(crate) fn has(&self, name: &str) -> bool {
        self.field(name).is_some()
    }

    /// Text field `name`, at most `max` characters.
    pub(crate) fn text(&self, name: &str, max: usize) -> Result<Option<&str>, ApiError> {
        match self.field(name) {
            None => Ok(None),
            Some(Value::String(text)) if text.chars().count() <= max => Ok(Some(text)),
            Some(_) => Err(ApiError::invalid(name)),
        }
    }

    /// Boolean field `name`.
    pub(crate) fn boolean(&self, name: &str) -> Result<Option<bool>, ApiError> {
        match self.field(name) {
            None => Ok(None),
            Some(Value::Bool(on)) => Ok(Some(*on)),
            Some(_) => Err(ApiError::invalid(name)),
        }
    }

    /// Integer field `name`, within `range`.
    pub(crate) fn integer(
        &self,
        name: &str,
        range: RangeInclusive<i64>,
    ) -> Result<Option<i64>, ApiError> {
        match self.field(name) {
            None => Ok(None),
            Some(value) => value
                .as_i64()
                .filter(|n| range.contains(n))
                .map(Some)
                .ok_or_else(|| ApiError::invalid(name)),
        }
    }

    /// Field `name`, one of `values`; missing is 1707.
    pub(crate) fn choice<'a>(&self, name: &str, values: &[&'a str]) -> Result<&'a str, ApiError> {
        match self.field(name) {
            None => Err(ApiError::missing(name)),
            Some(Value::String(text)) => values
                .iter()
                .find(|v| **v == text)
                .copied()
                .ok_or_else(|| ApiError::invalid(name)),
            Some(_) => Err(ApiError::invalid(name)),
        }
    }
}

/// Whether the request says its body is JSON.
fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

/// A `messageText`: at most 200 characters (FileMaker's limit).
pub(crate) fn message_text(text: &str) -> Result<&str, ApiError> {
    if text.chars().count() <= MAX_MESSAGE {
        Ok(text)
    } else {
        Err(ApiError::invalid("messageText"))
    }
}

/// A path's id `name`: a whole number from `min` (FileMaker's minimum: 1,
/// or 2 for schedules, whose id 1 FileMaker Server reserves). A path the
/// router could not decode is an invalid value too.
pub(crate) fn id(
    name: &str,
    path: Result<PathParam<String>, PathRejection>,
    min: u32,
) -> Result<u32, ApiError> {
    let PathParam(text) = path.map_err(|_| ApiError::invalid(name))?;
    text.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| text.parse::<u32>().ok())
        .flatten()
        .filter(|&id| id >= min)
        .ok_or_else(|| ApiError::invalid(name))
}

/// A time as the API writes one, `2026-11-11T18:05:01`, in the server's time
/// zone, without the zone.
pub(crate) fn time(time: SystemTime) -> String {
    Timestamp::try_from(time).map_or_else(
        |_| NEVER.to_owned(),
        |t| {
            t.to_zoned(TimeZone::system())
                .strftime("%Y-%m-%dT%H:%M:%S")
                .to_string()
        },
    )
}

/// A duration as the API writes one: `HH:MM:SS`, the hours not wrapping at a
/// day.
pub(crate) fn duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// A folder in FileMaker's path syntax, with its closing slash:
/// `filelinux:/srv/openmaker/Databases/` (`docs/re/admin-console.md` 4.5).
pub(crate) fn filemaker_folder(path: &Path) -> String {
    let text = path.to_string_lossy();
    let (prefix, text) = if cfg!(windows) {
        ("filewin:/", text.replace('\\', "/"))
    } else if cfg!(target_os = "macos") {
        ("filemac:", text.into_owned())
    } else {
        ("filelinux:", text.into_owned())
    };
    let slash = if text.ends_with('/') { "" } else { "/" };
    format!("{prefix}{text}{slash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_durations_and_logged_paths() {
        let id = |text: &str, min| id("db_id", Ok(PathParam(text.to_owned())), min).ok();
        assert_eq!(id("12", 1), Some(12));
        for text in ["0", "", "-1", "+1", "1.0", "4294967296", " 1", "x"] {
            assert_eq!(id(text, 1), None, "{text}");
        }
        assert_eq!((id("1", 2), id("2", 2)), (None, Some(2)));
        assert_eq!(duration(Duration::from_secs(5)), "00:00:05");
        assert_eq!(duration(Duration::from_secs(26 * 3600 + 61)), "26:01:01");
        // A token in a sign-out's path is never logged.
        let token = "ab".repeat(32);
        for path in [
            format!("/fmi/admin/api/v2/user/auth/{token}"),
            format!("/FMI/admin/API/v2/user/auth/{token}"),
        ] {
            assert!(!loggable_path(&path).contains(&token), "{path}");
        }
        assert_eq!(
            loggable_path("/fmi/admin/api/v2/clients/1"),
            "/fmi/admin/api/v2/clients/1"
        );
    }
}
