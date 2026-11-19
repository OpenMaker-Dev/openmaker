//! Account passwords as a file stores them: the hash a client checks a
//! password against when it opens a local file itself (local-files.md 2).
//!
//! An account record `[23, 1, 5, A]` keeps its password at `06`
//! (local-files.md 2.1): the format version `01`, a 10-byte salt,
//! [`compute_password_hash`] of the password with that salt, the
//! [`PEPPER`] and one iteration, then `hash[1] & 0x1f` check bytes, each
//! the XOR of two earlier bytes. FileMaker reads a value of 31 bytes, or of
//! 31 plus its check bytes when they hold; nothing else. Verified against
//! the Admin accounts of FileMaker Pro's starter solutions, whose password
//! is empty; a non-empty password follows the same `ComputePasswordHash`
//! chain the login nonce does (spec 4.3).
//! The password's UTF-16 code units go through FileMaker's
//! `Unicode::HashDecompose` ([`crate::decompose`]) as they were typed, with
//! no normalization of OpenMaker's own: FileMaker Pro hashes a pasted NFC
//! password that way (#211). Other algorithm bytes are not observed and are
//! not accepted.

use zeroize::Zeroizing;

use crate::decompose::hash_decompose;
use crate::{compute_password_hash, HashAlgo, PEPPER};

/// The hash algorithm byte of a SHA1 password.
const SHA1: u8 = 1;
/// Salt bytes before the hash.
pub const SALT_LEN: usize = 10;
const HASH_LEN: usize = 20;
/// The version byte, salt and hash, before the check bytes.
const BASE_LEN: usize = 1 + SALT_LEN + HASH_LEN;

/// A password hash read from an account record.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredPassword {
    salt: [u8; SALT_LEN],
    hash: [u8; HASH_LEN],
}

impl std::fmt::Debug for StoredPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoredPassword([redacted])")
    }
}

/// Why an account's stored password cannot be checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredPasswordError {
    /// Shorter than its algorithm byte, salt and hash.
    Truncated,
    /// An algorithm byte other than SHA1's.
    Algorithm(u8),
    /// Longer than the version, salt and hash, but not by the check bytes
    /// the hash calls for, or with check bytes that do not hold: FileMaker
    /// reads no password from it.
    CheckBytes,
}

impl std::fmt::Display for StoredPasswordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoredPasswordError::Truncated => f.write_str("the stored password is truncated"),
            StoredPasswordError::Algorithm(byte) => {
                write!(f, "the stored password uses hash algorithm {byte}")
            }
            StoredPasswordError::CheckBytes => {
                f.write_str("the stored password's check bytes do not hold")
            }
        }
    }
}

impl std::error::Error for StoredPasswordError {}

impl StoredPassword {
    /// Read the value of an account's `06` key.
    pub fn parse(bytes: &[u8]) -> Result<Self, StoredPasswordError> {
        match bytes.first() {
            Some(&SHA1) => {}
            Some(&other) => return Err(StoredPasswordError::Algorithm(other)),
            None => return Err(StoredPasswordError::Truncated),
        }
        let salt = bytes
            .get(1..1 + SALT_LEN)
            .ok_or(StoredPasswordError::Truncated)?;
        let hash = bytes
            .get(1 + SALT_LEN..BASE_LEN)
            .ok_or(StoredPasswordError::Truncated)?;
        if bytes.len() > BASE_LEN {
            let count = usize::from(bytes[12] & 0x1f);
            let holds = bytes.len() == BASE_LEN + count
                && (0..count).all(|i| bytes[BASE_LEN + i] == bytes[11 + i] ^ bytes[1 + i]);
            if !holds {
                return Err(StoredPasswordError::CheckBytes);
            }
        }
        Ok(Self {
            salt: salt.try_into().expect("salt length"),
            hash: hash.try_into().expect("hash length"),
        })
    }

