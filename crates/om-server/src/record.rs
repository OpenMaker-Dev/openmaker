//! Recording connections for analysis.
//!
//! Each connection is written to its own `conn-NNN` directory as the raw GIOP
//! streams `c2s.bin` (client to server) and `s2c.bin`, the layout of the
//! captures under `re/captures/`, so the same tools and tests read both.
//! Fragmented client messages are stored joined. The streams contain the
//! client's license key and obscured credentials, so the files are created
//! readable by the owner only.
//!
//! A server started again over the same folder numbers its connections after
//! the highest `conn-NNN` already there, and a number taken meanwhile (by
//! another server recording into the folder) is skipped, so a recording is
//! never lost to an existing folder.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The folder connections are recorded in, and the next number to use.
#[derive(Debug)]
pub(crate) struct Recordings {
    dir: PathBuf,
    next: AtomicUsize,
}

impl Recordings {
    /// Record into `dir`, created if missing, after its highest `conn-NNN`.
    pub fn open(dir: &Path) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut next = 0;
        for entry in std::fs::read_dir(dir)? {
            let name = entry?.file_name();
            if let Some(index) = name.to_str().and_then(connection_index) {
                next = next.max(index.saturating_add(1));
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            next: AtomicUsize::new(next),
        })
    }

    /// The number the next connection gets, unless it is taken meanwhile.
    pub fn next_index(&self) -> usize {
        self.next.load(Ordering::Relaxed)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Start recording a connection from `peer` in a new `conn-NNN`,
    /// returning its number.
    pub fn start(&self, peer: &str) -> io::Result<(usize, Recorder)> {
        loop {
            let index = self.next.fetch_add(1, Ordering::Relaxed);
            let conn = self.dir.join(format!("conn-{index:03}"));
            match std::fs::create_dir(&conn) {
                Ok(()) => return Recorder::create(&conn, index, peer).map(|r| (index, r)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

/// The number of a `conn-NNN` folder name.
fn connection_index(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("conn-")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Writes one connection's two message streams.
pub(crate) struct Recorder {
    c2s: File,
    s2c: File,
}

impl Recorder {
    /// Fill the new folder `conn` with an empty stream pair and a
    /// `meta.json` naming the peer.
    fn create(conn: &Path, index: usize, peer: &str) -> io::Result<Self> {
        let mut meta = private_file(&conn.join("meta.json"))?;
        writeln!(
            meta,
            "{{\"conn\": {index}, \"peer\": \"{}\", \"recorder\": \"om-server\"}}",
            peer.escape_default()
        )?;
        Ok(Self {
            c2s: private_file(&conn.join("c2s.bin"))?,
            s2c: private_file(&conn.join("s2c.bin"))?,
        })
    }

    /// Append a message the client sent.
    pub fn client(&mut self, msg: &[u8]) -> io::Result<()> {
        self.c2s.write_all(msg)
    }

    /// Append a message the server sent.
    pub fn server(&mut self, msg: &[u8]) -> io::Result<()> {
        self.s2c.write_all(msg)
    }
}

/// A new file only its owner can read.
fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::connection_index;

    #[test]
    fn reads_connection_numbers() {
        assert_eq!(connection_index("conn-000"), Some(0));
        assert_eq!(connection_index("conn-042"), Some(42));
        assert_eq!(connection_index("conn-1234"), Some(1234));
        for other in [
            "conn-",
            "conn-1a",
            "conn--1",
            "conn-+1",
            "meta.json",
            "xconn-001",
        ] {
            assert_eq!(connection_index(other), None, "{other}");
        }
    }
}
