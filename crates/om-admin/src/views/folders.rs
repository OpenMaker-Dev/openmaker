//! Configuration > Folders (`docs/re/admin-console.md` 4.5): the folders of
//! the hosted files and the backup folder, in FileMaker's path syntax.
//!
//! OpenMaker Server hosts the files named on its command line rather than
//! every file in a database folder, so the Database Folders are the
//! folders those files are in, and there are no additional database
//! folders to turn on (`docs/admin-console.md`, "Views").

use std::path::Path;

use maud::{html, Markup};

use crate::components::{help, section, title};
use crate::views::{not_shown, Ctx};

/// Folders listed at most; the rest are counted.
const MAX_FOLDERS: usize = 100;

const HOSTED_HELP: &str = "The folders that hold the files OpenMaker Server hosts. It hosts \
    the files named on its command line, wherever they are.";

const ADDITIONAL_TEXT: &str = "OpenMaker Server hosts the files named on its command line \
    (om-server <file.fmp12>...) rather than every file in a database folder, so there are no \
    additional database folders to turn on. To host another file, add it to the command line \
    and restart the server.";

const PROGRESSIVE_HELP: &str = "A progressive backup copies only what changed in the hosted \
    files since the previous one, into a folder of its own.";

pub(crate) fn page(ctx: &Ctx) -> Markup {
    let folders = match ctx.backend.folders() {
        Ok(folders) => folders,
        Err(e) => {
            return html! {
                (title("Folders"))
                (not_shown("Folders are not available in OpenMaker Server yet.", &e))
            }
        }
    };
    let total = folders.databases.len();
    let shown = &folders.databases[..total.min(MAX_FOLDERS)];
    html! {
        (title("Folders"))
        (section("Database Folders", Some(HOSTED_HELP)))
        @if shown.is_empty() {
            (path_row("Database Folder Path", None))
        }
        @for (index, folder) in shown.iter().enumerate() {
            @let label = if total == 1 {
                "Database Folder Path".to_owned()
            } else {
                format!("Database Folder {} Path", index + 1)
            };
            (path_row(&label, Some(folder)))
        }
        @if total > shown.len() {
            p.folders-more { "Showing " (shown.len()) " of " (total) " folders." }
        }
        (section("Additional Database Folders", None))
        p.section-note { (ADDITIONAL_TEXT) }
        (section("Backup Folders", None))
        @match &folders.backups {
            Some(folder) => (path_row("Default Backup Folder Path", Some(folder))),
            None => div.settings-row.path-row {
                div.settings-label { "Default Backup Folder Path" }
                div.settings-value { span.unavailable { "Not available: OpenMaker Server makes no backups" } }
                div.settings-action {}
            },
        }
        div.settings-row.path-row {
            div.settings-label { "Progressive Backup" (help(PROGRESSIVE_HELP)) }
            div.settings-value { span.unavailable { "Not available yet" } }
            div.settings-action {
                button.toggle type="button" role="switch" aria-checked="false"
                    aria-label="Progressive Backup" disabled { span.toggle-knob {} }
            }
        }
    }
}

/// A row of a folder's path with a Copy button, or "None" without one.
///
/// A path that is not valid Unicode (bytes that are not UTF-8 on Linux or
/// macOS, an unpaired surrogate on Windows) cannot be written exactly in
/// text: it is shown with its replacement characters, says so, and has no
/// Copy, since the copied text would name another folder.
fn path_row(label: &str, folder: Option<&Path>) -> Markup {
    let path = folder.map(|f| {
        f.to_str()
            .and_then(|native| filemaker_folder(native, THIS))
            .ok_or_else(|| {
                let lossy = f.to_string_lossy();
                filemaker_folder(&lossy, THIS).unwrap_or_else(|| lossy.into_owned())
            })
    });
    html! {
        div.settings-row.path-row {
            div.settings-label { (label) }
            div.settings-value {
                @match &path {
                    Some(Ok(path)) => span.folder-path { (path) },
                    Some(Err(approximate)) => {
                        span.folder-path { (approximate) }
                        span.folder-inexact {
                            "Shown approximately: part of this path is not valid Unicode, \
                             so it cannot be shown or copied exactly."
                        }
                    }
                    None => span.unavailable { "None" },
                }
            }
            div.settings-action {
                @if let Some(Ok(path)) = &path {
                    // The script shows it where the browser lets a page copy.
                    button.link-button.copy-button type="button" data-copy=(path)
                        aria-label={"Copy " (label)} hidden { "Copy" }
                }
            }
        }
    }
}

