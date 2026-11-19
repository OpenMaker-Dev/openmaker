//! The `CryptEncrypt` format, read from black-box round trips with the
//! engine (`CryptDecrypt` of its output with this code and of this code's
//! output with the engine; spec `calc-functions.md` 9, container oracle
//! answers). The Claris security guide names the parts: PBKDF2 for the key,
//! AES at 128 bits in GCM's counter mode, and an encrypted SHA-256 digest
//! that validates the data.
//!
//! ```text
//! salt      4 random bytes, in the clear
//! then, encrypted with AES-128-CTR:
//!   16 random bytes
//!   u8      high nibble random, low nibble = number of padding bytes at the end
//!   u8      1 text, 0 container data
//!   u16 BE  byte length of the file name (0 for text)
//!   "FP12"
//!   u64 BE  byte length of the data
//!   data    text as UTF-8, or the container's main stream
//!   name    the container's file name as UTF-8
//!   u8      0x20, the digest length
//!   32      SHA-256 of everything from the 16 random bytes to the name
//!   padding 0-15 random bytes
//! ```
//!
//! The key is PBKDF2-HMAC-SHA1 over the key text's UTF-8 with the salt and
//! 15 iterations, 60 bytes: bytes 0-15 are the AES key, bytes 48-59 the
//! 96-bit GCM nonce; the keystream starts at block counter 2, as GCM's does
//! (counter 1 is reserved for its tag, which the format does not use).
//! Decryption checks the magic and the digest; anything else is ignored.

use aes::Aes128;
use ctr::cipher::{KeyIvInit, StreamCipher};
use sha2::{Digest, Sha256};

const SALT: usize = 4;
const PREFIX: usize = 16;
const MAGIC: &[u8; 4] = b"FP12";
const ITERATIONS: u32 = 15;
const DIGEST_LEN: u8 = 32;

/// What was encrypted: text or container data with its file name.
pub enum Plain<'a> {
    Text(&'a str),
    File { name: &'a str, data: &'a [u8] },
}

/// What [`decrypt`] returns.
#[derive(Debug, PartialEq, Eq)]
pub enum Decrypted {
    Text(String),
    File { name: String, data: Vec<u8> },
}

/// The AES-128-CTR keystream cipher for a key and salt.
fn cipher(key: &[u8], salt: &[u8]) -> ctr::Ctr32BE<Aes128> {
    let mut derived = [0u8; 60];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(key, salt, ITERATIONS, &mut derived);
    let mut counter = [0u8; 16];
    counter[..12].copy_from_slice(&derived[48..60]);
    counter[15] = 2;
    ctr::Ctr32BE::<Aes128>::new(derived[..16].into(), &counter.into())
}

/// Encrypts with fresh random bytes from the system; `None` when the system
/// has no randomness to give.
pub fn encrypt(plain: Plain, key: &[u8]) -> Option<Vec<u8>> {
    let mut random = [0u8; SALT + PREFIX + 1 + 15];
    getrandom::getrandom(&mut random).ok()?;
    Some(encrypt_with(plain, key, &random))
}

/// Encrypts with the given random bytes: the salt, the 16-byte prefix, the
/// padding count byte and up to 15 padding bytes.
fn encrypt_with(plain: Plain, key: &[u8], random: &[u8; SALT + PREFIX + 1 + 15]) -> Vec<u8> {
    let (is_text, name, data) = match plain {
        Plain::Text(text) => (1, "", text.as_bytes()),
        Plain::File { name, data } => (0, name, data),
    };
    let (salt, rest) = random.split_at(SALT);
    let (prefix, rest) = rest.split_at(PREFIX);
    let padding = usize::from(rest[0] & 0x0F);
    let mut body = Vec::with_capacity(PREFIX + 16 + data.len() + name.len() + 33 + padding);
    body.extend_from_slice(prefix);
    body.push(rest[0]);
    body.push(is_text);
    body.extend_from_slice(&(name.len() as u16).to_be_bytes());
    body.extend_from_slice(MAGIC);
    body.extend_from_slice(&(data.len() as u64).to_be_bytes());
    body.extend_from_slice(data);
    body.extend_from_slice(name.as_bytes());
    let digest = Sha256::digest(&body);
    body.push(DIGEST_LEN);
    body.extend_from_slice(&digest);
    body.extend_from_slice(&rest[1..=padding]);
    cipher(key, salt).apply_keystream(&mut body);
    let mut out = salt.to_vec();
    out.append(&mut body);
    out
}

