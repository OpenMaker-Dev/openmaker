//! Truncated and mutated copies of every fixture in `tests/data` (and of
//! the import oracle's sources) through every reader and the import
//! engine: a hostile source may be refused, never panic. Workbooks are
//! also unpacked, each part mutated and packed again, so the XML readers
//! see damage past the zip layer.

mod support;

use std::io::{Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use om_transfer::import::{self, read, ImportRequest};
use om_transfer::spec::{import_flag, ImportAction, ImportOptions, Key, MapEntry, Mapping};
use om_transfer::{FileType, ImportCharset, Profile};
use support::{f, Mem, CONTACTS};

fn fixtures() -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push((path.display().to_string(), std::fs::read(&path).unwrap()));
            }
        }
    }
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    walk(&crate_dir.join("tests/data"), &mut out);
    walk(
        &crate_dir.join("../../tools/fms-transfer-oracle/sources"),
        &mut out,
    );
    assert!(out.len() > 50, "fixtures missing: {}", out.len());
    out
}

/// xorshift64*: deterministic, so a failure names a reproducible variant.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Bytes the formats give meaning to.
const SPECIAL: &[u8] = &[
    b'<', b'>', b'/', b'&', b';', b'#', b'"', b'\'', b'=', b'\r', b'\n', b'\t', b',', 0x0b, 0x1d,
    0x00, 0x0d, 0x1a, 0x2a, 0x80, 0xff, 0xfe, b'9', b'Z',
];

/// The run of bytes from `at` that `f` accepts.
fn run_at(bytes: &[u8], at: usize, f: impl Fn(u8) -> bool) -> std::ops::Range<usize> {
    let len = bytes[at..].iter().take_while(|&&b| f(b)).count();
    at..at + len
}

fn mutate(bytes: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for _ in 0..1 + rng.below(8) {
        let at = rng.below(out.len() + 1);
        // The next tag, number and capitals at or after `at`.
        let next =
            |out: &[u8], b: fn(u8) -> bool| out[at..].iter().position(|&c| b(c)).map(|i| at + i);
        match rng.below(9) {
            6 => {
                // Drop a whole tag: a cell out of its row, a row unclosed.
                if let Some(start) = next(&out, |c| c == b'<') {
                    if let Some(len) = out[start..].iter().position(|&c| c == b'>') {
                        out.drain(start..=start + len);
                    }
                }
            }
            7 => {
                // A number past every limit.
                if let Some(start) = next(&out, |c| c.is_ascii_digit()) {
                    let digits = run_at(&out, start, |c| c.is_ascii_digit());
                    let huge = ["1048577", "999999999", "18446744073709551616", "0"];
                    out.splice(digits, huge[rng.below(huge.len())].bytes());
                }
            }
            8 => {
                // A longer column name.
                if let Some(start) = next(&out, |c| c.is_ascii_uppercase()) {
                    let letters = run_at(&out, start, |c| c.is_ascii_uppercase());
                    let long = ["XFE", "ZZZZZZZZZZZZZZZZ", "AAAA"];
                    out.splice(letters, long[rng.below(long.len())].bytes());
                }
            }
            0 if at < out.len() => out[at] ^= 1 << rng.below(8),
            1 if at < out.len() => out[at] = SPECIAL[rng.below(SPECIAL.len())],
            2 => out.insert(at, SPECIAL[rng.below(SPECIAL.len())]),
            3 if at < out.len() => {
                out.remove(at);
            }
            4 => {
                // Repeat a slice: deeper nesting, more columns and rows.
                let len = 1 + rng.below(64);
                let end = (at + len).min(out.len());
                let slice = out[at..end].to_vec();
                for _ in 0..1 + rng.below(4) {
                    out.splice(at..at, slice.iter().copied());
                }
            }
            _ if at < out.len() => out[at] = rng.next() as u8,
            _ => {}
        }
    }
    out
}

/// The original, truncations and mutations.
fn variants(bytes: &[u8], rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut out = vec![bytes.to_vec()];
    for k in 0..16 {
        out.push(bytes[..bytes.len() * k / 16].to_vec());
    }
    out.push(bytes[..bytes.len().saturating_sub(1)].to_vec());
    for _ in 0..24 {
        out.push(mutate(bytes, rng));
    }
    out
}

/// A workbook's parts.
fn unpack(bytes: &[u8]) -> Option<Vec<(String, Vec<u8>)>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut parts = Vec::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).ok()?;
        let mut data = Vec::new();
        file.read_to_end(&mut data).ok()?;
        parts.push((file.name().ok()?.into_owned(), data));
    }
    Some(parts)
}

