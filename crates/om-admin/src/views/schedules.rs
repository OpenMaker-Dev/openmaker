//! Configuration > Schedules (`docs/re/admin-console.md` 4.5): the server's
//! schedules as FileMaker Server's grid shows them, with Run Now for the
//! checked schedules and an Enabled toggle in each row.
//!
//! OpenMaker Server reads its schedules from the file `om-server
//! --schedules` names (`om-server`'s `schedules/file.rs`), so the view says
//! so in place of Create Schedule, Duplicate, Delete and Edit. The grid is
//! a live region, so Last Run and Last Status follow the runs.

use std::net::SocketAddr;
use std::time::SystemTime;

use axum::extract::{ConnectInfo, Form, Path, State};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use jiff::tz::TimeZone;
use jiff::Timestamp;
use maud::{html, Markup};
use serde::Deserialize;

use crate::app::{Admin, AppState};
use crate::backend::{Backend, BackendError, Schedule, ScheduleKind};
use crate::components::{button, form_fields, long_datetime, title, toggle};
use crate::views::{finish, live, not_shown, refuse_stale, Ctx};

const PAGE: &str = "/admin-console/app/configuration/schedules";
const RUN: &str = "/admin-console/app/configuration/schedules/run";

/// The grid's live region: `/admin-console/fragments/configuration/schedules`.
pub(crate) const REGION: &str = "schedules";

/// The Run Now form, which the grid's checkboxes belong to.
const RUN_FORM: &str = "schedules-run";

/// The checkboxes' name.
const SELECTION: &str = "schedule";

/// Schedules listed at most, and checked at most for one Run Now.
const MAX_ROWS: usize = 500;

/// Characters of a schedule's name, or of why its run failed, shown at
/// most (FileMaker Server's names have at most 100).
const MAX_NAME: usize = 100;
const MAX_DETAIL: usize = 300;

const UNAVAILABLE: &str = "Schedules are not available in OpenMaker Server yet.";

const FROM_FILE: &str = "OpenMaker Server runs the schedules in the file named with \
    om-server --schedules. To create, change or delete a schedule, edit that file and restart \
    the server.";

pub(crate) fn page(ctx: &Ctx) -> Markup {
    let schedules = ctx.backend.schedules();
    html! {
        (title("Schedules"))
        @match schedules {
            Err(e) => (not_shown(UNAVAILABLE, &e)),
            Ok(schedules) => {
                form.toolbar.schedules-toolbar id=(RUN_FORM) method="post" action=(RUN) {
                    (form_fields(ctx))
                    (button("Run Now", "run", Some(SELECTION)))
                }
                (live("configuration", REGION, grid(ctx, &schedules)))
                p.schedules-note { (FROM_FILE) }
            }
        }
    }
}

/// The grid alone, as the live region refreshes it, or why the schedules
/// cannot be read, which the page shows when it is loaded.
pub(crate) fn region(ctx: &Ctx) -> Result<Markup, BackendError> {
    // The fragment's own path is the tab's; the grid's forms return here.
    let ctx = Ctx { path: PAGE, ..*ctx };
    Ok(grid(&ctx, &ctx.backend.schedules()?))
}

fn grid(ctx: &Ctx, schedules: &[Schedule]) -> Markup {
    let shown = &schedules[..schedules.len().min(MAX_ROWS)];
    html! {
        div.table-scroll {
            table.data-table.schedules-table {
                thead {
                    tr {
                        th.check-cell scope="col" {
                            input type="checkbox" data-select-all=(SELECTION)
                                aria-label="Select all schedules";
                        }
                        th scope="col" { "Schedule Type" }
                        th scope="col" { "Schedule Name" }
                        th scope="col" { "Database" }
                        th scope="col" { "Last Run" }
                        th scope="col" { "Last Status" }
                        th scope="col" { "Scheduled Time" }
                        th scope="col" { "User Name" }
                        th scope="col" { "Enabled" }
                    }
                }
                tbody {
                    @for schedule in shown { (grid_row(ctx, schedule)) }
                    @if shown.is_empty() {
                        tr.empty-row { td colspan="9" { "There are no schedules." } }
                    }
                }
            }
        }
        @if schedules.len() > shown.len() {
            p.showing { "Showing " (shown.len()) " of " (schedules.len()) }
        }
    }
}

fn grid_row(ctx: &Ctx, schedule: &Schedule) -> Markup {
    let name = clip(&schedule.name, MAX_NAME);
    let user = match (&schedule.account, schedule.kind) {
        (Some(account), _) => account.as_str(),
        (None, ScheduleKind::FileMakerScript) => "Guest",
        (None, ScheduleKind::Backup) => "",
    };
    html! {
        tr {
            td.check-cell {
                input type="checkbox" name=(SELECTION) value=(schedule.id) form=(RUN_FORM)
                    aria-label={"Select " (name)};
            }
            td title=(schedule.kind.label()) { (schedule.kind.label()) }
            td title=(name) { (name) }
            td title=[schedule.target.as_deref()] {
                (schedule.target.as_deref().unwrap_or("N/A"))
            }
            td {
                @if let Some(run) = &schedule.last_run { (grid_time(run.started_at)) }
            }
            td { (last_status(schedule)) }
            td {
                @if !schedule.enabled {
                    "Disabled"
                } @else if let Some(next) = schedule.next_run {
                    (grid_time(next))
                }
            }
            td title=(user) { (user) }
            td {
                (toggle(
                    ctx,
                    &format!("{PAGE}/{}/enabled", schedule.id),
                    schedule.enabled,
                    true,
                    &format!("Enabled: {name}"),
                ))
            }
        }
    }
}

