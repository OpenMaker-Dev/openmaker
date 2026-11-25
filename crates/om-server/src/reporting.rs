//! Error and panic reports to Sentry, for the `om-server` and `om-gui`
//! programs (docs/development.md, "Error reports"). Nothing is sent and no
//! Sentry client is made unless the environment sets `SENTRY_DSN` when the
//! program starts; builds carry no DSN of their own.
//!
//! With a DSN, a report holds the program's name (the `app` tag), its
//! release (`openmaker@` and the crate version), the environment
//! (`SENTRY_ENVIRONMENT`, else `development` for a debug build and
//! `production` for a release build), the machine's host name, operating
//! system and architecture, and one of:
//!
//! - a panic, with its location and stack trace, and its message only when
//!   the program wrote that message out in full (a `&'static str`): a
//!   message formatted at run time can hold any value the program had, such
//!   as a field's text, and is withheld ([`panic_event`]);
//! - a log record at the error level, with the warning and info records
//!   logged before it on the same thread as breadcrumbs. Warning and error
//!   records of OpenMaker's own crates also go to Sentry's log.
//!
//! Every message is passed through [`scrub`] first. Personal data is not
//! sent (`send_default_pii` is off), and no request, record or file content
//! is attached. Sentry reports take no part in performance tracing: the
//! crates have no `tracing` spans.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Display;
use std::net::{IpAddr, SocketAddr};
use std::panic::PanicHookInfo;
use std::sync::Arc;
use std::time::Duration;

use sentry::integrations::backtrace::AttachStacktraceIntegration;
use sentry::integrations::backtrace::{current_stacktrace, ProcessStacktraceIntegration};
use sentry::integrations::contexts::ContextIntegration;
use sentry::integrations::log::{LogFilter, NoopLogger, SentryLogger};
use sentry::integrations::panic::PanicIntegration;
use sentry::protocol::{Event, Exception, Level, Mechanism};
use sentry::types::Dsn;

/// The environment variable that turns reports on: the Sentry project's
/// DSN.
pub const DSN_VARIABLE: &str = "SENTRY_DSN";

/// The environment variable that names the reports' environment.
pub const ENVIRONMENT_VARIABLE: &str = "SENTRY_ENVIRONMENT";

/// The most detailed level Sentry reads from the log: info and warning
/// records become breadcrumbs, error records events.
pub const LOG_LEVEL: log::LevelFilter = log::LevelFilter::Info;

/// How long a flush before the process ends waits for queued reports
/// (Sentry's own shutdown timeout).
const FLUSH: Duration = Duration::from_secs(2);

/// The Sentry client while reports are on. Dropping it sends what is
/// queued, waiting up to two seconds.
pub struct Reporting {
    _client: sentry::ClientInitGuard,
}

/// Whether the environment asks for reports: `SENTRY_DSN` is set and not
/// empty. [`init`] still turns them on only for a DSN that parses.
pub fn requested() -> bool {
    std::env::var_os(DSN_VARIABLE).is_some_and(|dsn| !dsn.is_empty())
}

/// Turn reports on for the program `app` when `SENTRY_DSN` is set: make the
/// Sentry client and its threads, and catch panics. Call it once, from the
/// main thread and after the program's last change to its environment, and
/// keep what it returns until the program ends. Without a DSN, or with one
/// that does not parse (said on standard error, without the DSN), nothing
/// is made and `None` is returned.
pub fn init(app: &'static str) -> Option<Reporting> {
    init_from(std::env::var_os(DSN_VARIABLE).as_deref(), app)
}

