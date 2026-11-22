//! Insert from URL over HTTP and HTTPS, with the cURL options of Claris's
//! "Supported cURL options" that apply to HTTP: `-X`, `-H`, `-d`,
//! `--data-ascii`, `--data-binary`, `--data-raw`, `--data-urlencode`, `-u`,
//! `-L`, `-I`, `-D`, `-A`, `-e`, `-m`, `--connect-timeout`, `-S`, `-f`.
//! A FileMaker variable stands in for a file (`-d @$data`, `-D $headers`);
//! the runtime reads the named variables before the request and sets the
//! written ones after it. Other options are ignored, as FileMaker ignores
//! unsupported ones.
//!
//! Errors are Claris's codes: 1626 protocol not supported, 1627
//! authentication failed (a 401 without `--show-error`), 1628 SSL, 1629
//! timeout, 1630 URL format, 1631 connection failed (and, with
//! `--show-error`, any status of 400 or more), 1634 certificate
//! verification. Other statuses are not errors; the body is the result.
//! A request whose option values would split it (a method or header with
//! CR or LF, framing headers that disagree with the body) is not sent: 3
//! ([`fetch`]).
//!
//! The response becomes its target's value ([`response`]): text, or, into
//! a container field (or a variable with `--FM-return-container-variable`)
//! container data typed and named as FileMaker Server's engine does
//! (script oracle s34-containers-http, containers.md "Implementation
//! notes"). `file:` URLs are the host's to read ([`file_path`]): what a
//! script may read differs between the desktop and the server.
//!
//! Send Mail's `mailto:` URL ([`mailto`]) and its sending through SMTP
//! ([`send_smtp`]) live here too.
//!
//! Shared by the desktop client and OpenMaker Server.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use om_calc::{Container, Value};

mod smtp;
use om_scripts::runtime::{ErrorCode, UrlRequest, UrlResponse};
pub use smtp::{
    send_smtp, smtp_parts, Account as SmtpAccount, Auth as SmtpAuth, Message as SmtpMessage,
    Security as SmtpSecurity, SmtpError,
};

/// FileMaker's connection timeout (error 1629's text).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// The whole transfer's limit when `-m` gives none.
const DEFAULT_MAX_TIME: Duration = Duration::from_secs(3600);
/// The most bytes a response body may hold after decompression; a larger
/// one fails like a connection error rather than exhausting memory.
const MAX_BODY: u64 = 256 * 1024 * 1024;
/// Bytes read between checks for a user abort.
const CHUNK: usize = 64 * 1024;
/// How often a step waiting for its transfer asks whether it was cancelled.
const POLL: Duration = Duration::from_millis(50);
/// The most transfer helpers that may run at once, abandoned or not.
/// Leaves room for the server's default 100 scripts plus abandoned work.
const MAX_HELPERS: usize = 128;

const PROTOCOL: i32 = 1626;
const AUTHENTICATION: i32 = 1627;
const SSL: i32 = 1628;
const TIMEOUT: i32 = 1629;
const URL_FORMAT: i32 = 1630;
const CONNECTION: i32 = 1631;
const CERTIFICATE: i32 = 1634;
/// Binary data into a text field (s34-containers-http).
const NOT_TEXT: i32 = 507;

/// The parsed options.
#[derive(Debug, Default, PartialEq)]
struct Options {
    method: Option<String>,
    headers: Vec<(String, String)>,
    data: Vec<String>,
    user: Option<String>,
    location: bool,
    head: bool,
    dump_header: Option<String>,
    show_error: bool,
    fail: bool,
    max_time: Option<f64>,
    connect_timeout: Option<f64>,
    /// `--FM-return-container-variable`: a variable target gets container
    /// data.
    container: bool,
}

/// A transfer's outcome, before it becomes a value ([`response`]).
#[derive(Clone, Debug, Default)]
pub struct Fetched {
    /// The URL as requested (after automatic encoding).
    pub url: String,
    /// The body (the headers' text for `-I`).
    pub body: Vec<u8>,
    /// `Content-Disposition`, when the response has one.
    pub disposition: Option<String>,
    /// Variables options write (`-D $headers`).
    pub variables: Vec<(String, Value)>,
    /// `--FM-return-container-variable`.
    pub container_variable: bool,
}

/// What the response becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Into {
    /// A variable: text, or container data with
    /// `--FM-return-container-variable`.
    Variable,
    /// A field that is not a container: the body as UTF-8 text.
    TextField,
    /// A container field.
    Container,
}

