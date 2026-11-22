//! Connection settings shared by `om-author`'s CLI and MCP server.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use om_data::host::machine::user_config_dir;
use om_provider::NativeConfig;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::target::{read_password, read_secret_file, Target, DEFAULT_PORT};

/// Command-line overrides for the environment and the user configuration file.
#[derive(Clone, Default)]
pub struct ConnectionOptions {
    pub config: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<String>,
    pub file: Option<String>,
    pub account: Option<String>,
    pub password_file: Option<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    host: Option<String>,
    port: Option<u16>,
    file: Option<String>,
    account: Option<String>,
    password_file: Option<PathBuf>,
    ca_cert: Option<PathBuf>,
    license_file: Option<PathBuf>,
}

impl Settings {
    fn read(path: &Path, required: bool) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => Zeroizing::new(text),
            Err(e) if e.kind() == ErrorKind::NotFound && !required => return Ok(Self::default()),
            Err(e) => return Err(format!("Cannot read author config {}: {e}", path.display())),
        };
        // Some JSON diagnostics quote the input. Report the location without
        // echoing values that users may have accidentally put in the file.
        serde_json::from_str(&text).map_err(|e| {
            format!(
                "Invalid author config {} at line {}, column {}. See the authoring setup guide for supported keys.",
                path.display(),
                e.line(),
                e.column()
            )
        })
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn default_config_path() -> Option<PathBuf> {
    user_config_dir().map(|dir| dir.join("author.json"))
}

fn env_setting(variable: &str) -> Option<String> {
    std::env::var(variable).ok().filter(|v| !v.is_empty())
}

impl ConnectionOptions {
    fn explicit_config_path(&self) -> Option<PathBuf> {
        self.config
            .clone()
            .or_else(|| env_path("OPENMAKER_AUTHOR_CONFIG"))
    }

    /// The selected path, whether or not it exists.
    pub fn config_path(&self) -> Option<PathBuf> {
        self.explicit_config_path().or_else(default_config_path)
    }

    /// Resolve the hosted file and credentials when a command needs them.
    pub fn target(&self) -> Result<Target, String> {
        let path = self.explicit_config_path();
        let explicit = path.is_some();
        let path = path.or_else(default_config_path);
        let settings = path
            .as_deref()
            .map(|path| Settings::read(path, explicit))
            .transpose()?
            .unwrap_or_default();
        let env_host = env_setting("OPENMAKER_HOST");
        let env_file = env_setting("OPENMAKER_FILE");
        let env_account = env_setting("OPENMAKER_ACCOUNT");
        let env_port = env_setting("OPENMAKER_PORT");
        let required = |arg: &Option<String>,
                        env: &Option<String>,
                        saved: &Option<String>,
                        option,
                        variable| {
            arg.as_ref()
                    .or(if explicit { None } else { env.as_ref() })
                .or(saved.as_ref())
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or_else(|| {
                    if explicit {
                        format!("No {option}: give --{option} or add it to the selected author config.")
                    } else {
                        format!("No {option}: give --{option}, set {variable}, or configure author.json.")
                    }
                })
        };
        let host = required(
            &self.host,
            &env_host,
            &settings.host,
            "host",
            "OPENMAKER_HOST",
        )?;
        let file = required(
            &self.file,
            &env_file,
            &settings.file,
            "file",
            "OPENMAKER_FILE",
        )?;
        let account = required(
            &self.account,
            &env_account,
            &settings.account,
            "account",
            "OPENMAKER_ACCOUNT",
        )?;
        let saved_port = settings.port.unwrap_or(DEFAULT_PORT);
        let port = match self
            .port
            .as_ref()
            .or(if explicit { None } else { env_port.as_ref() })
        {
            Some(port) => port
                .parse()
                .map_err(|_| format!("“{port}” is not a port number."))?,
            None => saved_port,
        };

        // A host override must not inherit another endpoint's credentials or CA.
        let same_server = settings.host.as_deref() == Some(host.as_str()) && saved_port == port;
        let saved_path = |value: Option<PathBuf>| {
            if !same_server {
                return None;
            }
            let dir = path.as_deref()?.parent()?;
            value.map(|p| dir.join(p))
        };

        // An inherited environment may belong to a different file than the
        // explicit profile an agent was registered with.
        let env_server_matches = !explicit
            || (env_host.as_ref().is_none_or(|value| *value == host)
                && env_port
                    .as_ref()
                    .is_none_or(|value| value.parse() == Ok(port)));
        let env_login_matches = env_server_matches
            && (!explicit
                || (env_file.as_ref().is_none_or(|value| *value == file)
                    && env_account.as_ref().is_none_or(|value| *value == account)));
        let saved_login_matches = same_server
            && settings.file.as_ref().is_none_or(|value| *value == file)
            && settings
                .account
                .as_ref()
                .is_none_or(|value| *value == account);
        let had_saved_password = settings.password_file.is_some();
        let saved_password = if saved_login_matches {
            saved_path(settings.password_file)
        } else {
            None
        };
        let env_password_file = env_path("OPENMAKER_PASSWORD_FILE");
        let has_env_password =
            env_password_file.is_some() || std::env::var_os("OPENMAKER_PASSWORD").is_some();
        let password = if let Some(path) = self.password_file.as_deref() {
            read_password(Some(path))
        } else if env_login_matches && has_env_password && (!explicit || saved_password.is_none()) {
            read_password(env_password_file.as_deref())
        } else if let Some(path) = saved_password.as_deref() {
            read_password(Some(path))
        } else if had_saved_password || (!env_login_matches && has_env_password) {
            Err("No password for the selected host, port, file and account. The saved or inherited password belongs to a different connection. Give --password-file or update the selected author config.".into())
        } else {
            read_password(None)
        }?;
        let license_file = saved_path(settings.license_file);
        let license = read_license(
            license_file.as_deref(),
            env_server_matches && (!explicit || license_file.is_none()),
        )?;
        let saved_ca = saved_path(settings.ca_cert);
        let env_ca = if env_server_matches {
            std::env::var_os("OPENMAKER_CA_CERT").map(PathBuf::from)
        } else {
            None
        };
        let ca_cert = if explicit {
            saved_ca.or(env_ca)
        } else {
            env_ca.or(saved_ca)
        }
        .filter(|path| !path.as_os_str().is_empty());
        let mut tls = NativeConfig::from_env_with_ca_cert(ca_cert.as_deref(), env_server_matches)
            .map_err(|e| e.to_string())?;
        tls.set_client_license(license.trim())
            .map_err(|e| e.to_string())?;
        Ok(Target::new(host, port, file, account, password, tls))
    }
}

fn read_license(file: Option<&Path>, use_env: bool) -> Result<Zeroizing<String>, String> {
    if use_env {
        match std::env::var("OPENMAKER_LICENSE_KEY") {
            Ok(key) => return Ok(Zeroizing::new(key)),
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err("OPENMAKER_LICENSE_KEY is not valid Unicode.".into());
            }
        }
    }
    match file {
        Some(file) => read_secret_file(file, "license"),
        None => Ok(Zeroizing::default()),
    }
}
