//! Which hosted file to author and how to log in to it.
//!
//! [`crate::config::ConnectionOptions`] resolves the target and credentials.
//! A target keeps its password zeroed on drop and out of diagnostics.

use std::path::Path;

use om_data::Connection;
use om_provider::NativeConfig;
use zeroize::{Zeroize, Zeroizing};

/// FileMaker's port.
pub const DEFAULT_PORT: u16 = 5003;

/// A hosted file and the account to open it with.
#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    /// The hosted file's name, as the host lists it.
    pub file: String,
    pub account: String,
    password: Zeroizing<String>,
    /// The client TLS identity, additional CA and optional FileMaker client license.
    pub tls: NativeConfig,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("file", &self.file)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl Target {
    pub fn new(
        host: impl Into<String>,
        port: u16,
        file: impl Into<String>,
        account: impl Into<String>,
        password: Zeroizing<String>,
        tls: NativeConfig,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            file: file.into(),
            account: account.into(),
            password,
            tls,
        }
    }

    /// The connection to open, holding a copy of the password; zero it with
    /// [`forget_password`] once the file is open.
    pub(crate) fn connection(&self) -> Connection {
        Connection {
            host: self.host.clone(),
            port: self.port,
            file: self.file.clone(),
            account: self.account.clone(),
            password: self.password.to_string(),
        }
    }
}

/// Zero a connection's copy of the password.
pub(crate) fn forget_password(connection: &mut Connection) {
    connection.password.zeroize();
}

/// The password from `file` (its first line), else `OPENMAKER_PASSWORD`.
pub fn read_password(file: Option<&Path>) -> Result<Zeroizing<String>, String> {
    if let Some(path) = file {
        return read_secret_file(path, "password");
    }
    match std::env::var("OPENMAKER_PASSWORD") {
        Ok(password) => Ok(Zeroizing::new(password)),
        Err(std::env::VarError::NotPresent) => Err(
            "No password: set OPENMAKER_PASSWORD, or give a file holding it with \
             --password-file or OPENMAKER_PASSWORD_FILE."
                .into(),
        ),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("OPENMAKER_PASSWORD is not valid Unicode.".into())
        }
    }
}

pub(crate) fn read_secret_file(path: &Path, kind: &str) -> Result<Zeroizing<String>, String> {
    let text = Zeroizing::new(
        std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read the {kind} file {}: {e}", path.display()))?,
    );
    let line = text.split(['\n', '\r']).next().unwrap_or_default();
    Ok(Zeroizing::new(line.to_owned()))
}