fn init_from(dsn: Option<&OsStr>, app: &'static str) -> Option<Reporting> {
    let dsn = dsn.filter(|dsn| !dsn.is_empty())?;
    let Some(dsn) = dsn.to_str().and_then(|dsn| dsn.trim().parse::<Dsn>().ok()) else {
        eprintln!("{app}: {DSN_VARIABLE} is not a Sentry DSN: no error reports are sent");
        return None;
    };
    let environment = std::env::var(ENVIRONMENT_VARIABLE)
        .ok()
        .filter(|name| !name.is_empty())
        .map_or(
            Cow::Borrowed(if cfg!(debug_assertions) {
                "development"
            } else {
                "production"
            }),
            Cow::Owned,
        );
    let mut options = sentry::ClientOptions::new();
    options.dsn = Some(dsn);
    options.release = Some(Cow::Borrowed(concat!(
        "openmaker@",
        env!("CARGO_PKG_VERSION")
    )));
    options.environment = Some(environment);
    options.send_default_pii = false;
    // The stack of the thread that logged an error, as for a panic.
    options.attach_stacktrace = true;
    options.in_app_include = vec!["om_"];
    // Sentry's default integrations, but for a panic integration that
    // withholds messages formatted at run time (only one panic integration
    // takes effect) and for debug images, which only help with debug files
    // uploaded to Sentry.
    options.default_integrations = false;
    options.integrations = vec![
        Arc::new(AttachStacktraceIntegration),
        Arc::new(ContextIntegration::default()),
        Arc::new(PanicIntegration::new().add_extractor(|info| Some(panic_event(info)))),
        Arc::new(ProcessStacktraceIntegration),
    ];
    options.before_send = Some(Arc::new(move |mut event: Event<'static>| {
        event.tags.insert("app".into(), app.into());
        if let Some(message) = &mut event.message {
            *message = scrub(message);
        }
        if let Some(entry) = &mut event.logentry {
            entry.message = scrub(&entry.message);
            entry.params.clear();
        }
        for exception in event.exception.values.iter_mut() {
            if let Some(value) = &mut exception.value {
                *value = scrub(value);
            }
        }
        Some(event)
    }));
    options.before_breadcrumb = Some(Arc::new(|mut breadcrumb: sentry::Breadcrumb| {
        if let Some(message) = &mut breadcrumb.message {
            *message = scrub(message);
        }
        Some(breadcrumb)
    }));
    options.before_send_log = Some(Arc::new(move |mut entry: sentry::protocol::Log| {
        entry.body = scrub(&entry.body);
        entry.attributes.insert("app".into(), app.into());
        Some(entry)
    }));
    Some(Reporting {
        _client: sentry::init(options),
    })
}

/// `console`, the program's logger, with Sentry's log integration around
/// it. Every record reaches `console` as before, and first: an error's
/// line is written before Sentry takes the thread's stack. Error records
/// become events, warning and info records breadcrumbs, and warning and
/// error records of OpenMaker's own crates (`om_*`) entries of Sentry's
/// log too. Not read: debug and trace records, Sentry's own threads' records
/// and those of `ureq` (Sentry's transport, and Insert from URL's requests,
/// whose URLs may carry a file's data).
pub fn logger<L: log::Log>(console: L) -> impl log::Log {
    ConsoleFirst {
        console,
        sentry: SentryLogger::new().filter(route),
    }
}

/// The program's logger, then Sentry's.
struct ConsoleFirst<L> {
    console: L,
    sentry: SentryLogger<NoopLogger>,
}

impl<L: log::Log> log::Log for ConsoleFirst<L> {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.console.enabled(metadata) || self.sentry.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        self.console.log(record);
        self.sentry.log(record);
    }

    // Sentry's logger holds nothing to flush (and its `NoopLogger`'s
    // `flush` panics).
    fn flush(&self) {
        self.console.flush();
    }
}

fn route(metadata: &log::Metadata<'_>) -> LogFilter {
    let target = metadata.target();
    if target.starts_with("ureq")
        || target.starts_with("sentry")
        || std::thread::current()
            .name()
            .is_some_and(|name| name.starts_with("sentry-"))
    {
        return LogFilter::Ignore;
    }
    let mut filter = match metadata.level() {
        log::Level::Error => LogFilter::Event,
        log::Level::Warn | log::Level::Info => LogFilter::Breadcrumb,
        log::Level::Debug | log::Level::Trace => return LogFilter::Ignore,
    };
    if metadata.level() <= log::Level::Warn && target.starts_with("om_") {
        filter |= LogFilter::Log;
    }
    filter
}

/// Install `console`, whose most detailed level is `level`, as the
/// process's logger: inside Sentry's log integration ([`logger`]) and at
/// [`LOG_LEVEL`] at least when [`requested`], as it is otherwise.
pub fn install_logger<L: log::Log + 'static>(
    console: L,
    level: log::LevelFilter,
) -> Result<(), log::SetLoggerError> {
    if requested() {
        log::set_boxed_logger(Box::new(logger(console)))?;
        log::set_max_level(level.max(LOG_LEVEL));
    } else {
        log::set_boxed_logger(Box::new(console))?;
        log::set_max_level(level);
    }
    Ok(())
}

/// Report the error that ends the program, when reports are on.
pub fn report_failure(error: &dyn Display) {
    sentry::capture_message(&error.to_string(), Level::Fatal);
}

/// Send the queued reports, waiting up to two seconds, when reports are on:
/// before the process ends without returning from `main`.
pub fn flush() {
    if let Some(client) = sentry::Hub::main().client() {
        client.flush(Some(FLUSH));
    }
}