/// The value `fetched` becomes for a target `into` (script oracle
/// s34-containers-http, FileMaker Server's engine):
///
/// - container data is named by `Content-Disposition`'s filename when the
///   response has one, else by the URL's last path segment
///   (percent-decoded, without the query; a fragment stays, as FileMaker
///   keeps it), and typed by that segment as Set Field types a decoded
///   file of that name (`om_calc::functions::typed_file`), under a `file`
///   path; `Content-Type` plays no part; an empty response is 1631;
/// - a text field takes the body as UTF-8 text; a body that is not is 507;
/// - a variable takes text (invalid UTF-8 replaced).
pub fn response(fetched: Fetched, into: Into) -> Result<UrlResponse, ErrorCode> {
    let body = match into {
        Into::Container => container(&fetched)?,
        Into::Variable if fetched.container_variable => container(&fetched)?,
        Into::TextField => {
            Value::text(String::from_utf8(fetched.body).map_err(|_| ErrorCode(NOT_TEXT))?)
        }
        Into::Variable => Value::text(String::from_utf8_lossy(&fetched.body).into_owned()),
    };
    Ok(UrlResponse {
        body,
        variables: fetched.variables,
    })
}

fn container(fetched: &Fetched) -> Result<Value, ErrorCode> {
    if fetched.body.is_empty() {
        return Err(ErrorCode(CONNECTION));
    }
    let segment = last_segment(&fetched.url);
    let name = fetched
        .disposition
        .as_deref()
        .and_then(disposition_name)
        .unwrap_or_else(|| segment.clone());
    let data: Container =
        om_calc::functions::typed_file(&name, &segment, fetched.body.clone(), Some("file"));
    Ok(Value::Container(data))
}

/// The URL's last path segment, percent-decoded, without the query.
fn last_segment(url: &str) -> String {
    let before_query = url.split('?').next().unwrap_or("");
    let path = before_query
        .split_once("://")
        .map_or(before_query, |(_, rest)| rest);
    let segment = path.rsplit('/').next().unwrap_or("");
    percent_decode(segment)
}

