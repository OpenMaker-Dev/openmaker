//! Send Mail (63) through SMTP: the message a step describes, sent with
//! the step's SMTP options (script-steps.md 12.3) to the server they name.
//!
//! The session is RFC 5321's (EHLO, MAIL, RCPT, DATA), on a connection
//! that is plain, TLS from the start (RFC 8314's implicit TLS, FileMaker's
//! "SSL") or upgraded with STARTTLS (RFC 3207, FileMaker's "TLS"). It
//! authenticates (RFC 4954) with PLAIN (RFC 4616) or else LOGIN for
//! FileMaker's "Plain Password", and with CRAM-MD5 (RFC 2195). The message
//! is plain text in UTF-8 (RFC 5322; MIME, RFC 2045 and 2047).
//!
//! What FileMaker sends and which of its codes it answers for which
//! failure are unmeasured. The codes are matched by their names in
//! FileMaker's error list (1501 to 1507, docs/re/catalogs/error-codes.csv),
//! and the message's form is OpenMaker's.
//!
//! The server is hostile input: replies are parsed strictly and bounded
//! in length and lines, each reply, write and handshake has a time budget,
//! STARTTLS refuses bytes the server sent ahead of the handshake, and the
//! server's text never carries what the client sent to log in.

use std::cell::Cell;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use om_scripts::runtime::{ErrorCode, Mail};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use super::{base64, split_addresses, HelperSlot, LIVE_HELPERS, POLL};

/// How long connecting, one reply, one write or the TLS handshake may
/// take.
const IO_TIMEOUT: Duration = Duration::from_secs(60);

/// How long QUIT may take once the message is taken.
const QUIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the server may take to answer the message's end: RFC 5321
/// 4.5.3.2.6's ten minutes, as a shorter wait could report a message the
/// server takes as unsent, and a retry send it twice.
const DATA_END_TIMEOUT: Duration = Duration::from_secs(600);

/// The longest reply line read (RFC 5321 4.5.3.1.5 allows 512 octets;
/// servers write longer ones).
const MAX_LINE: usize = 4096;

/// The most lines one reply may have.
const MAX_LINES: usize = 200;

/// The longest command line (RFC 5321 4.5.3.1.4), CRLF included.
const MAX_COMMAND: usize = 512;

/// The longest chunk of text one RFC 2047 encoded word carries: 39 bytes
/// are 52 base64 characters, a 64-character word, which fits a 76-character
/// line after the longest field name used ("Reply-To: ").
const WORD_BYTES: usize = 39;

/// The connection's security: the step's Encryption (`[00 5, 03]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    /// 0: none.
    None,
    /// 1, "SSL": TLS from the start.
    Ssl,
    /// 2, "TLS": STARTTLS after the greeting.
    StartTls,
}

/// The step's Authentication (`[00 6, 03]`) with its user name and
/// password.
#[derive(Clone, PartialEq, Eq)]
pub enum Auth {
    /// 1: none.
    None,
    /// 2, "Plain Password": AUTH PLAIN, or LOGIN when the server offers
    /// only that.
    Password { user: String, password: String },
    /// 3: AUTH CRAM-MD5.
    CramMd5 { user: String, password: String },
}

/// The password is left out.
impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => f.write_str("None"),
            Self::Password { user, .. } => f
                .debug_struct("Password")
                .field("user", user)
                .finish_non_exhaustive(),
            Self::CramMd5 { user, .. } => f
                .debug_struct("CramMd5")
                .field("user", user)
                .finish_non_exhaustive(),
        }
    }
}

/// The server a message goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub server: String,
    pub port: u16,
    pub security: Security,
    pub auth: Auth,
}

/// One message, as the step's calculations give it. The address fields
/// are lists as FileMaker takes them (`split_addresses`), each address
/// plain or as `Name <address>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Message {
    /// The sender's name (slot 5).
    pub from_name: String,
    /// The sender's address (slot 6).
    pub from: String,
    /// Reply-To (slot 7).
    pub reply_to: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
}

/// Why a message was not sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmtpError {
    /// An address or sender OpenMaker cannot send with, found before
    /// connecting: 3.
    Address(String),
    /// A stored Encryption or Authentication OpenMaker does not know: 3.
    Options(String),
    /// No connection, or the server refused it: 1502.
    Connect(String),
    /// TLS failed, or the server offers no STARTTLS: 1503.
    Tls(String),
    /// TLS could not verify the server's certificate. Claris documents
    /// that FileMaker Pro then offers to connect anyway, that with error
    /// capture on the step "behaves as if the server were unavailable",
    /// and that in a server-side script it "behaves as if it had been
    /// canceled by the user". OpenMaker offers no choice: 1502 here, as
    /// with error capture on [I: the code]; a server host answers 1.
    Certificate(String),
    /// The server wants an encrypted connection first: 1504.
    NeedsEncryption(String),
    /// The server offers none of the mechanisms the step's authentication
    /// can use, or finds it too weak: 1505.
    AuthUnsupported(String),
    /// The server refused the user name or password: 1501.
    AuthFailed(String),
    /// Logging in failed otherwise, or the server wants a login the step
    /// does not make: 1507.
    Login(String),
    /// The server refused the sender, a recipient or the message, did not
    /// answer in time, or ended the session: 1506.
    Rejected(String),
    /// The message's end was sent and the server's answer never came:
    /// the message may have been sent. 1506 too, the one code FileMaker
    /// has for it.
    Unconfirmed(String),
    /// The user stopped the step: 1.
    Cancelled,
}

impl SmtpError {
    /// The step's error code.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Address(_) | Self::Options(_) => ErrorCode::UNAVAILABLE,
            Self::AuthFailed(_) => ErrorCode(1501),
            Self::Connect(_) | Self::Certificate(_) => ErrorCode(1502),
            Self::Tls(_) => ErrorCode(1503),
            Self::NeedsEncryption(_) => ErrorCode(1504),
            Self::AuthUnsupported(_) => ErrorCode(1505),
            Self::Rejected(_) | Self::Unconfirmed(_) => ErrorCode(1506),
            Self::Login(_) => ErrorCode(1507),
            Self::Cancelled => ErrorCode::CANCELED,
        }
    }
}

impl std::fmt::Display for SmtpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Address(detail) => write!(f, "The message cannot be sent: {detail}"),
            Self::Options(detail) => write!(f, "The step's SMTP options cannot be used: {detail}"),
            Self::Connect(detail) => write!(f, "Could not connect to the SMTP server: {detail}"),
            Self::Tls(detail) => write!(f, "The SMTP server's TLS failed: {detail}"),
            Self::Certificate(detail) => write!(
                f,
                "The SMTP server's certificate could not be verified: {detail}"
            ),
            Self::NeedsEncryption(detail) => write!(
                f,
                "The SMTP server requires an encrypted connection: {detail}"
            ),
            Self::AuthUnsupported(detail) => write!(
                f,
                "The SMTP server does not support the step's authentication: {detail}"
            ),
            Self::AuthFailed(detail) => write!(
                f,
                "The SMTP server refused the user name or password: {detail}"
            ),
            Self::Login(detail) => write!(f, "Could not log in to the SMTP server: {detail}"),
            Self::Rejected(detail) => {
                write!(f, "The SMTP server did not take the message: {detail}")
            }
            Self::Unconfirmed(detail) => write!(
                f,
                "The SMTP server did not confirm the message, which may have been sent: {detail}"
            ),
            Self::Cancelled => write!(f, "Sending was cancelled."),
        }
    }
}

