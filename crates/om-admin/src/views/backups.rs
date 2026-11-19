//! Backups (`docs/re/admin-console.md` 4.3). The tab's sidebar shows a
//! backup schedule's Repeat, Keep and Next Backup with Back Up Now, and the
//! running backup's status as a live region. Auto-Backup and On-Demand and
//! Preserved list the backups, with Preserve Backup, Edit Label and Delete
//! for the checked ones; Backup Settings says which settings OpenMaker
//! Server has. A server without backups (`Backend::backups` answering
//! `Unsupported`) says so, and the sidebar still lists the sub-views.

use std::collections::BTreeSet;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Form, State};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use maud::{html, Markup};
use serde::Deserialize;

use crate::app::{Admin, AppState};
use crate::backend::{Backend, BackendError, Backup, Schedule, ScheduleKind};
use crate::components::{
    button, datetime, form_fields, help, modal, modal_actions, modal_button, relative, row,
    section, title, unavailable_setting,
};
use crate::icons::Icon;
use crate::pages::Sidebar;
use crate::views::{finish, live, refuse_stale, Ctx};

/// The sidebar's live region: the running backup's status.
pub(crate) const STATUS_REGION: &str = "status";

const BACK_UP_NOW: &str = "/admin-console/app/backups/actions/backupnow";
const SELECTED: &str = "/admin-console/app/backups/actions/selected";

/// The longest label, in characters. FileMaker Server's limit is not known;
/// 100 is its limit for a schedule's name (inf).
const MAX_LABEL: usize = 100;
/// The longest backup folder name a form may name, in bytes: a file name.
const MAX_NAME: usize = 255;
/// The most backups one action changes.
const MAX_SELECTED: usize = 1000;
/// The longest Filter by Label text used, in characters.
const MAX_FILTER: usize = 100;
/// The longest query value carried from one of the tab's forms to another.
const MAX_CARRIED: usize = 255;

const STATUS_HELP: &str = "Cancel stops a running backup and removes what it has written. \
    OpenMaker Server cannot cancel a backup yet.";
const LABEL_HELP: &str = "A scheduled backup is labeled Auto-backup, and one made with Back \
    Up Now On-demand. Edit Label gives the checked backups a label of your own.";
const PRESERVED_HELP: &str = "A schedule never deletes a preserved backup to make room for \
    newer ones. Preserve Backup preserves the checked backups; delete them on the Preserved \
    tab.";
const PROGRESSIVE_HELP: &str = "A progressive backup keeps a copy of each hosted file up to \
    date by copying only what changed since its last pass. OpenMaker Server does not make \
    progressive backups yet.";
const PARALLEL_HELP: &str = "Parallel backup copies groups of related files at the same time \
    and lets several backup schedules run at once. OpenMaker Server does not have it yet.";
const KEEP_CANCELLED_HELP: &str = "Keeps what a cancelled backup wrote instead of removing \
    it. OpenMaker Server cannot cancel a backup yet.";

/// The backup schedules, in the server's order; none when the server has
/// no schedules.
fn backup_schedules(backend: &dyn Backend) -> Vec<Schedule> {
    backend
        .schedules()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.kind == ScheduleKind::Backup)
        .collect()
}

/// Hidden fields that carry the page's query parameters `names` into a
/// form that replaces the query, so that one choice does not undo another.
fn carry(ctx: &Ctx, names: &[&str]) -> Markup {
    html! {
        @for name in names {
            @if let Some(value) = ctx.param(name).filter(|v| v.len() <= MAX_CARRIED) {
                input type="hidden" name=(name) value=(value);
            }
        }
    }
}

/// A select with OpenMaker's chevron in place of the browser's.
fn select(id: &str, name: &str, form: Option<&str>, options: Markup) -> Markup {
    html! {
        span.select-wrap {
            select.select id=(id) name=(name) form=[form] data-submit-on-change {
                (options)
            }
            (Icon::Chevron.render(14))
        }
    }
}

