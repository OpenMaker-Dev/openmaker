//! The server's TLS certificate when the administrator gives none
//! (`--cert`/`--key`): the operating system's own self-signed certificate
//! when there is one the server may read, otherwise a certificate the server
//! makes at its first start, keeps, and uses again at every later start.
//! Either stays the same across restarts, so clients can be told to trust
//! it and a different certificate stands out.
//!
//! - The operating system's certificates are [`SYSTEM_CERTIFICATES`]: on
//!   Debian and Ubuntu the `ssl-cert` package's "snakeoil" certificate,
//!   whose key root and the `ssl-cert` group may read. Other Linux
//!   distributions, macOS and Windows have none a server can use (Fedora
//!   and RHEL's `localhost.crt` is Apache's, made by `mod_ssl`; macOS and
//!   Windows keep their machine identities in the keychain and the
//!   certificate store, not as files). One is used when both of its files
//!   can be read and it is a certificate clients can trust as it is
//!   ([`Identity::check_self_trusted`]); otherwise the reason is logged and
//!   the kept certificate is used.
//! - The kept certificate is `tls/cert.pem`, with its key `tls/key.pem`, in
//!   the server's state folder ([`state_folder`]), not beside the hosted
//!   files: those are copied, backed up and shared, and the certificate
//!   belongs to the server, not to a file. It is made under a folder name of
//!   its own (0700; the key 0600 whatever the umask, created new, never
//!   through a name that exists), written and synced, then renamed to `tls`
//!   in one step: a crash leaves no half identity, and of two servers
//!   starting at once the second finds the first one's and uses it.
//! - The `tls` folder is used only when it is a folder (not a link) of the
//!   server's user or root that nobody else may write, so another local user
//!   cannot plant an identity there.
//! - A kept identity that cannot be read, lacks a file, is not a
//!   certificate with its key, or that clients would refuse (expired, or
//!   not valid yet by this computer's clock) stops the server. It is never
//!   replaced on the server's own: clients were told to trust it, and a
//!   certificate that changes by itself is what this design avoids.
//!   Removing the folder has a new one made at the next start.
//!
//! A made certificate is ECDSA P-256, valid from the day before it is made
//! for [`LIFETIME_YEARS`] years (Debian's snakeoil lasts as long), marked
//! not a CA and for server authentication, and names the hosts it is made
//! for; its names stay as they were made.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use jiff::ToSpan;
use rustls::pki_types::ServerName;

use crate::tls::{Identity, IdentityError};

/// An operating system's own self-signed certificate and its key.
#[derive(Clone, Copy, Debug)]
pub struct SystemCertificate<P = &'static str> {
    pub cert: P,
    pub key: P,
}

/// The operating system's own self-signed certificates, tried in order.
#[cfg(target_os = "linux")]
pub const SYSTEM_CERTIFICATES: &[SystemCertificate] = &[
    // Debian and Ubuntu: made when the ssl-cert package is installed.
    SystemCertificate {
        cert: "/etc/ssl/certs/ssl-cert-snakeoil.pem",
        key: "/etc/ssl/private/ssl-cert-snakeoil.key",
    },
];

/// The operating system's own self-signed certificates: none on this
/// platform.
#[cfg(not(target_os = "linux"))]
pub const SYSTEM_CERTIFICATES: &[SystemCertificate] = &[];

/// How long a certificate the server makes is valid.
pub const LIFETIME_YEARS: i16 = 10;

/// The folder in the state folder holding the kept certificate and key.
const FOLDER: &str = "tls";
const CERT: &str = "cert.pem";
const KEY: &str = "key.pem";

/// The most bytes read from a certificate or key file.
const MAX_FILE: u64 = 1 << 20;

/// Where the server's certificate came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The administrator's, from this file (`--cert`).
    Given(PathBuf),
    /// The operating system's own, from this file.
    System(PathBuf),
    /// Made now and kept in this `tls` folder.
    Made(PathBuf),
    /// Kept in this `tls` folder since an earlier start.
    Kept(PathBuf),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Given(cert) => write!(f, "{} (--cert)", cert.display()),
            Source::System(cert) => write!(f, "the operating system's own, {}", cert.display()),
            Source::Made(folder) => {
                write!(f, "made now and kept as {}", folder.join(CERT).display())
            }
            Source::Kept(folder) => write!(
                f,
                "kept since an earlier start as {}",
                folder.join(CERT).display()
            ),
        }
    }
}

