//! Building the Login (op80) transaction (spec `docs/re/network-protocol.md`
//! 4.3 and 5.4).
//!
//! The account name and password are `PushEncrypt` fields: SCSU (XOR 0x5A)
//! bytes further XORed with a per-field PBKDF2 keystream keyed by a per-login
//! hash of the Open nonce and the file's `mInternalCreation` (`om_crypto`).
//! Every other field is plaintext. The body byte order below is the official
//! client's, confirmed by parsing two captured Logins with no bytes left over.

use om_crypto::{
    deobscure_field, login_nonce, obscure_field, password_hash_for_login, LOGIN_HASH_LEN,
};
use om_rctransaction::{Builder, Header, Reader};
use om_scsu::Framing;

use crate::{Opcode, PerformError};

/// Header `session_id` of file-scoped transactions after Open (observed as 1).
pub const FILE_SESSION_ID: u32 = 1;

/// The protocol version pushed in the Login body.
pub const LOGIN_VERSION: u16 = 8;

/// The plaintext client-environment strings a Login carries. The server records
/// them for session display but does not authenticate against them; only their
/// framing matters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginEnvironment {
    pub system_drive: String,
    pub preferences_path: String,
    pub documents_path: String,
    pub desktop_path: String,
    pub application_path: String,
    pub system_version: String,
    pub printer: String,
    pub app_language: String,
    /// The opened file as an `fmnet:/<host>/<file>` URL.
    pub file_path: String,
    /// The session's display name on the server.
    pub session_name: String,
}

impl LoginEnvironment {
    /// A Linux-style environment for a file opened from `host`.
    pub fn for_remote_file(host: &str, filename: &str) -> Self {
        Self {
            system_drive: "/".into(),
            preferences_path: "/home/openmaker/.config/".into(),
            documents_path: "/home/openmaker/Documents/".into(),
            desktop_path: "/home/openmaker/Desktop/".into(),
            application_path: "/usr/bin/".into(),
            system_version: "Linux".into(),
            printer: " ".into(),
            app_language: "English".into(),
            file_path: format!("fmnet:/{host}/{filename}"),
            session_name: "openmaker".into(),
        }
    }
}

/// Everything needed to build one Login transaction.
#[derive(Clone, Debug)]
pub struct LoginRequest<'a> {
    /// The connection's next file-transaction id (header transid-low).
    pub transaction_id: u32,
    /// The opened file's server id (`OpenReply::file_id`).
    pub file_id: u32,
    /// The Open reply's per-file nonce (`OpenReply::file_nonce`).
    pub open_nonce: u32,
    /// The file's `mInternalCreation` from its HBAM.
    pub internal_creation: u32,
    /// The 16-byte connection key from the Connect reply.
    pub connection_key: [u8; 16],
    pub account: &'a str,
    pub password: &'a str,
    pub environment: &'a LoginEnvironment,
}

impl LoginRequest<'_> {
    /// The per-login key that obscures the credential fields.
    pub fn password_hash(&self) -> [u8; LOGIN_HASH_LEN] {
        password_hash_for_login(
            login_nonce(self.open_nonce, self.internal_creation),
            &self.connection_key,
        )
    }
}

/// One obscured `PushEncrypt(unistring)` field: SCSU (XOR 0x5A) bytes XORed with
/// the per-field keystream, then a trailing `u32` big-endian length.
fn encrypted_string_field(
    text: &str,
    password_hash: &[u8; LOGIN_HASH_LEN],
    conn_key: &[u8; 16],
) -> Vec<u8> {
    let mut field = om_scsu::encode_field(text, Framing::TrailingU32)
        .expect("trailing-u32 length cannot overflow");
    let data_len = field.len() - 4;
    obscure_field(&mut field[..data_len], password_hash, conn_key);
    field
}

/// Build the Login (op80) transaction for a local account.
pub fn build_login(req: &LoginRequest<'_>) -> Vec<u8> {
    let hash = req.password_hash();
    let env = req.environment;

    let mut header = Header::new(Opcode::Login as u8);
    header.transid_lo = req.transaction_id;
    header.session_id = FILE_SESSION_ID;
    header.file_id = req.file_id;

    let password = encrypted_string_field(req.password, &hash, &req.connection_key);
    let account = encrypted_string_field(req.account, &hash, &req.connection_key);

    Builder::with_header(header)
        // Callback file-user id: server uses it only for perform-script-on-host
        // routing, never for auth. Zero skips that path (spec 5.4 item 2).
        .push_u32(0)
        .push_u16(0) // client flags
        .push_bool(false) // has-new-password
        .push_string(&env.system_drive)
        .push_string(&env.preferences_path)
        .push_string(&env.documents_path)
        .push_string(&env.desktop_path)
        .push_string(&env.application_path)
        .push_u16(LOGIN_VERSION)
        // Empty privilege extension: void* form writes only a u16 length (no
        // data, so it is not obscured), then an empty obscured string slot.
        .push_u16(0)
        .push_raw(&0u32.to_be_bytes())
        .push_raw(&password)
        .push_raw(&account)
        .push_string(&env.system_version)
        .push_string(&env.printer)
        .push_string(&env.app_language)
        .push_string(&env.file_path)
        .push_string(&env.session_name)
        .push_u8(0) // login type: local account
        .push_u32(req.file_id)
        .finish()
}

