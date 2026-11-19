//! The console's style sheet, script and favicon, compiled into the binary
//! and served under a URL that carries a hash of their content, so a
//! browser may cache them for good and still sees every new version.
//!
//! The style sheet and the script are each one file served, joined from
//! the parts below in order: the shell and the shared components first,
//! then one part per area of the console, so that areas do not share a
//! file.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// A file the console serves.
pub(crate) struct Asset {
    /// The URL's file name: `<stem>.<hash>.<extension>`.
    pub(crate) name: String,
    pub(crate) content_type: &'static str,
    pub(crate) body: &'static [u8],
}

pub(crate) const PREFIX: &str = "/admin-console/assets/";

/// The style sheet's parts, in order.
const STYLES: &[&str] = &[
    include_str!("../assets/css/shell.css"),
    include_str!("../assets/css/components.css"),
    include_str!("../assets/css/dashboard.css"),
    include_str!("../assets/css/databases.css"),
    include_str!("../assets/css/backups.css"),
    include_str!("../assets/css/configuration.css"),
    include_str!("../assets/css/logs.css"),
];

/// The script's parts, in order. Each part is a self-contained function
/// expression run once the page has loaded.
const SCRIPTS: &[&str] = &[
    include_str!("../assets/js/shell.js"),
    include_str!("../assets/js/components.js"),
    include_str!("../assets/js/databases.js"),
    include_str!("../assets/js/backups.js"),
    include_str!("../assets/js/configuration.js"),
];

/// The style sheet, script and favicon, in that order.
fn all() -> &'static [Asset; 3] {
    static ASSETS: OnceLock<[Asset; 3]> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let joined = |parts: &[&str]| -> &'static [u8] {
            Box::leak(parts.join("\n").into_bytes().into_boxed_slice())
        };
        [
            asset("console", "css", "text/css; charset=utf-8", joined(STYLES)),
            asset(
                "console",
                "js",
                "text/javascript; charset=utf-8",
                joined(SCRIPTS),
            ),
            asset(
                "favicon",
                "svg",
                "image/svg+xml",
                crate::icons::FAVICON.as_bytes(),
            ),
        ]
    })
}

fn asset(stem: &str, extension: &str, content_type: &'static str, body: &'static [u8]) -> Asset {
    let hash: String = Sha256::digest(body)[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Asset {
        name: format!("{stem}.{hash}.{extension}"),
        content_type,
        body,
    }
}

/// The asset whose URL file name is `name`.
pub(crate) fn find(name: &str) -> Option<&'static Asset> {
    all().iter().find(|a| a.name == name)
}

pub(crate) fn stylesheet_url() -> String {
    format!("{PREFIX}{}", all()[0].name)
}

pub(crate) fn script_url() -> String {
    format!("{PREFIX}{}", all()[1].name)
}

pub(crate) fn favicon_url() -> String {
    format!("{PREFIX}{}", all()[2].name)
}
