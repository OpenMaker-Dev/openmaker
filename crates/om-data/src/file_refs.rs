//! Stored FileMaker data-source paths (file-metadata.md 1.1). Decode the
//! complete list before using an alternative; never reinterpret a local or
//! another server's path as a file on the current server.

/// The character a stored `fmnet` location has on each side of its host
/// (and port), where the path shows `/`: FileMaker Pro 26.0.3 stores
/// `fmnet:/example.invalid/File` as `("fmnet", "\u{E000}example.invalid\u{E000}",
/// "File")` (#759; file-metadata.md 1.1).
pub const LOCATION_SEPARATOR: char = '\u{E000}';

/// FileMaker's port, the one an `fmnet` path without a port names.
pub const DEFAULT_PORT: u16 = 5003;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePath {
    pub scheme: String,
    pub location: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDataSource {
    /// The serial id used by stored script references, not its raw key bytes.
    pub id: u64,
    pub name: String,
    pub paths: Vec<FilePath>,
}

pub fn decode(bytes: &[u8]) -> Option<Vec<FilePath>> {
    let count = u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?);
    if count > 1024 {
        return None;
    }
    let mut rest = &bytes[4..];
    let mut paths = Vec::new();
    for _ in 0..count {
        let mut next = || {
            let (text, used) = om_scsu::decode_counted(rest).ok()?;
            rest = &rest[used..];
            Some(text)
        };
        paths.push(FilePath {
            scheme: next()?,
            location: next()?,
            name: next()?,
        });
    }
    rest.is_empty().then_some(paths)
}

/// File `name` on `host` at `port` as `fmnet:/host:port/Name`: how the
/// desktop names a file on another host or port than the open file's to
/// [`crate::DataProvider::open_peer`] and in the related-file sign-in
/// (#976), so that one name on two hosts is two files.
pub fn fmnet_name(host: &str, port: u16, name: &str) -> String {
    format!("fmnet:/{host}:{port}/{name}")
}

/// The host, port (5003 when none is written) and plain file name of an
/// `fmnet:/host[:port]/Name` name ([`fmnet_name`]); `None` for any other.
pub fn fmnet_target(name: &str) -> Option<(String, u16, String)> {
    let path = FilePath::calculated(name)?;
    let (host, port) = path.fmnet_endpoint()?;
    Some((host.to_owned(), port, path.name.clone()))
}

/// The display/calculation name, removing one case-insensitive extension.
pub fn file_stem(name: &str) -> &str {
    if name.to_ascii_lowercase().ends_with(".fmp12") {
        &name[..name.len() - 6]
    } else {
        name
    }
}

/// `Get ( FilePath )` of a file opened from this computer: `file:` and
/// its path as FileMaker writes it (`file:/Macintosh HD/Users/...`,
/// `file:/C:/Users/...`, `file:/home/...`).
pub fn local_file_path(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    let text = if cfg!(windows) {
        let raw = raw.replace('\\', "/");
        match raw.strip_prefix("//") {
            Some(unc) => format!("//{unc}"),
            None => format!("/{raw}"),
        }
    } else if cfg!(target_os = "macos") {
        match raw.strip_prefix("/Volumes/") {
            Some(rest) => format!("/{rest}"),
            None => format!("/{}{raw}", boot_volume()),
        }
    } else {
        raw.into_owned()
    };
    format!("file:{text}")
}

/// The startup disk's name on macOS: the volume under `/Volumes` that links
/// to `/`.
fn boot_volume() -> String {
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            if std::fs::read_link(entry.path()).is_ok_and(|t| t == std::path::Path::new("/")) {
                return entry.file_name().to_string_lossy().into_owned();
            }
        }
    }
    "Macintosh HD".into()
}

pub fn hosted_name(name: &str) -> String {
    if name.to_lowercase().ends_with(".fmp12") {
        name.to_owned()
    } else {
        format!("{name}.fmp12")
    }
}

