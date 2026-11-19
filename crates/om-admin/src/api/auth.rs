//! The Admin API's tokens (`docs/admin-console.md`, "Admin API"):
//! `POST /user/auth` with the administrator's HTTP Basic credentials opens
//! one, `DELETE /user/auth/{token}` closes it, and every other call carries
//! it as `Authorization: Bearer <token>`.
//!
//! The credentials are checked as the console's sign-in checks them, against
//! the same lockout of an address that keeps failing, with the same failure
//! count (`crate::auth`). Basic credentials on any other call open a new
//! token too, returned in the `X-FM-Access-Token` header, as FileMaker
//! Server's API documents.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::extract::rejection::PathRejection;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use log::{info, warn};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{ok, ApiError};
use crate::app::AppState;
use crate::auth::Attempt;
use crate::session;

/// Tokens open at once, as in FileMaker Server; a new one ends the one idle
/// longest.
pub(crate) const MAX_TOKENS: usize = 5;
/// A token unused this long has ended, as in FileMaker Server.
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// The longest `Authorization` header read: credentials are short.
const MAX_AUTHORIZATION: usize = 4096;

/// The response header that carries a token opened by Basic credentials on
/// a call other than `POST /user/auth`.
const ACCESS_TOKEN_HEADER: &str = "x-fm-access-token";

const LOCKED_OUT: &str =
    "Too many failed sign-in attempts came from this address. Try again in a few minutes.";

/// The open tokens, by the SHA-256 digest of their text: a lookup's timing
/// says nothing about a token's text, and the console's memory holds none.
#[derive(Default)]
pub(crate) struct Tokens {
    open: HashMap<[u8; 32], Token>,
}

struct Token {
    user: String,
    last_used: Instant,
}

impl Tokens {
    /// Open a token for `user`; returns its text. Ends the tokens that have
    /// idled out and, when [`MAX_TOKENS`] are still open, the one idle
    /// longest.
    pub(crate) fn open(&mut self, user: &str, now: Instant) -> String {
        self.open
            .retain(|_, t| now.saturating_duration_since(t.last_used) < IDLE_TIMEOUT);
        while self.open.len() >= MAX_TOKENS {
            let Some(oldest) = self
                .open
                .iter()
                .min_by_key(|(_, t)| t.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.open.remove(&oldest);
        }
        let text = session::hex(&session::random());
        self.open.insert(
            digest(&text),
            Token {
                user: user.to_owned(),
                last_used: now,
            },
        );
        text
    }

    /// The account of token `text`, which counts as using it at `now`; `None`
    /// when there is no such token or it has idled out.
    pub(crate) fn touch(&mut self, text: &str, now: Instant) -> Option<String> {
        let key = digest(text);
        let token = self.open.get_mut(&key)?;
        if now.saturating_duration_since(token.last_used) >= IDLE_TIMEOUT {
            self.open.remove(&key);
            return None;
        }
        token.last_used = now;
        Some(token.user.clone())
    }

    /// Close token `text`; returns its account, or `None` when there was no
    /// such token or it had idled out.
    pub(crate) fn close(&mut self, text: &str, now: Instant) -> Option<String> {
        let token = self.open.remove(&digest(text))?;
        (now.saturating_duration_since(token.last_used) < IDLE_TIMEOUT).then_some(token.user)
    }
}

fn digest(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

/// Who made a request: the account of its token and the address it came
/// from, for the log.
#[derive(Clone, Debug)]
pub(crate) struct Caller {
    pub(crate) user: String,
    pub(crate) peer: SocketAddr,
}

/// What a request's `Authorization` header holds.
enum Credentials {
    Bearer(String),
    Basic { user: String, password: String },
}

/// The request's credentials, or `None` when it has none the API takes
/// (PKI tokens included) or they are malformed.
fn credentials(headers: &HeaderMap) -> Option<Credentials> {
    let value = headers.get(AUTHORIZATION)?;
    if value.len() > MAX_AUTHORIZATION {
        return None;
    }
    let (scheme, rest) = value.to_str().ok()?.trim().split_once(' ')?;
    let rest = rest.trim();
    if scheme.eq_ignore_ascii_case("bearer") {
        return Some(Credentials::Bearer(rest.to_owned()));
    }
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = String::from_utf8(BASE64_STANDARD.decode(rest).ok()?).ok()?;
    let (user, password) = decoded.split_once(':')?;
    Some(Credentials::Basic {
        user: user.to_owned(),
        password: password.to_owned(),
    })
}

/// Check Basic credentials from `peer` and open a token for them.
fn open_token(
    state: &AppState,
    peer: SocketAddr,
    user: &str,
    password: &str,
    now: Instant,
) -> Result<String, ApiError> {
    match state.attempt_sign_in(peer.ip(), user, password, now) {
        Attempt::Accepted => {
            let token = state.api_tokens().open(state.user(), now);
            info!("{peer}: opened an Admin API session as {}", state.user());
            Ok(token)
        }
        Attempt::Refused => {
            // The name sent is not logged: it may be a password.
            warn!("{peer}: Admin API sign-in failed");
            Err(ApiError::unauthorized())
        }
        Attempt::LockedOut => {
            warn!("{peer}: Admin API sign-in refused: the address is locked out");
            Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "1703",
                LOCKED_OUT,
            ))
        }
    }
}

