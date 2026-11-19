//! Configuration > SSL Certificate (`docs/re/admin-console.md` 4.5): the
//! certificate the server presents, its Certificate Information and
//! Certificate Details.
//!
//! OpenMaker Server takes its certificate from `--cert` and `--key`, or
//! uses one it found or made when it started (`docs/development.md`, "The
//! server's certificate"), so the view explains that in place of FileMaker
//! Server's Create CSR, Let's Encrypt, Import and Delete Files.

use std::time::SystemTime;

use jiff::Timestamp;
use maud::{html, Markup};

use crate::certificate::{summarize, Summary};
use crate::components::{row, section, title};
use crate::views::{not_shown, Ctx};

/// Days before expiry from which the expiry is shown as a warning, as
/// FileMaker Server's own alert warns 30 days ahead
/// (`docs/re/admin-console.md` 4.5).
const WARN_DAYS: u64 = 30;

const INTRO: &str = "OpenMaker Server presents this certificate to FileMaker clients and to \
    the Admin Console. It uses the certificate and key given with --cert and --key when it \
    starts. Without them it uses the operating system's own self-signed certificate, or one \
    it made at its first start and keeps in its state folder.";

const NOT_AVAILABLE: &str = "Certificate signing requests, Let's Encrypt certificates and \
    importing or deleting certificate files are not available in the Admin Console. To use \
    another certificate, restart the server with --cert and --key.";

const SELF_SIGNED_HELP: &str = "No certificate authority vouches for a self-signed \
    certificate, so clients cannot verify it on their own: FileMaker Pro warns before it \
    connects, and OpenMaker clients must be given the certificate to trust.";

const EXPIRES_HELP: &str = "The certificate is valid until this time. After it, clients \
    refuse to connect with it.";

pub(crate) fn page(ctx: &Ctx) -> Markup {
    let chain = ctx.backend.certificate();
    let now = SystemTime::from(ctx.now.timestamp());
    html! {
        (title("SSL Certificate"))
        p.page-intro { (INTRO) }
        p.section-note { (NOT_AVAILABLE) }
        @match chain {
            Err(e) => {
                (section("Certificate Information", None))
                (not_shown("The certificate is not available in OpenMaker Server yet.", &e))
            }
            Ok(chain) => @match chain.first().map(|der| summarize(der)) {
                None => {
                    (section("Certificate Information", None))
                    p.unavailable { "The server presents no certificate." }
                }
                Some(None) => {
                    (section("Certificate Information", None))
                    p.unavailable.read-failed role="status" { "The server's certificate cannot be read." }
                }
                Some(Some(summary)) => (details(&summary, now)),
            },
        }
    }
}

fn details(summary: &Summary, now: SystemTime) -> Markup {
    let status = if summary.self_signed {
        "Self-signed certificate installed"
    } else {
        "Certificate installed"
    };
    let expired = summary.expires <= now;
    let not_yet = now < summary.not_before;
    html! {
        (section("Certificate Information", None))
        (row(
            "Status",
            summary.self_signed.then_some(SELF_SIGNED_HELP),
            html! {
                (status)
                @if not_yet {
                    span.cert-note.cert-problem {
                        "Not valid until " (utc(summary.not_before))
                    }
                }
            },
            html! {},
        ))
        (row("Issued To", None, text_or_none(summary.issued_to.as_deref()), html! {}))
        (row("Issued By", None, text_or_none(summary.issued_by.as_deref()), html! {}))
        @if expired {
            (row(
                "Expired On",
                Some(EXPIRES_HELP),
                html! { span.cert-problem { (utc(summary.expires)) } },
                html! {},
            ))
        } @else {
            (row(
                "Expires",
                Some(EXPIRES_HELP),
                html! { (utc(summary.expires)) (expires_in(summary.expires, now)) },
                html! {},
            ))
        }
        (section("Certificate Details", None))
        @if let Some(country) = &summary.country {
            (row("Country", None, html! { (country) }, html! {}))
        }
        @if let Some(state) = &summary.state {
            (row("State/Province", None, html! { (state) }, html! {}))
        }
        @if let Some(locality) = &summary.locality {
            (row("Locality", None, html! { (locality) }, html! {}))
        }
        @if let Some(organization) = &summary.organization {
            (row("Organization", None, html! { (organization) }, html! {}))
        }
        (row("Name", None, text_or_none(summary.issued_to.as_deref()), html! {}))
    }
}

fn text_or_none(text: Option<&str>) -> Markup {
    match text {
        Some(text) => html! { (text) },
        None => html! { span.unavailable { "None" } },
    }
}

/// `2027-01-15 08:30:00 UTC`, as FileMaker Server shows the expiry.
fn utc(time: SystemTime) -> String {
    Timestamp::try_from(time).map_or_else(
        |_| String::new(),
        |t| t.strftime("%Y-%m-%d %H:%M:%S UTC").to_string(),
    )
}

/// "Expires in 23 days" under the expiry, a warning within
/// [`WARN_DAYS`].
fn expires_in(expires: SystemTime, now: SystemTime) -> Markup {
    let days = expires
        .duration_since(now)
        .map_or(0, |left| left.as_secs() / 86_400);
    let text = match days {
        0 => "Expires today".to_owned(),
        1 => "Expires in 1 day".to_owned(),
        n => format!("Expires in {n} days"),
    };
    html! {
        span.cert-note.cert-warning[days < WARN_DAYS] { (text) }
    }
}
