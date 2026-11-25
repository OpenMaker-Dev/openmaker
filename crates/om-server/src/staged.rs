//! A new file written in a folder and given its name: an export
//! (docs/server-storage.md 5, "Exports") and the desktop's Save a Copy as.
//! Two ways:
//!
//! - **Unnamed, then linked.** On Linux with `/proc` mounted, on a file
//!   system that makes unnamed files (`O_TMPFILE`: ext2, ext3, ext4, XFS,
//!   Btrfs, tmpfs and others), the file is made with no name, written,
//!   flushed, and given its name by a link from its descriptor (`linkat`
//!   through `/proc/self/fd`), which fails when the name is taken. It is
//!   complete under its name or absent; it never replaces an entry.
//! - **Under its name**, everywhere else, as copies were made before
//!   exports: the file is created under its final name, only where nothing
//!   is, and written there. That covers other systems (macOS), file systems
//!   without unnamed files, and, for an export ([`Staged::new`]), a file
//!   system that refuses the link because it has no hard links, where the
//!   unnamed file, complete, is copied into the newly created name, a part
//!   at a time, stopping when asked. A crash, a stop or a failure while it
//!   is written can leave a partial file under its name, as before. The
//!   desktop's copy ([`Staged::unnamed`]) is never made this way: a refused
//!   link is its error, and nothing is under the name, as before exports.
//!
//! Nothing here removes a name. After the name is given the folder is
//! flushed; when that fails the file stays complete under its name and the
//! caller hears that the flush failed ([`Named::NotFlushed`]): whether the
//! name survives a crash is then not established.
//!
//! Systems without folder descriptors here (Windows) make no file: the
//! call fails as unsupported.

#[cfg(not(unix))]
pub(crate) use other::{is_stopped, Staged};
#[cfg(unix)]
pub(crate) use unix::{is_stopped, Staged};

/// How a published file's folder flush went.
#[derive(Debug)]
pub(crate) enum Named {
    Flushed,
    /// The file is complete under its name; flushing the folder failed.
    NotFlushed(std::io::Error),
}

