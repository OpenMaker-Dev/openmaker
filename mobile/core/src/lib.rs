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

use om_data::DataProvider;
use om_provider::{NativeConfig, NativeProvider};

uniffi::setup_scaffolding!();

mod accessibility;
mod document;
mod frame;
mod input;
mod state;
mod surface;

pub use accessibility::{Element, ElementKind};
pub use document::{Document, DocumentObserver};
pub use frame::{Batch, Frame, FrameTimings, TextureUpdate, TextureWrap};
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
        config.additional_ca_pem = host.certificate.as_bytes().to_vec();
        // Saved for this host and port: a file a script opens elsewhere is
        // not verified against it.
        return Ok(NativeProvider::new(config)
            .with_related_on_demand(true)
            .with_ca_scope(host.address.trim(), host.port));
    }
    Ok(NativeProvider::new(config).with_related_on_demand(true))
}

/// A host as Add Host takes it, or what to fix ([`GoError::Failed`]): the
/// address and port are checked as the desktop's Add Host checks them
/// (`om_gui::host_address`), and the certificate authority is kept as
/// given.
#[uniffi::export]
pub fn check_host(address: String, port: String, certificate: String) -> Result<Host, GoError> {
    let (address, port) = om_gui::host_address(&address, &port).map_err(GoError::failed)?;
    Ok(Host {
        address,
        port,
        certificate,
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
        .map_err(|error| GoError::from_failure(error.code, &error.message))?;
    Ok(files.into_iter().map(|file| file.name).collect())
}
