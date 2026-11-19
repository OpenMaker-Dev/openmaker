//! FileMaker's `Unicode::HashDecompose`, the step before every password hash
//! (network-protocol.md 4.4): each UTF-16 code unit is replaced by its
//! decomposition from FileMaker's own table, marks are never reordered, and
//! a text that starts with a combining mark gets a space before it.
//!
//! The table is the catalog measured by calling FileMaker Pro 26.0.3's
//! Support library on every non-surrogate BMP code unit
//! (`docs/re/catalogs/password-hash-decompose.tsv`). It is not NFD or NFKD:
//! Hangul syllables and compatibility characters stay, Greek tonos becomes
//! U+030D. Surrogates are not in the table, so a supplementary character's
//! two code units stay as they are, as the measured vectors show.

use std::sync::OnceLock;

use zeroize::Zeroizing;

const CATALOG: &str = include_str!("../../../docs/re/catalogs/password-hash-decompose.tsv");

/// The parsed catalog: every code unit it lists, sorted, with its
/// decomposition and whether it is a combining mark.
struct Table {
    units: Vec<Entry>,
}

struct Entry {
    unit: u16,
    decomposition: Box<[u16]>,
    mark: bool,
}

impl Table {
    fn parse(catalog: &str) -> Self {
        let hex = |text: &str| u16::from_str_radix(text, 16).expect("a code unit in the catalog");
        let mut units: Vec<Entry> = catalog
            .lines()
            .skip(1)
            .filter(|line| !line.is_empty())
            .map(|line| {
                let mut columns = line.split('\t');
                let mut column = || columns.next().expect("three catalog columns");
                let unit = hex(column());
                let decomposition = column().split(' ').map(hex).collect();
                let mark = column() == "1";
                Entry {
                    unit,
                    decomposition,
                    mark,
                }
            })
            .collect();
        units.sort_by_key(|entry| entry.unit);
        Self { units }
    }

    fn entry(&self, unit: u16) -> Option<&Entry> {
        self.units
            .binary_search_by_key(&unit, |entry| entry.unit)
            .ok()
            .map(|at| &self.units[at])
    }
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| Table::parse(CATALOG))
}

/// `text` as FileMaker hashes it: each code unit replaced by its
/// decomposition, and a space first when the first code unit is a
/// combining mark. The result is sized before it is filled, so no copy of
/// the password is left behind by a reallocation.
pub fn hash_decompose(text: &[u16]) -> Zeroizing<Vec<u16>> {
    let table = table();
    let parts = |unit: u16| table.entry(unit).map(|entry| &*entry.decomposition);
    let leading_mark = text
        .first()
        .and_then(|&unit| table.entry(unit))
        .is_some_and(|entry| entry.mark);
    let length = usize::from(leading_mark)
        + text
            .iter()
            .map(|&unit| parts(unit).map_or(1, <[u16]>::len))
            .sum::<usize>();
    let mut out = Zeroizing::new(Vec::with_capacity(length));
    if leading_mark {
        out.push(0x20);
    }
    for &unit in text {
        match parts(unit) {
            Some(parts) => out.extend_from_slice(parts),
            None => out.push(unit),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_read_whole() {
        let table = table();
        assert_eq!(table.units.len(), 1534);
        assert_eq!(table.units.iter().filter(|entry| entry.mark).count(), 552);
        assert!(table
            .units
            .windows(2)
            .all(|pair| pair[0].unit < pair[1].unit));
    }

    /// The vectors FileMaker Pro 26.0.3's Support library gave
    /// (`docs/re/catalogs/password-hash-vectors.tsv`): each text decomposes
    /// to the library's code units, and where the output is a whole digest,
    /// the decomposed text with the fixed pepper hashes to its hash.
    #[test]
    fn filemakers_vectors_decompose_and_hash() {
        use crate::{compute_password_hash, HashAlgo, PEPPER};
        const VECTORS: &str = include_str!("../../../docs/re/catalogs/password-hash-vectors.tsv");
        let units = |text: &str| -> Vec<u16> {
            if text == "-" {
                return Vec::new();
            }
            text.split(' ')
                .map(|unit| u16::from_str_radix(unit, 16).unwrap())
                .collect()
        };
        let bytes = |text: &str| -> Vec<u8> {
            (0..text.len() / 2)
                .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
                .collect()
        };
        let (mut decomposed, mut hashed) = (0, 0);
        for line in VECTORS.lines().skip(1) {
            let columns: Vec<&str> = line.split('\t').collect();
            let [text, salt, iterations, algorithm, length, expected, hash, note] = columns[..]
            else {
                panic!("{line}");
            };
            let output = hash_decompose(&units(text));
            assert_eq!(*output, units(expected), "{note}");
            decomposed += 1;
            let algo = match algorithm {
                "2" => HashAlgo::Sha256,
                "3" => HashAlgo::Sha384,
                "4" => HashAlgo::Sha512,
                _ => HashAlgo::Sha1,
            };
            if length.parse::<usize>().unwrap() != algo.digest_len() {
                continue;
            }
            let salt = if salt == "-" { Vec::new() } else { bytes(salt) };
            let iterations = iterations.parse().unwrap();
            assert_eq!(
                compute_password_hash(&output, &salt, &PEPPER, iterations, algo),
                bytes(hash),
                "{note}"
            );
            hashed += 1;
        }
        assert_eq!((decomposed, hashed), (50, 42));
    }
}
