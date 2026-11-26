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
//!   logged before it on the same thread as breadcrumbs.
//!
//! Sentry's own log (its `logs` feature) is not used: its batching thread
//! can lose its last batch at the program's end unless the client is
//! closed, and closing joins Sentry's transport thread, waiting for a
//! report in flight and panicking if that thread had panicked; its thread
//! also aborts the program's start when it cannot be made.
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
use sentry::protocol::{Event, Exception, Level, Mechanism};
use sentry::types::{Dsn, Scheme};

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

/// The longest one request to Sentry takes, its name lookup and connection
/// included (OpenMaker's choice). Sentry's transport sends one report at a
/// time, and a flush waits for room in its queue, so this bounds how long
/// an unanswering endpoint can hold up the program's end: this and
/// [`FLUSH`].
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The Sentry client while reports are on. Dropping it sends what is
/// queued, waiting up to two seconds, and leaves the client open: closing
/// it would join Sentry's transport thread, waiting for the report being
/// sent, and panic if that thread had panicked.
pub struct Reporting {
    client: Arc<sentry::Client>,
}

impl Drop for Reporting {
    fn drop(&mut self) {
        self.client.flush(Some(FLUSH));
    }
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
    options.transport = Some(Arc::new(Transport));
    // The stack of the thread that logged an error, as for a panic.
    options.attach_stacktrace = true;
    options.in_app_include = vec!["om_"];
    // Sentry's default integrations, but for its panic integration (panics
    // are reported by `report_panics`) and for debug images, which only
    // help with debug files uploaded to Sentry.
    options.default_integrations = false;
    options.integrations = vec![
        Arc::new(AttachStacktraceIntegration),
        Arc::new(ContextIntegration::default()),
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
    // As `sentry::init` does, but for its guard, which closes the client.
    let client = Arc::new(sentry::Client::with_options(sentry::apply_defaults(
        options,
    )));
    sentry::Hub::current().bind_client(Some(client.clone()));
    report_panics();
    Some(Reporting { client })
}

/// Report each panic, then send the report, before the panic hook that was
/// there (std's, which prints it) runs: an abort or the end of `main` may
/// follow. Not a panic on one of Sentry's own threads: there the report's
/// flush would wait for that very thread to take it from a full queue.
fn report_panics() {
    let next = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !on_sentry_thread() {
            sentry::capture_event(panic_event(info));
            flush();
        }
        next(info);
    }));
}

/// Whether this is one of Sentry's own threads (`sentry-transport`).
fn on_sentry_thread() -> bool {
    std::thread::current()
        .name()
        .is_some_and(|name| name.starts_with("sentry-"))
}

/// Sentry's ureq transport, with an agent whose requests end at
/// [`REQUEST_TIMEOUT`]: ureq sets no deadline of its own. Its TLS is
/// rustls's with ring and the webpki roots, as the SDK's own agent's, and
/// its proxy the one that agent takes: `HTTPS_PROXY` (or `https_proxy`)
/// for an `https` DSN, else `HTTP_PROXY` (or `http_proxy`), as
/// `sentry::init` read them; not ureq's own choice, which would put
/// `ALL_PROXY` first and follow `NO_PROXY`.
struct Transport;

impl sentry::TransportFactory for Transport {
    fn create_transport_with_options(
        &self,
        options: sentry::TransportOptions,
    ) -> Arc<dyn sentry::Transport> {
        let proxy = match (
            options.dsn.scheme(),
            &options.http_proxy,
            &options.https_proxy,
        ) {
            (Scheme::Https, _, Some(proxy)) | (_, Some(proxy), _) => ureq::Proxy::new(proxy).ok(),
            _ => None,
        };
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .proxy(proxy)
            .build()
            .new_agent();
        Arc::new(
            sentry::transports::UreqHttpTransportOptions::from(options)
                .with_agent(agent)
                .build(),
        )
    }
}

/// `console`, the program's logger, with Sentry's log integration around
/// it. Every record reaches `console` as before, and first: an error's
/// line is written before Sentry takes the thread's stack. Error records
/// become events, warning and info records breadcrumbs. Not read: debug
/// and trace records, Sentry's own threads' records
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
    if target.starts_with("ureq") || target.starts_with("sentry") || on_sentry_thread() {
        return LogFilter::Ignore;
    }
    match metadata.level() {
        log::Level::Error => LogFilter::Event,
        log::Level::Warn | log::Level::Info => LogFilter::Breadcrumb,
        log::Level::Debug | log::Level::Trace => LogFilter::Ignore,
    }
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

/// A panic's event, as Sentry's panic integration makes it, but for its
/// message, which is kept only
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
/// - A quoted string (`Debug` output of a text: field values, file,
///   account, script, schedule and machine names, a caught panic's
///   message) becomes `"…"`.
/// - An IP address, with or without a port, also inside a URL, becomes
///   `<address>`.
/// - A bracketed list (`Debug` output of bytes or of a list: key paths,
///   index keys, which are made from field values, lists of values)
///   becomes `[…]`.
/// - A key path in om-hbam's form (`3/5/x6b6579…`: decimal segments and
///   `x` and hex ones), and a long `x` and hex segment alone, become `…`:
///   an index key in one is made from a field's value.
pub fn scrub(text: &str) -> String {
    let (text, cut) = match text.find('{') {
        Some(at) => (&text[..at], true),
        None => (text, false),
    };
    let mut scrubbed = without_key_paths(&without_lists(&without_addresses(&without_quoted(text))));
    if cut {
        scrubbed.push_str("{…}");
    }
    scrubbed
}

