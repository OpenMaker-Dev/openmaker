//! The server's identity, run state and settings: `/server/metadata`,
//! `/server/status`, `/server/config/blocknewusers`,
//! `/server/currentfoldersettings` and `/server/certificate/info`.

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Extension;
use jiff::Zoned;
use serde_json::{json, Value};

use super::{filemaker_folder, logged, ok, refused, ApiError, Body, Caller, MAX_MESSAGE};
use crate::app::AppState;
use crate::certificate;

/// `GET /server/metadata`.
pub(crate) async fn metadata(State(state): State<AppState>) -> Response {
    let server = state.backend().server();
    let now = Zoned::now();
    // One address, as FileMaker Server gives: the first the machine is
    // reached by, or the one it listens on.
    let ip = server
        .ip_addresses
        .first()
        .cloned()
        .unwrap_or_else(|| server.address.ip().to_string());
    let mut response = json!({
        "serverName": server.name,
        "serverID": server.id,
        "serverIP": ip,
        "serverVersion": server.version,
        "serverHostTime": now.strftime("%Y-%m-%dT%H:%M:%S").to_string(),
        // Minutes from local time to UTC, as JavaScript's
        // `getTimezoneOffset` counts them: 300 for UTC-5.
        "offset": -(now.offset().seconds() / 60),
    });
    if let Some(name) = now.time_zone().iana_name() {
        response["timezone"] = name.into();
    }
    ok(response)
}

/// `GET /server/status`: the Database Server is running while the API
/// answers.
pub(crate) async fn status() -> Response {
    ok(json!({ "status": "RUNNING" }))
}

/// `PATCH /server/status`: `RUNNING` is what it is already; OpenMaker
/// Server cannot stop its Database Server and keep serving, so `STOPPED` is
/// refused as an operation invalid in the current state.
pub(crate) async fn set_status(
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let body = Body::parse(&headers, body)?;
    let status = body.choice("status", &["RUNNING", "STOPPED"])?;
    body.text("messageText", MAX_MESSAGE)?;
    body.integer("graceTime", 0..=3600)?;
    if status == "STOPPED" {
        refused(
            &caller,
            "stop the Database Server",
            &"OpenMaker Server cannot stop it and go on serving",
        );
        return Err(ApiError::invalid_state());
    }
    Ok(ok(json!({ "status": "RUNNING" })))
}

/// `GET /server/config/blocknewusers`.
pub(crate) async fn block_new_users(State(state): State<AppState>) -> Result<Response, ApiError> {
    let on = state
        .backend()
        .block_new_users()
        .map_err(ApiError::reading)?;
    Ok(ok(json!({ "blockNewUsers": on })))
}

/// `PATCH /server/config/blocknewusers`.
pub(crate) async fn set_block_new_users(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let body = Body::parse(&headers, body)?;
    let on = body
        .boolean("blockNewUsers")?
        .ok_or_else(|| ApiError::missing("blockNewUsers"))?;
    let what = if on {
        "turn Block New Users on"
    } else {
        "turn Block New Users off"
    };
    logged(&caller, what, state.backend().set_block_new_users(on))?;
    Ok(ok(json!({ "blockNewUsers": on })))
}

/// `GET /server/currentfoldersettings`. The first folder of hosted files is
/// the default database folder and the next two the additional ones, as
/// FileMaker Server has at most two; OpenMaker Server has no secure,
/// remote container or progressive backup folder.
pub(crate) async fn folders(State(state): State<AppState>) -> Result<Response, ApiError> {
    let folders = state.backend().folders().map_err(ApiError::reading)?;
    let additional = |n: usize| -> Value {
        match folders.databases.get(n) {
            Some(path) => json!(["enabled", filemaker_folder(path)]),
            None => json!(["disabled"]),
        }
    };
    let mut response = json!({
        "additionalDatabaseFolder": additional(1),
        "additionalDatabaseFolder2": additional(2),
        "additionalRemoteContainerFolder": ["disabled"],
        "additionalRemoteContainerFolder2": ["disabled"],
        "backupPath": folders.backups.as_deref().map(filemaker_folder),
        "progressiveBackupFolder": ["disabled"],
    });
    if let Some(first) = folders.databases.first() {
        response["defaultDatabaseFolder"] = filemaker_folder(first).into();
    }
    Ok(ok(response))
}

/// `GET /server/certificate/info`: the names the certificate in use is
/// issued to and by.
pub(crate) async fn certificate(State(state): State<AppState>) -> Result<Response, ApiError> {
    let chain = state.backend().certificate().map_err(ApiError::reading)?;
    let summary = chain
        .first()
        .and_then(|leaf| certificate::summarize(leaf))
        .ok_or_else(ApiError::unknown)?;
    let mut info = json!({});
    if let Some(issuer) = summary.issued_by {
        info["issuerName"] = issuer.into();
    }
    if let Some(subject) = summary.issued_to {
        info["subjectName"] = subject.into();
    }
    Ok(ok(json!({ "certificate": info })))
}