/// The account and message a Send Mail step by SMTP describes. An empty
/// port is 25, the SMTP Options dialog's documented default (that an empty
/// calculated port means it is OpenMaker's reading); one that is no port
/// number cannot connect (1502).
pub fn smtp_parts(mail: &Mail) -> Result<(Account, Message), SmtpError> {
    let options = mail
        .smtp
        .as_ref()
        .ok_or_else(|| SmtpError::Options("the step does not send by SMTP".into()))?;
    let security = match options.encryption {
        0 => Security::None,
        1 => Security::Ssl,
        2 => Security::StartTls,
        n => return Err(SmtpError::Options(format!("encryption {n}"))),
    };
    let (user, password) = (options.user.clone(), options.password.clone());
    let auth = match options.authentication {
        1 => Auth::None,
        2 => Auth::Password { user, password },
        3 => Auth::CramMd5 { user, password },
        n => return Err(SmtpError::Options(format!("authentication {n}"))),
    };
    let port = options.port.trim();
    let port = if port.is_empty() {
        25
    } else {
        port.parse::<u16>()
            .ok()
            .filter(|&p| p != 0)
            .ok_or_else(|| SmtpError::Connect(format!("\u{201c}{port}\u{201d} is not a port")))?
    };
    Ok((
        Account {
            server: options.server.trim().to_owned(),
            port,
            security,
            auth,
        },
        Message {
            from_name: options.name.clone(),
            from: options.email.clone(),
            reply_to: options.reply_to.clone(),
            to: mail.to.clone(),
            cc: mail.cc.clone(),
            bcc: mail.bcc.clone(),
            subject: mail.subject.clone(),
            body: mail.message.clone(),
        },
    ))
}

/// Send `message` through `account`'s server, by `deadline` if given;
/// `cancelled` is asked while the session runs. The message is composed,
/// and its addresses checked, before connecting.
///
/// A cancel or the deadline stops the session where it is, its socket
/// shut, unless the message's final `.` is already sent: from then on the
/// server's answer to it is the result, within [`DATA_END_TIMEOUT`], so a
/// message the server took is not reported as not sent, and one whose
/// answer never comes is [`SmtpError::Unconfirmed`].
pub fn send_smtp(
    account: &Account,
    message: &Message,
    deadline: Option<Instant>,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<(), SmtpError> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    send_with(account, message, roots, deadline, cancelled)
}

/// [`send_smtp`] trusting `roots`.
fn send_with(
    account: &Account,
    message: &Message,
    roots: RootCertStore,
    deadline: Option<Instant>,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<(), SmtpError> {
    let mail = compose(message, SystemTime::now())?;
    let tls = Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| SmtpError::Tls(e.to_string()))?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    if cancelled() {
        return Err(SmtpError::Cancelled);
    }
    let slot = HelperSlot::reserve(&LIVE_HELPERS)
        .ok_or_else(|| SmtpError::Connect("too many transfers are running".into()))?;
    let control = Arc::new(Control::default());
    let (sender, receiver) = mpsc::sync_channel(1);
    let account = account.clone();
    let shared = control.clone();
    std::thread::Builder::new()
        .name("om-url-smtp".into())
        .spawn(move || {
            let _slot = slot;
            match deliver(&account, &mail, tls, deadline, &shared) {
                Ok(mut session) => {
                    // The message is taken: the caller hears so now, not
                    // after QUIT.
                    let _ = sender.send(Ok(()));
                    session.deadline = Some(Instant::now() + QUIT_TIMEOUT);
                    let _ = session.command("QUIT");
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                }
            }
        })
        .map_err(|e| SmtpError::Connect(e.to_string()))?;
    loop {
        match receiver.recv_timeout(POLL) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(SmtpError::Rejected("the session ended unexpectedly".into()))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let stop = if cancelled() {
                    Some(SmtpError::Cancelled)
                } else if deadline.is_some_and(|d| Instant::now() >= d) {
                    Some(SmtpError::Rejected("the script's time limit passed".into()))
                } else {
                    None
                };
                // Past the message's end the session's answer stands; it
                // comes within its own budget.
                if let Some(error) = stop {
                    if control.cancel() {
                        return Err(error);
                    }
                }
            }
        }
    }
}

/// What the caller and a session share: whether it may still be stopped,
/// and its socket, to shut when it is.
#[derive(Default)]
struct Control {
    state: AtomicU8,
    socket: Mutex<Option<TcpStream>>,
}

impl Control {
    const RUNNING: u8 = 0;
    /// The end of the message is being sent.
    const COMMITTED: u8 = 1;
    const CANCELLED: u8 = 2;

    /// Stop the session unless its message's end is sent: whether it was
    /// stopped. A read or write in flight ends with the socket shut.
    fn cancel(&self) -> bool {
        let stopped = self
            .state
            .compare_exchange(
                Self::RUNNING,
                Self::CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
            || self.state.load(Ordering::Acquire) == Self::CANCELLED;
        if stopped {
            if let Ok(socket) = self.socket.lock() {
                if let Some(socket) = socket.as_ref() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            }
        }
        stopped
    }

    fn cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::CANCELLED
    }

    /// The session is about to send the message's end: false when it was
    /// stopped first.
    fn commit(&self) -> bool {
        self.state
            .compare_exchange(
                Self::RUNNING,
                Self::COMMITTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// The session's socket, shut at once if it was stopped already. A
    /// socket a cancel could not shut is refused: the session would go on
    /// after the step returned.
    fn adopt(&self, socket: &TcpStream) -> Result<(), SmtpError> {
        let watched = socket.try_clone().ok().and_then(|clone| {
            let mut held = self.socket.lock().ok()?;
            *held = Some(clone);
            Some(())
        });
        if watched.is_none() {
            let _ = socket.shutdown(Shutdown::Both);
            return Err(SmtpError::Connect(
                "the connection could not be watched for a cancel".into(),
            ));
        }
        if self.cancelled() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        Ok(())
    }
}

/// A message ready for the envelope and DATA.
#[derive(Debug)]
struct Composed {
    from: String,
    recipients: Vec<String>,
    /// The message, lines ending CRLF, not yet dot-stuffed.
    data: Vec<u8>,
}

/// One address of a list: its display name and its address.
fn mailbox(item: &str) -> Result<(Option<String>, String), SmtpError> {
    let item = item.trim();
    let (name, address) = match (item.strip_suffix('>'), angle_start(item)) {
        (Some(inner), Some(lt)) => {
            let name = item[..lt].trim();
            (
                (!name.is_empty()).then(|| unquote(name)),
                inner[lt + 1..].trim().to_owned(),
            )
        }
        _ => (None, item.to_owned()),
    };
    check_address(&address)?;
    Ok((name, address))
}

/// Where a trailing `<address>` starts: the last `<` outside quotes.
fn angle_start(item: &str) -> Option<usize> {
    let (mut quoted, mut escaped, mut at) = (false, false, None);
    for (i, c) in item.char_indices() {
        if quoted {
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
            '"' => quoted = true,
            '<' => at = Some(i),
            _ => {}
        }
    }
    at
}

/// A display name as text: the quotes and escapes of a quoted one gone.
fn unquote(name: &str) -> String {
    match name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        Some(inner) => {
            let mut out = String::new();
            let mut escaped = false;
            for c in inner.chars() {
                if !escaped && c == '\\' {
                    escaped = true;
                } else {
                    out.push(c);
                    escaped = false;
                }
            }
            out
        }
        None => name.to_owned(),
    }
}

/// RFC 5322's atext: what an atom, and a dot-string's parts, are made of.
fn atext(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&c)
}