/// A panic's event: Sentry's own, but for its message, which is kept only
/// when the program wrote it out in full (`panic!("text")`,
/// `Option::unwrap`'s, an arithmetic overflow's); a message formatted at
/// run time (`expect`, `Result::unwrap`, an index out of bounds, a string
/// sliced inside a character, `panic!("{value}")`) can hold any value the
/// program had and is withheld. The location is always given.
fn panic_event(info: &PanicHookInfo<'_>) -> Event<'static> {
    let message = info
        .payload()
        .downcast_ref::<&'static str>()
        .copied()
        .unwrap_or("(a message formatted at run time, not sent)");
    let value = match info.location() {
        Some(at) => format!("{message} at {}:{}:{}", at.file(), at.line(), at.column()),
        None => message.to_owned(),
    };
    Event {
        exception: vec![Exception {
            ty: "panic".into(),
            mechanism: Some(Mechanism {
                ty: "panic".into(),
                handled: Some(false),
                ..Default::default()
            }),
            value: Some(value),
            stacktrace: current_stacktrace(),
            ..Default::default()
        }]
        .into(),
        level: Level::Fatal,
        ..Default::default()
    }
}

/// `text`, a message, as it is sent: what may be a file's data or a
/// person's is taken out. Log messages are formatted at run time and their
/// values cannot be told from their wording, so this goes by how values
/// look in them:
///
/// - From the first `{` on, the text is replaced by `{…}`: a structure's
///   `Debug` output, such as an engine operation with the records it
///   commits or an engine's answer.
/// - A quoted string (`Debug` output of a text: field values, account,
///   script, schedule and machine names, a panic's message) becomes `"…"`,
///   but for a hosted file's name, which ends in `.fmp12`.
/// - An IP address, with or without a port, becomes `<address>`.
pub fn scrub(text: &str) -> String {
    let (text, cut) = match text.find('{') {
        Some(at) => (&text[..at], true),
        None => (text, false),
    };
    let mut quoted = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('"') {
        quoted.push_str(&rest[..=open]);
        rest = &rest[open + 1..];
        let mut escaped = false;
        let close = rest.char_indices().find_map(|(at, c)| match c {
            _ if escaped => {
                escaped = false;
                None
            }
            '\\' => {
                escaped = true;
                None
            }
            '"' => Some(at),
            _ => None,
        });
        let Some(close) = close else {
            // An unclosed quote: the rest may be a value cut short.
            quoted.push_str("…\"");
            rest = "";
            break;
        };
        let inside = &rest[..close];
        if inside.to_ascii_lowercase().ends_with(".fmp12") {
            quoted.push_str(inside);
        } else {
            quoted.push('…');
        }
        quoted.push('"');
        rest = &rest[close + 1..];
    }
    quoted.push_str(rest);
    let mut scrubbed = String::with_capacity(quoted.len());
    for piece in quoted.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let start = word.len() - word.trim_start_matches(['(', '<']).len();
        let end = word.trim_end_matches([':', ',', ';', ')', '.', '>']).len();
        let address = word
            .get(start..end.max(start))
            .is_some_and(|w| w.parse::<SocketAddr>().is_ok() || w.parse::<IpAddr>().is_ok());
        if address {
            scrubbed.push_str(&word[..start]);
            scrubbed.push_str("<address>");
            scrubbed.push_str(&piece[end..]);
        } else {
            scrubbed.push_str(piece);
        }
    }
    if cut {
        scrubbed.push_str("{…}");
    }
    scrubbed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No DSN, no client; and the values messages carry are withheld
    /// (the log lines quoted are om-server's own).
    #[test]
    fn reports_need_a_dsn_and_withhold_values() {
        assert!(init_from(None, "test").is_none());
        assert!(init_from(Some(OsStr::new("")), "test").is_none());
        assert!(sentry::Hub::main().client().is_none());
        for (message, sent) in [
            (
                r#""Tasks.fmp12": an operation came without the passwords it takes, or with some it does not: CommitRecords(CommitRecords { account: "Admin", list: [5, 1] })"#,
                r#""Tasks.fmp12": an operation came without the passwords it takes, or with some it does not: CommitRecords(CommitRecords {…}"#,
            ),
            (
                r#"no index key for Text value ["Smith", "Jo \"Jo\" Ann"]; record 7 is left out of the index"#,
                r#"no index key for Text value ["…", "…"]; record 7 is left out of the index"#,
            ),
            (
                r#"192.168.1.20:52114: login to "Contacts.FMP12" as "Admin": accepted"#,
                r#"<address>: login to "Contacts.FMP12" as "…": accepted"#,
            ),
            (
                r#"[fe80::1]:5003: connection ended by a panic in the server: "byte index 2 is not a char boundary; it is inside 'é' of `Zoë`""#,
                r#"<address>: connection ended by a panic in the server: "…""#,
            ),
            (
                "listening on 0.0.0.0:5003 (from 10.0.0.1).",
                "listening on <address> (from <address>).",
            ),
            (
                "Schedule \"Nightly\" aborted; time limit exceeded.",
                "Schedule \"…\" aborted; time limit exceeded.",
            ),
        ] {
            assert_eq!(scrub(message), sent, "{message}");
        }
    }
}
