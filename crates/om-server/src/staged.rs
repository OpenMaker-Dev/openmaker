//! A new file written in a folder without its name, and given its name only
//! once it is complete and flushed: an export (docs/server-storage.md 5,
//! "Exports") and the desktop's Save a Copy as.
//!
//! **Unnamed** where the system has it: Linux with `/proc` mounted, on a
//! file system that makes unnamed files (`O_TMPFILE`: ext2, ext3, ext4,
//! XFS, Btrfs, tmpfs and others; not NFS before 4.2 servers, FUSE file
//! systems that lack it, or vfat). The file is made in its folder with no
//! name, written, flushed, and given its name by a link from its
//! descriptor (`linkat` through `/proc/self/fd`), which fails when the name
//! is taken. Nothing has a name before the file is complete, so there is no
//! temporary name to collide, to be too long, to be swapped or to remove.
//!
//! **Named** elsewhere (other Unix systems such as macOS, and Linux file
//! systems without unnamed files): the file is made under a temporary name
//! of its own, `.omstage-<process>-<n>`, short whatever the final name,
//! only where nothing is (an entry there is an error and is never removed),
//! written and flushed, then given its final name by a hard link, which
//! fails when the name is taken, and its temporary name removed. Where
//! `/proc/self/fd` exists the link is made from the descriptor. Elsewhere
//! it is made from the temporary name, and the final name is then checked
//! to be the file written: an entry another program swaps under the
//! temporary name between the two calls is what gets linked, and the call
//! fails, leaving it, since it cannot show that it made it.
//!
//! **Failures.** Before the link nothing is under the final name: an
//! unnamed file is gone once it is closed, and a temporary name is removed
//! while it is still the file made. After the link, a folder flush that
//! fails removes the final name while it is still the file made, and the
//! call fails. Each such removal is a look at the entry, then its removal:
//! an entry swapped in between by another program is what is removed. Each
//! removal is followed by a folder flush; a removal or flush that fails is
//! logged.
//!
//! Systems without folder descriptors here (Windows) stage nothing: the
//! call fails as unsupported.

#[cfg(not(unix))]
pub(crate) use other::Staged;
#[cfg(unix)]
pub(crate) use unix::Staged;

#[cfg(unix)]
mod unix {
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
    use std::sync::atomic::{AtomicU64, Ordering};

    use log::warn;
    use rustix::fs::{self, AtFlags, Mode, OFlags};

    /// Numbers temporary names within this process.
    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// A file being written in a folder (the module doc).
    pub(crate) struct Staged {
        file: File,
        /// The folder, opened to read so that it can be flushed.
        folder: OwnedFd,
        /// `/proc/self/fd`, to link the file from its descriptor.
        descriptors: Option<OwnedFd>,
        /// The file's temporary name, while it has one.
        temporary: Option<OsString>,
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
    fn descriptors() -> Option<OwnedFd> {
        #[cfg(target_os = "linux")]
        {
            let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let fds = fs::open("/proc/self/fd", flags, Mode::empty()).ok()?;
            (fs::fstatfs(&fds).ok()?.f_type == fs::PROC_SUPER_MAGIC).then_some(fds)
        }
        #[cfg(not(target_os = "linux"))]
        None
    }