/// `filename` of a `Content-Disposition` header, quoted or not.
fn disposition_name(header: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("filename") {
            return None;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        Some(value.to_owned()).filter(|v| !v.is_empty())
    })
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            (b'%', Some(&h), Some(&l)) if hex(h).is_some() && hex(l).is_some() => {
                out.push((hex(h).unwrap() * 16 + hex(l).unwrap()) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The local path a `file:` URL names (`file:///Users/a/b.png`, the
/// host part empty or `localhost`), percent-decoded; `None` for another
/// scheme or a remote host.
pub fn file_path(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once(':')?;
    if !scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    let rest = rest.strip_prefix("//")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    Some(percent_decode(path))
}

/// The request's URL as it is sent: automatically encoded unless the step
/// says not to.
pub fn request_url(request: &UrlRequest) -> String {
    if request.encode_url {
        encode_url(request.url.trim())
    } else {
        request.url.trim().to_owned()
    }
}

/// Whether the request's cURL options ask for container data in a variable.
pub fn container_variable(request: &UrlRequest) -> bool {
    parse_options(
        request.curl_options.as_deref().unwrap_or(""),
        &request.variables,
    )
    .container
}

/// The most bytes a `file:` URL's file may hold, as for a response body.
pub const MAX_FILE: u64 = MAX_BODY;

/// Perform an HTTP or HTTPS request. The transfer, from its DNS lookup to
/// its last body byte, runs on a helper thread while the calling thread
/// asks `cancelled` every 50 ms; when it answers true the step ends at once
/// with the user-cancel error (1). The transfer's time limit (`-m`, else an
/// hour) ends no later than `deadline` when one is given (a server script's
/// time limit), and the connect timeout no later than that limit; past it
/// the step is 1629, also while a DNS lookup still runs.
///
/// The client is blocking, so a cancelled transfer is abandoned rather than
/// stopped: its helper ends when its socket answers or fails, when it reads
/// its next body chunk, or at the transfer's time limit, whichever comes
/// first (a DNS lookup only at the system resolver's own timeout). At most
/// 128 transfer helpers run at once, including abandoned ones. A slot is
/// reserved before starting a helper and released when it ends; while
/// all slots are held, a new transfer fails at once with 1631.
pub fn fetch(
    request: &UrlRequest,
    deadline: Option<Instant>,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<Fetched, ErrorCode> {
    let started = Instant::now();
    let options = parse_options(
        request.curl_options.as_deref().unwrap_or(""),
        &request.variables,
    );
    let url = request_url(request);
    match url
        .split_once(':')
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
    {
        Some(scheme) if scheme == "http" || scheme == "https" => {}
        Some(scheme) if !scheme.is_empty() && !scheme.contains('/') => {
            return Err(ErrorCode(PROTOCOL))
        }
        _ => return Err(ErrorCode(URL_FORMAT)),
    }
    let mut limit = seconds(options.max_time).unwrap_or(DEFAULT_MAX_TIME);
    if let Some(deadline) = deadline {
        limit = limit.min(deadline.saturating_duration_since(started));
    }
    if limit.is_zero() {
        return Err(ErrorCode(TIMEOUT));
    }
    let agent = ureq::AgentBuilder::new()
        .redirects(if options.location { 10 } else { 0 })
        .timeout_connect(
            seconds(options.connect_timeout)
                .unwrap_or(DEFAULT_TIMEOUT)
                .min(limit),
        )
        .timeout(limit)
        .build();
    let body = (!options.data.is_empty()).then(|| options.data.join("&"));
    let method = options.method.clone().unwrap_or_else(|| {
        if options.head {
            "HEAD".into()
        } else if body.is_some() {
            "POST".into()
        } else {
            "GET".into()
        }
    });
    check_request(
        &method,
        &options.headers,
        body.as_ref().map_or(0, String::len),
    )?;
    let mut call = agent.request(&method, &url);
    if body.is_some()
        && !options
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
    {
        call = call.set("Content-Type", "application/x-www-form-urlencoded");
    }
    if let Some(user) = &options.user {
        call = call.set(
            "Authorization",
            &format!("Basic {}", base64(user.as_bytes())),
        );
    }
    for (name, value) in &options.headers {
        call = call.set(name, value);
    }
    if cancelled() {
        return Err(ErrorCode::CANCELED);
    }
    let slot = HelperSlot::reserve(&LIVE_HELPERS).ok_or(ErrorCode(CONNECTION))?;
    let helper = Arc::new(Helper(AtomicU8::new(Helper::RUNNING)));
    let (sender, receiver) = mpsc::sync_channel(1);
    let finished = Finished {
        helper: helper.clone(),
        _slot: slot,
    };
    std::thread::Builder::new()
        .name("om-url".into())
        .spawn(move || {
            let result = transfer(call, body, options, url, &finished.helper);
            let _ = sender.send(result);
            drop(finished);
        })
        .map_err(|_| ErrorCode(CONNECTION))?;
    loop {
        match receiver.recv_timeout(POLL) {
            Ok(result) => return result,
            // The helper panicked.
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(ErrorCode(CONNECTION)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancelled() {
                    helper.abandon();
                    return Err(ErrorCode::CANCELED);
                }
                if started.elapsed() >= limit {
                    helper.abandon();
                    return Err(ErrorCode(TIMEOUT));
                }
            }
        }
    }
}

/// Helpers that have reserved capacity and have not finished yet.
static LIVE_HELPERS: AtomicUsize = AtomicUsize::new(0);

/// Reserved before spawn; dropping the closure on spawn failure or the
/// helper's completion guard releases the same slot.
struct HelperSlot<'a>(&'a AtomicUsize);

impl<'a> HelperSlot<'a> {
    fn reserve(counter: &'a AtomicUsize) -> Option<Self> {
        let mut running = counter.load(Ordering::Acquire);
        loop {
            if running >= MAX_HELPERS {
                return None;
            }
            match counter.compare_exchange_weak(
                running,
                running + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self(counter)),
                Err(actual) => running = actual,
            }
        }
    }
}

impl Drop for HelperSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A transfer's helper thread as its caller and the thread share it.
struct Helper(AtomicU8);

impl Helper {
    const RUNNING: u8 = 0;
    const ABANDONED: u8 = 1;
    const FINISHED: u8 = 2;

    /// The caller stops waiting. The helper keeps its already-reserved
    /// capacity until it ends, unless it has finished already.
    fn abandon(&self) {
        let _ = self.0.compare_exchange(
            Self::RUNNING,
            Self::ABANDONED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn abandoned(&self) -> bool {
        self.0.load(Ordering::Acquire) == Self::ABANDONED
    }
}

/// Owned by the helper thread: marks it finished and releases its slot
/// when it ends, including a panic or a failed spawn that drops the closure.
struct Finished<'a> {
    helper: Arc<Helper>,
    _slot: HelperSlot<'a>,
}

impl Drop for Finished<'_> {
    fn drop(&mut self) {
        self.helper.0.store(Helper::FINISHED, Ordering::Release);
    }
}

/// The transfer itself, on its helper thread; it stops between body chunks
/// once its caller has abandoned it.
fn transfer(
    call: ureq::Request,
    body: Option<String>,
    options: Options,
    url: String,
    helper: &Helper,
) -> Result<Fetched, ErrorCode> {
    let result = match &body {
        Some(body) => call.send_string(body),
        None => call.call(),
    };
    let response = match result {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            if status == 401 && !options.show_error {
                return Err(ErrorCode(AUTHENTICATION));
            }
            if (options.show_error || options.fail) && status >= 400 {
                return Err(ErrorCode(CONNECTION));
            }
            response
        }
        Err(ureq::Error::Transport(transport)) => return Err(transport_error(&transport)),
    };
    let mut headers = format!(
        "{} {} {}\r\n",
        response.http_version(),
        response.status(),
        response.status_text()
    );
    for name in response.headers_names() {
        for value in response.all(&name) {
            headers.push_str(&format!("{name}: {value}\r\n"));
        }
    }
    headers.push_str("\r\n");
    let mut variables = Vec::new();
    if let Some(name) = &options.dump_header {
        variables.push((name.clone(), Value::text(headers.clone())));
    }
    let disposition = response.header("Content-Disposition").map(str::to_owned);
    let body = if options.head {
        headers.into_bytes()
    } else {
        let mut reader = std::io::Read::take(response.into_reader(), MAX_BODY + 1);
        let mut bytes = Vec::new();
        let mut chunk = vec![0; CHUNK];
        loop {
            if helper.abandoned() {
                return Err(ErrorCode::CANCELED);
            }
            let n =
                std::io::Read::read(&mut reader, &mut chunk).map_err(|_| ErrorCode(CONNECTION))?;
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..n]);
            if bytes.len() as u64 > MAX_BODY {
                return Err(ErrorCode(CONNECTION));
            }
        }
        bytes
    };
    Ok(Fetched {
        url,
        body,
        disposition,
        variables,
        container_variable: options.container,
    })
}