/// `POST /user/auth`: open a token with the administrator's Basic
/// credentials.
pub(crate) async fn sign_in(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let Some(Credentials::Basic { user, password }) = credentials(&headers) else {
        warn!("{peer}: Admin API sign-in without Basic credentials");
        return Err(ApiError::unauthorized());
    };
    let token = open_token(&state, peer, &user, &password, Instant::now())?;
    Ok(ok(json!({ "token": token })))
}

/// `DELETE /user/auth/{token}`: close a token.
pub(crate) async fn sign_out(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    // A path that does not decode names no token.
    let Ok(Path(token)) = path else {
        return Err(ApiError::unauthorized());
    };
    let user = state
        .api_tokens()
        .close(&token, Instant::now())
        .ok_or_else(ApiError::unauthorized)?;
    info!("{peer}: closed an Admin API session of {user}");
    Ok(ok(json!({})))
}

/// Admit a request with a token, or with Basic credentials, which open one
/// returned in `X-FM-Access-Token`; refuse any other with 401 and 1703. The
/// handler finds the [`Caller`] in the request's extensions.
pub(crate) async fn authenticate(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    let now = Instant::now();
    let (user, opened) = match credentials(request.headers()) {
        Some(Credentials::Bearer(token)) => match state.api_tokens().touch(&token, now) {
            Some(user) => (user, None),
            None => return ApiError::unauthorized().into_response(),
        },
        Some(Credentials::Basic { user, password }) => {
            match open_token(&state, peer, &user, &password, now) {
                Ok(token) => (state.user().to_owned(), Some(token)),
                Err(e) => return e.into_response(),
            }
        }
        None => return ApiError::unauthorized().into_response(),
    };
    request.extensions_mut().insert(Caller { user, peer });
    let mut response = next.run(request).await;
    if let Some(value) = opened.and_then(|t| HeaderValue::from_str(&t).ok()) {
        response.headers_mut().insert(ACCESS_TOKEN_HEADER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_idle_out_and_the_longest_idle_makes_room() {
        let t0 = Instant::now();
        let mut tokens = Tokens::default();
        let first = tokens.open("admin", t0);
        assert_eq!(first.len(), 64);
        let others: Vec<_> = (1..MAX_TOKENS as u64)
            .map(|i| tokens.open("admin", t0 + Duration::from_secs(i)))
            .collect();
        // Using the first keeps it; a sixth token then ends the one idle
        // longest, the second.
        let t1 = t0 + Duration::from_secs(60);
        assert_eq!(tokens.touch(&first, t1).as_deref(), Some("admin"));
        tokens.open("admin", t1);
        assert_eq!(tokens.touch(&others[0], t1), None);
        assert!(tokens.touch(&others[1], t1).is_some());
        // Idle for the timeout, a token has ended; a closed one is gone.
        assert_eq!(tokens.touch(&first, t1 + IDLE_TIMEOUT), None);
        assert!(tokens.close(&others[1], t1).is_some());
        assert_eq!(tokens.touch(&others[1], t1), None);
        assert_eq!(tokens.close(&others[1], t1), None);
    }
}