fn last_status(schedule: &Schedule) -> Markup {
    if schedule.running {
        return html! { "Running..." };
    }
    let Some(run) = &schedule.last_run else {
        return html! {};
    };
    let detail = run.detail.as_deref().map(|d| clip(d, MAX_DETAIL));
    html! {
        @if run.succeeded {
            "Succeeded"
        } @else {
            span.schedule-failed { "Failed" }
        }
        @if let Some(detail) = &detail {
            span.schedule-detail title=(detail) { (detail) }
        }
    }
}

/// `11/11/2026 18:05` in the server's time zone, as FileMaker Server's grid
/// shows times, with the long form as the tooltip.
fn grid_time(time: SystemTime) -> Markup {
    let short = Timestamp::try_from(time).map_or_else(
        |_| String::new(),
        |t| {
            t.to_zoned(TimeZone::system())
                .strftime("%m/%d/%Y %H:%M")
                .to_string()
        },
    );
    html! { span title=(long_datetime(time)) { (short) } }
}

/// At most `max` characters of `text`, with an ellipsis when cut.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_owned(),
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route(RUN, post(run_now)).route(
        "/admin-console/app/configuration/schedules/{id}/enabled",
        post(set_enabled),
    )
}

/// Schedule `id`'s name for a message, quoted.
fn quoted_name(schedules: &[Schedule], id: u32) -> String {
    // Quoted with its control characters escaped, as it also goes to the log.
    schedules.iter().find(|s| s.id == id).map_or_else(
        || format!("schedule {id}"),
        |s| format!("{:?}", clip(&s.name, MAX_NAME)),
    )
}

/// `"A"`, `"A" and "B"`, or `"A", "B", "C" and 2 more`.
fn list(names: &[String]) -> String {
    const LISTED: usize = 3;
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) if names.len() <= LISTED => format!("{} and {last}", rest.join(", ")),
        Some(_) => format!(
            "{} and {} more",
            names[..LISTED].join(", "),
            names.len() - LISTED
        ),
    }
}

/// Run Now: the form's checked schedules, each run once.
async fn run_now(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Admin(admin): Admin,
    Form(fields): Form<Vec<(String, String)>>,
) -> Response {
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map_or("", |(_, value)| value.as_str())
    };
    if let Some(refused) = refuse_stale(&admin, field("csrf"), peer) {
        return refused;
    }
    let backend = state.backend();
    let ids = selected(&fields);
    let what = match &ids {
        Ok(ids) => {
            let schedules = backend.schedules().unwrap_or_default();
            let names: Vec<String> = ids.iter().map(|&id| quoted_name(&schedules, id)).collect();
            format!("run {}", list(&names))
        }
        Err(_) => "run the checked schedules".to_owned(),
    };
    let outcome = ids.and_then(|ids| run(backend, &ids));
    finish(&state, &admin, peer, field("back"), &what, outcome)
}

/// The checked schedules' ids, each once.
fn selected(fields: &[(String, String)]) -> Result<Vec<u32>, BackendError> {
    let mut ids = Vec::new();
    for (_, value) in fields.iter().filter(|(key, _)| key == SELECTION) {
        let id = value
            .parse::<u32>()
            .map_err(|_| BackendError::Invalid("The form is not complete.".into()))?;
        if !ids.contains(&id) {
            if ids.len() == MAX_ROWS {
                return Err(BackendError::Invalid(format!(
                    "Run at most {MAX_ROWS} schedules at once."
                )));
            }
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err(BackendError::Invalid("Select the schedules to run.".into()));
    }
    Ok(ids)
}

fn run(backend: &dyn Backend, ids: &[u32]) -> Result<String, BackendError> {
    let schedules = backend.schedules().unwrap_or_default();
    let mut started = Vec::new();
    let mut failed = Vec::new();
    for &id in ids {
        match backend.run_schedule(id) {
            Ok(()) => started.push(quoted_name(&schedules, id)),
            Err(e) => failed.push(format!("{}: {e}", quoted_name(&schedules, id))),
        }
    }
    let started_text = format!("Run Now started {}.", list(&started));
    match (started.is_empty(), failed.as_slice()) {
        (_, []) => Ok(started_text),
        (true, [one]) => Err(BackendError::Failed(format!("Could not run {one}"))),
        (true, _) => Err(BackendError::Failed(format!(
            "Could not run {}",
            list(&failed)
        ))),
        (false, _) => Err(BackendError::Failed(format!(
            "{started_text} Could not run {}",
            list(&failed)
        ))),
    }
}

#[derive(Deserialize)]
struct ToggleForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    back: String,
    #[serde(default)]
    on: String,
}

/// The Enabled toggle of schedule `id`, which saves at once.
async fn set_enabled(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    Form(form): Form<ToggleForm>,
) -> Response {
    if let Some(refused) = refuse_stale(&admin, &form.csrf, peer) {
        return refused;
    }
    let backend = state.backend();
    let on = match form.on.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(BackendError::Invalid("The form is not complete.".into())),
    };
    let id = id.parse::<u32>().map_err(|_| BackendError::NotFound);
    let name = match &id {
        Ok(id) => quoted_name(&backend.schedules().unwrap_or_default(), *id),
        Err(_) => "the schedule".to_owned(),
    };
    let outcome = on.and_then(|on| {
        backend.enable_schedule(id.clone()?, on)?;
        Ok(if on {
            format!("{name} is enabled.")
        } else {
            format!("{name} is disabled.")
        })
    });
    let what = match form.on.as_str() {
        "true" => format!("enable {name}"),
        "false" => format!("disable {name}"),
        _ => format!("change whether {name} is enabled"),
    };
    finish(&state, &admin, peer, &form.back, &what, outcome)
}