/// Refuse, before anything is sent, a request whose option values would
/// end it early and write more onto the connection: a method (`-X`) that
/// is not an HTTP token (RFC 9110 9.1 and 5.6.2: no space, CR or LF), a
/// header (`-H`, `-A`, `-e`) whose name is not a token or whose value holds
/// a byte other than a visible ASCII character, space or tab, and framing
/// headers that disagree with the body: a `Content-Length` other than the
/// body's length or given twice, a `Transfer-Encoding` other than one
/// `chunked` (which the client then frames the body with), or both. The URL
/// cannot split the request: the client's URL parser drops CR, LF and tab
/// and percent-encodes spaces and controls, and `-u` is sent in base64.
///
/// cURL sends such values as given (FileMaker's behavior is unmeasured);
/// OpenMaker does not, and fails the step as unsupported (3).
fn check_request(method: &str, headers: &[(String, String)], body: usize) -> Result<(), ErrorCode> {
    let token = |text: &str| !text.is_empty() && text.bytes().all(is_tchar);
    let value = |text: &str| {
        text.bytes()
            .all(|b| matches!(b, b' ' | b'\t' | 0x21..=0x7e))
    };
    let named = |name: &'static str| {
        headers
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
    };
    let mut lengths = named("Content-Length");
    let mut encodings = named("Transfer-Encoding");
    let framed = match (lengths.next(), encodings.next()) {
        (None, None) => true,
        (Some((_, length)), None) => {
            lengths.next().is_none()
                && length.bytes().all(|b| b.is_ascii_digit())
                && length.parse::<usize>() == Ok(body)
        }
        (None, Some((_, encoding))) => encodings.next().is_none() && encoding == "chunked",
        (Some(_), Some(_)) => false,
    };
    if token(method) && framed && headers.iter().all(|(n, v)| token(n) && value(v)) {
        Ok(())
    } else {
        Err(ErrorCode::UNAVAILABLE)
    }
}

/// RFC 9110 5.6.2 `tchar`.
fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// A cURL time in seconds; values that are not a positive, finite duration
/// (`-m -1`, `NaN`, `inf`, `0`) leave the default, as cURL treats 0 as no
/// limit.
fn seconds(value: Option<f64>) -> Option<Duration> {
    value
        .filter(|s| *s > 0.0)
        .and_then(|s| Duration::try_from_secs_f64(s).ok())
}

fn transport_error(error: &ureq::Transport) -> ErrorCode {
    use ureq::ErrorKind;
    let text = error.to_string().to_ascii_lowercase();
    ErrorCode(match error.kind() {
        ErrorKind::InvalidUrl => URL_FORMAT,
        ErrorKind::UnknownScheme => PROTOCOL,
        _ if text.contains("timed out") || text.contains("timeout") => TIMEOUT,
        _ if text.contains("certificate") => CERTIFICATE,
        _ if text.contains("tls") || text.contains("ssl") => SSL,
        _ => CONNECTION,
    })
}

/// Split cURL options as a shell would: whitespace separates, `"..."` and
/// `'...'` quote, a backslash escapes the next character.
fn tokens(options: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = options.chars();
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', q) if q != Some('\'') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    started = true;
                }
            }
            (c, Some(q)) if c == q => quote = None,
            (c, Some(_)) => current.push(c),
            ('"' | '\'', None) => {
                quote = Some(c);
                started = true;
            }
            (c, None) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (c, None) => {
                current.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(current);
    }
    out
}

/// A variable's text, for `@$name` and `$name` values.
fn variable<'a>(variables: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    variables.iter().find(|(n, _)| n == name).map(|(_, v)| v)
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Text(text) => text.as_str().to_owned(),
        other => format!("{other:?}"),
    }
}