/// Why there is no certificate to serve with.
#[derive(Debug)]
pub enum Error {
    /// No state folder was given and the environment names none.
    NoStateFolder,
    /// A host name to make the certificate for is not one.
    Name(String),
    /// The state folder could not be made, or a new identity written in it.
    Io {
        path: PathBuf,
        error: io::Error,
    },
    /// The kept identity in this `tls` folder, which the server will not
    /// replace on its own.
    Unusable {
        folder: PathBuf,
        problem: String,
    },
    Generate(IdentityError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoStateFolder => write!(
                f,
                "no folder to keep the server's certificate in: give --state-dir, \
                 or --cert and --key"
            ),
            Error::Name(name) => write!(f, "{name:?} is not a host name or IP address"),
            Error::Io { path, error } => write!(f, "{}: {error}", path.display()),
            Error::Unusable { folder, problem } => write!(
                f,
                "the server's certificate kept in {} cannot be used: {problem}. The server \
                 does not replace it on its own, since clients may trust it: remove the \
                 folder to have a new one made at the next start (clients must then be \
                 given the new certificate), or give --cert and --key",
                folder.display()
            ),
            Error::Generate(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Whether `name` is a DNS name or an IP address a certificate can name.
pub fn is_host_name(name: &str) -> bool {
    ServerName::try_from(name).is_ok()
}

/// The folder the server keeps its state in when the administrator names
/// none: systemd's `$STATE_DIRECTORY` (its first folder, from the unit's
/// `StateDirectory=`), otherwise the user's own state folder:
/// `$XDG_STATE_HOME/openmaker-server` or `~/.local/state/openmaker-server`
/// (Linux and other Unix systems), `~/Library/Application Support/OpenMaker
/// Server` (macOS), `%LOCALAPPDATA%\OpenMaker Server` (Windows). `None` when
/// the variables these need are unset or not absolute paths.
pub fn state_folder() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(dirs) = std::env::var_os("STATE_DIRECTORY") {
        if let Some(first) = std::env::split_paths(&dirs).find(|path| path.is_absolute()) {
            return Some(first);
        }
    }
    if cfg!(windows) {
        absolute("LOCALAPPDATA").map(|path| path.join("OpenMaker Server"))
    } else if cfg!(target_os = "macos") {
        absolute("HOME").map(|home| home.join("Library/Application Support/OpenMaker Server"))
    } else {
        absolute("XDG_STATE_HOME")
            .map(|path| path.join("openmaker-server"))
            .or_else(|| absolute("HOME").map(|home| home.join(".local/state/openmaker-server")))
    }
}

/// The server's certificate when the administrator gives none: the first
/// of `system` the server can read and clients can trust as it is,
/// otherwise the one kept in the `state` folder, made for `names` (DNS
/// names and IP addresses) when there is none yet.
pub fn choose<P: AsRef<Path>>(
    system: &[SystemCertificate<P>],
    state: Option<&Path>,
    names: &[String],
) -> Result<(Identity, Source), Error> {
    for candidate in system {
        let cert = candidate.cert.as_ref();
        if let Some(identity) = system_identity(cert, candidate.key.as_ref()) {
            return Ok((identity, Source::System(cert.to_path_buf())));
        }
    }
    kept(state.ok_or(Error::NoStateFolder)?, names)
}

/// The operating system's certificate `cert` with its `key`, when both can
/// be read and clients can trust it; why not is logged when it is there.
fn system_identity(cert: &Path, key: &Path) -> Option<Identity> {
    let cert_pem = match File::open(cert).and_then(read_capped) {
        Ok(pem) => pem,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::info!("not using the system certificate {}: {e}", cert.display());
            return None;
        }
    };
    let key_pem = match File::open(key).and_then(read_capped) {
        Ok(pem) => pem,
        Err(e) => {
            let hint = if e.kind() == io::ErrorKind::PermissionDenied {
                "; the server's user may read it by being in the group that owns it"
            } else {
                ""
            };
            log::info!(
                "not using the system certificate {}: its key {}: {e}{hint}",
                cert.display(),
                key.display()
            );
            return None;
        }
    };
    let checked = Identity::from_pem(&cert_pem, &key_pem)
        .map_err(|e| e.to_string())
        .and_then(|identity| identity.check_self_trusted().map(|()| identity));
    match checked {
        Ok(identity) => Some(identity),
        Err(problem) => {
            log::warn!(
                "not using the system certificate {}: {problem}",
                cert.display()
            );
            None
        }
    }
}

/// The identity kept in `state`, made for `names` when there is none.
fn kept(state: &Path, names: &[String]) -> Result<(Identity, Source), Error> {
    let folder = state.join(FOLDER);
    let io_error = |error| Error::Io {
        path: state.to_path_buf(),
        error,
    };
    let parent = sys::state_folder(state).map_err(io_error)?;
    if let Some(dir) = sys::open_kept(&parent, FOLDER.as_ref()).map_err(|e| unusable(&folder, e))? {
        return Ok((load(&dir, &folder)?, Source::Kept(folder)));
    }
    let made = make(names)?;
    let (temporary, dir) = sys::make_temporary(&parent).map_err(io_error)?;
    let written = sys::write_new(&dir, KEY.as_ref(), made.key_pem.as_bytes(), true)
        .and_then(|()| sys::write_new(&dir, CERT.as_ref(), made.cert_pem.as_bytes(), false))
        .and_then(|()| sys::sync(&dir))
        .and_then(|()| sys::rename(&parent, &temporary, FOLDER.as_ref()));
    match written {
        Ok(()) => {
            // The rename is on disk once the state folder is.
            sys::sync(&parent).map_err(io_error)?;
            Ok((made.identity, Source::Made(folder)))
        }
        Err(error) => {
            sys::remove_temporary(&parent, &temporary);
            // Another server starting at the same time made it first.
            match sys::open_kept(&parent, FOLDER.as_ref()).map_err(|e| unusable(&folder, e))? {
                Some(dir) => Ok((load(&dir, &folder)?, Source::Kept(folder))),
                None => Err(io_error(error)),
            }
        }
    }
}

fn unusable(folder: &Path, problem: impl fmt::Display) -> Error {
    Error::Unusable {
        folder: folder.to_path_buf(),
        problem: problem.to_string(),
    }
}

/// The identity in the kept folder `dir` (shown as `folder`).
fn load(dir: &sys::Dir, folder: &Path) -> Result<Identity, Error> {
    let read = |name: &str| {
        sys::read(dir, name.as_ref()).map_err(|e| unusable(folder, format_args!("{name}: {e}")))
    };
    let identity =
        Identity::from_pem(&read(CERT)?, &read(KEY)?).map_err(|e| unusable(folder, e))?;
    identity
        .check_self_trusted()
        .map_err(|e| unusable(folder, e))?;
    Ok(identity)
}

/// A new identity and the PEM files that hold it.
struct Made {
    identity: Identity,
    cert_pem: String,
    key_pem: String,
}

/// A new self-signed certificate for `names` (see the module docs).
fn make(names: &[String]) -> Result<Made, Error> {
    if let Some(name) = names.iter().find(|name| !is_host_name(name)) {
        return Err(Error::Name(name.clone()));
    }
    let generate = |e| Error::Generate(IdentityError::Generate(e));
    let key = rcgen::KeyPair::generate().map_err(generate)?;
    let mut params = rcgen::CertificateParams::new(names.to_vec()).map_err(generate)?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "OpenMaker Server");
    // From yesterday, so a client whose clock is a little behind accepts it.
    let today = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    let from = today.yesterday().unwrap_or(today);
    let until = from.saturating_add(LIFETIME_YEARS.years());
    let ymd = |date: jiff::civil::Date| {
        // Months and days are 1-based and small; years fit an i32.
        rcgen::date_time_ymd(
            i32::from(date.year()),
            date.month().unsigned_abs(),
            date.day().unsigned_abs(),
        )
    };
    params.not_before = ymd(from);
    params.not_after = ymd(until);
    params.is_ca = rcgen::IsCa::ExplicitNoCa;
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params.self_signed(&key).map_err(generate)?;
    let (cert_pem, key_pem) = (cert.pem(), key.serialize_pem());
    let identity =
        Identity::from_pem(cert_pem.as_bytes(), key_pem.as_bytes()).map_err(Error::Generate)?;
    Ok(Made {
        identity,
        cert_pem,
        key_pem,
    })
}