/// Decrypts, or `None` when the key is wrong or the bytes are not this
/// format.
pub fn decrypt(blob: &[u8], key: &[u8]) -> Option<Decrypted> {
    let (salt, encrypted) = blob.split_at_checked(SALT)?;
    let mut body = encrypted.to_vec();
    cipher(key, salt).apply_keystream(&mut body);
    let header = body.get(PREFIX..PREFIX + 16)?;
    if &header[4..8] != MAGIC {
        return None;
    }
    let is_text = header[1] == 1;
    let name_len = usize::from(u16::from_be_bytes([header[2], header[3]]));
    let data_len = usize::try_from(u64::from_be_bytes(header[8..16].try_into().ok()?)).ok()?;
    let data_start = PREFIX + 16;
    let name_start = data_start.checked_add(data_len)?;
    let digest_at = name_start.checked_add(name_len)?;
    if *body.get(digest_at)? != DIGEST_LEN {
        return None;
    }
    let digest = body.get(digest_at + 1..digest_at + 1 + usize::from(DIGEST_LEN))?;
    if Sha256::digest(&body[..digest_at]).as_slice() != digest {
        return None;
    }
    let data = body[data_start..name_start].to_vec();
    Some(if is_text {
        Decrypted::Text(String::from_utf8_lossy(&data).into_owned())
    } else {
        Decrypted::File {
            name: String::from_utf8_lossy(&body[name_start..digest_at]).into_owned(),
            data,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Ciphertexts made by FileMaker Server's engine with the key `key`.
    #[test]
    fn decrypts_engine_output() {
        let text = hex(
            "4292C1FF52D3FAE54F9BD24EF54AD713A516BD5F8E7E0810FF7E8B7D7EBD2FBCDCD81F40\
             36AAF5F0138C622F2C14C0518F5D1BFDF902596E968695477A8B25B471AFDC22A91A29FF",
        );
        assert_eq!(decrypt(&text, b"key"), Some(Decrypted::Text("abc".into())));
        assert_eq!(decrypt(&text, b"wrong"), None);
        let file = hex(
            "D919248BD73057E800B1DC7B52468B56406B79FF98FC0F68547F99ADE877627DA33F007D\
             F9BC0B49119E3B6DFCD29AEAD0A183FD39AE24C0865133ABFA8A981CA4161DCC06196F79\
             39E5347D69D07996D616C04557",
        );
        assert_eq!(
            decrypt(&file, b"key"),
            Some(Decrypted::File {
                name: "x.bin".into(),
                data: vec![0x00, 0xFF, 0x10],
            })
        );
    }

    #[test]
    fn round_trips() {
        let mut random = [0u8; SALT + PREFIX + 1 + 15];
        for (i, b) in random.iter_mut().enumerate() {
            *b = (i * 37 + 11) as u8;
        }
        let blob = encrypt_with(Plain::Text("é😀"), b"k", &random);
        assert_eq!(decrypt(&blob, b"k"), Some(Decrypted::Text("é😀".into())));
        let blob = encrypt_with(
            Plain::File {
                name: "é.bin",
                data: &[],
            },
            b"k",
            &random,
        );
        assert_eq!(
            decrypt(&blob, b"k"),
            Some(Decrypted::File {
                name: "é.bin".into(),
                data: vec![],
            })
        );
        assert_eq!(decrypt(b"abc", b"k"), None);
        assert_eq!(decrypt(&[0; 80], b"k"), None);
    }
}