/// `text` with each double-quoted string's content made `…`; an unclosed
/// quote's rest, which may be a value cut short, too.
fn without_quoted(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut quoted = false;
    let mut escaped = false;
    for c in text.chars() {
        if !quoted {
            kept.push(c);
            quoted = c == '"';
            continue;
        }
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => {
                kept.push_str("…\"");
                quoted = false;
            }
            _ => {}
        }
    }
    if quoted {
        kept.push_str("…\"");
    }
    kept
}

/// `text` with each IP address, or socket address, made `<address>`: a
/// run of hexadecimal digits, colons, dots and square brackets that is not
/// part of a word (so not `a::b` in a Rust path) and parses as one, less
/// any colons, dots and brackets that end it.
fn without_addresses(text: &str) -> String {
    let part = |c: char| c.is_ascii_hexdigit() || matches!(c, ':' | '.' | '[' | ']');
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut kept = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    while let Some(offset) = text[at..].find(part) {
        let start = at + offset;
        let end = text[start..]
            .find(|c: char| !part(c))
            .map_or(text.len(), |length| start + length);
        at = end;
        if word(text[..start].chars().next_back()) || word(text[end..].chars().next()) {
            continue;
        }
        let mut run = &text[start..end];
        while !run.is_empty() && !is_address(run) {
            run = run.strip_suffix([':', '.', '[', ']']).unwrap_or_default();
        }
        if !run.is_empty() {
            kept.push_str(&text[copied..start]);
            kept.push_str("<address>");
            copied = start + run.len();
        }
    }
    kept.push_str(&text[copied..]);
    kept
}

/// `text` with each run of ASCII letters, digits and slashes that is a key
/// path, or one long `x` and hex segment, made `…` (see [`scrub`]). A file's
/// path is not one: it has a name among its segments, or begins with `/`.
fn without_key_paths(text: &str) -> String {
    let part = |c: char| c.is_ascii_alphanumeric() || c == '/';
    // om-hbam's `KeySegment` form: decimal for 1 to 4 bytes, else `x` and
    // hex, a bare `x` for none.
    let hex = |segment: &str| {
        segment
            .strip_prefix('x')
            .is_some_and(|hex| hex.chars().all(|c| c.is_ascii_hexdigit()))
    };
    let mut kept = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    while let Some(offset) = text[at..].find(part) {
        let start = at + offset;
        let end = text[start..]
            .find(|c: char| !part(c))
            .map_or(text.len(), |length| start + length);
        at = end;
        let run = &text[start..end];
        let segments = run.split('/');
        let key = segments
            .clone()
            .all(|s| !s.is_empty() && (s.chars().all(|c| c.is_ascii_digit()) || hex(s)))
            && (segments.count() > 1 || (hex(run) && run.len() > 8));
        if key {
            kept.push_str(&text[copied..start]);
            kept.push('…');
            copied = end;
        }
    }
    kept.push_str(&text[copied..]);
    kept
}

fn is_address(run: &str) -> bool {
    run.chars().any(|c| c.is_ascii_hexdigit())
        && (run.parse::<SocketAddr>().is_ok()
            || run.parse::<IpAddr>().is_ok()
            || run
                .strip_prefix('[')
                .and_then(|run| run.strip_suffix(']'))
                .is_some_and(|run| run.parse::<IpAddr>().is_ok()))
}

/// `text` with each bracketed list, nested ones within it, made `[…]`.
fn without_lists(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '[' => {
                if depth == 0 {
                    kept.push_str("[…");
                }
                depth += 1;
            }
            ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    kept.push(']');
                }
            }
            _ if depth > 0 => {}
            _ => kept.push(c),
        }
    }
    kept
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
                r#""…": an operation came without the passwords it takes, or with some it does not: CommitRecords(CommitRecords {…}"#,
            ),
            (
                r#"no index key for Date value ["Patient 42.fmp12", "Jo \"Jo\" Ann"]; record 7 is left out of the index"#,
                "no index key for Date value […]; record 7 is left out of the index",
            ),
            (
                "Tasks.fmp12: index posting [[05], [01], [0b], [01], [6b, 65, 79]] has a RID encoding this server cannot edit losslessly",
                "Tasks.fmp12: index posting […] has a RID encoding this server cannot edit losslessly",
            ),
            (
                r#"192.168.1.20:52114: login to "Contacts.fmp12" as "Admin": accepted"#,
                r#"<address>: login to "…" as "…": accepted"#,
            ),
            (
                r#"[fe80::1]:5003: connection ended by a panic in the server: "byte index 2 is not a char boundary; it is inside 'é' of `Zoë`""#,
                r#"<address>: connection ended by a panic in the server: "…""#,
            ),
            (
                "Admin Console listening on https://192.0.2.10:16000/admin-console (from 10.0.0.1).",
                "Admin Console listening on https://<address>/admin-console (from <address>).",
            ),
            (
                "om_server::server: bound to 2001:db8:: at 21:04:32Z",
                "om_server::server: bound to <address> at 21:04:32Z",
            ),
            (
                "Tasks.fmp12: key out of order or duplicated at 3/5/x6b65792061626364/x; x6b65792061626364 alone, a path /srv/db/1/2, 1/2",
                "Tasks.fmp12: key out of order or duplicated at …; … alone, a path /srv/db/1/2, …",
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