fn pack(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in parts {
        zip.start_file(name.as_str(), options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// Workbooks with one part truncated or mutated.
fn workbook_variants(bytes: &[u8], rng: &mut Rng) -> Vec<Vec<u8>> {
    let Some(parts) = unpack(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..parts.len() {
        for k in [1, 2, 3] {
            let mut damaged = parts.clone();
            let data = &mut damaged[i].1;
            data.truncate(data.len() * k / 4);
            out.push(pack(&damaged));
        }
        for _ in 0..6 {
            let mut damaged = parts.clone();
            damaged[i].1 = mutate(&damaged[i].1, rng);
            out.push(pack(&damaged));
        }
    }
    out
}

fn request(file_type: FileType, charset: ImportCharset) -> ImportRequest {
    ImportRequest {
        options: ImportOptions {
            table: Key::from_id(CONTACTS),
            action: ImportAction::Add,
            flags: import_flag::AUTO_ENTER,
            charset,
            map: [f::FIRST, f::LAST, f::N, f::D, f::R, f::TS]
                .iter()
                .map(|&id| MapEntry {
                    field: Key::from_id(id),
                    mapping: Mapping::Import,
                })
                .collect(),
        },
        profile: Profile::new(file_type),
        profile_ex: None,
        field_flags: None,
    }
}

const TYPES: [FileType; 6] = [
    FileType::TabSeparated,
    FileType::CommaSeparated,
    FileType::Merge,
    FileType::XmlImport,
    FileType::DBase,
    FileType::Excel2007,
];

const CHARSETS: [ImportCharset; 3] = [
    ImportCharset::Utf8,
    ImportCharset::Windows,
    ImportCharset::Utf16,
];

/// Where the last panic was.
static PANIC_AT: Mutex<Option<String>> = Mutex::new(None);

/// Every reader over `bytes`, then the whole import as the name's format
/// and as updates matching by name; each run on its own, so that one
/// reader's panic does not hide another's. The readers that panicked.
fn read_all(name: &str, bytes: &[u8]) -> Vec<String> {
    let mut panicked = Vec::new();
    let mut run = |what: String, f: &dyn Fn()| {
        if catch_unwind(AssertUnwindSafe(f)).is_err() {
            let at = PANIC_AT.lock().unwrap().take().unwrap_or_default();
            panicked.push(format!("{what} at {at}"));
        }
    };
    for file_type in TYPES {
        let charsets: &[ImportCharset] = match file_type {
            FileType::DBase | FileType::Excel2007 => &CHARSETS[..1],
            _ => &CHARSETS,
        };
        for &charset in charsets {
            run(format!("parse {file_type:?} {charset:?}"), &|| {
                let _ = import::parse(&request(file_type, charset), bytes);
            });
        }
    }
    run("workbook by name".into(), &|| {
        let _ = read::workbook(bytes, 1, "Sheet2");
    });
    let file_type = match name.rsplit('.').next().unwrap_or("") {
        "csv" => FileType::CommaSeparated,
        "mer" => FileType::Merge,
        "xml" => FileType::XmlImport,
        "dbf" => FileType::DBase,
        "xlsx" => FileType::Excel2007,
        _ => FileType::TabSeparated,
    };
    run(format!("import {file_type:?}"), &|| {
        let locale = om_calc::Locale::en_us();
        let mut target = Mem::new();
        let mut add = request(file_type, ImportCharset::Utf8);
        let _ = import::import(&mut target, &add, bytes, &locale);
        add.options.action = ImportAction::UpdateMatching;
        add.options.flags |= import_flag::MATCH_NAMES | import_flag::ADD_REMAINING;
        add.options.map[0].mapping = Mapping::Match;
        let _ = import::import(&mut target, &add, bytes, &locale);
    });
    panicked
}

#[test]
fn damaged_fixtures_never_panic() {
    // Each panic's place is reported with its variant below.
    std::panic::set_hook(Box::new(|info| {
        *PANIC_AT.lock().unwrap() = info.location().map(|l| l.to_string());
    }));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut panics = Vec::new();
    let mut runs = 0usize;
    for (name, bytes) in fixtures() {
        let mut all = variants(&bytes, &mut rng);
        if bytes.starts_with(b"PK") {
            all.extend(workbook_variants(&bytes, &mut rng));
        }
        for (i, variant) in all.iter().enumerate() {
            runs += 1;
            for reader in read_all(&name, variant) {
                panics.push(format!("{name} variant {i}: {reader}"));
            }
        }
    }
    let _ = std::panic::take_hook();
    assert!(
        panics.is_empty(),
        "{} panics in {runs} variants: {panics:#?}",
        panics.len()
    );
}