/// `@$var` stands for the variable's content; anything else is literal.
fn data_value(value: &str, variables: &[(String, Value)]) -> String {
    match value.strip_prefix('@') {
        Some(name) if name.starts_with('$') => variable(variables, name)
            .map(value_text)
            .unwrap_or_default(),
        _ => value.to_owned(),
    }
}

fn parse_options(options: &str, variables: &[(String, Value)]) -> Options {
    let mut out = Options::default();
    let mut args = tokens(options).into_iter();
    while let Some(arg) = args.next() {
        // `--opt=value` is not cURL syntax; options take the next word.
        let mut value = || args.next().unwrap_or_default();
        match arg.as_str() {
            "-X" | "--request" => out.method = Some(value().to_ascii_uppercase()),
            "-H" | "--header" => {
                let header = value();
                if let Some((name, val)) = header.split_once(':') {
                    out.headers
                        .push((name.trim().to_owned(), val.trim().to_owned()));
                }
            }
            "-d" | "--data" | "--data-ascii" | "--data-binary" => {
                let v = value();
                out.data.push(data_value(&v, variables));
            }
            "--data-raw" => out.data.push(value()),
            "--data-urlencode" => {
                let v = value();
                let encoded = match v.split_once('=') {
                    Some((name, content)) if !name.contains('@') => {
                        if name.is_empty() {
                            percent(content)
                        } else {
                            format!("{name}={}", percent(content))
                        }
                    }
                    _ => match v.split_once('@') {
                        Some((name, var)) => {
                            let content =
                                variable(variables, var).map(value_text).unwrap_or_default();
                            if name.is_empty() {
                                percent(&content)
                            } else {
                                format!("{name}={}", percent(&content))
                            }
                        }
                        None => percent(&v),
                    },
                };
                out.data.push(encoded);
            }
            "-u" | "--user" => out.user = Some(value()),
            "-L" | "--location" | "--location-trusted" => out.location = true,
            "-I" | "--head" => out.head = true,
            "-D" | "--dump-header" => {
                let v = value();
                if v.starts_with('$') {
                    out.dump_header = Some(v);
                }
            }
            "-A" | "--user-agent" => out.headers.push(("User-Agent".into(), value())),
            "-e" | "--referer" => {
                let v = value();
                let v = v.strip_suffix(";auto").unwrap_or(&v).to_owned();
                out.headers.push(("Referer".into(), v));
            }
            "-m" | "--max-time" => out.max_time = value().parse().ok(),
            "--connect-timeout" => out.connect_timeout = value().parse().ok(),
            "-S" | "--show-error" => out.show_error = true,
            "-f" | "--fail" => out.fail = true,
            "--FM-return-container-variable" => out.container = true,
            _ => {}
        }
    }
    out
}

/// Percent-encode everything but unreserved characters (`--data-urlencode`).
fn percent(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The longest `mailto:` URL [`mailto`] builds: within Windows' command
/// line (32,767 characters) and Linux's limit on one argument (128 KiB),
/// with room for the launcher's own arguments.
pub const MAX_MAILTO: usize = 32_000;

/// Why [`mailto`] builds no URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailtoError {
    /// An address whose quoted part does not end: where it stops cannot
    /// be told.
    Address(String),
    /// The URL would be this long, more than [`MAX_MAILTO`].
    TooLong(usize),
}

impl std::fmt::Display for MailtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MailtoError::Address(address) => {
                write!(f, "The address “{address}” has a quote that does not end.")
            }
            MailtoError::TooLong(length) => write!(
                f,
                "The message is too long to hand to the mail client ({length} characters \
                 as a mailto: URL, more than {MAX_MAILTO})."
            ),
        }
    }
}

/// A `mailto:` URL (RFC 6068) for a new message to the addresses of `to`,
/// with `cc`, `bcc`, `subject` and `body`. An address list is split at
/// returns, semicolons and commas outside a quoted local part
/// (`"Sales, West"@example.org` is one address). In each address the
/// local part and the domain are percent-encoded apart, so that only the
/// last `@` stays one (RFC 6068 6.2: `"not@me"@example.org` is
/// `%22not%40me%22@example.org`). Every other part is percent-encoded but
/// for the unreserved characters, and the body's line breaks are written
/// as CRLF (`%0D%0A`), as RFC 6068 asks. The URL is at most
/// [`MAX_MAILTO`] long.
pub fn mailto(
    to: &str,
    cc: &str,
    bcc: &str,
    subject: &str,
    body: &str,
) -> Result<String, MailtoError> {
    // Encoding never shortens text: input longer than the bound is
    // refused before any copy is made of it.
    let raw = [to, cc, bcc, subject, body].iter().map(|s| s.len()).sum();
    if raw > MAX_MAILTO {
        return Err(MailtoError::TooLong(raw));
    }
    let body = body
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    let fields: Vec<String> = [
        ("cc", addresses(cc)?),
        ("bcc", addresses(bcc)?),
        ("subject", percent(subject)),
        ("body", percent(&body)),
    ]
    .into_iter()
    .filter(|(_, value)| !value.is_empty())
    .map(|(name, value)| format!("{name}={value}"))
    .collect();
    let mut url = format!("mailto:{}", addresses(to)?);
    if !fields.is_empty() {
        url.push('?');
        url.push_str(&fields.join("&"));
    }
    if url.len() > MAX_MAILTO {
        return Err(MailtoError::TooLong(url.len()));
    }
    Ok(url)
}

