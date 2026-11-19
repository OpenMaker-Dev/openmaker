//! The Dashboard (`docs/re/admin-console.md` 4.1): System Overview with
//! the connection counters, client types and the server's facts; Volume
//! Status; and System Statistics, one chart at a time, picked by
//! `?chart=<cpu|memory|network|disk>`. Its three parts are live regions.

use std::time::SystemTime;

use jiff::tz::TimeZone;
use jiff::Timestamp;
use maud::{html, Markup};

use crate::backend::{BackendError, Client, DatabaseStatus};
use crate::certificate;
use crate::chart::{doughnut, line_chart, LineChart};
use crate::components::{bytes, datetime, help, link_button};
use crate::stats::Metric;
use crate::views::{live, live_query, Ctx};

const TAB: &str = "dashboard";
const PAGE: &str = "/admin-console/app/dashboard";
const CERTIFICATE_PAGE: &str = "/admin-console/app/configuration/sslcertificate";

/// What OpenMaker Server shows for a figure it does not have.
const NOT_AVAILABLE: &str = "Not available";

const PRO_HELP: &str = "FileMaker Pro clients connected now. FileMaker Server counts them \
    against the client limit of its license; OpenMaker Server has no such limit.";
const ADDITIONAL_HELP: &str = "Clients of other kinds connected now, such as the Data API, \
    ODBC and JDBC, or other servers. FileMaker Server does not count them against the \
    client limit of its license.";

/// The client families of the System Overview's chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    Pro,
    Go,
    WebDirect,
    Additional,
}

impl Family {
    const ALL: [Family; 4] = [
        Family::Pro,
        Family::Go,
        Family::WebDirect,
        Family::Additional,
    ];

    /// The family of a client of FileMaker application type `app_type`
    /// (`ApplicationInfo::AppType`, `docs/re/calc-functions.md` 6):
    /// FileMaker Pro (0, the former Pro Advanced, and 1), FileMaker Go (5,
    /// mobile) and FileMaker WebDirect (8); every other type is Additional.
    /// Which types FileMaker Server counts where is inferred from its chips'
    /// help, not measured.
    pub(crate) fn of(app_type: u8) -> Family {
        match app_type {
            0 | 1 => Family::Pro,
            5 => Family::Go,
            8 => Family::WebDirect,
            _ => Family::Additional,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Family::Pro => "FileMaker Pro",
            Family::Go => "FileMaker Go",
            Family::WebDirect => "FileMaker WebDirect",
            Family::Additional => "Additional",
        }
    }

    fn class(self) -> &'static str {
        match self {
            Family::Pro => "pro",
            Family::Go => "go",
            Family::WebDirect => "webdirect",
            Family::Additional => "additional",
        }
    }

    fn help(self) -> Option<&'static str> {
        match self {
            Family::Pro => Some(PRO_HELP),
            Family::Additional => Some(ADDITIONAL_HELP),
            Family::Go | Family::WebDirect => None,
        }
    }
}