/// An RFC 5321 Mailbox (4.1.2), ASCII (no SMTPUTF8): a local part that is
/// a dot-string or a quoted string, `@`, and a domain of LDH labels or an
/// address literal, within the lengths of 4.5.3.1.
fn check_address(address: &str) -> Result<(), SmtpError> {
    let refuse = || {
        SmtpError::Address(format!(
            "\u{201c}{address}\u{201d} is not an address OpenMaker can send to"
        ))
    };
    let bytes = address.as_bytes();
    let local_end = if bytes.first() == Some(&b'"') {
        // A quoted string: qtextSMTP and quoted-pairSMTP to its closing
        // quote.
        let mut i = 1;
        loop {
            match bytes.get(i) {
                Some(b'"') => break i + 1,
                Some(b'\\') if bytes.get(i + 1).is_some_and(|c| (32..=126).contains(c)) => i += 2,
                Some(c) if matches!(c, 32 | 33 | 35..=91 | 93..=126) => i += 1,
                _ => return Err(refuse()),
            }
        }
    } else {
        let end = address.find('@').ok_or_else(refuse)?;
        let dot_string = address[..end]
            .split('.')
            .all(|atom| !atom.is_empty() && atom.bytes().all(atext));
        if !dot_string {
            return Err(refuse());
        }
        end
    };
    if bytes.get(local_end) != Some(&b'@') || local_end > 64 {
        return Err(refuse());
    }
    let domain = &address[local_end + 1..];
    // An IPv4 or IPv6 address literal (4.1.3); a general one's tags are
    // not taken.
    let literal = domain
        .strip_prefix('[')
        .and_then(|d| d.strip_suffix(']'))
        .is_some_and(|inner| match inner.strip_prefix("IPv6:") {
            Some(v6) => v6.parse::<Ipv6Addr>().is_ok(),
            None => inner.parse::<Ipv4Addr>().is_ok(),
        });
    let labels = domain.split('.').all(|label| {
        let label = label.as_bytes();
        (1..=63).contains(&label.len())
            && label
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || *c == b'-')
            && label.first() != Some(&b'-')
            && label.last() != Some(&b'-')
    });
    // The path, its angle brackets included, is at most 256 octets
    // (4.5.3.1.3).
    if domain.len() > 255 || address.len() + 2 > 256 || !(literal || labels) {
        return Err(refuse());
    }
    Ok(())
}

/// The message as RFC 5322 has it, sent at `now`.
fn compose(message: &Message, now: SystemTime) -> Result<Composed, SmtpError> {
    let sender = message.from.trim();
    if sender.is_empty() {
        return Err(SmtpError::Address(
            "the step names no sender's address".into(),
        ));
    }
    check_address(sender)?;
    let mut headers = Vec::new();
    headers.push(format!("Date: {}", rfc5322_date(now)));
    let from = mailbox_header(
        (!message.from_name.trim().is_empty()).then(|| message.from_name.trim().to_owned()),
        sender,
    );
    headers.push(format!("From: {from}"));
    let list = |text: &str| -> Result<Vec<(Option<String>, String)>, SmtpError> {
        split_addresses(text)
            .map_err(|item| {
                SmtpError::Address(format!("the quote in \u{201c}{item}\u{201d} does not end"))
            })?
            .iter()
            .map(|item| mailbox(item))
            .collect()
    };
    let reply_to = list(&message.reply_to)?;
    let (to, cc, bcc) = (list(&message.to)?, list(&message.cc)?, list(&message.bcc)?);
    for (name, entries) in [("Reply-To", &reply_to), ("To", &to), ("Cc", &cc)] {
        if !entries.is_empty() {
            let joined: Vec<String> = entries
                .iter()
                .map(|(display, address)| mailbox_header(display.clone(), address))
                .collect();
            headers.push(format!("{name}: {}", joined.join(",\r\n ")));
        }
    }
    let recipients: Vec<String> = to
        .iter()
        .chain(&cc)
        .chain(&bcc)
        .map(|(_, address)| address.clone())
        .collect();
    if recipients.is_empty() {
        return Err(SmtpError::Address("the step names no recipient".into()));
    }
    let subject: String = message
        .subject
        .chars()
        .map(|c| if matches!(c, '\r' | '\n') { ' ' } else { c })
        .collect();
    if !subject.is_empty() {
        headers.push(format!("Subject: {}", header_text(&subject)));
    }
    let domain = &sender[sender.rfind('@').map_or(0, |at| at + 1)..];
    headers.push(format!("Message-ID: <{}@{domain}>", message_id(now)));
    headers.push("MIME-Version: 1.0".into());
    headers.push("Content-Type: text/plain; charset=utf-8".into());
    let body = crlf(&message.body);
    // 7bit holds ASCII without NUL in lines of at most 998 octets (RFC
    // 2045 2.7).
    let plain =
        body.is_ascii() && !body.contains('\0') && body.split("\r\n").all(|line| line.len() <= 998);
    let body = if plain {
        headers.push("Content-Transfer-Encoding: 7bit".into());
        body
    } else {
        headers.push("Content-Transfer-Encoding: base64".into());
        let encoded = base64(body.as_bytes());
        let mut wrapped = String::with_capacity(encoded.len() + encoded.len() / 38);
        for (i, chunk) in encoded.as_bytes().chunks(76).enumerate() {
            if i > 0 {
                wrapped.push_str("\r\n");
            }
            wrapped.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        }
        wrapped
    };
    let mut data = headers.join("\r\n").into_bytes();
    data.extend_from_slice(b"\r\n\r\n");
    data.extend_from_slice(body.as_bytes());
    if !data.ends_with(b"\r\n") {
        data.extend_from_slice(b"\r\n");
    }
    Ok(Composed {
        from: sender.to_owned(),
        recipients,
        data,
    })
}

/// FileMaker's returns (CR), and LF, as CRLF.
fn crlf(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 32);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            c => out.push(c),
        }
    }
    out
}

/// One mailbox of a header: `address`, or `name <address>` with the name
/// quoted or encoded as it needs. An encoded name ends its line, the
/// mailbox folded onto the next.
fn mailbox_header(name: Option<String>, address: &str) -> String {
    let Some(name) = name else {
        return address.to_owned();
    };
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let form = if name.bytes().all(|c| c == b' ' || atext(c)) {
        Some(name.clone())
    } else if name.is_ascii() {
        Some(format!(
            "\"{}\"",
            name.replace('\\', "\\\\").replace('"', "\\\"")
        ))
    } else {
        None
    };
    // The line, the longest field name ("Reply-To: ") and the trailing
    // comma included, within RFC 5322's 998 octets.
    match form.filter(|form| form.len() + address.len() + 3 + 11 <= 998) {
        Some(form) => format!("{form} <{address}>"),
        None => format!("{}\r\n <{address}>", encoded_words(&name)),
    }
}

/// Header text: as it is when it is short printable ASCII, else RFC 2047
/// encoded words.
fn header_text(text: &str) -> String {
    if text.len() <= 900 && text.chars().all(|c| c == ' ' || c.is_ascii_graphic()) {
        text.to_owned()
    } else {
        encoded_words(text)
    }
}

/// `text` as UTF-8 "B" encoded words of [`WORD_BYTES`] each at most, one
/// per line, no character split between two.
fn encoded_words(text: &str) -> String {
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in text.chars() {
        if chunk.len() + c.len_utf8() > WORD_BYTES {
            words.push(format!("=?utf-8?b?{}?=", base64(chunk.as_bytes())));
            chunk.clear();
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(format!("=?utf-8?b?{}?=", base64(chunk.as_bytes())));
    }
    words.join("\r\n ")
}

/// `now` as RFC 5322 3.3 dates it, in UTC.
fn rfc5322_date(now: SystemTime) -> String {
    let seconds = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = (seconds / 86_400) as i64;
    let of_day = seconds % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} +0000",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[(month - 1) as usize],
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    )
}