/// `Get ( FilePath )` of hosted file `name` on `host`: `fmnet:/host/Name.fmp12`,
/// with `:port` after the host when `port` is given, the form
/// [`FilePath::calculated`] reads back with [`FilePath::fmnet_endpoint`].
/// The extension is added once ([`hosted_name`]). Whether FileMaker Pro
/// names a port in it is unmeasured: OpenMaker names one only to tell a
/// file on another port of a host apart.
pub fn hosted_file_path(host: &str, port: Option<u16>, name: &str) -> String {
    let name = hosted_name(name);
    match port {
        Some(port) => format!("fmnet:/{host}:{port}/{name}"),
        None => format!("fmnet:/{host}/{name}"),
    }
}

/// Which file a reference names, as the desktop tells files apart: a
/// hosted file by its host's text in any case, its port and its name as a
/// host compares names ([`hosted_key`]: any case, the extension added once,
/// a doubled one kept); a file on this computer by its path with links
/// followed. One name on two hosts, or on two ports of one host, is two
/// files. Nothing is looked up: two texts naming one host are two hosts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileIdentity {
    Hosted {
        host: String,
        port: u16,
        name: String,
    },
    Local(std::path::PathBuf),
}

impl FileIdentity {
    /// Hosted file `name` on `host` at `port`.
    pub fn hosted(host: &str, port: u16, name: &str) -> Self {
        Self::Hosted {
            host: host.to_lowercase(),
            port,
            name: hosted_key(name),
        }
    }