/// An address list as `mailto:` lists it: [`split_addresses`], each
/// address encoded by [`mail_address`], joined by commas.
fn addresses(list: &str) -> Result<String, MailtoError> {
    let split = split_addresses(list).map_err(MailtoError::Address)?;
    Ok(split
        .iter()
        .map(|address| mail_address(address))
        .collect::<Vec<_>>()
        .join(","))
}

/// The addresses of a list as Send Mail takes it: split at returns,
/// semicolons and commas outside double quotes (a backslash escapes inside
/// them), each trimmed, empty ones left out. Err holds the address whose
/// quote does not end.
fn split_addresses(list: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut escaped) = (false, false);
    let push = |address: &str, out: &mut Vec<String>| {
        let address = address.trim();
        if !address.is_empty() {
            out.push(address.to_owned());
        }
    };
    for c in list.chars() {
        if quoted {
            current.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                current.push(c);
            }
            ',' | ';' | '\r' | '\n' => {
                push(&current, &mut out);
                current.clear();
            }
            c => current.push(c),
        }
    }
    if quoted {
        return Err(current.trim().to_owned());
    }
    push(&current, &mut out);
    Ok(out)
}

/// One address: its local part and domain, split at the last `@` (a
/// domain holds none), percent-encoded apart.
fn mail_address(address: &str) -> String {
    match address.rfind('@') {
        Some(at) => format!(
            "{}@{}",
            percent(&address[..at]),
            percent(&address[at + 1..])
        ),
        None => percent(address),
    }
}