/// A Message-ID's left part, unique within this process and unlikely to
/// repeat across processes.
fn message_id(now: SystemTime) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!(
        "om.{nanos:x}.{:x}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The socket, each read and write it makes bounded by the current
/// operation's end, so the several a TLS record or a long write make stay
/// within it too.
struct Timed {
    socket: TcpStream,
    end: Rc<Cell<Instant>>,
}

impl Timed {
    fn left(&self) -> io::Result<Duration> {
        let left = self.end.get().saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the server did not answer in time",
            ));
        }
        Ok(left)
    }
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(self.left()?))?;
        self.socket.read(buf)
    }
}

impl Write for Timed {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.socket.set_write_timeout(Some(self.left()?))?;
        self.socket.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

/// The connection, plain or TLS.
enum Stream {
    Plain(Timed),
    Tls(Box<StreamOwned<ClientConnection, Timed>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(socket) => socket.read(buf),
            Self::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(socket) => socket.write(buf),
            Self::Tls(tls) => tls.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(socket) => socket.flush(),
            Self::Tls(tls) => tls.flush(),
        }
    }
}

/// A server reply: its code and its lines' text.
#[derive(Debug)]
struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    fn text(&self) -> String {
        format!("{} {}", self.code, self.lines.join(" "))
    }

    /// Whether the text says encryption is wanted (530 5.7.0 "Must issue
    /// a STARTTLS command first").
    fn wants_encryption(&self) -> bool {
        let text = self.lines.join(" ").to_ascii_lowercase();
        text.contains("tls") || text.contains("encrypt")
    }
}

/// Why reading a reply or sending failed.
fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// An SMTP session on a connection, with what it read ahead.
struct Session<'a> {
    stream: Stream,
    buffer: Vec<u8>,
    /// The script's deadline, if any.
    deadline: Option<Instant>,
    /// When the current reply, write or handshake must be done.
    operation: Instant,
    /// What [`Timed`] bounds each read and write by: the operation's end
    /// or the deadline, the earlier.
    end: Rc<Cell<Instant>>,
    control: &'a Control,
}

impl Session<'_> {
    /// Start an operation's budget.
    fn begin(&mut self, budget: Duration) {
        self.operation = Instant::now() + budget;
    }

    /// Bound the socket's reads and writes by what is left of the
    /// operation's budget and the deadline; an error once none is, or once
    /// stopped.
    fn arm(&self) -> io::Result<()> {
        if self.control.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let mut end = self.operation;
        if let Some(deadline) = self.deadline {
            end = end.min(deadline);
        }
        if end <= Instant::now() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the server did not answer in time",
            ));
        }
        self.end.set(end);
        Ok(())
    }

    /// One reply line without its CRLF; a bare LF, a CR or NUL inside it,
    /// or a line of more than [`MAX_LINE`] octets with its CRLF is
    /// refused.
    fn line(&mut self) -> io::Result<String> {
        loop {
            if let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
                if end + 1 > MAX_LINE {
                    return Err(invalid("a reply line is too long"));
                }
                if end == 0 || self.buffer[end - 1] != b'\r' {
                    return Err(invalid("a reply line does not end with CRLF"));
                }
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                let text = &line[..line.len() - 2];
                if text.iter().any(|&b| b == b'\r' || b == 0) {
                    return Err(invalid("a reply line holds a CR or NUL"));
                }
                return Ok(String::from_utf8_lossy(text).into_owned());
            }
            if self.buffer.len() > MAX_LINE {
                return Err(invalid("a reply line is too long"));
            }
            self.arm()?;
            let mut chunk = [0u8; 1024];
            let read = self.stream.read(&mut chunk)?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the server closed the connection",
                ));
            }
            self.buffer.extend_from_slice(&chunk[..read]);
        }
    }

    /// One whole reply (RFC 5321 4.2): every line the same code, `-`
    /// between lines and a space or nothing after the last code.
    fn reply(&mut self) -> io::Result<Reply> {
        self.reply_within(IO_TIMEOUT)
    }

    /// [`Self::reply`] within `budget`.
    fn reply_within(&mut self, budget: Duration) -> io::Result<Reply> {
        self.begin(budget);
        let mut lines = Vec::new();
        let mut code = None;
        loop {
            let line = self.line()?;
            let bytes = line.as_bytes();
            let valid = bytes.len() >= 3
                && (b'2'..=b'5').contains(&bytes[0])
                && (b'0'..=b'5').contains(&bytes[1])
                && bytes[2].is_ascii_digit()
                && matches!(bytes.get(3), None | Some(b' ' | b'-'));
            if !valid {
                return Err(invalid("not an SMTP reply"));
            }
            let this: u16 = line[..3]
                .parse()
                .map_err(|_| invalid("not an SMTP reply"))?;
            if code.is_some_and(|c| c != this) {
                return Err(invalid("a reply's lines have different codes"));
            }
            code = Some(this);
            lines.push(line.get(4..).unwrap_or_default().to_owned());
            if lines.len() > MAX_LINES {
                return Err(invalid("a reply has too many lines"));
            }
            if bytes.get(3) != Some(&b'-') {
                return Ok(Reply { code: this, lines });
            }
        }
    }

    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        // No command is pipelined: what the server sent beyond the last
        // reply was asked for by no one, and would answer the next.
        if !self.buffer.is_empty() {
            return Err(invalid("the server sent a reply no command asked for"));
        }
        self.begin(IO_TIMEOUT);
        // In pieces, a cancel checked before each.
        for piece in bytes.chunks(16 * 1024) {
            self.arm()?;
            self.stream.write_all(piece)?;
        }
        self.arm()?;
        self.stream.flush()
    }

    fn command(&mut self, command: &str) -> io::Result<Reply> {
        self.send(format!("{command}\r\n").as_bytes())?;
        self.reply()
    }

    /// TLS on the connection, the handshake done within its budget.
    fn upgrade(
        &mut self,
        tls: &Arc<ClientConfig>,
        name: ServerName<'static>,
    ) -> Result<(), SmtpError> {
        let Stream::Plain(plain) = &self.stream else {
            return Err(SmtpError::Tls("the connection is already encrypted".into()));
        };
        let mut socket = Timed {
            socket: plain
                .socket
                .try_clone()
                .map_err(|e| SmtpError::Tls(e.to_string()))?,
            end: self.end.clone(),
        };
        let mut connection =
            ClientConnection::new(tls.clone(), name).map_err(|e| SmtpError::Tls(e.to_string()))?;
        self.begin(IO_TIMEOUT);
        while connection.is_handshaking() {
            self.arm().map_err(|e| SmtpError::Tls(e.to_string()))?;
            connection.complete_io(&mut socket).map_err(|e| {
                let certificate = e
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<rustls::Error>())
                    .is_some_and(|inner| matches!(inner, rustls::Error::InvalidCertificate(_)));
                if certificate {
                    SmtpError::Certificate(e.to_string())
                } else {
                    SmtpError::Tls(e.to_string())
                }
            })?;
        }
        self.stream = Stream::Tls(Box::new(StreamOwned::new(connection, socket)));
        Ok(())
    }

    /// EHLO, or HELO when the server does not know it: the extensions
    /// the server lists, upper case.
    fn hello(&mut self, client: &str) -> Result<Vec<String>, SmtpError> {
        let reply = self.command(&format!("EHLO {client}")).map_err(broken)?;
        if reply.code == 250 {
            return Ok(reply
                .lines
                .iter()
                .skip(1)
                .map(|line| line.trim().to_ascii_uppercase())
                .collect());
        }
        let reply = self.command(&format!("HELO {client}")).map_err(broken)?;
        if reply.code != 250 {
            return Err(SmtpError::Connect(reply.text()));
        }
        Ok(Vec::new())
    }

    /// Log in as `auth` says. The errors carry the server's codes only: a
    /// server may echo what it was sent, the credentials among it.
    fn authenticate(
        &mut self,
        auth: &Auth,
        extensions: &[String],
        encrypted: bool,
    ) -> Result<(), SmtpError> {
        let (user, password, cram) = match auth {
            Auth::None => return Ok(()),
            Auth::Password { user, password } => (user, password, false),
            Auth::CramMd5 { user, password } => (user, password, true),
        };
        let mechanisms: Vec<&str> = extensions
            .iter()
            .filter_map(|e| {
                e.strip_prefix("AUTH")
                    .filter(|rest| rest.starts_with([' ', '=']))
            })
            .flat_map(|rest| rest[1..].split_whitespace())
            .collect();
        let offered = |m: &str| mechanisms.contains(&m);
        let unsupported = |wanted: &str| {
            // A server that lists STARTTLS and no usable AUTH on a plain
            // connection is read as one that offers it after STARTTLS [I].
            if !encrypted && extensions.iter().any(|e| e == "STARTTLS") {
                SmtpError::NeedsEncryption(format!(
                    "the server offers {wanted} only over an encrypted connection"
                ))
            } else {
                SmtpError::AuthUnsupported(format!(
                    "the server offers {}",
                    if mechanisms.is_empty() {
                        "no AUTH".to_owned()
                    } else {
                        mechanisms.join(" ")
                    }
                ))
            }
        };
        let outcome = |reply: &Reply| -> Result<(), SmtpError> {
            let detail = format!("the server answered {}", reply.code);
            match reply.code {
                235 => Ok(()),
                535 => Err(SmtpError::AuthFailed(detail)),
                534 => Err(SmtpError::AuthUnsupported(format!(
                    "{detail}: the mechanism is too weak"
                ))),
                538 => Err(SmtpError::NeedsEncryption(detail)),
                530 if reply.wants_encryption() => Err(SmtpError::NeedsEncryption(detail)),
                504 => Err(SmtpError::AuthUnsupported(detail)),
                _ => Err(SmtpError::Login(detail)),
            }
        };
        let quiet = |e: io::Error| SmtpError::Login(format!("the session broke: {}", e.kind()));
        if cram {
            if !offered("CRAM-MD5") {
                return Err(unsupported("CRAM-MD5"));
            }
            let reply = self.command("AUTH CRAM-MD5").map_err(quiet)?;
            if reply.code != 334 {
                return outcome(&reply);
            }
            let challenge = unbase64(reply.lines.first().map_or("", |l| l.trim()))
                .ok_or_else(|| SmtpError::Login("the server's challenge is not base64".into()))?;
            let response = format!("{user} {}", cram_md5(password.as_bytes(), &challenge));
            let reply = self.command(&base64(response.as_bytes())).map_err(quiet)?;
            return outcome(&reply);
        }
        if offered("PLAIN") {
            let token = base64(format!("\0{user}\0{password}").as_bytes());
            let initial = format!("AUTH PLAIN {token}");
            // Without the initial response when it would make the command
            // too long (RFC 4954 4).
            let short = initial.len() + 2 <= MAX_COMMAND;
            let mut reply = self
                .command(if short { &initial } else { "AUTH PLAIN" })
                .map_err(quiet)?;
            if reply.code == 334 {
                reply = self.command(&token).map_err(quiet)?;
            }
            return outcome(&reply);
        }
        if offered("LOGIN") {
            let mut reply = self.command("AUTH LOGIN").map_err(quiet)?;
            for answer in [user, password] {
                if reply.code != 334 {
                    return outcome(&reply);
                }
                reply = self.command(&base64(answer.as_bytes())).map_err(quiet)?;
            }
            return outcome(&reply);
        }
        Err(unsupported("PLAIN or LOGIN"))
    }
}

