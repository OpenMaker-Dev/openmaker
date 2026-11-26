//! OpenMaker Go's engine: the desktop client's window
//! (`om_gui::OpenMakerApp`) inside a phone or tablet app's native chrome
//! (docs/mobile-client.md).
//!
//! Each open file is a [`Document`], whose window and egui state stay on a
//! thread of its own. The native app sends it [`Input`] and draws the
//! [`Frame`]s it makes: egui's triangles, textures and clip rectangles at
//! the device's pixel density, which the app's Metal or OpenGL ES painter
//! draws as egui's own painters do. A frame is made only when input
//! arrives or the window asks to be drawn again. The chrome's menus,
//! commands and state come with the frames ([`DocumentState`], [`Menu`]).
//! File contents and credentials stay in the engine.

// `GoError` crosses UniFFI by value, so its fields cannot be boxed, and
// its `Untrusted` case carries what a certificate says for the user to be
// asked about it. It is returned once per open or listing, not in a loop.
#![allow(clippy::result_large_err)]

use om_data::DataProvider;
use om_provider::{NativeConfig, NativeProvider};

uniffi::setup_scaffolding!();

mod accessibility;
mod document;
mod frame;
mod hosts;
mod input;
mod state;
mod surface;

pub use accessibility::{Element, ElementKind};
pub use document::{Document, DocumentObserver};
pub use frame::{Batch, Frame, FrameTimings, TextureUpdate, TextureWrap};
pub use hosts::{
    connect_certificate, distinct_hosts, former_hosts, host_key, remove_host, save_host,
    trusted_certificate, HostChange, HostSaved, ListedHost, SavedHosts,
};
pub use input::{Command, Input, Key};
pub use state::{Caret, DocumentMode, DocumentState, Menu, MenuRow, MenuRowKind};

/// Why a file did not open, or a request failed. The text is `reason`:
/// Kotlin's exceptions already have a `message`.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum GoError {
    /// The host or the file refused the account and password, or the host
    /// wants a client license it was not sent: the sign-in can be tried
    /// again.
    #[error("{reason}")]
    Credentials { reason: String },
    /// The file opens only with an account and password.
    #[error("{reason}")]
    SignInRequired { reason: String },
    /// Any other failure, with FileMaker's error code when it has one.
    #[error("{reason}")]
    Failed { code: Option<i32>, reason: String },
    /// The host's certificate could not be verified, and was not permitted:
    /// what to ask the user before connecting anyway, as FileMaker Go asks
    /// (docs/mobile-client.md). Connecting again with `fingerprint` as the
    /// host's [`Host::permitted`] accepts this certificate, and no other.
    #[error("{reason}")]
    Untrusted {
        host: String,
        port: u16,
        /// Why the certificate could not be verified, as a sentence.
        reason: String,
        /// The certificate's subject and issuer (`CN=..., O=...`) and its
        /// validity (`2027-11-25 20:22:31 UTC`); empty when they do not
        /// read.
        subject: String,
        issuer: String,
        valid_from: String,
        valid_until: String,
        /// Its SHA-256 fingerprint, as `openssl x509 -fingerprint` prints
        /// one: `AB:CD:...`.
        fingerprint: String,
        /// Another certificate was permitted for this host: this one
        /// replaced it.
        changed: bool,
    },
}

impl GoError {
    fn failed(message: impl ToString) -> Self {
        GoError::Failed {
            code: None,
            reason: message.to_string(),
        }
    }

    /// FileMaker's error 212 ("Invalid user account and/or password") and
    /// 213 ("User account and/or password does not exist") ask for the
    /// sign-in again; an inactive account (210) or one whose password must
    /// be changed first (211) cannot be fixed by retyping it. A host that
    /// refuses the client license (or wants one) does too: the license is
    /// typed in the sign-in, and the desktop's text points to its own
    /// License dialog.
    /// A failure the provider reported: [`Self::Untrusted`] for a host whose
    /// certificate it could not verify, else as [`Self::from_failure`].
    fn from_data(error: &om_data::DataError) -> Self {
        match &error.untrusted {
            Some(host) => Self::untrusted(host),
            None => Self::from_failure(error.code, &error.message),
        }
    }

    fn untrusted(host: &om_data::UntrustedHost) -> Self {
        GoError::Untrusted {
            host: host.host.clone(),
            port: host.port,
            reason: host.message(),
            subject: host.subject.clone(),
            issuer: host.issuer.clone(),
            valid_from: host.valid_from.clone(),
            valid_until: host.valid_until.clone(),
            fingerprint: host
                .fingerprint
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(":"),
            changed: host.changed,
        }
    }

    fn from_failure(code: Option<i32>, message: &str) -> Self {
        match code {
            Some(212 | 213) => GoError::Credentials {
                reason: message.to_owned(),
            },
            Some(om_provider::CLIENT_LICENSE_REFUSED) => GoError::Credentials {
                reason: "The host did not accept the client license. Enter the \
                         FileMaker client license this host requires."
                    .to_owned(),
            },
            _ => GoError::Failed {
                code,
                reason: message.to_owned(),
            },
        }
    }
}

/// A FileMaker or OpenMaker Server.
#[derive(Clone, Debug, uniffi::Record)]
pub struct Host {
    pub address: String,
    pub port: u16,
    /// A PEM certificate authority to trust for this host besides the
    /// system's, for a privately administered server; empty for none.
    pub certificate: String,
    /// The SHA-256 fingerprint of a server certificate the user permitted
    /// for this host and port after a [`GoError::Untrusted`], in
    /// hexadecimal with or without colons: accepted when it cannot be
    /// verified. Empty for none.
    #[uniffi(default = "")]
    pub permitted: String,
    /// The certificates the user permitted for other hosts, each accepted
    /// on its own host and port only: for a file a script opens there.
    #[uniffi(default = [])]
    pub permitted_elsewhere: Vec<Permit>,
}