    /// An unnamed owner-only file in `folder`, `None` where its file system
    /// makes none.
    #[cfg(target_os = "linux")]
    fn unnamed(folder: &OwnedFd) -> io::Result<Option<File>> {
        use rustix::io::Errno;
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

    impl Staged {
        /// A new file in `folder`: unnamed where the system can make one,
        /// else under a temporary name of its own.
        pub(crate) fn new(folder: BorrowedFd<'_>) -> io::Result<Staged> {
            let folder = reopen(folder)?;
            let descriptors = descriptors();
            #[cfg(target_os = "linux")]
            if descriptors.is_some() {
                if let Some(file) = unnamed(&folder)? {
                    return Ok(Staged {
                        file,
                        folder,
                        descriptors,
                        temporary: None,
                    });
                }
            }
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let temporary = OsString::from(format!(".omstage-{}-{n}", std::process::id()));
            Self::named(folder, temporary, descriptors)
        }

        /// An unnamed file in `folder` and nothing else, refused as
        /// unsupported where the system cannot make one or link it: the
        /// desktop's copies, which are made nowhere else.
        #[cfg(target_os = "linux")]
        pub(crate) fn unnamed(folder: BorrowedFd<'_>) -> io::Result<Staged> {
            let unsupported = |why| io::Error::new(io::ErrorKind::Unsupported, why);
            let folder = reopen(folder)?;
            let descriptors =
                descriptors().ok_or_else(|| unsupported("copy publication needs /proc/self/fd"))?;
            let file = unnamed(&folder)?.ok_or_else(|| {
                unsupported("the copy folder must support unnamed temporary files")
            })?;
            Ok(Staged {
                file,
                folder,
                descriptors: Some(descriptors),
                temporary: None,
            })
        }

        /// A new owner-only file in `folder` (opened to read) named
        /// `temporary`, only where nothing is: an entry there is
        /// [`io::ErrorKind::AlreadyExists`] and is left as it is.
        pub(crate) fn named(
            folder: OwnedFd,
            temporary: OsString,
            descriptors: Option<OwnedFd>,
        ) -> io::Result<Staged> {
            let file = fs::openat(
                &folder,
                &temporary,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?;
            Ok(Staged {
                file: file.into(),
                folder,
                descriptors,
                temporary: Some(temporary),
            })
        }

        /// The file being written.
        pub(crate) fn file(&self) -> &File {
            &self.file
        }

        pub(crate) fn file_mut(&mut self) -> &mut File {
            &mut self.file
        }

        /// Flush the file, give it `name`, never over an entry there
        /// ([`io::ErrorKind::AlreadyExists`]), and flush the folder (the
        /// module doc). On an error nothing this call can show it made is
        /// left.
        pub(crate) fn publish(mut self, name: &OsStr) -> io::Result<()> {
            self.file.sync_all()?;
            let linked = match (&self.descriptors, &self.temporary) {
                (Some(descriptors), _) => fs::linkat(
                    descriptors,
                    self.file.as_raw_fd().to_string(),
                    &self.folder,
                    name,
                    AtFlags::SYMLINK_FOLLOW,
                ),
                (None, Some(temporary)) => fs::linkat(
                    &self.folder,
                    temporary,
                    &self.folder,
                    name,
                    AtFlags::empty(),
                ),
                (None, None) => unreachable!("an unnamed file is made only with /proc/self/fd"),
            };
            linked?;
            if !self.is_entry(name) {
                return Err(io::Error::other(
                    "another entry was swapped in under the temporary name and was linked under \
                     the final name; it is left there",
                ));
            }
            self.remove_temporary();
            if let Err(e) = fs::fsync(&self.folder) {
                self.remove(name);
                return Err(e.into());
            }
            Ok(())
        }

        /// Whether entry `name` of the folder is the file.
        fn is_entry(&self, name: &OsStr) -> bool {
            let (Ok(entry), Ok(file)) = (
                fs::statat(&self.folder, name, AtFlags::SYMLINK_NOFOLLOW),
                fs::fstat(&self.file),
            ) else {
                return false;
            };
            (entry.st_dev, entry.st_ino) == (file.st_dev, file.st_ino)
        }

        /// Remove entry `name` while it is the file, then flush the folder;
        /// a failure is logged.
        fn remove(&self, name: &OsStr) {
            if !self.is_entry(name) {
                return;
            }
            let removed = fs::unlinkat(&self.folder, name, AtFlags::empty())
                .and_then(|()| fs::fsync(&self.folder));
            if let Err(e) = removed {
                warn!(
                    "could not remove {} after a failed copy: {e}",
                    name.to_string_lossy()
                );
            }
        }

        /// Remove the temporary name, if the file has one.
        fn remove_temporary(&mut self) {
            if let Some(temporary) = self.temporary.take() {
                self.remove(&temporary);
            }
        }
    }

    impl Drop for Staged {
        fn drop(&mut self) {
            self.remove_temporary();
        }
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

        /// How a stage is made.
        type Make<'a> = Box<dyn Fn() -> Staged + 'a>;

        /// Each way of staging, made when called: unnamed (where this file
        /// system has it), named and linked from the descriptor, named and
        /// linked from its name.
        fn ways(scratch: &Scratch) -> Vec<(&'static str, Make<'_>)> {
            let mut ways: Vec<(&'static str, Make<'_>)> = Vec::new();
            #[cfg(target_os = "linux")]
            if Staged::unnamed(std::os::fd::AsFd::as_fd(&scratch.folder())).is_ok() {
                ways.push((
                    "unnamed",
                    Box::new(|| {
                        Staged::unnamed(std::os::fd::AsFd::as_fd(&scratch.folder())).unwrap()
                    }),
                ));
            }
            let named = |n: &'static str, proc: bool| -> Make<'_> {
                Box::new(move || {
                    Staged::named(
                        scratch.folder(),
                        OsString::from(format!(".omstage-test-{n}")),
                        if proc { descriptors() } else { None },
                    )
                    .unwrap()
                })
            };
            if descriptors().is_some() {
                ways.push((
                    "named, linked from its descriptor",
                    named("descriptor", true),
                ));
            }
            ways.push(("named, linked from its name", named("name", false)));
            ways
        }

        /// Published, a file is under its name, complete, and nothing else
        /// is left; where the name is taken, publication fails, the entry
        /// there is untouched, and nothing else is left.
        #[test]
        fn a_staged_file_is_named_only_where_nothing_is() {
            let scratch = Scratch::new("publish");
            for (way, make) in ways(&scratch) {
                use std::io::Write as _;
                let mut staged = make();
                staged.file_mut().write_all(b"staged").unwrap();
                staged.publish(OsStr::new("final")).unwrap();
                assert_eq!(scratch.names(), ["final"], "{way}");
                assert_eq!(std::fs::read(scratch.0.join("final")).unwrap(), b"staged");
                std::fs::remove_file(scratch.0.join("final")).unwrap();
            }
            std::fs::write(scratch.0.join("taken"), b"theirs").unwrap();
            for (way, make) in ways(&scratch) {
                use std::io::Write as _;
                let mut staged = make();
                staged.file_mut().write_all(b"staged").unwrap();
                let refused = staged.publish(OsStr::new("taken")).unwrap_err();
                assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists, "{way}");
                assert_eq!(scratch.names(), ["taken"], "{way}");
                assert_eq!(std::fs::read(scratch.0.join("taken")).unwrap(), b"theirs");
            }
            // Dropped unpublished: nothing is left.
            for (way, make) in ways(&scratch) {
                drop(make());
                assert_eq!(scratch.names(), ["taken"], "{way}");
            }
            // An entry at the temporary name is an error and stays.
            std::fs::write(scratch.0.join(".omstage-test-in-the-way"), b"theirs").unwrap();
            let in_the_way = Staged::named(
                scratch.folder(),
                OsString::from(".omstage-test-in-the-way"),
                None,
            );
            assert_eq!(
                in_the_way.err().map(|e| e.kind()),
                Some(io::ErrorKind::AlreadyExists)
            );
            assert_eq!(scratch.names(), [".omstage-test-in-the-way", "taken"]);
        }
    }
}

/// On systems without folder descriptors here, nothing is staged.
#[cfg(not(unix))]
mod other {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io;

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

        pub(crate) fn file(&self) -> &File {
            &self.file
        }

        pub(crate) fn file_mut(&mut self) -> &mut File {
            &mut self.file
        }

        pub(crate) fn publish(self, _name: &OsStr) -> io::Result<()> {
            Err(Self::unsupported())
        }
    }
}