/// FileMaker's automatic URL encoding: characters a URL cannot hold
/// (spaces, quotes, non-ASCII) are percent-encoded; reserved characters and
/// existing escapes stay. Insert from URL and web viewers use it.
pub fn encode_url(url: &str) -> String {
    let mut out = String::new();
    for c in url.chars() {
        if c.is_ascii_graphic()
            && !matches!(c, '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '|' | '}')
        {
            out.push(c);
        } else {
            let mut buffer = [0; 4];
            for byte in c.encode_utf8(&mut buffer).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailto_urls_encode_each_part_as_rfc_6068_asks() {
        assert_eq!(
            mailto(
                "pat@example.com; lee@example.com\r",
                "",
                "boss@example.com",
                "Weekly report: 5 & 6",
                "See attached.\rThanks",
            )
            .unwrap(),
            "mailto:pat@example.com,lee@example.com?bcc=boss@example.com\
             &subject=Weekly%20report%3A%205%20%26%206&body=See%20attached.%0D%0AThanks"
        );
        assert_eq!(mailto("", "", "", "", "").unwrap(), "mailto:");
        // A quoted local part keeps its comma and its `@` (RFC 6068 6.2).
        assert_eq!(
            mailto(
                "\"Sales, West\"@example.org, \"not@me\"@example.org",
                "",
                "",
                "",
                ""
            )
            .unwrap(),
            "mailto:%22Sales%2C%20West%22@example.org,%22not%40me%22@example.org"
        );
        assert_eq!(
            mailto("\"open@example.org", "", "", "", ""),
            Err(MailtoError::Address("\"open@example.org".into()))
        );
        // Too long for the system to take as one argument.
        let long = "x".repeat(MAX_MAILTO);
        assert!(matches!(
            mailto("pat@example.com", "", "", "", &long),
            Err(MailtoError::TooLong(_))
        ));
        let spaced = " ".repeat(MAX_MAILTO / 2);
        assert!(matches!(
            mailto("pat@example.com", "", "", "", &spaced),
            Err(MailtoError::TooLong(_))
        ));
    }

    #[test]
    fn helpers_reserve_the_last_slot_before_they_can_be_abandoned() {
        let count = AtomicUsize::new(MAX_HELPERS - 1);
        let helper = Arc::new(Helper(AtomicU8::new(Helper::RUNNING)));
        let finished = Finished {
            helper: helper.clone(),
            _slot: HelperSlot::reserve(&count).unwrap(),
        };
        assert!(HelperSlot::reserve(&count).is_none());
        helper.abandon();
        assert_eq!(count.load(Ordering::Acquire), MAX_HELPERS);
        assert!(HelperSlot::reserve(&count).is_none());
        drop(finished);
        assert_eq!(count.load(Ordering::Acquire), MAX_HELPERS - 1);
        let next = HelperSlot::reserve(&count).unwrap();
        drop(next);
        assert_eq!(count.load(Ordering::Acquire), MAX_HELPERS - 1);
    }

    #[test]
    fn options_parse_with_quotes_and_variables() {
        let vars = vec![("$body".to_owned(), Value::text("{\"a\":1}"))];
        let options = parse_options(
            r#"-X put --header "Content-Type: application/json" -d @$body -D $$h -L --data-urlencode "q=a b""#,
            &vars,
        );
        assert_eq!(options.method.as_deref(), Some("PUT"));
        assert_eq!(
            options.headers,
            [("Content-Type".to_owned(), "application/json".to_owned())]
        );
        assert_eq!(options.data, ["{\"a\":1}", "q=a%20b"]);
        assert_eq!(options.dump_header.as_deref(), Some("$$h"));
        assert!(options.location);
        assert_eq!(tokens(r#"-d "say \"hi\"""#), ["-d", "say \"hi\""]);
    }

    #[test]
    fn urls_encode_and_schemes_are_checked() {
        assert_eq!(
            encode_url("https://example.com/a b?q=é"),
            "https://example.com/a%20b?q=%C3%A9"
        );
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"ab"), "YWI=");
        let request = |url: &str| UrlRequest {
            url: url.into(),
            curl_options: None,
            verify_ssl: false,
            with_dialog: false,
            encode_url: true,
            variables: Vec::new(),
            target: None,
        };
        assert_eq!(
            fetch(&request("ftp://example.com/x"), None, &mut || false).unwrap_err(),
            ErrorCode(PROTOCOL)
        );
        assert_eq!(
            fetch(&request("not a url"), None, &mut || false).unwrap_err(),
            ErrorCode(URL_FORMAT)
        );
    }

    /// s34-containers-http: names, types and errors of responses.
    #[test]
    fn responses_become_their_targets_values() {
        // A 3 x 2 PNG.
        const PNG: &str = "89504e470d0a1a0a0000000d49484452000000030000000208020000001216f14d\
                           0000001049444154789c63f8cfc000410c70160041d205fb87f0b94800000000\
                           49454e44ae426082";
        let hex: String = PNG.split_whitespace().collect();
        let png: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let fetched = |url: &str, disposition: Option<&str>, body: &[u8]| Fetched {
            url: url.into(),
            body: body.to_vec(),
            disposition: disposition.map(str::to_owned),
            ..Fetched::default()
        };
        let data = |f: Fetched| match response(f, Into::Container).unwrap().body {
            Value::Container(c) => c,
            other => panic!("{other:?}"),
        };
        let red = data(fetched(
            "http://h/image+png/-/a%20b.png?x=1&y=2",
            None,
            &png,
        ));
        assert_eq!(red.text.as_str(), "a b.png");
        assert_eq!(&red.streams[0].0 .0, b"PNGf");
        assert_eq!(red.path_type, Some("file"));
        // Content-Disposition names; the URL's segment types.
        let named = data(fetched(
            "http://h/red.png",
            Some("attachment; filename=\"x.gif\""),
            &png,
        ));
        assert_eq!(named.text.as_str(), "x.gif");
        assert_eq!(&named.streams[0].0 .0, b"PNGf");
        let gif = data(fetched("http://h/x.gif", None, &png));
        assert_eq!(&gif.streams[0].0 .0, b"GIFf");
        assert_eq!(
            data(fetched("http://h/red.png#frag", None, &png))
                .text
                .as_str(),
            "red.png#frag"
        );
        assert_eq!(
            &data(fetched("http://h/red.txt", None, &png)).streams[0]
                .0
                 .0,
            b"FILE"
        );
        assert_eq!(
            response(fetched("http://h/", None, b""), Into::Container).unwrap_err(),
            ErrorCode(CONNECTION)
        );
        // Text fields take UTF-8 text only.
        let text = response(
            fetched("http://h/t", None, "héllo".as_bytes()),
            Into::TextField,
        );
        assert_eq!(
            text.unwrap()
                .body
                .to_text(&om_calc::Ctx::new(
                    &om_calc::Locale::en_us(),
                    om_calc::Date::from_ymd(2026, 10, 30).unwrap()
                ))
                .as_str(),
            "héllo"
        );
        assert_eq!(
            response(fetched("http://h/t", None, &png), Into::TextField).unwrap_err(),
            ErrorCode(NOT_TEXT)
        );
        // file: URLs.
        assert_eq!(
            file_path("file:///Users/a%20b/c.png").as_deref(),
            Some("/Users/a b/c.png")
        );
        assert_eq!(file_path("file://localhost/x").as_deref(), Some("/x"));
        assert_eq!(file_path("file://server/x"), None);
        assert_eq!(file_path("http://h/x"), None);
    }

    /// #298: a cancel ends the step at once while a server that accepted
    /// the request never answers, or sends its headers slowly; the
    /// abandoned transfer hangs up by the caller's deadline, which `-m`
    /// cannot extend.
    #[test]
    fn a_cancel_ends_a_stalled_transfer_at_once() {
        use std::io::{Read, Write};
        const LIMIT: Duration = Duration::from_secs(3);
        let deadline = Instant::now() + LIMIT;
        let mut hung_up = Vec::new();
        for slow_headers in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/a", listener.local_addr().unwrap());
            let (reached, has_request) = mpsc::channel();
            let (closed, hangs_up) = mpsc::channel();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut head = Vec::new();
                let mut buffer = [0; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buffer).unwrap();
                    head.extend_from_slice(&buffer[..n]);
                }
                if slow_headers {
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Le").unwrap();
                }
                reached.send(()).unwrap();
                while stream.read(&mut buffer).is_ok_and(|n| n > 0) {}
                closed.send(()).unwrap();
            });
            let request = UrlRequest {
                url,
                curl_options: Some("-m 1e9".into()),
                verify_ssl: true,
                with_dialog: false,
                encode_url: true,
                variables: Vec::new(),
                target: None,
            };
            let result = fetch(&request, Some(deadline), &mut || {
                has_request.try_recv().is_ok()
            });
            assert_eq!(result.unwrap_err(), ErrorCode::CANCELED, "{slow_headers}");
            hung_up.push(hangs_up);
        }
        for hangs_up in hung_up {
            hangs_up
                .recv_timeout(LIMIT + Duration::from_secs(30))
                .expect("the abandoned transfer outlived its deadline");
        }
    }

    /// #300: option values cannot write a second request onto the
    /// connection: a refused request connects to nothing, an accepted one
    /// arrives as exactly one well-formed request.
    #[test]
    fn option_values_cannot_split_the_request() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        // Answers each request and hands back what each connection carried;
        // an empty connection ends it.
        let server = std::thread::spawn(move || {
            let mut connections = Vec::new();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .unwrap();
                let mut wire = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    if n == 0 {
                        break;
                    }
                    let answered = wire.windows(4).any(|w| w == b"\r\n\r\n");
                    wire.extend_from_slice(&buffer[..n]);
                    if !answered && wire.windows(4).any(|w| w == b"\r\n\r\n") {
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                            .unwrap();
                    }
                }
                if wire.is_empty() {
                    break;
                }
                connections.push(String::from_utf8(wire).unwrap());
            }
            connections
        });
        let request = |url: String, options: &str| UrlRequest {
            url,
            curl_options: Some(options.into()),
            verify_ssl: true,
            with_dialog: false,
            encode_url: false,
            variables: Vec::new(),
            target: None,
        };
        let url = format!("http://{address}/a");
        for options in [
            "-X \"GET /a HTTP/1.1\r\nX-Injected: yes\r\n\r\nDELETE\"",
            "-H \"Content-Length: 0\" -d \"GET /b HTTP/1.1\r\nHost: x\r\n\r\n\"",
            "-H \"Transfer-Encoding: Chunked\" -d \"0\r\n\r\nGET /b HTTP/1.1\r\n\r\n\"",
        ] {
            let refused = fetch(&request(url.clone(), options), None, &mut || false);
            assert_eq!(refused.unwrap_err(), ErrorCode::UNAVAILABLE, "{options:?}");
        }
        // CR and LF in an unencoded URL and in `-u` stay inside the request.
        let options = "-X propfind -u \"u:p\r\nX-Injected: yes\"";
        let injected = format!("{url}\r\nX-Injected: yes\r\n\r\nDELETE /b");
        let fetched = fetch(&request(injected, options), None, &mut || false).unwrap();
        assert_eq!(fetched.body, b"ok");
        drop(std::net::TcpStream::connect(address).unwrap());
        let connections = server.join().unwrap();
        let [wire] = &connections[..] else {
            panic!("{connections:?}")
        };
        let (head, rest) = wire.split_once("\r\n\r\n").unwrap();
        assert_eq!(rest, "", "{wire:?}");
        let mut lines = head.split("\r\n");
        assert_eq!(
            lines.next(),
            Some("PROPFIND /aX-Injected:%20yesDELETE%20/b HTTP/1.1")
        );
        for line in lines {
            let (name, _) = line.split_once(": ").unwrap();
            assert!(name.bytes().all(is_tchar), "{line:?}");
            assert!(!name.eq_ignore_ascii_case("x-injected"), "{wire:?}");
        }
    }

    #[test]
    fn bad_timeouts_fall_back_instead_of_panicking() {
        for bad in [-1.0, 0.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert_eq!(seconds(Some(bad)), None, "{bad}");
        }
        assert_eq!(seconds(Some(1.5)), Some(Duration::from_millis(1500)));
        let options = parse_options("-m -1 --connect-timeout inf", &[]);
        assert_eq!(seconds(options.max_time), None);
        assert_eq!(seconds(options.connect_timeout), None);
    }
}