#[cfg(unix)]
mod unix {
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};

    use rustix::fs::{self, AtFlags, Mode, OFlags};
    use rustix::io::Errno;

    use super::Named;

    /// How many bytes the copy into a name copies between two looks at
    /// whether to stop: a backup's part.
    const PART: u64 = 8 << 20;

    /// A file being written for a name in a folder (the module doc).
    pub(crate) struct Staged {
        file: File,
        /// The folder, opened to read so that it can be flushed.
        folder: OwnedFd,
        name: OsString,
        /// `/proc/self/fd`, for an unnamed file: it is linked from there.
        descriptors: Option<OwnedFd>,
        /// A refused link for lack of hard links makes the file under its
        /// name by a copy (an export), rather than failing (the desktop).
        copy_if_unlinked: bool,
        /// The link is to be refused as a file system without hard links
        /// refuses it (tests).
        no_links: bool,
        /// The folder flush is to fail (tests).
        #[cfg(test)]
        fail_flush: bool,
    }

    /// `folder` opened again to read, so that it can be flushed (a
    /// search-only handle cannot be).
    fn reopen(folder: BorrowedFd<'_>) -> io::Result<OwnedFd> {
        Ok(fs::openat(
            folder,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?)
    }

    /// `/proc/self/fd`, when it is procfs.
    #[cfg(target_os = "linux")]
    fn descriptors() -> Option<OwnedFd> {
        let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fds = fs::open("/proc/self/fd", flags, Mode::empty()).ok()?;
        (fs::fstatfs(&fds).ok()?.f_type == fs::PROC_SUPER_MAGIC).then_some(fds)
    }

    /// An unnamed owner-only file in `folder`, `None` where its file system
    /// makes none.
    #[cfg(target_os = "linux")]
    fn unnamed(folder: &OwnedFd) -> io::Result<Option<File>> {
        match fs::openat(
            folder,
            ".",
            OFlags::TMPFILE | OFlags::RDWR | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(file) => Ok(Some(file.into())),
            Err(Errno::INVAL | Errno::OPNOTSUPP | Errno::ISDIR | Errno::NOENT | Errno::NOSYS) => {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// The new owner-only file `name` in `folder`, for reading and writing,
    /// only where nothing is ([`io::ErrorKind::AlreadyExists`]), never
    /// through a link at the name.
    fn create(folder: &OwnedFd, name: &OsStr) -> io::Result<File> {
        let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
        Ok(fs::openat(
            folder,
            name,
            flags | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?
        .into())
    }

    impl Staged {
        /// A new file for `name` in `folder`: unnamed where the system can
        /// make one, else `name` itself, created now
        /// ([`io::ErrorKind::AlreadyExists`] when it is taken).
        pub(crate) fn new(folder: BorrowedFd<'_>, name: &OsStr) -> io::Result<Staged> {
            let folder = reopen(folder)?;
            #[cfg(target_os = "linux")]
            if let Some(descriptors) = descriptors() {
                if let Some(file) = unnamed(&folder)? {
                    let mut staged = Staged::of(file, folder, name, Some(descriptors));
                    staged.copy_if_unlinked = true;
                    return Ok(staged);
                }
            }
            let file = create(&folder, name)?;
            Ok(Staged::of(file, folder, name, None))
        }

        /// An unnamed file for `name` in `folder` and nothing else, refused
        /// as unsupported where the system cannot make one or link it: the
        /// desktop's copies, which are made no other way.
        #[cfg(target_os = "linux")]
        pub(crate) fn unnamed(folder: BorrowedFd<'_>, name: &OsStr) -> io::Result<Staged> {
            let unsupported = |why| io::Error::new(io::ErrorKind::Unsupported, why);
            let folder = reopen(folder)?;
            let descriptors =
                descriptors().ok_or_else(|| unsupported("copy publication needs /proc/self/fd"))?;
            let file = unnamed(&folder)?.ok_or_else(|| {
                unsupported("the copy folder must support unnamed temporary files")
            })?;
            Ok(Staged::of(file, folder, name, Some(descriptors)))
        }

        fn of(file: File, folder: OwnedFd, name: &OsStr, descriptors: Option<OwnedFd>) -> Self {
            Staged {
                file,
                folder,
                name: name.to_owned(),
                descriptors,
                copy_if_unlinked: false,
                no_links: false,
                #[cfg(test)]
                fail_flush: false,
            }
        }

        /// Refuse the link as a file system without hard links refuses it
        /// (tests: no file system the tests run on does).
        #[cfg_attr(not(debug_assertions), allow(dead_code))]
        pub(crate) fn refuse_links(&mut self) {
            self.no_links = true;
        }

        /// The file being written.
        pub(crate) fn file(&self) -> &File {
            &self.file
        }

        pub(crate) fn file_mut(&mut self) -> &mut File {
            &mut self.file
        }

        /// Flush the file and give it its name, never over an entry there
        /// ([`io::ErrorKind::AlreadyExists`]), then flush the folder (the
        /// module doc); the file under the name. A link refused for lack of
        /// hard links is the error of a file made by [`Staged::unnamed`] (the
        /// desktop's: nothing is then under the name); a file made by
        /// [`Staged::new`] (an export's) is then copied into its name a part
        /// at a time, asking `stop` before each, and a stop is an error
        /// [`is_stopped`] recognizes. On an error before the name is given,
        /// nothing has the name but a file created under it.
        pub(crate) fn publish(self, stop: &dyn Fn() -> bool) -> io::Result<(File, Named)> {
            self.file.sync_all()?;
            let mut named = None;
            if let Some(descriptors) = &self.descriptors {
                let linked = if self.no_links {
                    Err(Errno::PERM)
                } else {
                    fs::linkat(
                        descriptors,
                        self.file.as_raw_fd().to_string(),
                        &self.folder,
                        &self.name,
                        AtFlags::SYMLINK_FOLLOW,
                    )
                };
                match linked {
                    Ok(()) => {}
                    // No hard links on this file system: the complete file
                    // is copied into its name, created only where nothing
                    // is.
                    Err(Errno::PERM | Errno::OPNOTSUPP | Errno::NOSYS) if self.copy_if_unlinked => {
                        if stop() {
                            return Err(stopped());
                        }
                        let mut copy = create(&self.folder, &self.name)?;
                        let mut from = &self.file;
                        io::Seek::seek(&mut from, io::SeekFrom::Start(0))?;
                        loop {
                            if stop() {
                                return Err(stopped());
                            }
                            if io::copy(&mut io::Read::take(from, PART), &mut copy)? == 0 {
                                break;
                            }
                        }
                        copy.set_permissions(self.file.metadata()?.permissions())?;
                        copy.sync_all()?;
                        named = Some(copy);
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            #[cfg(test)]
            let flushed = if self.fail_flush {
                Err(Errno::IO)
            } else {
                fs::fsync(&self.folder)
            };
            #[cfg(not(test))]
            let flushed = fs::fsync(&self.folder);
            let flushed = match flushed {
                Ok(()) => Named::Flushed,
                Err(e) => Named::NotFlushed(e.into()),
            };
            Ok((named.unwrap_or(self.file), flushed))
        }
    }

    /// The error of a copy into a name stopped by its caller.
    #[derive(Debug)]
    struct Stopped;

    impl std::fmt::Display for Stopped {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("stopped")
        }
    }

    impl std::error::Error for Stopped {}

    fn stopped() -> io::Error {
        io::Error::other(Stopped)
    }

    /// Whether `e` is a stop [`Staged::publish`] was asked for.
    pub(crate) fn is_stopped(e: &io::Error) -> bool {
        e.get_ref().is_some_and(|inner| inner.is::<Stopped>())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A folder in the build's target folder, removed when dropped.
        struct Scratch(std::path::PathBuf);

        impl Scratch {
            fn new(tag: &str) -> Scratch {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/om-staged-tests")
                    .join(format!("{tag}-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).unwrap();
                Scratch(path)
            }

            fn folder(&self) -> OwnedFd {
                let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
                fs::open(&self.0, flags, Mode::empty()).unwrap()
            }

            fn names(&self) -> Vec<String> {
                let mut names: Vec<String> = std::fs::read_dir(&self.0)
                    .unwrap()
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names
            }
        }

        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        /// How a file for a name is made.
        type Make<'a> = Box<dyn Fn(&str) -> io::Result<Staged> + 'a>;

        /// Each way of making a file for a name, made when called: unnamed
        /// and linked (where this file system has unnamed files), unnamed
        /// with links refused, and under its name.
        fn ways(scratch: &Scratch) -> Vec<(&'static str, Make<'_>)> {
            let mut ways: Vec<(&'static str, Make<'_>)> = Vec::new();
            #[cfg(target_os = "linux")]
            {
                let folder = scratch.folder();
                let folder = std::os::fd::AsFd::as_fd(&folder);
                if Staged::unnamed(folder, OsStr::new("probe")).is_ok() {
                    ways.push((
                        "unnamed",
                        Box::new(|name| {
                            let folder = scratch.folder();
                            Staged::unnamed(std::os::fd::AsFd::as_fd(&folder), OsStr::new(name))
                        }),
                    ));
                    ways.push((
                        "an export's unnamed file, links refused",
                        Box::new(|name| {
                            let folder = scratch.folder();
                            let mut staged =
                                Staged::new(std::os::fd::AsFd::as_fd(&folder), OsStr::new(name))?;
                            assert!(staged.descriptors.is_some());
                            staged.refuse_links();
                            Ok(staged)
                        }),
                    ));
                }
            }
            ways.push((
                "under its name",
                Box::new(|name| {
                    let folder = scratch.folder();
                    let file = create(&folder, OsStr::new(name))?;
                    Ok(Staged::of(file, folder, OsStr::new(name), None))
                }),
            ));
            ways
        }

        /// Published, a file is complete under its name and nothing else is
        /// there, also where links are refused; where the name is taken,
        /// the entry there is left as it was and nothing else is made; a
        /// folder flush that fails leaves the file complete and says so.
        #[test]
        fn a_file_is_named_complete_and_never_replaces_an_entry() {
            use std::io::Write as _;
            let scratch = Scratch::new("named");
            let ways = ways(&scratch);
            assert!(ways.len() >= 2, "{}", ways.len());
            for (way, make) in &ways {
                let mut staged = make("final").unwrap();
                staged.file_mut().write_all(b"staged").unwrap();
                assert!(
                    matches!(staged.publish(&|| false).unwrap().1, Named::Flushed),
                    "{way}"
                );
                assert_eq!(scratch.names(), ["final"], "{way}");
                assert_eq!(std::fs::read(scratch.0.join("final")).unwrap(), b"staged");
                std::fs::remove_file(scratch.0.join("final")).unwrap();
            }
            std::fs::write(scratch.0.join("taken"), b"theirs").unwrap();
            for (way, make) in &ways {
                let refused = match make("taken") {
                    Err(e) => e,
                    Ok(mut staged) => {
                        staged.file_mut().write_all(b"staged").unwrap();
                        staged.publish(&|| false).map(|_| ()).unwrap_err()
                    }
                };
                assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists, "{way}");
                assert_eq!(scratch.names(), ["taken"], "{way}");
                assert_eq!(std::fs::read(scratch.0.join("taken")).unwrap(), b"theirs");
            }
            for (way, make) in &ways {
                let mut staged = make("unflushed").unwrap();
                staged.file_mut().write_all(b"staged").unwrap();
                staged.fail_flush = true;
                assert!(
                    matches!(staged.publish(&|| false).unwrap().1, Named::NotFlushed(_)),
                    "{way}"
                );
                assert_eq!(scratch.names(), ["taken", "unflushed"], "{way}");
                assert_eq!(
                    std::fs::read(scratch.0.join("unflushed")).unwrap(),
                    b"staged"
                );
                std::fs::remove_file(scratch.0.join("unflushed")).unwrap();
            }
            // The desktop's copy is linked or fails: a refused link is its
            // error, and nothing is under the name.
            #[cfg(target_os = "linux")]
            if let Ok(mut desktop) = Staged::unnamed(
                std::os::fd::AsFd::as_fd(&scratch.folder()),
                OsStr::new("desktop"),
            ) {
                desktop.file_mut().write_all(b"staged").unwrap();
                desktop.refuse_links();
                let refused = desktop.publish(&|| false).map(|_| ()).unwrap_err();
                assert_eq!(refused.raw_os_error(), Some(Errno::PERM.raw_os_error()));
                assert_eq!(scratch.names(), ["taken"]);
            }
            // An export's copy into its name stops when asked, before it
            // makes the name.
            let folder = scratch.folder();
            let mut stopped =
                Staged::new(std::os::fd::AsFd::as_fd(&folder), OsStr::new("stopped")).unwrap();
            if stopped.descriptors.is_some() {
                stopped.file_mut().write_all(b"staged").unwrap();
                stopped.refuse_links();
                let refused = stopped.publish(&|| true).map(|_| ()).unwrap_err();
                assert!(is_stopped(&refused), "{refused}");
                assert_eq!(scratch.names(), ["taken"]);
            }
        }
    }
}

/// On systems without folder descriptors here, no file is made.
#[cfg(not(unix))]
mod other {
    use std::fs::File;
    use std::io;

    use super::Named;

    pub(crate) struct Staged {
        file: File,
    }

    impl Staged {
        pub(crate) fn unsupported() -> io::Error {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "a copy is made only where folders have descriptors (not on Windows)",
            )
        }

        pub(crate) fn refuse_links(&mut self) {}

        pub(crate) fn file(&self) -> &File {
            &self.file
        }

        pub(crate) fn file_mut(&mut self) -> &mut File {
            &mut self.file
        }

        pub(crate) fn publish(self, _stop: &dyn Fn() -> bool) -> io::Result<(File, Named)> {
            Err(Self::unsupported())
        }
    }

    pub(crate) fn is_stopped(_e: &io::Error) -> bool {
        false
    }
}