/// A Login (op80) transaction as a server receives it, credentials recovered.
#[derive(Clone, PartialEq, Eq)]
pub struct ReceivedLogin {
    /// The target file id the client pushed last (the Open reply's file id).
    pub file_id: u32,
    /// 0 = local account.
    pub login_type: u8,
    pub account: String,
    pub password: String,
    pub environment: LoginEnvironment,
    pub version: u16,
    pub has_new_password: bool,
    pub client_flags: u16,
    pub callback_file_user: u32,
}

impl std::fmt::Debug for ReceivedLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceivedLogin")
            .field("file_id", &self.file_id)
            .field("login_type", &self.login_type)
            .field("account", &self.account)
            .field("password", &"<redacted>")
            .field("environment", &self.environment)
            .field("version", &self.version)
            .field("has_new_password", &self.has_new_password)
            .field("client_flags", &self.client_flags)
            .field("callback_file_user", &self.callback_file_user)
            .finish()
    }
}

/// Parse a Login (op80) transaction, popping the fields [`build_login`] pushes
/// and recovering the two obscured credentials.
///
/// The server derives the same per-login key as the client from the values it
/// handed out: the Open nonce it sent, the file's `mInternalCreation`, and the
/// connection key from its Connect reply (spec 4.3).
pub fn parse_login_request(
    transaction: &[u8],
    open_nonce: u32,
    internal_creation: u32,
    connection_key: &[u8; 16],
) -> Result<ReceivedLogin, PerformError> {
    let hash = password_hash_for_login(login_nonce(open_nonce, internal_creation), connection_key);
    let mut r = Reader::parse(transaction).ok_or(PerformError::Truncated)?;
    let file_id = r.pop_u32()?;
    let login_type = r.pop_u8()?;
    let session_name = r.pop_string()?;
    let file_path = r.pop_string()?;
    let app_language = r.pop_string()?;
    let printer = r.pop_string()?;
    let system_version = r.pop_string()?;
    let account = pop_obscured_string(&mut r, &hash, connection_key)?;
    let password = pop_obscured_string(&mut r, &hash, connection_key)?;
    // The empty privilege-extension slot `build_login` writes: an obscured
    // string, then its u16 length.
    pop_obscured_string(&mut r, &hash, connection_key)?;
    r.pop_u16()?;
    let version = r.pop_u16()?;
    let application_path = r.pop_string()?;
    let desktop_path = r.pop_string()?;
    let documents_path = r.pop_string()?;
    let preferences_path = r.pop_string()?;
    let system_drive = r.pop_string()?;
    let has_new_password = r.pop_bool()?;
    let client_flags = r.pop_u16()?;
    let callback_file_user = r.pop_u32()?;
    Ok(ReceivedLogin {
        file_id,
        login_type,
        account,
        password,
        environment: LoginEnvironment {
            system_drive,
            preferences_path,
            documents_path,
            desktop_path,
            application_path,
            system_version,
            printer,
            app_language,
            file_path,
            session_name,
        },
        version,
        has_new_password,
        client_flags,
        callback_file_user,
    })
}

/// Pop one `PushEncrypt(unistring)` field: the trailing `u32` length, then the
/// obscured bytes, de-obscured and SCSU-decoded.
fn pop_obscured_string(
    r: &mut Reader<'_>,
    password_hash: &[u8; LOGIN_HASH_LEN],
    conn_key: &[u8; 16],
) -> Result<String, PerformError> {
    let len = r.pop_u32()? as usize;
    let mut bytes = r.pop_raw(len)?.to_vec();
    deobscure_field(&mut bytes, password_hash, conn_key);
    om_scsu::decode_scsu(&bytes).map_err(|_| PerformError::Underflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_recovers_client_credentials() {
        let environment = LoginEnvironment::for_remote_file("server", "test project.fmp12");
        let request = LoginRequest {
            transaction_id: 2,
            file_id: 0x84,
            open_nonce: 0x1234_5678,
            internal_creation: 0xc08c_3393,
            connection_key: *b"0123456789abcdef",
            account: "Admin",
            password: "s\u{e9}cret pass",
            environment: &environment,
        };
        let txn = build_login(&request);
        let login =
            parse_login_request(&txn, 0x1234_5678, 0xc08c_3393, b"0123456789abcdef").unwrap();
        assert_eq!(login.account, "Admin");
        assert_eq!(login.password, "s\u{e9}cret pass");
        assert_eq!(login.file_id, 0x84);
        assert_eq!(login.login_type, 0);
        assert_eq!(login.version, LOGIN_VERSION);
        assert_eq!(login.environment, environment);
        assert!(
            !format!("{login:?}").contains("cret"),
            "Debug redacts the password"
        );

        // A server holding a different nonce recovers different (wrong) text.
        let wrong = parse_login_request(&txn, 0x1234_5679, 0xc08c_3393, b"0123456789abcdef");
        assert!(wrong.map_or(true, |l| l.password != "s\u{e9}cret pass"));
    }
}