/// The sidebar above the sub-views: the chosen backup schedule (the
/// page's `schedule`, or the first), its plan, Back Up Now and the status;
/// and the number of backups after the lists' titles.
pub(crate) fn sidebar(ctx: &Ctx) -> Sidebar {
    let Ok(backups) = ctx.backend.backups() else {
        return Sidebar::default();
    };
    let schedules = backup_schedules(ctx.backend);
    let chosen = ctx
        .param("schedule")
        .and_then(|id| id.parse::<u32>().ok())
        .and_then(|id| schedules.iter().find(|s| s.id == id))
        .or_else(|| schedules.first());
    let plan = chosen.and_then(|s| ctx.backend.backup_plan(s.id).ok());
    let next = match chosen {
        None => String::new(),
        Some(s) if !s.enabled => "Disabled".to_owned(),
        Some(s) => s.next_run.map_or_else(|| "None".to_owned(), datetime),
    };
    let preserved = backups.items.iter().filter(|b| b.preserved).count();
    let top = html! {
        div.backup-plan {
            form.plan-schedule method="get" action=(ctx.path) {
                label.plan-label for="backup-schedule" { "Backup Schedule" }
                @if schedules.is_empty() {
                    span.unavailable { "None" }
                } @else {
                    (select("backup-schedule", "schedule", None, html! {
                        @for s in &schedules {
                            option value=(s.id) selected[chosen.is_some_and(|c| c.id == s.id)] {
                                (s.name)
                            }
                        }
                    }))
                    (carry(ctx, &["showing", "label"]))
                    noscript { button.btn type="submit" { "Show" } }
                }
            }
            div.plan-pair {
                div {
                    div.plan-label { "Repeat" }
                    div { (plan.as_ref().map_or("-", |p| p.repeat.as_str())) }
                }
                div.plan-end {
                    div.plan-label { "Keep" }
                    div { @if let Some(p) = &plan { (p.keep) } @else { "-" } }
                }
            }
            div.plan-item {
                div.plan-label { "Next Backup" }
                div.plan-line {
                    span { (next) }
                    @if chosen.is_some() {
                        a.link-button href="#back-up-now" { "Back Up Now" }
                    } @else {
                        button.link-button type="button" disabled { "Back Up Now" }
                    }
                }
            }
            (live("backups", STATUS_REGION, status_line(ctx.backend, Ok(backups.running))))
            @if let Some(schedule) = chosen {
                form method="post" action=(BACK_UP_NOW) {
                    (form_fields(ctx))
                    input type="hidden" name="schedule" value=(schedule.id);
                    (modal("back-up-now", "Back Up Now", html! {
                        p.modal-text {
                            "Back up the hosted databases with schedule \"" (schedule.name)
                            "\" now? The backup is labeled On-demand."
                        }
                        (modal_actions("OK", "run"))
                    }))
                }
            }
        }
    };
    Sidebar {
        top: Some(top),
        counts: vec![
            ("allbackups", backups.items.len()),
            ("preservedbackups", preserved),
        ],
    }
}

/// The live region: whether a backup is running.
pub(crate) fn status(ctx: &Ctx) -> Markup {
    status_line(ctx.backend, ctx.backend.backups().map(|b| b.running))
}

