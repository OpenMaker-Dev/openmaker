//! The schedules: `GET /schedules`, `GET /schedules/{id}`, and running,
//! enabling or disabling one (`PATCH /schedules/{id}`).

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Extension;
use serde_json::{json, Map, Value};

use super::{id, logged, ok, refused, time, ApiError, Body, Caller, NEVER};
use crate::app::AppState;
use crate::backend::{Schedule, ScheduleKind};

/// The fields of `PATCH /schedules/{id}` that edit a schedule, which
/// OpenMaker Server cannot: its schedules come from its configuration.
const EDITS: &[&str] = &[
    "name",
    "onceType",
    "weeklyType",
    "everyndaysType",
    "sendEmail",
    "backupType",
    "filemakerScriptType",
    "messageType",
    "scriptSequenceType",
    "systemScriptType",
    "verifyType",
];

/// `GET /schedules`.
pub(crate) async fn list(State(state): State<AppState>) -> Result<Response, ApiError> {
    let schedules = state.backend().schedules().map_err(ApiError::reading)?;
    let schedules: Vec<_> = schedules.iter().map(schedule).collect();
    Ok(ok(json!({ "schedules": schedules })))
}

/// Schedule ids start at 2: FileMaker Server reserves 1, and its API
/// refuses it as an invalid `schedule_id`.
const MIN_ID: u32 = 2;

/// `GET /schedules/{id}`.
pub(crate) async fn get(
    State(state): State<AppState>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = id("schedule_id", path, MIN_ID)?;
    Ok(ok(json!({ "schedule": schedule(&find(&state, id)?) })))
}

fn find(state: &AppState, id: u32) -> Result<Schedule, ApiError> {
    state
        .backend()
        .schedules()
        .map_err(ApiError::reading)?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(ApiError::not_found)
}

/// A schedule as the API gives one. Its type is the `...Type` member it
/// has; OpenMaker's model knows a script schedule's file and account, and
/// not yet its script, parameter or timing.
fn schedule(s: &Schedule) -> Value {
    let last_error = match &s.last_run {
        Some(run) if !run.succeeded => -1,
        _ => 0,
    };
    let mut value = json!({
        "id": s.id.to_string(),
        "name": s.name,
        "enabled": s.enabled,
        "status": if s.running { "RUNNING" } else { "IDLE" },
        "lastError": last_error,
        "lastRun": s.last_run.as_ref().map_or_else(|| NEVER.to_owned(), |r| time(r.started_at)),
        "nextRun": s.next_run.map_or_else(|| NEVER.to_owned(), time),
    });
    match s.kind {
        ScheduleKind::FileMakerScript => {
            let mut script = Map::new();
            if let Some(file) = &s.target {
                script.insert("resource".into(), file.as_str().into());
            }
            if let Some(account) = &s.account {
                script.insert("fmScriptAccount".into(), account.as_str().into());
            }
            value["filemakerScriptType"] = script.into();
        }
        ScheduleKind::Backup => value["backupType"] = json!({}),
    }
    value
}

/// `PATCH /schedules/{id}`: `enabled` turns it on or off, and `status`
/// `RUNNING` runs it now (`IDLE` asks nothing). The reply is the schedule
/// after the change.
pub(crate) async fn change(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    path: Result<Path<String>, PathRejection>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    let id = id("schedule_id", path, MIN_ID)?;
    let body = Body::parse(&headers, body)?;
    let run = body
        .has("status")
        .then(|| body.choice("status", &["IDLE", "RUNNING"]))
        .transpose()?
        == Some("RUNNING");
    let enabled = body.boolean("enabled")?;
    let edits = EDITS.iter().any(|name| body.has(name));
    if !edits && !body.has("status") && enabled.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "1707",
            "Required parameter wasn't specified in the request",
        ));
    }
    find(&state, id)?;
    if edits {
        refused(
            &caller,
            &format!("edit schedule {id}"),
            &"OpenMaker Server's schedules come from its configuration",
        );
        return Err(ApiError::invalid_state());
    }
    let backend = state.backend();
    if let Some(on) = enabled {
        let what = if on { "enable" } else { "disable" };
        logged(
            &caller,
            &format!("{what} schedule {id}"),
            backend.enable_schedule(id, on),
        )?;
    }
    if run {
        logged(
            &caller,
            &format!("run schedule {id}"),
            backend.run_schedule(id),
        )?;
    }
    Ok(ok(json!({ "schedule": schedule(&find(&state, id)?) })))
}
