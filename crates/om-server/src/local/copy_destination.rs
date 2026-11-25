//! Linux local-copy publication. The destination is checked before a script
//! can commit its record, and the copy has no pathname until publication.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path};

use log::warn;
use rustix::fs::{self, AtFlags, Mode, OFlags};
use rustix::io::Errno;

use crate::staged::{Named, Staged};

const EXT_SUPER_MAGIC: u64 = 0xef53; // ext2, ext3 and ext4
const TMPFS_MAGIC: u64 = 0x0102_1994;

fn unsupported(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, reason)
}

fn directory_stat(dir: &OwnedFd) -> io::Result<fs::Stat> {
    if !matches!(
        fs::fstatfs(dir)?.f_type as u64,
        EXT_SUPER_MAGIC | TMPFS_MAGIC
    ) {
        return Err(unsupported(
            "copy destinations require ext2, ext3, ext4 or tmpfs throughout the directory path",
        ));
    }
    Ok(fs::fstat(dir)?)
}

/// A new unnamed file on the output filesystem, to be linked to a
/// previously unused name (`crate::staged`, which exports share).
pub(super) struct Destination {
    staged: Staged,
}

impl Destination {
    pub(super) fn open(to: &Path) -> io::Result<Self> {
        let name = to
            .file_name()
            .ok_or_else(|| unsupported("the copy needs a file name"))?;
        // The ASCII extension stays distinct from .omsave under ext4's
        // Unicode case folding and normalization as well as byte lookup.
        if !name.as_encoded_bytes().is_ascii()
            || !to
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("fmp12"))
        {
            return Err(unsupported("copy names must be ASCII and end in .fmp12"));
        }
        let path = std::path::absolute(to)?;
        let parent_path = path.parent().expect("absolute path with a file name");
        let uid = rustix::process::geteuid().as_raw();
        let walk = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut parent = fs::open("/", walk, Mode::empty())?;
        let mut stat = directory_stat(&parent)?;
        for component in parent_path.components() {
            let name = match component {
                Component::RootDir => continue,
                Component::Normal(name) => name,
                _ => return Err(unsupported("copy paths cannot contain parent traversal")),
            };
            // A trusted sticky ancestor (for example /tmp) protects its
            // trusted-owned children. With POSIX ACLs the group mode bits
            // include the effective mask for named users and groups.
            if (stat.st_uid != 0 && stat.st_uid != uid)
                || (stat.st_mode & 0o022 != 0 && stat.st_mode & 0o1000 == 0)
            {
                return Err(unsupported("a copy path crosses a shared directory"));
            }
            parent = fs::openat(&parent, name, walk, Mode::empty()).map_err(|e| match e {
                Errno::NOENT | Errno::NOTDIR | Errno::LOOP => {
                    unsupported("copy folders must already exist without symbolic links")
                }
                _ => e.into(),
            })?;
            stat = directory_stat(&parent)?;
        }
        if stat.st_uid != uid || stat.st_mode & 0o722 != 0o700 {
            return Err(unsupported(
                "the copy folder must be owned by this user, with owner access and no group or other write access",
            ));
        }
        // Use the already opened directory, with an fd that can be synced.
        let parent = fs::openat(
            &parent,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        match fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
            Err(Errno::NOENT) => {}
            Err(e) => return Err(e.into()),
        }
        let staged = Staged::unnamed(parent.as_fd(), name)?;
        // Creation mode bounds any inherited POSIX ACL from the start.
        // Restore the owner bits if the ACL or umask narrowed them.
        let private = Mode::RUSR | Mode::WUSR;
        fs::fchmod(staged.file(), private)?;
        let stat = fs::fstat(staged.file())?;
        if stat.st_uid != uid || stat.st_mode & 0o777 != 0o600 {
            return Err(unsupported("the copy must be accessible only to its owner"));
        }
        Ok(Self { staged })
    }

    /// The copy, written by the caller from its first byte.
    pub(super) fn file_mut(&mut self) -> &mut File {
        self.staged.file_mut()
    }

    /// Flush the copy, link it to its name, never over an entry there,
    /// and flush the folder. A folder that cannot be flushed is logged and
    /// the copy kept.
    pub(super) fn publish(self) -> io::Result<()> {
        if let (_, Named::NotFlushed(error)) = self.staged.publish()? {
            warn!("saved a copy, but flushing its folder failed: {error}");
        }
        Ok(())
    }
}