pub(crate) fn page(ctx: &Ctx) -> Markup {
    let metric = metric(ctx.query);
    html! {
        div.dashboard {
            h1.m-title { "System Overview" }
            (live(TAB, "overview", overview(ctx)))
            div.dashboard-lower {
                section.volume aria-labelledby="volume-title" {
                    h2.m-title id="volume-title" { "Volume Status" }
                    (live(TAB, "volume", volume(ctx)))
                }
                section.statistics aria-labelledby="statistics-title" {
                    h2.m-title id="statistics-title" { "System Statistics" }
                    (live_query(TAB, "statistics", &chart_query(metric), statistics(ctx, metric)))
                    nav.chart-tabs aria-label="Statistics" {
                        @for (i, m) in Metric::ALL.into_iter().enumerate() {
                            @if i > 0 { span.chart-tab-separator aria-hidden="true" { "|" } }
                            @if m == metric {
                                span.chart-tab.current aria-current="true" { (m.label()) }
                            } @else {
                                a.chart-tab href={(PAGE) "?" (chart_query(m))} { (m.label()) }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Live region `region` of the Dashboard.
pub(crate) fn fragment(ctx: &Ctx, region: &str) -> Option<Markup> {
    match region {
        "overview" => Some(overview(ctx)),
        "volume" => Some(volume(ctx)),
        "statistics" => Some(statistics(ctx, metric(ctx.query))),
        _ => None,
    }
}

/// The chart the query picks; CPU when it picks none.
fn metric(query: &str) -> Metric {
    form_urlencoded::parse(query.as_bytes())
        .take(16)
        .find(|(key, _)| key == "chart")
        .and_then(|(_, value)| Metric::from_key(&value))
        .unwrap_or(Metric::Cpu)
}

fn chart_query(metric: Metric) -> String {
    format!("chart={}", metric.key())
}

/// The counters, the client types and the server's facts.
fn overview(ctx: &Ctx) -> Markup {
    let clients = ctx.backend.clients();
    let databases = ctx.backend.databases();
    let open = databases
        .iter()
        .filter(|d| !matches!(d.status, DatabaseStatus::Closed | DatabaseStatus::Opening))
        .count();
    let server = ctx.backend.server();
    let addresses = if !server.ip_addresses.is_empty() {
        server.ip_addresses.join(", ")
    } else if server.address.ip().is_unspecified() {
        NOT_AVAILABLE.to_owned()
    } else {
        server.address.ip().to_string()
    };
    html! {
        div.overview-counters {
            div.counter {
                div.counter-number { (clients.len()) }
                div.counter-caption { "Total Connections" }
            }
            div.counter {
                div.counter-number {
                    (open) span.counter-total { " / " (databases.len()) }
                }
                div.counter-caption { "Hosted Databases" }
            }
        }
        div.client-types {
            @for family in Family::ALL {
                span.client-type {
                    span class={"client-chip " (family.class())} { (family.label()) }
                    span.client-count { (count(&clients, family)) }
                    span.client-caption { "Connections" }
                    @if let Some(text) = family.help() { (help(text)) }
                }
            }
        }
        div.server-facts {
            div.facts-column {
                (fact("Server Name", html! { (server.name) }))
                (fact("Server IP Addresses", html! { (addresses) }))
                (fact("Server Version", html! { (server.version) }))
            }
            div.facts-column {
                (fact("FileMaker Server License Expiration", html! { "None" }))
                (fact("SSL Certificate", certificate_expiry(ctx)))
                (fact("Last Backup", last_backup(ctx)))
            }
            div.facts-column {
                (fact("FileMaker Data API Connections", unavailable()))
                (fact("ODBC and JDBC Connections", unavailable()))
                (fact("FileMaker Script Engine Connections", unavailable()))
            }
        }
    }
}

fn count(clients: &[Client], family: Family) -> usize {
    clients
        .iter()
        .filter(|c| Family::of(c.app_type) == family)
        .count()
}

fn fact(label: &str, value: Markup) -> Markup {
    html! {
        div.fact {
            div.fact-label { (label) }
            div.fact-value { (value) }
        }
    }
}

fn unavailable() -> Markup {
    html! { span.unavailable { (NOT_AVAILABLE) } }
}

/// A figure the server failed to give: an error, not "not available".
fn read_failed(why: &str) -> Markup {
    html! { span.read-failed { "Could not be read: " (why) } }
}

/// "Expires on 2026-12-04 02:10:40 UTC", with Details.
fn certificate_expiry(ctx: &Ctx) -> Markup {
    let chain = match ctx.backend.certificate() {
        Ok(chain) => chain,
        Err(BackendError::Unsupported) => return unavailable(),
        Err(e) => return read_failed(&e.to_string()),
    };
    let expires = chain
        .first()
        .and_then(|der| certificate::summarize(der))
        .and_then(|summary| Timestamp::try_from(summary.expires).ok());
    match expires {
        Some(expires) => html! {
            span.fact-text {
                "Expires on " (expires.to_zoned(TimeZone::UTC).strftime("%Y-%m-%d %H:%M:%S UTC"))
            }
            (link_button("Details", CERTIFICATE_PAGE))
        },
        None => read_failed("the server's certificate is not one the console can read."),
    }
}

/// The newest backup: "Daily (11/11/2026, 00:00)", or "None", also when
/// the server makes no backups.
fn last_backup(ctx: &Ctx) -> Markup {
    let newest = match ctx.backend.backups() {
        Ok(backups) => backups.items.into_iter().max_by_key(|b| b.time),
        Err(BackendError::Unsupported) => None,
        Err(e) => return read_failed(&e.to_string()),
    };
    match newest {
        Some(backup) => html! { (backup.schedule) " (" (datetime(backup.time)) ")" },
        None => html! { "None" },
    }
}

/// The doughnut and the space of the file system that holds the files.
fn volume(ctx: &Ctx) -> Markup {
    let reading = ctx.statistics.volume();
    let volume = reading.as_ref().and_then(|r| r.as_ref().ok()).copied();
    let fraction = volume
        .filter(|v| v.total > 0)
        .map(|v| v.used as f64 / v.total as f64);
    let figure = |value: Option<u64>| value.map_or_else(|| "N/A".to_owned(), bytes);
    html! {
        div.volume-chart {
            (doughnut(fraction.unwrap_or(0.0)))
            div.volume-center {
                @match fraction {
                    Some(fraction) => {
                        span.volume-percent {
                            ((fraction * 100.0).round()) span.volume-unit { "%" }
                        }
                    }
                    None => span.volume-percent { "N/A" },
                }
                span.volume-caption { "Space Used" }
            }
        }
        dl.volume-rows {
            div.volume-row {
                dt { "Total Space" }
                dd { (figure(volume.map(|v| v.total))) }
            }
            div.volume-row {
                dt { span.volume-key.used {} "Space Used" }
                dd { (figure(volume.map(|v| v.used))) }
            }
            div.volume-row {
                dt { span.volume-key.free {} "Space Remaining" }
                dd { (figure(volume.map(|v| v.free))) }
            }
        }
        @if let Some(Err(why)) = &reading {
            p.volume-error { (read_failed(why)) }
        }
    }
}

/// The chart of `metric` over the samples kept.
fn statistics(ctx: &Ctx, metric: Metric) -> Markup {
    let samples = ctx.statistics.samples();
    if !samples.is_empty() && samples.iter().all(|s| metric.value(s).is_none()) {
        return html! {
            p.unavailable.statistics-unavailable {
                (metric.title()) " is not available on this system."
            }
        };
    }
    let labels: Vec<String> = samples.iter().map(|s| clock(s.time)).collect();
    let values: Vec<Option<f64>> = samples.iter().map(|s| metric.value(s)).collect();
    html! {
        div.chart-frame {
            (line_chart(&LineChart {
                title: metric.title(),
                y_title: metric.unit(),
                x_title: "Time",
                labels: &labels,
                values: &values,
            }))
        }
    }
}

/// A sample's time as `17:55:01`, in the server's time zone.
fn clock(time: SystemTime) -> String {
    Timestamp::try_from(time).map_or_else(
        |_| String::new(),
        |t| {
            t.to_zoned(TimeZone::system())
                .strftime("%H:%M:%S")
                .to_string()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::Family;

    #[test]
    fn application_types_fall_into_the_dashboards_families() {
        // ApplicationInfo::AppType: kDeveloper 0, kPro 1, kRuntime 2,
        // kServer 3, kCWP 4, kMobile 5, kXDBC 6, kSASE 7, kIWP 8,
        // kWebIntgrPlat 9, kAdminServer 10, kOData 11, kFCC 12, kTool 99.
        let families: Vec<Family> = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 99, 255]
            .into_iter()
            .map(Family::of)
            .collect();
        use Family::*;
        assert_eq!(
            families,
            [
                Pro, Pro, Additional, Additional, Additional, Go, Additional, Additional,
                WebDirect, Additional, Additional, Additional, Additional, Additional, Additional
            ]
        );
    }
}
