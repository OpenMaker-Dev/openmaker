//! The hosted files: `GET /databases`, and opening, closing, pausing and
//! resuming one (`PATCH /databases/{id}`) or all (`PATCH /databases`).

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Extension;
use log::info;
use serde_json::{json, Value};

use super::{filemaker_folder, id, logged, ok, refused, ApiError, Body, Caller, MAX_MESSAGE};
use crate::app::AppState;
use crate::backend::{BackendError, Database, DatabaseChange, DatabaseStatus, Notice};

/// How long a closing file's clients have to close it, as in FileMaker
/// Server, whose Close takes no grace time (`docs/re/admin-console.md` 9).
const CLOSE_GRACE: Duration = Duration::from_secs(90);

/// `GET /databases`.
pub(crate) async fn list(State(state): State<AppState>) -> Response {
    let databases = state.backend().databases();
    let open = databases.iter().filter(|d| is_open(d.status)).count();
    ok(json!({
        "databases": databases.iter().map(database).collect::<Vec<_>>(),
        "totalDBCount": databases.len(),
        "openDBCount": open,
    }))
}

/// Whether a file in this state is open: clients can use it, or will once
/// it resumes. A file being opened is not yet; one being closed still is.
fn is_open(status: DatabaseStatus) -> bool {
    !matches!(status, DatabaseStatus::Closed | DatabaseStatus::Opening)
}

fn database(d: &Database) -> Value {
    let mut value = json!({
        "id": d.id.to_string(),
        "filename": d.name,
        "folder": filemaker_folder(&d.folder),
        "status": d.status.api_name(),
        "clients": d.clients,
        "isEncrypted": d.encrypted,
        // OpenMaker Server keeps no encryption passwords or hints.
        "decryptHint": "",
        "hasSavedDecryptKey": false,
    });
    if let Some(size) = d.size {
        value["size"] = size.into();
    }
    value
}

/// The change a `PATCH` body asks for, checked as the reference's schema
/// says: `messageText` is required with `CLOSED` and taken with nothing
/// else (one file only), `key` and `saveKey` only with `OPENED` (one file
/// only), `force` only with `CLOSED`. A field its status does not take is
/// refused (1708). `FLUSHED` and `PRESNAPSHOT` are FileMaker Server's
/// operations OpenMaker Server does not have: `None`.
fn requested(body: &Body, one: bool) -> Result<(&'static str, Option<DatabaseChange>), ApiError> {
    let status = body.choice(
        "status",
        &[
            "OPENED",
            "PAUSED",
            "RESUMED",
            "CLOSED",
            "FLUSHED",
            "PRESNAPSHOT",
        ],
    )?;
    let opened = status == "OPENED";
    let closed = status == "CLOSED";
    let only_with = |name: &str, given: bool, allowed: bool| {
        if given && !allowed {
            Err(ApiError::invalid(name))
        } else {
            Ok(())
        }
    };
    let mut message = "";
    if one {
        only_with("key", body.text("key", usize::MAX)?.is_some(), opened)?;
        only_with("saveKey", body.boolean("saveKey")?.is_some(), opened)?;
        let text = body.text("messageText", MAX_MESSAGE)?;
        only_with("messageText", text.is_some(), closed)?;
        if closed {
            message = text.ok_or_else(|| ApiError::missing("messageText"))?;
        }
    }
    let force = body.boolean("force")?;
    only_with("force", force.is_some(), closed)?;
    let force = force.unwrap_or(false);
    let change = match status {
        "OPENED" => Some(DatabaseChange::Open),
        "PAUSED" => Some(DatabaseChange::Pause),
        "RESUMED" => Some(DatabaseChange::Resume),
        "CLOSED" => Some(DatabaseChange::Close(Notice {
            message: message.to_owned(),
            // `force` closes at once, disconnecting the file's clients.
            grace: if force { Duration::ZERO } else { CLOSE_GRACE },
        })),
        _ => None,
    };
    Ok((status, change))
}

/// What a change does, for the log.
fn verb(change: &DatabaseChange) -> &'static str {
    match change {
        DatabaseChange::Open => "open",
        DatabaseChange::Close(_) => "close",
        DatabaseChange::Pause => "pause",
        DatabaseChange::Resume => "resume",
    }
}

/// `PATCH /databases/{id}`. The reply gives the file's state after the
/// change, which may be a transition such as `CLOSING`.
pub(crate) async fn change(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let id = id("db_id", path, 1)?;
    let body = Body::parse(&headers, body)?;
    let (status, change) = requested(&body, true)?;
    let Some(change) = change else {
        refused(
            &caller,
            &format!("make database {id} {status}"),
            &"OpenMaker Server does not have this operation",
        );
        return Err(ApiError::invalid_state());
    };
    let backend = state.backend();
    logged(
        &caller,
        &format!("{} database {id}", verb(&change)),
        backend.change_database(id, &change),
    )?;
    let current = backend.databases().into_iter().find(|d| d.id == id);
    let (filename, status) = match current {
        Some(d) => (d.name, d.status.api_name()),
        None => (String::new(), status),
    };
    Ok(ok(json!({
        "database": { "id": id.to_string(), "filename": filename, "status": status },
    })))
}

/// `PATCH /databases`: the change for every hosted file whose state allows
/// it; the others are left as they are. The reply lists every file's state
/// after it.
pub(crate) async fn change_all(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let body = Body::parse(&headers, body)?;
    let (status, change) = requested(&body, false)?;
    let Some(change) = change else {
        refused(
            &caller,
            &format!("make every database {status}"),
            &"OpenMaker Server does not have this operation",
        );
        return Err(ApiError::invalid_state());
    };
    let backend = state.backend();
    let databases = backend.databases();
    let mut changed = 0;
    for database in &databases {
        match backend.change_database(database.id, &change) {
            Ok(()) => changed += 1,
            // Not in a state for it, or gone meanwhile.
            Err(BackendError::InvalidState(_) | BackendError::NotFound) => {}
            Err(e) => {
                let what = format!("{} every database", verb(&change));
                let why = format!("{e} ({changed} of {} done before)", databases.len());
                refused(&caller, &what, &why);
                return Err(ApiError::changing(e));
            }
        }
    }
    info!(
        "{}: {} (Admin API): {} every database: done for {changed} of {}",
        caller.peer,
        caller.user,
        verb(&change),
        databases.len()
    );
    let databases: Vec<_> = backend
        .databases()
        .iter()
        .map(|d| json!({ "id": d.id.to_string(), "filename": d.name, "status": d.status.api_name() }))
        .collect();
    Ok(ok(json!({ "databases": databases })))
}