/// Read a certificate or key file, refusing one larger than [`MAX_FILE`].
fn read_capped(file: File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "larger than a certificate or key",
        ));
    }
    Ok(bytes)
}

fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, why.to_string())
}

/// Folder handles and the calls on them (`openat` and its kin with
/// `O_NOFOLLOW`), as in `scripts/host/files.rs`.
#[cfg(unix)]
mod sys {
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io::{self, Write};
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::DirBuilderExt;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    use rustix::io::Errno;

    pub(super) type Dir = OwnedFd;

    /// A folder below another, never through a symbolic link.
    const FOLDER: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);

    /// The state folder, made private (0700) if missing. Symbolic links in
    /// its path are the administrator's.
    pub(super) fn state_folder(path: &Path) -> io::Result<Dir> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        Ok(fs::open(path, flags, Mode::empty())?)
    }

    /// The kept folder `name` in `parent`, or `None` when there is none.
    /// Refused unless it is a folder of this user or root that nobody else
    /// may write.
    pub(super) fn open_kept(parent: &Dir, name: &OsStr) -> io::Result<Option<Dir>> {
        let dir = match fs::openat(parent, name, FOLDER, Mode::empty()) {
            Ok(dir) => dir,
            Err(Errno::NOENT) => return Ok(None),
            Err(Errno::LOOP | Errno::NOTDIR) => {
                return Err(super::refused("not a folder (a link or a file)"))
            }
            Err(e) => return Err(e.into()),
        };
        let stat = fs::fstat(&dir)?;
        let owner = stat.st_uid;
        if owner != rustix::process::geteuid().as_raw() && owner != 0 {
            return Err(super::refused("not the server's user's folder"));
        }
        if Mode::from_raw_mode(stat.st_mode).intersects(Mode::WGRP | Mode::WOTH) {
            return Err(super::refused("other users may write in it"));
        }
        Ok(Some(dir))
    }

    /// A new private (0700) folder of this user in `parent`, under a name
    /// of its own.
    pub(super) fn make_temporary(parent: &Dir) -> io::Result<(OsString, Dir)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let name = OsString::from(format!(
                ".{}-{}-{}.tmp",
                super::FOLDER,
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::mkdirat(parent, &name, Mode::RWXU) {
                Ok(()) => {}
                Err(Errno::EXIST) => continue,
                Err(e) => return Err(e.into()),
            }
            let dir = fs::openat(parent, &name, FOLDER, Mode::empty())?;
            // Whoever can write in the state folder could have swapped it.
            if fs::fstat(&dir)?.st_uid != rustix::process::geteuid().as_raw() {
                return Err(super::refused("the new folder was replaced"));
            }
            return Ok((name, dir));
        }
        Err(io::ErrorKind::AlreadyExists.into())
    }

    /// Write `bytes` as the new file `name` in `dir` (0600 when `private`,
    /// 0644 otherwise, whatever the umask allows), synced.
    pub(super) fn write_new(
        dir: &Dir,
        name: &OsStr,
        bytes: &[u8],
        private: bool,
    ) -> io::Result<()> {
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mode = if private { 0o600 } else { 0o644 };
        let file = fs::openat(dir, name, flags, Mode::from_bits_truncate(mode))?;
        let mut file = File::from(file);
        file.write_all(bytes)?;
        file.sync_all()
    }

    pub(super) fn sync(dir: &Dir) -> io::Result<()> {
        Ok(fs::fsync(dir)?)
    }

    /// Rename the folder `from` in `parent` to `to`; fails when `to` is a
    /// folder with something in it.
    pub(super) fn rename(parent: &Dir, from: &OsStr, to: &OsStr) -> io::Result<()> {
        Ok(fs::renameat(parent, from, parent, to)?)
    }

    /// Remove the temporary folder `name` and the files written in it.
    pub(super) fn remove_temporary(parent: &Dir, name: &OsStr) {
        if let Ok(dir) = fs::openat(parent, name, FOLDER, Mode::empty()) {
            for file in [super::KEY, super::CERT] {
                let _ = fs::unlinkat(&dir, file, AtFlags::empty());
            }
        }
        let _ = fs::unlinkat(parent, name, AtFlags::REMOVEDIR);
    }

    /// The regular file `name` in `dir`.
    pub(super) fn read(dir: &Dir, name: &OsStr) -> io::Result<Vec<u8>> {
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
        let file = match fs::openat(dir, name, flags, Mode::empty()) {
            Err(Errno::LOOP) => return Err(super::refused("a link")),
            result => result?,
        };
        if FileType::from_raw_mode(fs::fstat(&file)?.st_mode) != FileType::RegularFile {
            return Err(super::refused("not a regular file"));
        }
        super::read_capped(File::from(file))
    }
}

