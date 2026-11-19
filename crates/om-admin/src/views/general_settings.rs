//! Configuration > General Settings (`docs/re/admin-console.md` 4.5): the
//! server's identity, and the startup settings OpenMaker Server has, which
//! so far is Block New Users.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Form, State};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use maud::{html, Markup};
use serde::Deserialize;

use crate::app::{Admin, AppState};
use crate::backend::BackendError;
use crate::components::{enabled, long_datetime, row, section, title, toggle};
use crate::views::{finish, refuse_stale, Ctx};

const BLOCK_NEW_USERS: &str = "/admin-console/app/configuration/generalsettings/blocknewusers";

const BLOCK_NEW_USERS_HELP: &str = "While Block New Users is on, clients cannot open the \
    hosted files. Clients that have files open keep working in them.";

pub(crate) fn page(ctx: &Ctx) -> Markup {
    let server = ctx.backend.server();
    let addresses = if server.ip_addresses.is_empty() {
        server.address.ip().to_string()
    } else {
        server.ip_addresses.join(", ")
    };
    let block = ctx.backend.block_new_users();
    html! {
        (title("General Settings"))
        (section("Server Information", None))
        (row("Database Server", None, html! { "Running" }, html! {}))
        (row("Start Time", None, html! { (long_datetime(server.started_at)) }, html! {}))
        (row("Server Name", None, html! { (server.name) }, html! {}))
        (row(
            "Server ID",
            Some("The server ID identifies this OpenMaker Server."),
            html! { span.server-id { (server.id) } },
            html! {},
        ))
        (row("Server IP Address", None, html! { (addresses) }, html! {}))
        (row("Server Version", None, html! { (server.version) }, html! {}))
        (row("Display Language", None, html! { "English" }, html! {}))
        (section("Startup Settings", None))
        @match block {
            Ok(on) => (row(
                "Block New Users",
                Some(BLOCK_NEW_USERS_HELP),
                html! { (enabled(on)) },
                toggle(ctx, BLOCK_NEW_USERS, on, true, "Block New Users"),
            )),
            Err(BackendError::Unsupported) => (row(
                "Block New Users",
                Some(BLOCK_NEW_USERS_HELP),
                html! { span.unavailable { "Not available yet" } },
                toggle(ctx, BLOCK_NEW_USERS, false, false, "Block New Users"),
            )),
            Err(e) => (row(
                "Block New Users",
                Some(BLOCK_NEW_USERS_HELP),
                html! { span.read-failed { "Could not be read: " (e) } },
                toggle(ctx, BLOCK_NEW_USERS, false, false, "Block New Users"),
            )),
        }
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route(BLOCK_NEW_USERS, post(set_block_new_users))
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

async fn set_block_new_users(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Admin(admin): Admin,
    Form(form): Form<ToggleForm>,
) -> Response {
    if let Some(refused) = refuse_stale(&admin, &form.csrf, peer) {
        return refused;
    }
    let outcome = match form.on.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(BackendError::Invalid("The form is not complete.".into())),
    }
    .and_then(|on| {
        state.backend().set_block_new_users(on)?;
        Ok(if on {
            "Block New Users is on: clients cannot open the hosted files.".to_owned()
        } else {
            "Block New Users is off.".to_owned()
        })
    });
    finish(
        &state,
        &admin,
        peer,
        &form.back,
        "change Block New Users",
        outcome,
    )
}