    /// The stored form of `password` with `salt`, as FileMaker writes it
    /// (`DBPassword::Write`): the version, salt and hash, then `hash[1] &
    /// 0x1f` check bytes, byte `31 + i` the XOR of bytes `11 + i` and
    /// `1 + i` of the value as written so far.
    pub fn encode(password: &str, salt: [u8; SALT_LEN]) -> Vec<u8> {
        let mut out = vec![SHA1];
        out.extend_from_slice(&salt);
        out.extend_from_slice(&hash(password, &salt));
        for i in 0..usize::from(out[12] & 0x1f) {
            let check = out[11 + i] ^ out[1 + i];
            out.push(check);
        }
        out
    }

    /// Whether `password` is this account's password. The comparison takes
    /// the same time whichever byte differs.
    pub fn verify(&self, password: &str) -> bool {
        let offered = hash(password, &self.salt);
        offered
            .iter()
            .zip(self.hash.iter())
            .fold(0u8, |differ, (a, b)| differ | (a ^ b))
            == 0
    }
}

fn hash(password: &str, salt: &[u8; SALT_LEN]) -> Vec<u8> {
    let mut text = Zeroizing::new(Vec::with_capacity(password.len()));
    text.extend(password.encode_utf16());
    compute_password_hash(&hash_decompose(&text), salt, &PEPPER, 1, HashAlgo::Sha1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SALT: [u8; SALT_LEN] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

    #[test]
    fn a_stored_password_verifies_only_its_password() {
        let stored = StoredPassword::encode("correct horse", SALT);
        let password = StoredPassword::parse(&stored).unwrap();
        assert!(password.verify("correct horse"));
        for wrong in ["", "correct horsf", "Correct horse", "correct horse "] {
            assert!(!password.verify(wrong), "{wrong:?} accepted");
        }
    }

    #[test]
    fn an_empty_password_is_pbkdf2_of_nothing() {
        // The pepper mixes into no code units: the hash is PBKDF2-HMAC-SHA1
        // of an empty password with the salt, which the starter solutions'
        // Admin accounts store.
        let stored = StoredPassword::encode("", SALT);
        let mut expected = [0u8; HASH_LEN];
        crate::pbkdf2_hmac_sha1(b"", &SALT, 1, &mut expected);
        assert_eq!(stored[1 + SALT_LEN..], expected);
        assert!(StoredPassword::parse(&stored).unwrap().verify(""));
    }

    #[test]
    fn other_algorithms_and_short_values_are_refused() {
        let mut stored = StoredPassword::encode("x", SALT);
        stored[0] = 2;
        assert_eq!(
            StoredPassword::parse(&stored),
            Err(StoredPasswordError::Algorithm(2))
        );
        let stored = StoredPassword::encode("x", SALT);
        assert_eq!(
            StoredPassword::parse(&stored[..BASE_LEN - 1]),
            Err(StoredPasswordError::Truncated)
        );
        assert_eq!(
            StoredPassword::parse(&[]),
            Err(StoredPasswordError::Truncated)
        );
    }

    /// FileMaker Pro 26.0.3's English Contacts starter: Admin's `06`, the
    /// empty password with 30 check bytes (its hash's second byte is
    /// `fe`). OpenMaker writes the same bytes for the same salt, reads them,
    /// and reads no password from a value whose check bytes or length are
    /// off; a value of the version, salt and hash alone reads as FileMaker
    /// reads it.
    #[test]
    fn the_check_bytes_are_filemakers() {
        let stored: Vec<u8> = (0..61)
            .map(|i| {
                u8::from_str_radix(
                    &"01997d057fc14f708478cfc8fe9de31e08301255df87577357746441b6129a5183989cdf4740962d104fa9eeb46a6c71a44745d6d4ebcbab2301203f8a"[2 * i..2 * i + 2],
                    16,
                )
                .unwrap()
            })
            .collect();
        let salt: [u8; SALT_LEN] = stored[1..11].try_into().unwrap();
        assert_eq!(StoredPassword::encode("", salt), stored);
        assert!(StoredPassword::parse(&stored).unwrap().verify(""));
        assert!(StoredPassword::parse(&stored[..BASE_LEN])
            .unwrap()
            .verify(""));
        let mut wrong = stored.clone();
        *wrong.last_mut().unwrap() ^= 1;
        for value in [&wrong[..], &stored[..60], &[&stored[..], &[0]].concat()[..]] {
            assert_eq!(
                StoredPassword::parse(value),
                Err(StoredPasswordError::CheckBytes)
            );
        }
    }

    /// Four accounts FileMaker Pro 26.0.3 made in a copy of the Contacts
    /// starter, each password pasted as NFC text, and each admitted by Pro
    /// at sign-in (#211, comment 6874981316): their `06` values and the
    /// UTF-16 code units of their passwords. Pro hashes these NFC code units
    /// after `HashDecompose`, decomposing nothing first; OpenMaker verifies them and writes the
    /// same bytes for the same salt. Unicode NFD forms are other code units
    /// and are refused, the Greek one because FileMaker decomposes tonos to
    /// U+030D, the Hangul one because FileMaker keeps syllables whole.
    #[test]
    fn filemaker_pros_passwords_outside_ascii_verify() {
        let unhex = |text: &str| -> Vec<u8> {
            (0..text.len() / 2)
                .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
                .collect()
        };
        let cases: [(&str, &[u16], &[u16]); 4] = [
            (
                "01498e03a2d2e41c33db18211e42422a6a687805d73a8bfdaedaec458bec22689041e0f88e744bdecf1b95bfecf0862df3e9f5521bbc4e226231c032ed",
                &[0x70, 0xE4, 0x73, 0x73, 0x77, 0xF6, 0x72, 0x64],
                &[0x70, 0xE4, 0x73, 0x73, 0x77, 0xF6, 0x72, 0x65],
            ),
            (
                "0149eaf7585c14cf0165fad0b6dfe11c707f6bd72cca267f403c422d9d2bec995c28b94064b06ab2d61a90a0a1203252f6fcc0537a",
                &[0x3BA, 0x3C9, 0x3B4, 0x3B9, 0x3BA, 0x3CC, 0x3C2],
                &[0x3BA, 0x3C9, 0x3B4, 0x3B9, 0x3BA, 0x3BF, 0x301, 0x3C2],
            ),
            (
                "014a45eb0fe64482cfefdcadb55ae556b362dea5150e10d2207edb9a725748e7f0b1eab0f7e0114ac9a3a588c52868f8acf25de9",
                &[0xBE44, 0xBC00, 0xBC88, 0xD638],
                &[0x1107, 0x1175, 0x1106, 0x1175, 0x11AF, 0x1107, 0x1165, 0x11AB, 0x1112, 0x1169],
            ),
            (
                "014aa0e0c57074359d79be6e61af70a3b99d117270176351807fe74bfe6fe824",
                &[0x6B, 0x65, 0x79, 0xD83D, 0xDD11],
                &[0x6B, 0x65, 0x79],
            ),
        ];
        for (stored, password, other) in cases {
            let stored = unhex(stored);
            let password = String::from_utf16(password).unwrap();
            let other = String::from_utf16(other).unwrap();
            let read = StoredPassword::parse(&stored).unwrap();
            assert!(read.verify(&password), "{password:?} refused");
            assert!(!read.verify(&other), "{other:?} accepted");
            let salt: [u8; SALT_LEN] = stored[1..11].try_into().unwrap();
            assert_eq!(StoredPassword::encode(&password, salt), stored);
        }
    }

    #[test]
    fn diagnostics_leave_the_hash_out() {
        let password = StoredPassword::parse(&StoredPassword::encode("x", SALT)).unwrap();
        assert_eq!(format!("{password:?}"), "StoredPassword([redacted])");
    }
}