/// A server certificate the user permitted for a host and port, by its
/// SHA-256 fingerprint (as [`Host::permitted`]).
#[derive(Clone, Debug, uniffi::Record)]
pub struct Permit {
    pub address: String,
    pub port: u16,
    pub fingerprint: String,
}

/// A fingerprint in hexadecimal, with or without colons or spaces.
fn fingerprint(text: &str) -> Option<[u8; 32]> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| !matches!(byte, b':' | b' '))
        .collect();
    if digits.len() != 64 {
        return None;
    }
    let mut out = [0; 32];
    for (byte, pair) in out.iter_mut().zip(digits.chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

/// Where a file is.
#[derive(Clone, Debug, uniffi::Enum)]
pub enum Source {
    /// A file in the app's documents.
    Local { path: String },
    /// A file a host publishes.
    Hosted { host: Host, file: String },
}

/// What the user typed to open a file. It lasts as long as the document.
#[derive(Clone, uniffi::Record)]
pub struct SignIn {
    pub account: String,
    pub password: String,
    /// A FileMaker client license, for hosts that require one; empty for
    /// none.
    pub license: String,
}

/// The canvas a document draws on: its size in points (Android's dp), its
/// pixels per point, and the largest texture side its GPU takes.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub max_texture_side: u32,
}

/// The largest canvas side, in pixels: what Metal and OpenGL ES 3 devices
/// take at least, and a bound on what one frame can cost.
const MAX_SIDE_PIXELS: f32 = 8192.0;

impl Viewport {
    fn check(&self) -> Result<Self, GoError> {
        let finite = [self.width, self.height, self.scale]
            .iter()
            .all(|value| value.is_finite());
        if !finite
            || self.width < 1.0
            || self.height < 1.0
            || !(0.5..=8.0).contains(&self.scale)
            || self.width * self.scale > MAX_SIDE_PIXELS
            || self.height * self.scale > MAX_SIDE_PIXELS
            || self.max_texture_side < 2048
        {
            return Err(GoError::failed("Unsupported document size."));
        }
        Ok(Viewport {
            max_texture_side: self.max_texture_side.min(16384),
            ..*self
        })
    }
}

fn provider(host: Option<&Host>) -> Result<NativeProvider, GoError> {
    let mut config = NativeConfig::from_env_with_ca_cert(None, false).map_err(GoError::failed)?;
    if let Some(host) = host {
        if host.address.trim().is_empty() || host.port == 0 {
            return Err(GoError::failed("Enter a host name and port."));
        }
        // Past the bounds a host is refused, never cut short (`hosts`).
        hosts::check_bundle(&host.certificate)?;
        config.additional_ca_pem = host.certificate.as_bytes().to_vec();
        // Saved for this host and port: a file a script opens elsewhere is
        // not verified against it.
        let mut provider = NativeProvider::new(config)
            .with_related_on_demand(true)
            .with_ca_scope(host.address.trim(), host.port);
        if !host.permitted.trim().is_empty() {
            let permitted = fingerprint(&host.permitted).ok_or_else(|| {
                GoError::failed("The permitted certificate's fingerprint is not valid.")
            })?;
            provider =
                provider.with_permitted_certificate(host.address.trim(), host.port, permitted);
        }
        // Each for its own host and port only; one that does not read is
        // left out rather than stop this host opening.
        for permit in &host.permitted_elsewhere {
            if let Some(fingerprint) = fingerprint(&permit.fingerprint) {
                provider = provider.with_permitted_certificate(
                    permit.address.trim(),
                    permit.port,
                    fingerprint,
                );
            }
        }
        return Ok(provider);
    }
    Ok(NativeProvider::new(config).with_related_on_demand(true))
}

/// A host as Add Host takes it, or what to fix ([`GoError::Failed`]): the
/// address and port are checked as the desktop's Add Host checks them
/// (`om_gui::host_address`) and the address given in the form it is
/// connected as (`hosts`); the certificate authority, kept as given, must be
/// empty or certificates alone, within bounds.
#[uniffi::export]
pub fn check_host(address: String, port: String, certificate: String) -> Result<Host, GoError> {
    let (address, port) = om_gui::host_address(&address, &port).map_err(GoError::failed)?;
    let address = hosts::canonical_address(&address)
        .ok_or_else(|| GoError::failed("Enter a valid host name or IP address."))?;
    hosts::readable(&certificate)?;
    Ok(Host {
        address,
        port,
        certificate,
        permitted: String::new(),
        permitted_elsewhere: Vec::new(),
    })
}

/// A file name typed to open on a host, as the host lists it: with the
/// `.fmp12` extension, which the name may leave out.
#[uniffi::export]
pub fn hosted_file_name(name: String) -> String {
    om_data::file_refs::hosted_name(name.trim())
}

/// The files `host` publishes. Blocks while it asks the host.
#[uniffi::export]
pub fn hosted_files(host: Host) -> Result<Vec<String>, GoError> {
    let files = provider(Some(&host))?
        .list_files(host.address.trim(), host.port)
        .map_err(|error| GoError::from_data(&error))?;
    Ok(files.into_iter().map(|file| file.name).collect())
}