    /// The file at `path` on this computer, links followed when it is
    /// there (else the path as given).
    pub fn local(path: &std::path::Path) -> Self {
        Self::Local(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
    }

    /// The file an `fmnet:/host[:port]/Name` text names, port 5003 when it
    /// writes none ([`fmnet_target`]: [`fmnet_name`]'s form and
    /// [`hosted_file_path`]'s); `None` for any other text.
    pub fn from_fmnet(text: &str) -> Option<Self> {
        let (host, port, name) = fmnet_target(text)?;
        Some(Self::hosted(&host, port, &name))
    }
}

pub fn hosted_key(name: &str) -> String {
    hosted_name(name).to_lowercase()
}

fn plain(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

/// Whether `name` is a plain file name: no folder, volume or scheme in
/// it, so it names a file beside the file it is relative to.
pub fn is_plain_name(name: &str) -> bool {
    plain(name)
}

impl FilePath {
    /// The file a relative `file` alternative names (`file:Name`, a plain
    /// name, the stored `("file", "", Name)`), with `.fmp12` added when
    /// absent: on the same host as the file it is relative to, or, for a
    /// file on this computer, in the same folder. `None` for every other
    /// form (folders, volumes, `fmnet`, other schemes).
    pub fn relative_name(&self) -> Option<String> {
        (self.scheme.eq_ignore_ascii_case("file") && self.location.is_empty() && plain(&self.name))
            .then(|| hosted_name(&self.name))
    }

    /// The host and port an `fmnet` alternative names, as stored: the
    /// location is one host, with `:port` or without (then
    /// [`DEFAULT_PORT`]), between two [`LOCATION_SEPARATOR`]s. `None` for
    /// every other scheme or location form, a folder on the host
    /// included (unmeasured). Nothing is looked up: the host is the text.
    pub fn fmnet_endpoint(&self) -> Option<(&str, u16)> {
        if !self.scheme.eq_ignore_ascii_case("fmnet") {
            return None;
        }
        let authority = self
            .location
            .strip_prefix(LOCATION_SEPARATOR)?
            .strip_suffix(LOCATION_SEPARATOR)?;
        if authority.contains(LOCATION_SEPARATOR) || authority.contains('/') {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
                (host, port.parse().ok()?)
            }
            _ => (authority, DEFAULT_PORT),
        };
        (!host.is_empty() && !host.chars().any(char::is_control)).then_some((host, port))
    }

    /// The file this alternative names on the host `host` at `port`, the
    /// host and port a hosted file was opened from: a relative `file`
    /// alternative, or an `fmnet` one naming that host (its text, any case)
    /// and port ([`Self::fmnet_endpoint`]). `None` for anything else, so a
    /// path to another host, another port or a folder is never taken for
    /// a file on this one.
    pub fn same_host_name(&self, host: &str, port: u16) -> Option<String> {
        let relative = self.scheme.eq_ignore_ascii_case("file") && self.location.is_empty();
        let same_host = self.fmnet_endpoint().is_some_and(|(named, named_port)| {
            named.eq_ignore_ascii_case(host) && named_port == port
        });
        ((relative || same_host) && plain(&self.name)).then(|| hosted_name(&self.name))
    }

    /// A path a calculation gives (Open File, a CR/LF list): `fmnet:/host/Name`,
    /// `file:Name` or a plain name, as a stored alternative would hold it.
    pub fn calculated(line: &str) -> Option<Self> {
        let (scheme, location, name) = if let Some(path) = line.strip_prefix("fmnet:/") {
            let (host, name) = path.split_once('/')?;
            (
                "fmnet",
                format!("{LOCATION_SEPARATOR}{host}{LOCATION_SEPARATOR}"),
                name,
            )
        } else if let Some(name) = line.strip_prefix("file:") {
            ("file", String::new(), name)
        } else if plain(line) {
            ("file", String::new(), line)
        } else {
            return None;
        };
        plain(name).then(|| Self {
            scheme: scheme.into(),
            location,
            name: name.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bytes(paths: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut out = (paths.len() as u32).to_be_bytes().to_vec();
        for (scheme, location, name) in paths {
            for text in [scheme, location, name] {
                out.extend(om_scsu::encode_counted(text));
            }
        }
        out
    }
    #[test]
    fn complete_stored_alternatives_are_required() {
        let valid = bytes(&[("file", "", "Peer"), ("fmnet", "other.invalid", "Other")]);
        assert_eq!(decode(&valid).unwrap().len(), 2);
        for end in 0..valid.len() {
            assert!(decode(&valid[..end]).is_none());
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(decode(&trailing).is_none());
        assert!(decode(&1025_u32.to_be_bytes()).is_none());
    }
    #[test]
    fn only_plain_relative_or_exact_endpoint_paths_are_supported() {
        for path in ["file:Peer", "Peer.FMP12", "fmnet:/host.invalid:5100/Peer"] {
            assert!(FilePath::calculated(path)
                .unwrap()
                .same_host_name("host.invalid", 5100)
                .is_some());
        }
        for path in [
            "file:/tmp/Peer",
            "file:../Peer",
            "fmnet:/other.invalid/Peer",
            "fmnet:/host.invalid/Peer",
            "fmp://host.invalid/Peer",
        ] {
            assert!(FilePath::calculated(path)
                .and_then(|p| p.same_host_name("host.invalid", 5100))
                .is_none());
        }
        assert_eq!(
            fmnet_target(&fmnet_name("other.invalid", 5100, "Peer")),
            Some(("other.invalid".into(), 5100, "Peer".into()))
        );
        assert_eq!(
            fmnet_target("fmnet:/other.invalid/Peer"),
            Some(("other.invalid".into(), DEFAULT_PORT, "Peer".into()))
        );
        for name in [
            "Peer",
            "file:Peer",
            "fmnet:/other.invalid/Folder/Peer",
            "fmnet://Peer",
        ] {
            assert_eq!(fmnet_target(name), None, "{name}");
        }
        assert_eq!(file_stem("Peer.FmP12"), "Peer");
        assert_eq!(file_stem("Peer.fmp12.fmp12"), "Peer.fmp12");
        assert_ne!(hosted_key("Peer"), hosted_key("Peer.fmp12.fmp12"));
        assert_eq!(hosted_key("peer.FMP12"), hosted_key("PEER"));
        // A hosted file's own path reads back to its host, port and name.
        for (port, path, endpoint) in [
            (None, "fmnet:/host.invalid/Peer.fmp12", 5003),
            (Some(5100), "fmnet:/host.invalid:5100/Peer.fmp12", 5100),
        ] {
            assert_eq!(hosted_file_path("host.invalid", port, "Peer"), path);
            assert_eq!(
                hosted_file_path("host.invalid", port, "Peer.FMP12").len(),
                path.len()
            );
            let read = FilePath::calculated(path).unwrap();
            assert_eq!(read.fmnet_endpoint(), Some(("host.invalid", endpoint)));
            assert_eq!(read.name, "Peer.fmp12");
            assert_eq!(
                FileIdentity::from_fmnet(path),
                Some(FileIdentity::hosted("HOST.invalid", endpoint, "peer"))
            );
        }
        // One name on two ports of a host is two files; a doubled extension
        // is another name.
        assert_ne!(
            FileIdentity::hosted("host.invalid", 5003, "Peer"),
            FileIdentity::hosted("host.invalid", 5100, "Peer")
        );
        assert_ne!(
            FileIdentity::hosted("host.invalid", 5003, "Peer"),
            FileIdentity::hosted("host.invalid", 5003, "Peer.fmp12.fmp12")
        );
        assert_eq!(FileIdentity::from_fmnet("file:Peer"), None);
        assert_eq!(
            FileIdentity::from_fmnet(&fmnet_name("host.invalid", 5100, "Peer")),
            Some(FileIdentity::hosted("host.invalid", 5100, "Peer.fmp12"))
        );
    }

    /// FileMaker Pro 26.0.3's own data-source path leaves `[32, 5, 1, 82]`,
    /// from two local copies whose one path it saved as
    /// `fmnet:/example.invalid/Related-214.fmp12` and
    /// `fmnet:/example.invalid:5003/Related-214.fmp12` (#759, comment
    /// 6882330541; the files stay private, these are their bytes). Each
    /// names port 5003 of `example.invalid`, the host's text in any case,
    /// and nothing else; a calculation's path reads the same. Other
    /// location forms are not followed.
    #[test]
    fn filemaker_pros_fmnet_paths_name_one_host_and_port() {
        const IMPLICIT: &str = "00000001053c37343f2e114132da3f223b372a363f7433342c3b36333eda11083f363b2e3f3e77686b6e743c372a6b68";
        const EXPLICIT: &str = "00000001053c37343f2e164132da3f223b372a363f7433342c3b36333e606f6a6a69da11083f363b2e3f3e77686b6e743c372a6b68";
        let unhex = |text: &str| -> Vec<u8> {
            (0..text.len() / 2)
                .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
                .collect()
        };
        for (leaf, typed) in [
            (IMPLICIT, "fmnet:/example.invalid/Related-214.fmp12"),
            (EXPLICIT, "fmnet:/example.invalid:5003/Related-214.fmp12"),
        ] {
            let paths = decode(&unhex(leaf)).unwrap();
            let [path] = &paths[..] else {
                panic!("{paths:?}");
            };
            assert_eq!(path.fmnet_endpoint(), Some(("example.invalid", 5003)));
            for (host, port) in [("example.invalid", 5003), ("Example.INVALID", 5003)] {
                assert_eq!(
                    path.same_host_name(host, port).as_deref(),
                    Some("Related-214.fmp12")
                );
            }
            for (host, port) in [
                ("example.invalid", 5100),
                ("other.invalid", 5003),
                ("127.0.0.1", 5003),
            ] {
                assert_eq!(path.same_host_name(host, port), None, "{host}:{port}");
            }
            assert_eq!(path.relative_name(), None);
            assert_eq!(FilePath::calculated(typed).as_ref(), Some(path));
        }
        let sep = LOCATION_SEPARATOR;
        for location in [
            "example.invalid".to_owned(),
            "/example.invalid:5003/".to_owned(),
            format!("{sep}{sep}"),
            format!("{sep}example.invalid{sep}Folder{sep}"),
            format!("{sep}example.invalid:{sep}"),
            format!("{sep}example.invalid:99999{sep}"),
        ] {
            let path = FilePath {
                scheme: "fmnet".into(),
                location,
                name: "Related-214.fmp12".into(),
            };
            assert_eq!(
                path.same_host_name("example.invalid", 5003),
                None,
                "{path:?}"
            );
        }
    }
}