/// Backup Status after reading the backups: the running backup's schedule
/// (`running`), idle only after a read that succeeded, and otherwise not
/// available or unknown, with the reason as the tooltip.
fn status_line(backend: &dyn Backend, running: Result<Option<u32>, BackendError>) -> Markup {
    let running = running.map(|id| {
        id.map(|id| {
            backend
                .schedules()
                .ok()
                .and_then(|all| all.into_iter().find(|s| s.id == id))
                .map_or_else(|| format!("schedule {id}"), |s| s.name)
        })
    });
    html! {
        div.plan-item {
            div.plan-label { "Backup Status" (help(STATUS_HELP)) }
            div.plan-line {
                @match &running {
                    Ok(Some(name)) => span { "Running: " (name) },
                    Ok(None) => span { "No Backup Running" },
                    Err(BackendError::Unsupported) => span.unavailable { "Not available yet" },
                    Err(e) => span.read-failed title=(e) { "Status unknown" },
                }
                button.link-button type="button" disabled
                    title="OpenMaker Server cannot cancel a backup yet." { "Cancel" }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum List {
    /// Auto-Backup and On-Demand: every backup.
    All,
    Preserved,
}

/// Auto-Backup and On-Demand.
pub(crate) fn all(ctx: &Ctx) -> Markup {
    list(ctx, List::All)
}

/// Preserved.
pub(crate) fn preserved(ctx: &Ctx) -> Markup {
    list(ctx, List::Preserved)
}

/// The Filter by Label text as typed (at most [`MAX_FILTER`] characters),
/// when there is one.
fn label_filter(ctx: &Ctx) -> Option<String> {
    let text: String = ctx.param("label")?.chars().take(MAX_FILTER).collect();
    (!text.trim().is_empty()).then_some(text)
}

fn list(ctx: &Ctx, which: List) -> Markup {
    let heading = match which {
        List::All => "Auto-Backup and On-Demand",
        List::Preserved => "Preserved",
    };
    let backups = match ctx.backend.backups() {
        Ok(backups) => backups,
        Err(BackendError::Unsupported) => {
            return html! {
                (title(heading))
                p.unavailable { "Backups are not available in OpenMaker Server yet." }
            }
        }
        Err(e) => {
            return html! {
                (title(heading))
                p.read-failed { "The backups could not be read: " (e) }
            }
        }
    };
    let listed: Vec<&Backup> = backups
        .items
        .iter()
        .filter(|b| which == List::All || b.preserved)
        .collect();
    // "Showing" offers the backup schedules, then the other schedules a
    // listed backup came from, such as one since deleted.
    let mut schedules: Vec<String> = backup_schedules(ctx.backend)
        .into_iter()
        .map(|s| s.name)
        .collect();
    for backup in &listed {
        if !schedules.contains(&backup.schedule) {
            schedules.push(backup.schedule.clone());
        }
    }
    let showing = ctx
        .param("showing")
        .filter(|s| schedules.iter().any(|n| n == s));
    let rows: Vec<&Backup> = listed
        .iter()
        .copied()
        .filter(|b| showing.is_none_or(|s| b.schedule == s))
        .collect();
    let filter = match which {
        List::All => label_filter(ctx),
        List::Preserved => None,
    };
    let needle = filter.as_ref().map(|f| f.trim().to_lowercase());
    let visible = |b: &Backup| {
        needle
            .as_ref()
            .is_none_or(|n| b.label.to_lowercase().contains(n.as_str()))
    };
    let shown = rows.iter().filter(|b| visible(b)).count();
    let columns = match which {
        List::All => 5,
        List::Preserved => 4,
    };
    let empty = match (which, showing) {
        (_, Some(_)) => "No backups of this schedule were found.",
        (List::All, None) => "No backups were found.",
        (List::Preserved, None) => "No preserved backups were found.",
    };
    html! {
        (title(heading))
        form id="backup-filter" method="get" action=(ctx.path) {
            (carry(ctx, &["schedule"]))
        }
        form.backups-form method="post" action=(SELECTED) {
            (form_fields(ctx))
            // The Edit Label modal comes first, so that its OK is the form's
            // default button: Enter in the new label sends the label.
            (edit_label_modal())
            @if which == List::Preserved { (delete_modal()) }
            div.toolbar.backups-toolbar {
                @match which {
                    List::All => {
                        (button("Preserve Backup", "preserve", Some("backup")))
                        (modal_button("Edit Label", "edit-label", Some("backup")))
                    }
                    List::Preserved => {
                        (modal_button("Edit Label", "edit-label", Some("backup")))
                        (modal_button("Delete", "delete-backups", Some("backup")))
                    }
                }
                div.toolbar-end {
                    label.toolbar-label for="backup-showing" { "Showing" }
                    (select("backup-showing", "showing", Some("backup-filter"), html! {
                        option value="" selected[showing.is_none()] { "All Schedules" }
                        hr;
                        @for name in &schedules {
                            option value=(name) selected[showing == Some(name.as_str())] { (name) }
                        }
                    }))
                    @if which == List::All {
                        input.input.label-filter type="search" name="label" form="backup-filter"
                            placeholder="Filter by Label" aria-label="Filter by Label"
                            maxlength=(MAX_FILTER) autocomplete="off" value=[filter.as_deref()]
                            data-filter-rows="backup-table";
                    }
                    noscript { button.btn type="submit" form="backup-filter" { "Filter" } }
                }
            }
            div.table-scroll {
                table.data-table.backups-table id="backup-table" {
                    thead {
                        tr {
                            th.check-cell {
                                input type="checkbox" data-select-all="backup"
                                    aria-label="Select all backups" disabled[shown == 0];
                            }
                            th scope="col" { "Folder Name" }
                            th scope="col" { "Time" }
                            th scope="col" { span.th-help { "Label" (help(LABEL_HELP)) } }
                            @if which == List::All {
                                th scope="col" {
                                    span.th-help { "Preserved" (help(PRESERVED_HELP)) }
                                }
                            }
                        }
                    }
                    tbody {
                        @for backup in &rows {
                            @let on = visible(backup);
                            tr hidden[!on] data-label=(backup.label) {
                                td.check-cell {
                                    input type="checkbox" name="backup" value=(backup.name)
                                        aria-label={"Select " (backup.name)} disabled[!on];
                                }
                                td.backup-name { (backup.name) }
                                td { (relative(backup.time, ctx.now)) }
                                td.backup-label { (backup.label) }
                                @if which == List::All {
                                    td {
                                        @if backup.preserved {
                                            span.preserved-mark role="img" aria-label="Preserved" {
                                                (Icon::Check.render(18))
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        @if rows.is_empty() {
                            tr.empty-row { td colspan=(columns) { (empty) } }
                        } @else {
                            tr.empty-row.filter-empty hidden[shown > 0] {
                                td colspan=(columns) { "No backups match the filter." }
                            }
                        }
                    }
                }
            }
            @if !rows.is_empty() {
                p.showing.backups-showing {
                    "Showing " span data-shown-count { (shown) } " of " (rows.len())
                }
            }
        }
    }
}

fn edit_label_modal() -> Markup {
    modal(
        "edit-label",
        "Edit Label",
        html! {
            div.modal-field data-current-label hidden {
                span.modal-label { "Current Label" }
                span.current-label {}
            }
            div.modal-field {
                label.required for="new-label" { "New Label" }
                input.input id="new-label" type="text" name="new_label" maxlength=(MAX_LABEL)
                    autocomplete="off" placeholder={"Maximum " (MAX_LABEL) " characters"};
            }
            p.modal-text data-label-note { "Each checked backup gets the new label." }
            (modal_actions("OK", "relabel"))
        },
    )
}

fn delete_modal() -> Markup {
    modal(
        "delete-backups",
        "Confirm Deletion",
        html! {
            p.modal-text {
                "Delete the checked backups? Their folders, with the copies of the databases "
                "in them, are removed from the backup folder. This cannot be undone."
            }
            (modal_actions("Delete", "delete"))
        },
    )
}

/// Backup Settings: the settings OpenMaker Server does not have say so.
pub(crate) fn settings() -> Markup {
    let setting = |label: &str, help_text: &str| {
        let (value, toggle) = unavailable_setting(label);
        html! {
            (section(label, None))
            (row(label, Some(help_text), value, toggle))
        }
    };
    let (progressive, _) = unavailable_setting("Progressive Backup");
    html! {
        (title("Backup Settings"))
        (section("Progressive Backup", None))
        (row(
            "Progressive Backup",
            Some(PROGRESSIVE_HELP),
            progressive,
            html! { button.link-button type="button" disabled { "Change" } },
        ))
        (setting("Parallel Backup", PARALLEL_HELP))
        (setting("Keep Cancelled Backups", KEEP_CANCELLED_HELP))
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(BACK_UP_NOW, post(back_up_now))
        .route(SELECTED, post(change_selected))
}

#[derive(Deserialize)]
struct BackUpNowForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    back: String,
    #[serde(default)]
    schedule: String,
}

async fn back_up_now(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Admin(admin): Admin,
    Form(form): Form<BackUpNowForm>,
) -> Response {
    if let Some(refused) = refuse_stale(&admin, &form.csrf, peer) {
        return refused;
    }
    let backend = state.backend();
    let outcome = form
        .schedule
        .parse::<u32>()
        .map_err(|_| BackendError::Invalid("Choose a backup schedule.".into()))
        .and_then(|id| {
            backend.back_up_now(id)?;
            let name = backup_schedules(backend)
                .into_iter()
                .find(|s| s.id == id)
                .map_or_else(|| format!("schedule {id}"), |s| format!("\"{}\"", s.name));
            Ok(format!("Back Up Now started a backup with {name}."))
        });
    finish(&state, &admin, peer, &form.back, "back up now", outcome)
}

/// Preserve Backup, Edit Label's OK and Delete's confirmation, for the
/// checked backups of a list. The checkboxes repeat one field, so the form
/// is read as its pairs.
async fn change_selected(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Admin(admin): Admin,
    Form(fields): Form<Vec<(String, String)>>,
) -> Response {
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if let Some(refused) = refuse_stale(&admin, field("csrf").unwrap_or_default(), peer) {
        return refused;
    }
    let backend = state.backend();
    let (what, outcome) = match selection(&fields) {
        Err(e) => ("change backups".to_owned(), Err(e)),
        Ok(names) if field("relabel").is_some() => (
            format!("label {}", count(names.len())),
            new_label(field("new_label").unwrap_or_default()).and_then(|label| {
                let done = each(&names, "Labeled", |name| backend.label_backup(name, &label))?;
                Ok(format!("Labeled {} \"{label}\".", count(done)))
            }),
        ),
        Ok(names) if field("delete").is_some() => (
            format!("delete {}", count(names.len())),
            each(&names, "Deleted", |name| backend.delete_backup(name))
                .map(|done| format!("Deleted {}.", count(done))),
        ),
        Ok(names) if field("preserve").is_some() => (
            format!("preserve {}", count(names.len())),
            each(&names, "Preserved", |name| {
                backend.preserve_backup(name, true)
            })
            .map(|done| format!("Preserved {}.", count(done))),
        ),
        Ok(_) => (
            "change backups".to_owned(),
            Err(BackendError::Invalid("The form is not complete.".into())),
        ),
    };
    finish(
        &state,
        &admin,
        peer,
        field("back").unwrap_or_default(),
        &what,
        outcome,
    )
}

/// The checked backups' folder names, each once, in the form's order.
fn selection(fields: &[(String, String)]) -> Result<Vec<String>, BackendError> {
    let mut seen = BTreeSet::new();
    let mut names = Vec::new();
    for (key, name) in fields {
        if key != "backup" || !seen.insert(name.as_str()) {
            continue;
        }
        if names.len() == MAX_SELECTED {
            return Err(BackendError::Invalid(format!(
                "Check at most {MAX_SELECTED} backups at a time."
            )));
        }
        check_name(name)?;
        names.push(name.clone());
    }
    if names.is_empty() {
        return Err(BackendError::Invalid("Check a backup first.".into()));
    }
    Ok(names)
}

/// Refuse what cannot be a backup's folder name. A server may find the
/// folder by the name, so nothing that could lead out of the backup folder
/// passes.
fn check_name(name: &str) -> Result<(), BackendError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control());
    if valid {
        Ok(())
    } else {
        Err(BackendError::Invalid(
            "A checked backup's name is not a backup folder's name.".into(),
        ))
    }
}

/// The label Edit Label gives, trimmed: one line of at most [`MAX_LABEL`]
/// characters, without control characters. A server may keep a label on a
/// line of a settings file, where a line break could forge another
/// setting.
fn new_label(text: &str) -> Result<String, BackendError> {
    let label = text.trim();
    if label.is_empty() {
        return Err(BackendError::Invalid("Enter a new label.".into()));
    }
    if label.chars().count() > MAX_LABEL {
        return Err(BackendError::Invalid(format!(
            "A label has at most {MAX_LABEL} characters."
        )));
    }
    if label.chars().any(char::is_control) {
        return Err(BackendError::Invalid(
            "A label is one line of text, without control characters.".into(),
        ));
    }
    Ok(label.to_owned())
}

/// Do `act` to each named backup; the number done, or why not all were.
fn each(
    names: &[String],
    verb: &str,
    mut act: impl FnMut(&str) -> Result<(), BackendError>,
) -> Result<usize, BackendError> {
    let mut done = 0;
    let mut failed = None;
    for name in names {
        match act(name) {
            Ok(()) => done += 1,
            Err(BackendError::Unsupported) => return Err(BackendError::Unsupported),
            Err(e) => {
                failed.get_or_insert((name, e));
            }
        }
    }
    match failed {
        None => Ok(done),
        Some((_, e)) if names.len() == 1 => Err(e),
        Some((name, e)) => Err(BackendError::Failed(format!(
            "{verb} {done} of {}. \"{name}\": {e}",
            count(names.len())
        ))),
    }
}

fn count(n: usize) -> String {
    if n == 1 {
        "1 backup".to_owned()
    } else {
        format!("{n} backups")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockBackend;

    #[test]
    fn the_status_is_idle_only_after_a_read_that_succeeded() {
        let backend = MockBackend::empty();
        let line = |read| status_line(&backend, read).into_string();
        assert!(line(Ok(None)).contains("No Backup Running"));
        assert!(line(Ok(Some(2))).contains("Running: schedule 2"));
        let failed = line(Err(BackendError::Failed("The disk is full.".into())));
        assert!(failed.contains("Status unknown") && failed.contains("The disk is full."));
        assert!(!failed.contains("No Backup Running"));
        assert!(line(Err(BackendError::Unsupported)).contains("Not available yet"));
    }
}
