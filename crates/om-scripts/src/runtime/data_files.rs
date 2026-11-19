//! The open files of the data file steps, for every host (script-runtime.md,
//! "Files"; Claris script steps reference): Open Data File's ids, Write to
//! and Read from Data File with their encodings, Get and Set Data File
//! Position, Close Data File and `Get ( OpenDataFileInfo )`, with FileMaker's
//! errors and the limits:
//!
//! - at most [`MAX_OPEN`] files are open (error 2), each once (300); ids
//!   count from 1;
//! - a read or a write moves at most [`MAX_TRANSFER`] bytes (2);
//! - a write goes at the position (overwriting): text as UTF-16 little
//!   endian without a byte-order mark or as UTF-8, container data as its
//!   bytes whatever the encoding, then a line feed when asked;
//! - a read takes an amount of bytes (UTF-8, Bytes) or UTF-16 code units
//!   from the position, the rest of the file without one, and 10 when
//!   nothing is left; Bytes give container data named after the file;
//! - a position counts bytes from the start; a negative one is 10;
//! - an id that is not open is 100.
//!
//! Where a path leads, and creating, deleting and renaming files, are the
//! host's: its folders and the paths it accepts differ (the desktop reaches
//! what the user may, OpenMaker Server only its Documents and temporary
//! folders). A host opens the file and hands it here with a key that names
//! it (so that a file is open once) and the path `Get ( OpenDataFileInfo )`
//! shows.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use om_calc::{Container, Value};

use super::{Encoding, ErrorCode};

/// FileMaker errors of the file steps (catalogs/error-codes.csv).
pub mod code {
    use super::ErrorCode;

    /// "Memory error": too many files open, or a transfer too large.
    pub const MEMORY: ErrorCode = ErrorCode(2);
    /// "Requested data is missing": the end of the file.
    pub const END_OF_FILE: ErrorCode = ErrorCode(10);
    pub const FILE_MISSING: ErrorCode = ErrorCode(100);
    pub const IN_USE: ErrorCode = ErrorCode(300);
    pub const CANNOT_CREATE: ErrorCode = ErrorCode(800);
    pub const CANNOT_OPEN: ErrorCode = ErrorCode(802);
    pub const DISK_FULL: ErrorCode = ErrorCode(809);
    /// "File path specified is not a valid file path".
    pub const INVALID_PATH: ErrorCode = ErrorCode(826);
}

/// Open data files at once (Claris "Open Data File").
pub const MAX_OPEN: usize = 25;
/// The most one read or write moves (Claris "Read from Data File").
pub const MAX_TRANSFER: u64 = 64 * 1024 * 1024;

/// An OS error as a file step's error: missing 100, disk full 809, else
/// `otherwise` (800 when creating, 802 when opening, reading or writing).
pub fn io_error(e: &io::Error, otherwise: ErrorCode) -> ErrorCode {
    match e.kind() {
        io::ErrorKind::NotFound => code::FILE_MISSING,
        io::ErrorKind::StorageFull => code::DISK_FULL,
        _ => otherwise,
    }
}

struct OpenFile<K> {
    key: K,
    /// What `Get ( OpenDataFileInfo )` shows after the id.
    shown: String,
    /// The name container data read as Bytes takes.
    name: String,
    file: File,
}

/// The data files a session has open, under keys of type `K` that name a
/// file (its path, or its device and inode).
pub struct DataFiles<K> {
    open: BTreeMap<i64, OpenFile<K>>,
    next: i64,
}

impl<K> Default for DataFiles<K> {
    fn default() -> Self {
        DataFiles {
            open: BTreeMap::new(),
            next: 1,
        }
    }
}

impl<K: PartialEq> DataFiles<K> {
    /// Whether the file `key` names is open.
    pub fn is_open(&self, key: &K) -> bool {
        self.open.values().any(|f| f.key == *key)
    }

    /// The open files, with their keys: for hosts that compare a file by
    /// what it is rather than by the key that names it.
    pub fn files(&self) -> impl Iterator<Item = (&K, &File)> {
        self.open.values().map(|f| (&f.key, &f.file))
    }

    /// Whether Open Data File may open the file `key` names: 300 when it
    /// is open already, 2 when [`MAX_OPEN`] are. A host may ask before it
    /// opens the file; [`Self::insert`] asks again.
    pub fn admit(&self, key: &K) -> Result<(), ErrorCode> {
        if self.is_open(key) {
            return Err(code::IN_USE);
        }
        if self.open.len() >= MAX_OPEN {
            return Err(code::MEMORY);
        }
        Ok(())
    }

    /// Open Data File's last checks ([`Self::admit`]) and its id.
    pub fn insert(
        &mut self,
        key: K,
        shown: String,
        name: String,
        file: File,
    ) -> Result<i64, ErrorCode> {
        self.admit(&key)?;
        let id = self.next;
        self.next += 1;
        self.open.insert(
            id,
            OpenFile {
                key,
                shown,
                name,
                file,
            },
        );
        Ok(id)
    }

    fn file(&mut self, id: i64) -> Result<&mut OpenFile<K>, ErrorCode> {
        self.open.get_mut(&id).ok_or(code::FILE_MISSING)
    }

    /// Close Data File.
    pub fn close(&mut self, id: i64) -> Result<(), ErrorCode> {
        self.open.remove(&id).map(drop).ok_or(code::FILE_MISSING)
    }

    /// Close every file (the session or the database file ends).
    pub fn close_all(&mut self) {
        self.open.clear();
    }

    /// `Get ( OpenDataFileInfo )`: one line per open file, its id, a tab
    /// and its path.
    pub fn info(&self) -> String {
        self.open
            .iter()
            .map(|(id, f)| format!("{id}\t{}", f.shown))
            .collect::<Vec<_>>()
            .join("\r")
    }