/// The operating systems whose paths FileMaker writes differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Linux,
    Mac,
    Windows,
}

const THIS: Platform = if cfg!(windows) {
    Platform::Windows
} else if cfg!(target_os = "macos") {
    Platform::Mac
} else {
    Platform::Linux
};

/// Folder `native` (a path of this machine) in FileMaker's path syntax, as
/// FileMaker Server's console shows folders: `filelinux:/srv/Databases/`,
/// `filemac:/Macintosh HD/Users/Shared/`, `filewin:/C:/Databases/` or
/// `filewin://computer/share/`, ending with `/`
/// (`docs/re/script-runtime.md`, "Files", for the full paths). `None` when
/// the startup disk's name, which a macOS path needs, is not valid Unicode.
fn filemaker_folder(native: &str, platform: Platform) -> Option<String> {
    let mut text = match platform {
        Platform::Linux => format!("filelinux:{native}"),
        Platform::Mac => match native.strip_prefix("/Volumes/") {
            Some(rest) => format!("filemac:/{rest}"),
            None => format!("filemac:/{}{native}", boot_volume()?),
        },
        Platform::Windows => {
            let raw = native.replace('\\', "/");
            // A canonical path's verbatim prefix is not part of the path.
            let raw = match raw.strip_prefix("//?/") {
                Some(rest) => match rest.strip_prefix("UNC/") {
                    Some(share) => format!("//{share}"),
                    None => rest.to_owned(),
                },
                None => raw,
            };
            if raw.starts_with("//") {
                format!("filewin:{raw}")
            } else {
                format!("filewin:/{raw}")
            }
        }
    };
    if !text.ends_with('/') {
        text.push('/');
    }
    Some(text)
}

/// The startup disk's name on macOS: the volume under `/Volumes` that links
/// to `/`; `None` when that name is not valid Unicode.
fn boot_volume() -> Option<String> {
    if cfg!(target_os = "macos") {
        if let Ok(entries) = std::fs::read_dir("/Volumes") {
            for entry in entries.flatten().take(256) {
                if std::fs::read_link(entry.path()).is_ok_and(|t| t == Path::new("/")) {
                    return entry.file_name().into_string().ok();
                }
            }
        }
    }
    Some("Macintosh HD".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_read_in_filemaker_path_syntax() {
        let cases = [
            (
                "/srv/openmaker/Databases",
                Platform::Linux,
                "filelinux:/srv/openmaker/Databases/",
            ),
            ("/", Platform::Linux, "filelinux:/"),
            (
                "/Volumes/Data/FileMaker",
                Platform::Mac,
                "filemac:/Data/FileMaker/",
            ),
            (
                r"C:\FileMaker\Databases",
                Platform::Windows,
                "filewin:/C:/FileMaker/Databases/",
            ),
            (r"\\?\D:\Hosted", Platform::Windows, "filewin:/D:/Hosted/"),
            (
                r"\\fileserver\share\Hosted",
                Platform::Windows,
                "filewin://fileserver/share/Hosted/",
            ),
            (
                r"\\?\UNC\fileserver\share",
                Platform::Windows,
                "filewin://fileserver/share/",
            ),
        ];
        for (native, platform, expected) in cases {
            assert_eq!(
                filemaker_folder(native, platform).as_deref(),
                Some(expected),
                "{native}"
            );
        }
        // The startup disk is named by its volume (here, off macOS, the
        // default name).
        let boot = filemaker_folder("/Users/Shared", Platform::Mac).unwrap();
        assert!(boot.starts_with("filemac:/") && boot.ends_with("/Users/Shared/"));
        assert_ne!(boot, "filemac:/Users/Shared/");
    }

    /// Copying a path whose bytes are not UTF-8 would copy the replacement
    /// character in their place, which names another folder.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_unicode_is_not_offered_for_copying() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let row = |path: &Path| path_row("Database Folder Path", Some(path)).into_string();
        let exact = row(Path::new("/srv/Hosted-a"));
        assert!(exact.contains("data-copy=") && !exact.contains("approximately"));
        let inexact = row(Path::new(OsStr::from_bytes(b"/srv/Hosted-\xff")));
        assert!(!inexact.contains("data-copy"), "{inexact}");
        assert!(inexact.contains("Shown approximately"), "{inexact}");
    }
}