/// Paths instead of folder handles. Windows keeps a folder in the user's
/// `%LOCALAPPDATA%` to that user (and administrators) by its inherited
/// permissions; the modes of the Unix version have no equivalent here.
#[cfg(not(unix))]
mod sys {
    use std::ffi::{OsStr, OsString};
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(super) type Dir = PathBuf;

    pub(super) fn state_folder(path: &Path) -> io::Result<Dir> {
        std::fs::create_dir_all(path)?;
        Ok(path.to_path_buf())
    }

    pub(super) fn open_kept(parent: &Dir, name: &OsStr) -> io::Result<Option<Dir>> {
        let path = parent.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => Ok(Some(path)),
            Ok(_) => Err(super::refused("not a folder (a link or a file)")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(super) fn make_temporary(parent: &Dir) -> io::Result<(OsString, Dir)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let name = OsString::from(format!(
                ".{}-{}-{}.tmp",
                super::FOLDER,
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(parent.join(&name)) {
                Ok(()) => return Ok((name.clone(), parent.join(name))),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::ErrorKind::AlreadyExists.into())
    }

    pub(super) fn write_new(dir: &Dir, name: &OsStr, bytes: &[u8], _: bool) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(name))?;
        file.write_all(bytes)?;
        file.sync_all()
    }

    pub(super) fn sync(_: &Dir) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn rename(parent: &Dir, from: &OsStr, to: &OsStr) -> io::Result<()> {
        std::fs::rename(parent.join(from), parent.join(to))
    }

    pub(super) fn remove_temporary(parent: &Dir, name: &OsStr) {
        let _ = std::fs::remove_dir_all(parent.join(name));
    }

    pub(super) fn read(dir: &Dir, name: &OsStr) -> io::Result<Vec<u8>> {
        let path = dir.join(name);
        if !std::fs::symlink_metadata(&path)?.is_file() {
            return Err(super::refused("not a regular file"));
        }
        super::read_capped(File::open(path)?)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::sync::{Arc, Barrier};

    use super::*;

    /// A folder in the system's temporary directory, removed when dropped.
    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Temp {
            let path = std::env::temp_dir()
                .join(format!("om-default-identity-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Temp(path)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const NO_SYSTEM: &[SystemCertificate] = &[];

    fn names() -> Vec<String> {
        vec!["localhost".into(), "127.0.0.1".into()]
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn listing(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A self-signed certificate and its key, as PEM.
    fn pem_pair(params: rcgen::CertificateParams) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    fn localhost() -> rcgen::CertificateParams {
        rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap()
    }

    #[test]
    fn makes_the_identity_once_and_keeps_it() {
        let state = Temp::new("kept");
        // Servers starting at the same moment agree on one identity.
        let starts = 8;
        let barrier = Arc::new(Barrier::new(starts));
        let threads: Vec<_> = (0..starts)
            .map(|_| {
                let (barrier, state) = (barrier.clone(), state.0.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let (identity, source) = choose(NO_SYSTEM, Some(&state), &names()).unwrap();
                    (identity.fingerprint(), source)
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        let folder = state.0.join(FOLDER);
        let made = results
            .iter()
            .filter(|(_, source)| *source == Source::Made(folder.clone()))
            .count();
        assert_eq!(made, 1, "{results:?}");
        let fingerprint = results[0].0.clone();
        for (other, source) in &results {
            assert_eq!(other, &fingerprint);
            assert!(matches!(source, Source::Made(_) | Source::Kept(_)));
        }

        // A later start uses it again: the files serve the same certificate.
        let (identity, source) = choose(NO_SYSTEM, Some(&state.0), &names()).unwrap();
        assert_eq!(source, Source::Kept(folder.clone()));
        assert_eq!(identity.fingerprint(), fingerprint);
        let cert = std::fs::read(folder.join(CERT)).unwrap();
        let key = std::fs::read(folder.join(KEY)).unwrap();
        assert_eq!(
            Identity::from_pem(&cert, &key).unwrap().fingerprint(),
            fingerprint
        );
        assert!(identity.names("localhost") && identity.names("127.0.0.1"));
        assert!(!identity.names("example.com"));

        // The key is its owner's alone, and nothing else is left behind.
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(mode(&folder.join(KEY)), 0o600);
        assert_eq!(listing(&state.0), [FOLDER]);
        assert_eq!(listing(&folder), [CERT, KEY]);
    }

    #[test]
    fn prefers_the_systems_certificate_it_may_read() {
        let system = Temp::new("system");
        let state = Temp::new("system-state");
        let (cert_pem, key_pem) = pem_pair(localhost());
        let cert = system.0.join("snakeoil.pem");
        let key = system.0.join("snakeoil.key");
        std::fs::write(&cert, &cert_pem).unwrap();
        std::fs::write(&key, &key_pem).unwrap();
        let expected = Identity::from_pem(cert_pem.as_bytes(), key_pem.as_bytes())
            .unwrap()
            .fingerprint();
        let candidates = [
            // Not installed: passed over.
            SystemCertificate {
                cert: system.0.join("absent.pem"),
                key: system.0.join("absent.key"),
            },
            SystemCertificate {
                cert: cert.clone(),
                key: key.clone(),
            },
        ];

        let (identity, source) = choose(&candidates, Some(&state.0), &names()).unwrap();
        assert_eq!(source, Source::System(cert.clone()));
        assert_eq!(identity.fingerprint(), expected);
        assert_eq!(listing(&state.0), Vec::<String>::new());

        // A key the server's user may not read: the kept certificate instead.
        if rustix::process::geteuid().is_root() {
            return; // root reads it regardless of its mode
        }
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (identity, source) = choose(&candidates, Some(&state.0), &names()).unwrap();
        assert_eq!(source, Source::Made(state.0.join(FOLDER)));
        assert_ne!(identity.fingerprint(), expected);
    }

    #[test]
    fn refuses_a_kept_identity_it_cannot_use() {
        let (cert, key) = pem_pair(localhost());
        let (_, other_key) = pem_pair(localhost());
        let mut expired = localhost();
        expired.not_before = rcgen::date_time_ymd(2000, 1, 1);
        expired.not_after = rcgen::date_time_ymd(2001, 1, 1);
        let (expired_cert, expired_key) = pem_pair(expired);
        let elsewhere = Temp::new("elsewhere");
        std::fs::write(elsewhere.0.join(CERT), &cert).unwrap();
        std::fs::write(elsewhere.0.join(KEY), &key).unwrap();

        let cases: [(&str, Option<&str>, Option<&str>, &str); 4] = [
            (
                "mismatch",
                Some(&cert),
                Some(&other_key),
                "not the certificate's",
            ),
            (
                "expired",
                Some(&expired_cert),
                Some(&expired_key),
                "expired",
            ),
            ("no-key", Some(&cert), None, "key.pem"),
            ("link", None, None, "not a folder"),
        ];
        for (tag, cert, key, problem) in cases {
            let state = Temp::new(tag);
            let folder = state.0.join(FOLDER);
            if tag == "link" {
                std::os::unix::fs::symlink(&elsewhere.0, &folder).unwrap();
            } else {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&folder)
                    .unwrap();
            }
            for (name, pem) in [(CERT, cert), (KEY, key)] {
                if let Some(pem) = pem {
                    std::fs::write(folder.join(name), pem).unwrap();
                }
            }
            let before = (listing(&state.0), listing(&folder));

            let error = choose(NO_SYSTEM, Some(&state.0), &names()).unwrap_err();
            match &error {
                Error::Unusable {
                    folder: f,
                    problem: p,
                } => {
                    assert_eq!(f, &folder, "{tag}");
                    assert!(p.contains(problem), "{tag}: {p}");
                }
                other => panic!("{tag}: {other}"),
            }
            // Left as it was for the administrator.
            assert_eq!((listing(&state.0), listing(&folder)), before, "{tag}");
        }
    }
}
