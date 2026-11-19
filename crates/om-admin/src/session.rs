//! Console sessions: who is signed in, by the random identifier in their
//! cookie (`docs/admin-console.md`, "Security").

use std::collections::HashMap;
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq;

/// The session cookie's name.
pub(crate) const COOKIE: &str = "om_admin_session";
/// Sessions open at once; a new sign-in ends the one idle longest.
pub(crate) const MAX_SESSIONS: usize = 5;
/// A session unused this long has ended.
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The open sessions.
#[derive(Default)]
pub(crate) struct Sessions {
    open: HashMap<[u8; 32], Session>,
}

struct Session {
    user: String,
    csrf: String,
    last_seen: Instant,
    /// What the next page shows once: the outcome of the session's latest
    /// action.
    flash: Option<Flash>,
    /// The Log Viewer's rows per page, when the administrator chose them.
    log_block_size: Option<usize>,
}

/// A signed-in session, as a request sees it.
#[derive(Clone, Debug)]
pub(crate) struct SignedIn {
    pub(crate) user: String,
    /// The token every form of this session carries.
    pub(crate) csrf: String,
    /// The session's cookie value, to leave it a [`Flash`].
    pub(crate) cookie: String,
    /// The Log Viewer's rows per page, when the administrator chose them
    /// (Log Settings > Block Size).
    pub(crate) log_block_size: Option<usize>,
}

/// The outcome of an action, shown once on the page it returns to. Kept in
/// the session rather than the address, so that a link cannot put words in
/// the console's mouth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Flash {
    pub(crate) ok: bool,
    pub(crate) text: String,
}

impl SignedIn {
    /// Whether a form's token is this session's.
    pub(crate) fn csrf_matches(&self, token: &str) -> bool {
        self.csrf.as_bytes().ct_eq(token.as_bytes()).into()
    }
}

/// What a request's cookie names.
pub(crate) enum Lookup {
    Valid(SignedIn),
    /// A session that ended by idling: the sign-in page says so.
    TimedOut,
    /// No cookie, or no such session.
    None,
}

impl Sessions {
    /// Open a session for `user`; returns the cookie value. Ends the
    /// sessions that have idled out and, when [`MAX_SESSIONS`] are still
    /// open, the one idle longest.
    pub(crate) fn open(&mut self, user: &str, now: Instant) -> String {
        self.open
            .retain(|_, s| now.saturating_duration_since(s.last_seen) < IDLE_TIMEOUT);
        while self.open.len() >= MAX_SESSIONS {
            let Some(oldest) = self
                .open
                .iter()
                .min_by_key(|(_, s)| s.last_seen)
                .map(|(id, _)| *id)
            else {
                break;
            };
            self.open.remove(&oldest);
        }
        let id = random();
        self.open.insert(
            id,
            Session {
                user: user.to_owned(),
                csrf: hex(&random()),
                last_seen: now,
                flash: None,
                log_block_size: None,
            },
        );
        hex(&id)
    }

    /// The session `cookie` names, which counts as using it at `now`.
    pub(crate) fn touch(&mut self, cookie: &str, now: Instant) -> Lookup {
        self.lookup(cookie, now, true)
    }

    /// The session `cookie` names, without counting as using it: a page's
    /// live refresh does not keep an unattended console signed in.
    pub(crate) fn peek(&mut self, cookie: &str, now: Instant) -> Lookup {
        self.lookup(cookie, now, false)
    }

    fn lookup(&mut self, cookie: &str, now: Instant, used: bool) -> Lookup {
        let Some(id) = unhex(cookie) else {
            return Lookup::None;
        };
        let Some(session) = self.open.get_mut(&id) else {
            return Lookup::None;
        };
        if now.saturating_duration_since(session.last_seen) >= IDLE_TIMEOUT {
            self.open.remove(&id);
            return Lookup::TimedOut;
        }
        if used {
            session.last_seen = now;
        }
        Lookup::Valid(SignedIn {
            user: session.user.clone(),
            csrf: session.csrf.clone(),
            cookie: cookie.to_owned(),
            log_block_size: session.log_block_size,
        })
    }

    /// Leave the session `cookie` names a [`Flash`] for its next page,
    /// replacing one it has not shown.
    pub(crate) fn set_flash(&mut self, cookie: &str, flash: Flash) {
        if let Some(session) = unhex(cookie).and_then(|id| self.open.get_mut(&id)) {
            session.flash = Some(flash);
        }
    }

    /// Keep the Log Viewer's rows per page for the session `cookie` names.
    pub(crate) fn set_log_block_size(&mut self, cookie: &str, rows: usize) {
        if let Some(session) = unhex(cookie).and_then(|id| self.open.get_mut(&id)) {
            session.log_block_size = Some(rows);
        }
    }

    /// Take the session's [`Flash`], if it has one.
    pub(crate) fn take_flash(&mut self, cookie: &str) -> Option<Flash> {
        unhex(cookie)
            .and_then(|id| self.open.get_mut(&id))
            .and_then(|session| session.flash.take())
    }

    /// End the session `cookie` names (Sign Out).
    pub(crate) fn close(&mut self, cookie: &str) {
        if let Some(id) = unhex(cookie) {
            self.open.remove(&id);
        }
    }
}

/// 256 random bits from the operating system.
pub(crate) fn random() -> [u8; 32] {
    let mut bytes = [0; 32];
    // Without the operating system's random numbers no session can be made
    // safely; there is nothing to fall back to.
    getrandom::getrandom(&mut bytes).expect("the operating system's random number generator");
    bytes
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    let text = text.as_bytes();
    if text.len() != 64 {
        return None;
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(text.chunks_exact(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_idle_out_and_the_longest_idle_makes_room() {
        let t0 = Instant::now();
        let mut sessions = Sessions::default();
        let first = sessions.open("admin", t0);
        let others: Vec<_> = (1..MAX_SESSIONS as u64)
            .map(|i| sessions.open("admin", t0 + Duration::from_secs(i)))
            .collect();
        // Using the first keeps it; a sixth sign-in then ends the session idle
        // longest, the second.
        let t1 = t0 + Duration::from_secs(60);
        assert!(matches!(sessions.touch(&first, t1), Lookup::Valid(_)));
        sessions.open("admin", t1);
        assert!(matches!(sessions.touch(&others[0], t1), Lookup::None));
        assert!(matches!(sessions.touch(&others[1], t1), Lookup::Valid(_)));
        // Idle for the timeout, a session has ended, and says so once.
        let t2 = t1 + IDLE_TIMEOUT;
        assert!(matches!(sessions.touch(&first, t2), Lookup::TimedOut));
        assert!(matches!(sessions.touch(&first, t2), Lookup::None));
    }
}
