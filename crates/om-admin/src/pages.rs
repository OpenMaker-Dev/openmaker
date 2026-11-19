//! The console's HTML: the sign-in page and the signed-in application shell
//! (top bar, tab row, sidebar, footer), laid out as FileMaker Server's
//! (`docs/re/admin-console.md` sections 3 and 6). `maud` escapes all text.

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use maud::{html, Markup, DOCTYPE};

use crate::assets;
use crate::backend::Alert;
use crate::components;
use crate::icons::{self, Icon};
use crate::nav::{Tab, View, TABS};
use crate::session::{Flash, SignedIn};

/// Alerts the bell's panel lists at most; it says how many there are.
const ALERTS_SHOWN: usize = 20;

/// The product name in titles.
const PRODUCT: &str = "OpenMaker Server Admin Console";

fn document(title: &str, body_class: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="icon" type="image/svg+xml" href=(assets::favicon_url());
                link rel="stylesheet" href=(assets::stylesheet_url());
                script src=(assets::script_url()) defer {}
            }
            body class=(body_class) {
                (body)
            }
        }
    }
}

fn wordmark(mark_size: u32) -> Markup {
    html! {
        span.wordmark {
            (icons::mark(mark_size))
            span.wordmark-name { "OpenMaker" }
            span.wordmark-product { "Server" }
        }
    }
}

fn footer() -> Markup {
    html! {
        footer.footer {
            p { "OpenMaker Server is open-source software under the MIT License." }
            p.footer-note {
                "FileMaker and Claris are trademarks of Claris International Inc. "
                "OpenMaker is not affiliated with or endorsed by Claris."
            }
        }
    }
}

/// The sign-in page, with `message` (a failure, a lockout, a timed-out
/// session) above the fields and `user` typed again.
pub(crate) fn sign_in(message: Option<&str>, user: &str) -> Markup {
    document(
        &format!("Sign In - {PRODUCT}"),
        "signin-page",
        html! {
            main.signin {
                div.signin-brand {
                    (wordmark(40))
                    div.signin-caption { "ADMIN CONSOLE" }
                }
                h1.visually-hidden { "Admin Console Sign In" }
                form.signin-form method="post" action="/admin-console/signin" {
                    @if let Some(message) = message {
                        p.signin-message role="alert" { (message) }
                    }
                    div.signin-fields {
                        input type="text" name="username" placeholder="User Name"
                            aria-label="User Name" autocomplete="username" autocapitalize="none"
                            spellcheck="false" required autofocus[user.is_empty()] value=(user);
                        input type="password" name="password" placeholder="Password"
                            aria-label="Password" autocomplete="current-password" required
                            autofocus[!user.is_empty()];
                    }
                    button.signin-button type="submit" { "Sign In" }
                }
                details.password-help {
                    summary { "Password Help" }
                    p {
                        "The Admin Console account is set when OpenMaker Server starts: "
                        "the user name with --admin-user, and the password in the file "
                        "--admin-password-file names or in the OM_ADMIN_PASSWORD "
                        "environment variable. To change the password, restart the "
                        "server with a new one."
                    }
                }
            }
            (footer())
        },
    )
}

/// What a signed-in page's chrome shows.
pub(crate) struct Chrome<'a> {
    pub(crate) session: &'a SignedIn,
    /// The page's own address, with its view's query when it has one:
    /// acknowledging an alert returns there.
    pub(crate) path: &'a str,
    /// Unacknowledged alerts, newest first.
    pub(crate) alerts: &'a [Alert],
    /// The server's clock.
    pub(crate) now: &'a Zoned,
    /// The outcome of the session's latest action, shown once.
    pub(crate) flash: Option<&'a Flash>,
    /// What the tab's sidebar shows besides its sub-views.
    pub(crate) sidebar: &'a Sidebar,
}

