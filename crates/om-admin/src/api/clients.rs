//! The connected clients: `GET /clients`, disconnecting one
//! (`DELETE /clients/{id}`) and sending one a message
//! (`POST /clients/{id}/message`).

use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Extension;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{duration, id, logged, message_text, ok, time, ApiError, Body, Caller, MAX_MESSAGE};
use crate::app::AppState;
use crate::backend::{Client, Notice};

/// How long a client being disconnected has to close its files when the
/// request does not say (`graceTime`'s default).
const DEFAULT_GRACE: u64 = 90;
/// The longest grace time the API takes, in seconds.
const MAX_GRACE: u64 = 3600;

/// The client families FileMaker Server counts (`docs/re/admin-console.md`
/// 7), from FileMaker's application type number (`docs/re/calc-functions.md`
/// 6).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Pro,
    Go,
    WebDirect,
    ScriptEngine,
    Xdbc,
    Other,
}

impl Family {
    fn of(app_type: u8) -> Self {
        match app_type {
            // kDeveloper (FileMaker Pro Advanced) and kPro.
            0 | 1 => Family::Pro,
            5 => Family::Go,
            4 => Family::WebDirect,
            7 => Family::ScriptEngine,
            6 => Family::Xdbc,
            _ => Family::Other,
        }
    }

    /// The `appType` string. FileMaker Pro's `FMPRO` is documented; the
    /// others follow the stems of the API's counts (`fmgoCount`, ...), which
    /// is inferred.
    fn app_type(self) -> &'static str {
        match self {
            Family::Pro => "FMPRO",
            Family::Go => "FMGO",
            Family::WebDirect => "FMWEBD",
            Family::ScriptEngine => "FMSASE",
            Family::Xdbc => "FMXDBC",
            Family::Other => "FMMISC",
        }
    }
}

/// `GET /clients`.
pub(crate) async fn list(State(state): State<AppState>) -> Response {
    let clients = state.backend().clients();
    let now = SystemTime::now();
    let count = |family: Family| {
        clients
            .iter()
            .filter(|c| Family::of(c.app_type) == family)
            .count()
    };
    ok(json!({
        "clients": clients.iter().map(|c| client(c, now)).collect::<Vec<_>>(),
        "fmproCount": count(Family::Pro),
        "fmgoCount": count(Family::Go),
        "fmwebdCount": count(Family::WebDirect),
        // OpenMaker Server has no licenses, so no concurrent ones.
        "fmconcurrentCount": 0,
        "fmsaseCount": count(Family::ScriptEngine),
        "fmxdbcCount": count(Family::Xdbc),
        "fmmiscCount": count(Family::Other),
    }))
}

fn client(c: &Client, now: SystemTime) -> Value {
    let family = Family::of(c.app_type);
    let files: Vec<_> = c
        .files
        .iter()
        .map(|f| {
            json!({
                "id": f.id.to_string(),
                "filename": f.name,
                "accountName": f.account.as_deref().unwrap_or_default(),
            })
        })
        .collect();
    let mut value = json!({
        "id": c.id.to_string(),
        "userName": c.session_name.as_deref().unwrap_or_default(),
        "computerName": c.machine_name,
        "ipaddress": c.address.ip().to_canonical().to_string(),
        "appType": family.app_type(),
        "appVersion": c.version,
        "connectTime": time(c.connected_at),
        "connectDuration": duration(now.duration_since(c.connected_at).unwrap_or_default()),
        "status": if c.disconnecting { "DISCONNECTING" } else { "NORMAL" },
        "concurrent": false,
        "teamLicensed": false,
        "guestFiles": files,
    });
    // FileMaker Pro and Go come in through the FileMaker network protocol,
    // which is what the `fmapp` extended privilege allows.
    if matches!(family, Family::Pro | Family::Go) {
        value["extpriv"] = "fmapp".into();
    }
    value
}

#[derive(Deserialize)]
pub(crate) struct DisconnectQuery {
    #[serde(rename = "messageText")]
    message_text: Option<String>,
    #[serde(rename = "graceTime")]
    grace_time: Option<String>,
}

/// `DELETE /clients/{id}?messageText=...&graceTime=...`: ask the client to
/// close its files, with the message, and disconnect it when the grace time
/// ends. With no message and a grace time of 0 it is disconnected at once.
pub(crate) async fn disconnect(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<DisconnectQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let id = id("client_id", path, 1)?;
    let Query(query) = query.map_err(|_| ApiError::invalid_value())?;
    let message = message_text(query.message_text.as_deref().unwrap_or_default())?;
    let grace = match query.grace_time.as_deref() {
        None => DEFAULT_GRACE,
        Some(text) => text
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| text.parse::<u64>().ok())
            .flatten()
            .filter(|&seconds| seconds <= MAX_GRACE)
            .ok_or_else(|| ApiError::invalid("graceTime"))?,
    };
    let notice = (!message.is_empty() || grace > 0).then(|| Notice {
        message: message.to_owned(),
        grace: Duration::from_secs(grace),
    });
    logged(
        &caller,
        &format!("disconnect client {id} (grace time {grace} s)"),
        state.backend().disconnect_client(id, notice.as_ref()),
    )?;
    Ok(ok(json!({ "client": { "id": id.to_string() } })))
}

/// `POST /clients/{id}/message` with `{"messageText": ...}`: 1 to 200
/// characters, not only white space.
pub(crate) async fn message(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let id = id("client_id", path, 1)?;
    let body = Body::parse(&headers, body)?;
    let text = body
        .text("messageText", MAX_MESSAGE)?
        .ok_or_else(|| ApiError::missing("messageText"))?;
    if text.trim().is_empty() {
        return Err(ApiError::invalid("messageText"));
    }
    logged(
        &caller,
        &format!("send client {id} a message"),
        state.backend().send_message(id, text),
    )?;
    Ok(ok(json!({ "client": { "id": id.to_string() } })))
}