    /// Get Data File Position.
    pub fn position(&mut self, id: i64) -> Result<i64, ErrorCode> {
        let file = self.file(id)?;
        let position = file
            .file
            .stream_position()
            .map_err(|e| io_error(&e, code::CANNOT_OPEN))?;
        Ok(position as i64)
    }

    /// Set Data File Position.
    pub fn set_position(&mut self, id: i64, position: i64) -> Result<(), ErrorCode> {
        let file = self.file(id)?;
        let position = u64::try_from(position).map_err(|_| code::END_OF_FILE)?;
        file.file
            .seek(SeekFrom::Start(position))
            .map_err(|e| io_error(&e, code::CANNOT_OPEN))?;
        Ok(())
    }

    /// Write to Data File: `data` at the file's position (module docs),
    /// text as `text` gives it in the host's formats.
    pub fn write(
        &mut self,
        id: i64,
        data: &Value,
        text: impl FnOnce(&Value) -> String,
        encoding: Encoding,
        line_feed: bool,
    ) -> Result<(), ErrorCode> {
        let utf16 = encoding == Encoding::Utf16;
        let encode = |text: &str| -> Vec<u8> {
            if utf16 {
                text.encode_utf16().flat_map(u16::to_le_bytes).collect()
            } else {
                text.as_bytes().to_vec()
            }
        };
        let mut bytes = match data {
            Value::Container(container) => container.data().to_vec(),
            value => encode(&text(value)),
        };
        if line_feed {
            bytes.extend(encode("\n"));
        }
        if bytes.len() as u64 > MAX_TRANSFER {
            return Err(code::MEMORY);
        }
        let file = self.file(id)?;
        file.file
            .write_all(&bytes)
            .map_err(|e| io_error(&e, code::CANNOT_OPEN))
    }

    /// Read from Data File (module docs).
    pub fn read(
        &mut self,
        id: i64,
        amount: Option<i64>,
        encoding: Encoding,
    ) -> Result<Value, ErrorCode> {
        let file = self.file(id)?;
        let unit = if encoding == Encoding::Utf16 { 2 } else { 1 };
        let limit = match amount {
            Some(n) if n < 0 => return Err(code::END_OF_FILE),
            Some(n) => (n as u64).saturating_mul(unit),
            None => MAX_TRANSFER,
        }
        .min(MAX_TRANSFER);
        let mut bytes = Vec::new();
        (&mut file.file)
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&e, code::CANNOT_OPEN))?;
        if bytes.is_empty() && limit > 0 {
            return Err(code::END_OF_FILE);
        }
        Ok(match encoding {
            Encoding::Bytes => Value::Container(Container::file(file.name.clone(), bytes)),
            Encoding::Utf8 => Value::text(String::from_utf8_lossy(&bytes).into_owned()),
            Encoding::Utf16 => {
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair))
                    .collect();
                Value::text(String::from_utf16_lossy(&units))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(v: &Value) -> String {
        match v {
            Value::Text(t) => t.as_str().to_owned(),
            _ => String::new(),
        }
    }

    /// A scratch file of this test's own, open for reading and writing.
    fn scratch(name: &str) -> (std::path::PathBuf, File) {
        let path =
            std::env::temp_dir().join(format!("om-data-files-{name}-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        (path, file)
    }

    #[test]
    fn handles_encodings_positions_and_limits() {
        let (path, file) = scratch("rw");
        let mut files = DataFiles::default();
        let id = files
            .insert(1, "file:/x/change.log".into(), "change.log".into(), file)
            .unwrap();
        assert_eq!(id, 1);
        let again = File::open(&path).unwrap();
        assert_eq!(
            files.insert(1, String::new(), String::new(), again),
            Err(code::IN_USE)
        );
        files
            .write(id, &Value::text("Completed"), text, Encoding::Utf8, false)
            .unwrap();
        assert_eq!(files.position(id), Ok(9));
        files
            .write(id, &Value::text("é"), text, Encoding::Utf16, true)
            .unwrap();
        files.set_position(id, 0).unwrap();
        assert_eq!(
            text(&files.read(id, Some(4), Encoding::Utf8).unwrap()),
            "Comp"
        );
        files.set_position(id, 9).unwrap();
        assert_eq!(text(&files.read(id, None, Encoding::Utf16).unwrap()), "é\n");
        assert_eq!(
            files.read(id, None, Encoding::Utf8).unwrap_err(),
            code::END_OF_FILE
        );
        assert_eq!(files.set_position(id, -1), Err(code::END_OF_FILE));
        files.set_position(id, 0).unwrap();
        let Value::Container(bytes) = files.read(id, Some(3), Encoding::Bytes).unwrap() else {
            panic!("bytes are container data");
        };
        assert_eq!(bytes.data(), b"Com");
        assert_eq!(files.info(), "1\tfile:/x/change.log");
        files.close(id).unwrap();
        assert_eq!(files.close(id), Err(code::FILE_MISSING));
        assert_eq!(files.position(id), Err(code::FILE_MISSING));
        assert_eq!(files.info(), "");
        // At most 25 at once; ids go on counting.
        for key in 0..MAX_OPEN {
            files
                .insert(
                    key + 10,
                    String::new(),
                    String::new(),
                    File::open(&path).unwrap(),
                )
                .unwrap();
        }
        assert_eq!(
            files.insert(99, String::new(), String::new(), File::open(&path).unwrap()),
            Err(code::MEMORY)
        );
        files.close_all();
        assert_eq!(
            files.insert(99, String::new(), String::new(), File::open(&path).unwrap()),
            Ok(27)
        );
        std::fs::remove_file(path).unwrap();
    }
}