/// What a tab's sidebar shows besides its title, introduction and
/// sub-views (`crate::views::sidebar`).
#[derive(Default)]
pub(crate) struct Sidebar {
    /// Shown between the introduction and the sub-view list, as the Backups
    /// tab's backup schedule and status are.
    pub(crate) top: Option<Markup>,
    /// A count after a sub-view's title, by the sub-view's route:
    /// "Preserved (2)".
    pub(crate) counts: Vec<(&'static str, usize)>,
}

/// A signed-in page: the shell around `content`, with `tab` and `view`
/// selected.
pub(crate) fn app(chrome: &Chrome, tab: &Tab, view: Option<&View>, content: Markup) -> Markup {
    let title = view.map_or(tab.title, |v| v.title);
    document(
        &format!("{title} - {PRODUCT}"),
        "app-page",
        html! {
            (top_bar(chrome))
            (tab_row(tab))
            @if tab.views.is_empty() {
                main.content.full-width {
                    section.view {
                        @if let Some(flash) = chrome.flash { (components::flash(flash)) }
                        (content)
                    }
                }
            } @else {
                main.content.two-column {
                    (sidebar(tab, view, chrome.sidebar))
                    section.view {
                        @if let Some(flash) = chrome.flash { (components::flash(flash)) }
                        (content)
                    }
                }
            }
            (footer())
        },
    )
}

fn top_bar(chrome: &Chrome) -> Markup {
    let now = chrome.now;
    html! {
        header.topbar {
            a.topbar-home href="/admin-console/app/dashboard" aria-label="OpenMaker Server, Dashboard" {
                (wordmark(26))
            }
            div.topbar-tools {
                span.clock data-epoch=(now.timestamp().as_millisecond())
                    data-offset=(now.offset().seconds()) {
                    (now.strftime("%A, %B %-d, %Y at %H:%M"))
                }
                button.time-toggle type="button" hidden aria-pressed="false"
                    title="Show the time in this browser's time zone" { "SERVER" }
                span.divider {}
                (bell(chrome))
                span.divider {}
                form.signout method="post" action="/admin-console/signout" {
                    input type="hidden" name="csrf" value=(chrome.session.csrf);
                    button.link-button type="submit" { "Sign Out" }
                }
            }
        }
    }
}

fn bell(chrome: &Chrome) -> Markup {
    let total = chrome.alerts.len();
    let shown = &chrome.alerts[..total.min(ALERTS_SHOWN)];
    html! {
        details.bell {
            summary.bell-button aria-label={"Alerts: " (total)} {
                (Icon::Bell.render(18))
                @if total > 0 { span.badge { (total) } }
            }
            div.bell-panel {
                @if shown.is_empty() {
                    p.bell-empty { "There are no alerts." }
                }
                @for alert in shown {
                    div.alert {
                        div.alert-time { (alert_time(alert)) }
                        div.alert-message {
                            (icons::severity(alert.severity))
                            span { (alert.message) }
                        }
                        form.alert-ack method="post"
                            action={"/admin-console/alerts/" (alert.id) "/acknowledge"} {
                            input type="hidden" name="csrf" value=(chrome.session.csrf);
                            input type="hidden" name="back" value=(chrome.path);
                            button.link-button type="submit" { "OK" }
                        }
                    }
                }
                p.bell-count { "Showing " (shown.len()) " of " (total) }
            }
        }
    }
}

fn alert_time(alert: &Alert) -> String {
    match Timestamp::try_from(alert.time) {
        Ok(time) => time
            .to_zoned(TimeZone::system())
            .strftime("%m/%d/%Y, %H:%M:%S")
            .to_string(),
        Err(_) => String::new(),
    }
}

fn tab_row(selected: &Tab) -> Markup {
    html! {
        nav.tabs aria-label="Admin Console" {
            ul {
                @for tab in TABS {
                    @let current = tab.route == selected.route;
                    li {
                        a.tab.active[current] href=(tab.url())
                            aria-current=[current.then_some("page")] { (tab.title) }
                    }
                }
            }
        }
    }
}

fn sidebar(tab: &Tab, selected: Option<&View>, extra: &Sidebar) -> Markup {
    html! {
        aside.sidebar {
            h2.m-title { (tab.title) }
            p.sidebar-intro { (tab.intro) }
            @if let Some(top) = &extra.top { (top) }
            ul.sidebar-nav.compact[tab.compact] {
                @for view in tab.views {
                    @let current = selected.is_some_and(|s| s.route == view.route);
                    @let count = extra.counts.iter().find(|(r, _)| *r == view.route);
                    li {
                        a.active[current] href=(tab.view_url(view))
                            aria-current=[current.then_some("page")] {
                            (view.icon.render(20))
                            span {
                                (view.title)
                                @if let Some((_, n)) = count { " (" (n) ")" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A view whose feature OpenMaker Server does not have yet.
pub(crate) fn unavailable(title: &str) -> Markup {
    html! {
        h1.m-title { (title) }
        p.unavailable { "This view is not available in OpenMaker Server yet." }
    }
}

/// A page outside the application: not found, refused.
pub(crate) fn message(title: &str, text: &str) -> Markup {
    document(
        &format!("{title} - {PRODUCT}"),
        "message-page",
        html! {
            main.message {
                (wordmark(32))
                h1.m-title { (title) }
                p { (text) }
                p { a href="/admin-console/app/dashboard" { "Go to the Admin Console" } }
            }
        },
    )
}