/// An I/O failure after connecting: the session broken.
fn broken(error: io::Error) -> SmtpError {
    SmtpError::Rejected(error.to_string())
}

/// What a 530 asks: encryption, or a login the step did not make.
fn required(reply: &Reply) -> SmtpError {
    if reply.wants_encryption() {
        SmtpError::NeedsEncryption(reply.text())
    } else {
        SmtpError::Login(reply.text())
    }
}

/// Connect to `account`'s server and send `mail`: the session, once the
/// server has taken the message.
fn deliver<'a>(
    account: &Account,
    mail: &Composed,
    tls: Arc<ClientConfig>,
    deadline: Option<Instant>,
    control: &'a Control,
) -> Result<Session<'a>, SmtpError> {
    let server = account.server.trim();
    if server.is_empty() {
        return Err(SmtpError::Connect("the step names no SMTP server".into()));
    }
    let name = ServerName::try_from(server.to_owned()).map_err(|_| {
        SmtpError::Connect(format!("\u{201c}{server}\u{201d} is not a server name"))
    })?;
    // The system resolver has no deadline of its own here; its timeouts
    // end a lookup that does not answer.
    let addresses: Vec<SocketAddr> = (server, account.port)
        .to_socket_addrs()
        .map_err(|e| SmtpError::Connect(e.to_string()))?
        .collect();
    let budget = Instant::now() + IO_TIMEOUT;
    let mut last = SmtpError::Connect(format!("no address for {server}"));
    let mut socket = None;
    for address in addresses {
        let mut end = budget;
        if let Some(deadline) = deadline {
            end = end.min(deadline);
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() || control.cancelled() {
            break;
        }
        match TcpStream::connect_timeout(&address, left) {
            Ok(connected) => {
                socket = Some(connected);
                break;
            }
            Err(e) => last = SmtpError::Connect(e.to_string()),
        }
    }
    let socket = socket.ok_or(last)?;
    control.adopt(&socket)?;
    let local = socket
        .local_addr()
        .map_err(|e| SmtpError::Connect(e.to_string()))?;
    let end = Rc::new(Cell::new(Instant::now()));
    let mut session = Session {
        stream: Stream::Plain(Timed {
            socket,
            end: end.clone(),
        }),
        buffer: Vec::new(),
        deadline,
        operation: Instant::now(),
        end,
        control,
    };
    if account.security == Security::Ssl {
        session.upgrade(&tls, name.clone())?;
    }
    let greeting = session
        .reply()
        .map_err(|e| SmtpError::Connect(e.to_string()))?;
    if greeting.code != 220 {
        return Err(SmtpError::Connect(greeting.text()));
    }
    // An address literal names the client, not its host name.
    let client = match local.ip() {
        std::net::IpAddr::V4(ip) => format!("[{ip}]"),
        std::net::IpAddr::V6(ip) => format!("[IPv6:{ip}]"),
    };
    let mut extensions = session.hello(&client)?;
    if account.security == Security::StartTls {
        if !extensions.iter().any(|e| e == "STARTTLS") {
            return Err(SmtpError::Tls("the server offers no STARTTLS".into()));
        }
        let reply = session.command("STARTTLS").map_err(broken)?;
        if reply.code != 220 {
            return Err(SmtpError::Tls(reply.text()));
        }
        // What the server sent ahead of the handshake would be read as
        // if it came over TLS (RFC 3207 6).
        if !session.buffer.is_empty() {
            return Err(SmtpError::Tls(
                "the server sent data ahead of the TLS handshake".into(),
            ));
        }
        session.upgrade(&tls, name)?;
        extensions = session.hello(&client)?;
    }
    let encrypted = account.security != Security::None;
    session.authenticate(&account.auth, &extensions, encrypted)?;
    let reply = session
        .command(&format!("MAIL FROM:<{}>", mail.from))
        .map_err(broken)?;
    if reply.code != 250 {
        return Err(match reply.code {
            530 => required(&reply),
            _ => SmtpError::Rejected(reply.text()),
        });
    }
    // One recipient refused stops the message for all, before DATA.
    for recipient in &mail.recipients {
        let reply = session
            .command(&format!("RCPT TO:<{recipient}>"))
            .map_err(broken)?;
        if !matches!(reply.code, 250 | 251) {
            return Err(match reply.code {
                530 => required(&reply),
                _ => SmtpError::Rejected(format!("{recipient}: {}", reply.text())),
            });
        }
    }
    let reply = session.command("DATA").map_err(broken)?;
    if reply.code != 354 {
        return Err(SmtpError::Rejected(reply.text()));
    }
    // The body may still be cancelled: without its final `.` the server
    // takes nothing.
    session.send(&dot_stuffed(&mail.data)).map_err(broken)?;
    if !control.commit() {
        return Err(SmtpError::Cancelled);
    }
    // From here the server's answer is the result: the script's deadline
    // no longer cuts it short, and the wait is RFC 5321's for it.
    session.deadline = None;
    let unconfirmed = |e: io::Error| SmtpError::Unconfirmed(e.to_string());
    session.send(b".\r\n").map_err(unconfirmed)?;
    let reply = session
        .reply_within(DATA_END_TIMEOUT)
        .map_err(unconfirmed)?;
    if reply.code != 250 {
        return Err(SmtpError::Rejected(reply.text()));
    }
    Ok(session)
}

/// CRAM-MD5's digest: HMAC-MD5 of `challenge` keyed by `password`, in
/// lower-case hex (RFC 2195 2).
fn cram_md5(password: &[u8], challenge: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<md5::Md5> as Mac>::new_from_slice(password)
        .unwrap_or_else(|_| unreachable!("HMAC takes a key of any length"));
    mac.update(challenge);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Standard base64 decoded, padding optional; `None` for anything else.
fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0u8);
    for c in text.trim_end_matches('=').bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 1;
        if count == 4 {
            out.extend_from_slice(&[(bits >> 16) as u8, (bits >> 8) as u8, bits as u8]);
            (bits, count) = (0, 0);
        }
    }
    match count {
        0 => {}
        2 => out.push((bits >> 4) as u8),
        3 => out.extend_from_slice(&[(bits >> 10) as u8, (bits >> 2) as u8]),
        _ => return None,
    }
    Some(out)
}

/// DATA's bytes before its final `.`: a line that starts with a period
/// gets another (RFC 5321 4.5.2).
fn dot_stuffed(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut line_start = true;
    for &b in data {
        if line_start && b == b'.' {
            out.push(b'.');
        }
        out.push(b);
        line_start = b == b'\n';
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;

    fn message() -> Message {
        Message {
            from_name: "Pat Example".into(),
            from: "pat@example.org".into(),
            to: "\"Sales, West\" <sales@example.org>, kim@example.org".into(),
            bcc: "audit@example.org".into(),
            subject: "Weekly report".into(),
            body: "Line one\r.hidden\rLine three".into(),
            ..Default::default()
        }
    }

    /// The message and envelope as composed: CRLF lines, the Bcc in the
    /// envelope only, a quoted display name kept, a non-ASCII subject,
    /// name and body encoded within RFC 2047's line length, and addresses
    /// outside RFC 5321's syntax refused.
    #[test]
    fn a_message_is_composed_as_rfc_5322_has_it() {
        let at = UNIX_EPOCH + Duration::from_secs(1_795_000_000);
        let composed = compose(&message(), at).unwrap();
        assert_eq!(composed.from, "pat@example.org");
        assert_eq!(
            composed.recipients,
            ["sales@example.org", "kim@example.org", "audit@example.org"]
        );
        let text = String::from_utf8(composed.data).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        assert!(
            head.starts_with("Date: Wed, 18 Nov 2026 11:06:40 +0000\r\n"),
            "{head}"
        );
        assert!(head.contains("\r\nFrom: Pat Example <pat@example.org>\r\n"));
        assert!(
            head.contains("\r\nTo: \"Sales, West\" <sales@example.org>,\r\n kim@example.org\r\n")
        );
        assert!(!head.contains("audit@"));
        assert!(head.contains("\r\nSubject: Weekly report\r\n"));
        assert!(head.contains("\r\nContent-Transfer-Encoding: 7bit"));
        assert_eq!(body, "Line one\r\n.hidden\r\nLine three\r\n");

        let accented = Message {
            from_name: "Zoë".repeat(20),
            subject: "Café".into(),
            body: "Grüße".into(),
            ..message()
        };
        let text = String::from_utf8(compose(&accented, at).unwrap().data).unwrap();
        assert!(
            text.contains("\r\nSubject: =?utf-8?b?Q2Fmw6k=?=\r\n"),
            "{text}"
        );
        assert!(text.ends_with("\r\n\r\nR3LDvMOfZQ==\r\n"));
        let head = text.split_once("\r\n\r\n").unwrap().0;
        assert!(head.contains("?=\r\n <pat@example.org>\r\n"), "{head}");
        for line in head.split("\r\n").filter(|l| l.contains("=?utf-8?")) {
            assert!(line.len() <= 76, "{line}");
        }
        let nul = Message {
            body: "a\0b".into(),
            ..message()
        };
        let text = String::from_utf8(compose(&nul, at).unwrap().data).unwrap();
        assert!(text.contains("Content-Transfer-Encoding: base64"));

        for bad in [
            "no-at-sign",
            "a b@example.org",
            "a,b@example.org",
            "\"a\"b\"@example.org",
            "a..b@example.org",
            "a@-example.org",
            "a@exa_mple.org",
            "zoë@example.org",
            "a@[999.1.1.1]",
            "a@[example]",
        ] {
            let wrong = Message {
                to: bad.into(),
                ..message()
            };
            assert!(
                matches!(compose(&wrong, at), Err(SmtpError::Address(_))),
                "{bad}"
            );
        }
        for good in [
            "\"a b\\\"c\"@example.org",
            "a.b+c@[192.0.2.1]",
            "a@[IPv6:2001:db8::1]",
        ] {
            assert!(check_address(good).is_ok(), "{good}");
        }
        let long = format!(
            "{}@{}.org",
            "a".repeat(64),
            vec!["b".repeat(63); 3].join(".")
        );
        assert!(
            matches!(check_address(&long), Err(SmtpError::Address(_))),
            "{long}"
        );
        // A 900-character ASCII name with a long mailbox is encoded rather
        // than written past 998 octets.
        let header = mailbox_header(
            Some("n".repeat(900)),
            &format!("{}@example.org", "m".repeat(60)),
        );
        assert!(
            header.split("\r\n").all(|line| line.len() + 11 <= 998),
            "{header}"
        );
        let empty = Message {
            from: String::new(),
            ..message()
        };
        assert!(matches!(compose(&empty, at), Err(SmtpError::Address(_))));
        assert_eq!(dot_stuffed(b".a\r\nb\r\n.\r\n"), b"..a\r\nb\r\n..\r\n");
    }

    /// RFC 2195's own example, and the credentials kept out of Debug.
    #[test]
    fn cram_md5_answers_rfc_2195s_example() {
        let challenge =
            unbase64("PDE4OTYuNjk3MTcwOTUyQHBvc3RvZmZpY2UucmVzdG9uLm1jaS5uZXQ+").unwrap();
        assert_eq!(challenge, b"<1896.697170952@postoffice.reston.mci.net>");
        assert_eq!(
            cram_md5(b"tanstaaftanstaaf", &challenge),
            "b913a602c7eda7a495b4e6e7334d3890"
        );
        let auth = Auth::Password {
            user: "pat".into(),
            password: "secret".into(),
        };
        assert!(!format!("{auth:?}").contains("secret"));
    }

    /// A scripted SMTP server: it answers each command it reads with the
    /// next reply and records what it read, until QUIT or the end of the
    /// script. `ended` is set when it has read the message's end, and its
    /// reply to that waits until `release` is set (or five seconds).
    fn server(
        replies: &'static [&'static str],
        ended: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
        quit_delay: Duration,
    ) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut writer = socket.try_clone().unwrap();
            let mut reader = io::BufReader::new(socket);
            let mut read = Vec::new();
            let mut replies = replies.iter();
            let _ = writer.write_all(replies.next().unwrap().as_bytes());
            let mut in_data = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                read.push(line.clone());
                if in_data {
                    if line != ".\r\n" {
                        continue;
                    }
                    in_data = false;
                    ended.store(true, Ordering::SeqCst);
                    let waited = Instant::now();
                    while !release.load(Ordering::SeqCst)
                        && waited.elapsed() < Duration::from_secs(5)
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                } else if line == "DATA\r\n" {
                    in_data = true;
                }
                let Some(reply) = replies.next() else { break };
                if line == "QUIT\r\n" {
                    std::thread::sleep(quit_delay);
                }
                if writer.write_all(reply.as_bytes()).is_err() || line == "QUIT\r\n" {
                    break;
                }
            }
            read
        });
        (port, handle)
    }

    fn scripted(replies: &'static [&'static str]) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        server(
            replies,
            Arc::default(),
            Arc::new(AtomicBool::new(true)),
            Duration::ZERO,
        )
    }

    fn account(port: u16, auth: Auth) -> Account {
        Account {
            server: "127.0.0.1".into(),
            port,
            security: Security::None,
            auth,
        }
    }

    fn login(password: &str) -> Auth {
        Auth::Password {
            user: "pat".into(),
            password: password.into(),
        }
    }

    const TAKEN: [&str; 7] = [
        "250 OK\r\n",
        "250 OK\r\n",
        "250 OK\r\n",
        "250 OK\r\n",
        "354 Go ahead\r\n",
        "250 OK queued\r\n",
        "221 Bye\r\n",
    ];

    /// The session as RFC 5321 runs it with each login, the message
    /// dot-stuffed; and each refusal answers its FileMaker code, the
    /// server's echo of a login left out of it.
    #[test]
    fn the_session_sends_the_message_and_answers_each_refusal_with_its_code() {
        let (port, server) = scripted(&[
            "220 mail.example.org ESMTP\r\n",
            "250-mail.example.org\r\n250-AUTH LOGIN PLAIN\r\n250 8BITMIME\r\n",
            "235 2.7.0 Authentication successful\r\n",
            "250 OK\r\n",
            "250 OK\r\n",
            "250 OK\r\n",
            "250 OK\r\n",
            "354 Go ahead\r\n",
            "250 OK queued\r\n",
            "221 Bye\r\n",
        ]);
        send_with(
            &account(port, login("secret")),
            &message(),
            RootCertStore::empty(),
            None,
            &mut || false,
        )
        .unwrap();
        let read = server.join().unwrap();
        assert!(read[0].starts_with("EHLO [127.0.0.1]"), "{read:?}");
        assert_eq!(
            read[1],
            format!("AUTH PLAIN {}\r\n", base64(b"\0pat\0secret"))
        );
        assert_eq!(read[2], "MAIL FROM:<pat@example.org>\r\n");
        assert_eq!(
            &read[3..6],
            [
                "RCPT TO:<sales@example.org>\r\n",
                "RCPT TO:<kim@example.org>\r\n",
                "RCPT TO:<audit@example.org>\r\n"
            ]
        );
        assert_eq!(read[6], "DATA\r\n");
        assert!(read.contains(&"..hidden\r\n".to_owned()), "{read:?}");
        assert_eq!(read.last().unwrap(), "QUIT\r\n");

        // LOGIN, and a full CRAM-MD5 exchange.
        let challenge = base64(b"<1.2@example.org>");
        let cram: &'static str = Box::leak(format!("334 {challenge}\r\n").into_boxed_str());
        for (auth, first, second, expected) in [
            (
                login("secret"),
                "250-hi\r\n250 AUTH LOGIN\r\n",
                "334 VXNlcm5hbWU6\r\n",
                vec![
                    "AUTH LOGIN\r\n".to_owned(),
                    format!("{}\r\n", base64(b"pat")),
                    format!("{}\r\n", base64(b"secret")),
                ],
            ),
            (
                Auth::CramMd5 {
                    user: "pat".into(),
                    password: "secret".into(),
                },
                "250-hi\r\n250 AUTH CRAM-MD5\r\n",
                cram,
                vec![
                    "AUTH CRAM-MD5\r\n".to_owned(),
                    format!(
                        "{}\r\n",
                        base64(
                            format!("pat {}", cram_md5(b"secret", b"<1.2@example.org>")).as_bytes()
                        )
                    ),
                ],
            ),
        ] {
            let mut replies = vec!["220 hi\r\n", first, second];
            if expected.len() == 3 {
                replies.push("334 UGFzc3dvcmQ6\r\n");
            }
            replies.push("235 OK\r\n");
            replies.extend(TAKEN.iter().copied());
            let (port, server) = scripted(Vec::leak(replies));
            send_with(
                &account(port, auth),
                &message(),
                RootCertStore::empty(),
                None,
                &mut || false,
            )
            .unwrap();
            let read = server.join().unwrap();
            assert_eq!(&read[1..1 + expected.len()], expected.as_slice());
        }

        let cases: [(&'static [&'static str], Auth, ErrorCode); 12] = [
            (
                &[
                    "220 hi\r\n",
                    "250-hi\r\n250 AUTH PLAIN\r\n",
                    "535 5.7.8 Bad credentials AGRhAHNlY3JldA==\r\n",
                ],
                login("wrong"),
                ErrorCode(1501),
            ),
            (&["554 No service\r\n"], Auth::None, ErrorCode(1502)),
            // A bare LF, a NUL, a second digit past 5, and a reply whose
            // lines' codes differ.
            (&["220 hi\n"], Auth::None, ErrorCode(1502)),
            (&["220 h\0i\r\n"], Auth::None, ErrorCode(1502)),
            (&["290 hi\r\n"], Auth::None, ErrorCode(1502)),
            // A 250 sent with the 354, ahead of the message.
            (
                &[
                    "220 hi\r\n",
                    "250 hi\r\n",
                    "250 OK\r\n",
                    "250 OK\r\n",
                    "250 OK\r\n",
                    "250 OK\r\n",
                    "354 Go\r\n250 OK\r\n",
                ],
                Auth::None,
                ErrorCode(1506),
            ),
            (
                &[
                    "220 hi\r\n",
                    "250 hi\r\n",
                    "250 OK\r\n",
                    "250 OK\r\n",
                    "250 OK\r\n",
                    "250 OK\r\n",
                    "354 Go\r\n",
                    "550-no\r\n250 OK\r\n",
                ],
                Auth::None,
                ErrorCode(1506),
            ),
            (
                &["220 hi\r\n", "250-hi\r\n250 STARTTLS\r\n"],
                login("x"),
                ErrorCode(1504),
            ),
            (
                &["220 hi\r\n", "250-hi\r\n250 AUTH LOGIN\r\n"],
                Auth::CramMd5 {
                    user: "pat".into(),
                    password: "x".into(),
                },
                ErrorCode(1505),
            ),
            (
                &[
                    "220 hi\r\n",
                    "250-hi\r\n250 AUTH PLAIN\r\n",
                    "534 5.7.9 Mechanism too weak\r\n",
                ],
                login("x"),
                ErrorCode(1505),
            ),
            (
                &[
                    "220 hi\r\n",
                    "250 hi\r\n",
                    "250 OK\r\n",
                    "550 5.1.1 No such user\r\n",
                ],
                Auth::None,
                ErrorCode(1506),
            ),
            (
                &[
                    "220 hi\r\n",
                    "250 hi\r\n",
                    "250 OK\r\n",
                    "530 5.7.0 Authentication required\r\n",
                ],
                Auth::None,
                ErrorCode(1507),
            ),
        ];
        for (replies, auth, code) in cases {
            let (port, server) = scripted(replies);
            let error = send_with(
                &account(port, auth),
                &message(),
                RootCertStore::empty(),
                None,
                &mut || false,
            )
            .unwrap_err();
            assert_eq!(error.code(), code, "{error}");
            assert!(!error.to_string().contains("AGRhAHNlY3JldA"), "{error}");
            drop(server);
        }
    }

    /// A cancel stops the session where it is, the message unsent; once
    /// the message's end is sent the server's answer stands, and it is
    /// given before QUIT's.
    #[test]
    fn a_cancel_stops_the_session_unless_the_message_is_sent() {
        // A server that never greets: the cancel shuts the connection and
        // the server reads nothing, at once.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (shut, closed) = mpsc::channel();
        std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut line = String::new();
            let read = io::BufReader::new(socket).read_line(&mut line).unwrap_or(0);
            let _ = shut.send(read);
        });
        let started = Instant::now();
        let mut polls = 0;
        let error = send_with(
            &account(port, Auth::None),
            &message(),
            RootCertStore::empty(),
            None,
            &mut || {
                polls += 1;
                polls > 4
            },
        )
        .unwrap_err();
        assert_eq!(error, SmtpError::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(5));
        let read = closed
            .recv_timeout(Duration::from_secs(5))
            .expect("the cancel shuts the connection");
        assert_eq!(read, 0);

        // Cancelled once the server has read the message's end, its 250
        // held until the cancel was seen: sent, and told before QUIT's
        // delayed reply.
        let (ended, seen) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let mut replies = vec!["220 hi\r\n", "250 hi\r\n"];
        replies.extend(TAKEN.iter().copied());
        let (port, server) = server(
            Vec::leak(replies),
            ended.clone(),
            seen.clone(),
            Duration::from_secs(3),
        );
        let started = Instant::now();
        send_with(
            &account(port, Auth::None),
            &message(),
            RootCertStore::empty(),
            None,
            &mut || {
                let cancel = ended.load(Ordering::SeqCst);
                if cancel {
                    seen.store(true, Ordering::SeqCst);
                }
                cancel
            },
        )
        .unwrap();
        assert!(seen.load(Ordering::SeqCst));
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(server);
    }

    /// A TLS server with a fresh certificate for "localhost": its port,
    /// the certificate, and what it read; with `implicit`, TLS from the
    /// start, else after STARTTLS (and, with `inject`, a reply sent ahead
    /// of the handshake).
    fn tls_server(
        implicit: bool,
        inject: bool,
    ) -> (
        u16,
        rustls::pki_types::CertificateDer<'static>,
        std::thread::JoinHandle<Vec<String>>,
    ) {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let der = certified.cert.der().clone();
        let key =
            rustls::pki_types::PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![der.clone()], key)
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            if !implicit {
                socket.write_all(b"220 hi\r\n").unwrap();
                let mut reader = io::BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                socket.write_all(b"250-hi\r\n250 STARTTLS\r\n").unwrap();
                line.clear();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "STARTTLS\r\n");
                let ready: &[u8] = if inject {
                    b"220 Go ahead\r\n250 injected\r\n"
                } else {
                    b"220 Go ahead\r\n"
                };
                socket.write_all(ready).unwrap();
            }
            let connection = rustls::ServerConnection::new(config).unwrap();
            let mut reader = io::BufReader::new(StreamOwned::new(connection, socket));
            let mut read = Vec::new();
            let mut replies = vec!["250-hi\r\n250 AUTH PLAIN\r\n", "235 OK\r\n"];
            replies.extend(TAKEN[..5].iter().copied());
            if implicit {
                replies.insert(0, "220 hi\r\n");
                if reader
                    .get_mut()
                    .write_all(replies.remove(0).as_bytes())
                    .is_err()
                    || reader.get_mut().flush().is_err()
                {
                    return read;
                }
            }
            for reply in replies {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return read;
                }
                read.push(line);
                reader.get_mut().write_all(reply.as_bytes()).unwrap();
                reader.get_mut().flush().unwrap();
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return read;
                }
                if line == ".\r\n" {
                    read.push(line);
                    break;
                }
            }
            if reader.get_mut().write_all(b"250 queued\r\n").is_err()
                || reader.get_mut().flush().is_err()
            {
                return read;
            }
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) > 0 {
                read.push(line);
                let _ = reader.get_mut().write_all(b"221 Bye\r\n");
                let _ = reader.get_mut().flush();
            }
            read
        });
        (port, der, handle)
    }

    fn tls_account(port: u16, security: Security) -> Account {
        Account {
            server: "localhost".into(),
            port,
            security,
            auth: login("secret"),
        }
    }

    /// STARTTLS and TLS from the start: the session goes on over TLS with
    /// the server's certificate trusted. With it untrusted the step is
    /// 1502, as if the server were unavailable; with no STARTTLS offered,
    /// or a reply sent ahead of the handshake, 1503.
    #[test]
    fn tls_carries_the_login_and_refuses_what_it_cannot_trust() {
        for security in [Security::StartTls, Security::Ssl] {
            let (port, der, server) = tls_server(security == Security::Ssl, false);
            let mut roots = RootCertStore::empty();
            roots.add(der).unwrap();
            send_with(
                &tls_account(port, security),
                &message(),
                roots,
                None,
                &mut || false,
            )
            .unwrap();
            let read = server.join().unwrap();
            assert!(read[0].starts_with("EHLO "), "{read:?}");
            assert!(read[1].starts_with("AUTH PLAIN "));
            // The message's end reached the server before its 250.
            assert_eq!(&read[read.len() - 2..], [".\r\n", "QUIT\r\n"]);
        }
        for (security, inject, code) in [
            (Security::Ssl, false, 1502),
            (Security::StartTls, false, 1502),
            (Security::StartTls, true, 1503),
        ] {
            let (port, der, server) = tls_server(security == Security::Ssl, inject);
            let mut roots = RootCertStore::empty();
            if inject {
                roots.add(der).unwrap();
            }
            let error = send_with(
                &tls_account(port, security),
                &message(),
                roots,
                None,
                &mut || false,
            )
            .unwrap_err();
            assert_eq!(error.code(), ErrorCode(code), "{error}");
            drop(server);
        }
        let (port, server) = scripted(&["220 hi\r\n", "250 hi\r\n"]);
        let error = send_with(
            &tls_account(port, Security::StartTls),
            &message(),
            RootCertStore::empty(),
            None,
            &mut || false,
        )
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode(1503), "{error}");
        drop(server);
    }
}
