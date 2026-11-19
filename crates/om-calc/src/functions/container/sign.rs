//! Digests, HMACs and RSA signatures (`CryptDigest`, `CryptAuthCode`,
//! `CryptGenerateSignature`, `CryptVerifySignature`).
//!
//! The algorithm names are those of the `CryptAuthCode` page in any case:
//! `MD5`, `SHA1`, `SHA224`, `SHA256`, `SHA384`, `SHA512`, and empty for
//! SHA-512; anything else (`SHA-256`, `SHA3-256`, a space) is error 4.
//! Signatures are RSASSA-PKCS1-v1_5 over the digest, byte for byte what the
//! engine (OpenSSL) produces. Keys are PEM: a private key as PKCS#1
//! (`RSA PRIVATE KEY`, optionally with OpenSSL's `Proc-Type: 4,ENCRYPTED`
//! headers) or PKCS#8 (`PRIVATE KEY`, `ENCRYPTED PRIVATE KEY`), a public
//! key as SubjectPublicKeyInfo (`PUBLIC KEY`) or PKCS#1 (`RSA PUBLIC KEY`).
//! FileMaker text ends lines with CR; any line break is accepted. A key
//! that does not read, or a wrong password, is error 5.

use super::{encodings, UNKNOWN_NAME};
use crate::error::CalcError;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use md5::Md5;
use rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncryptedPrivateKeyInfo};
use rsa::{Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};

/// 5: "Command is invalid", for a key that cannot be used (oracle).
const BAD_KEY: CalcError = super::err(5);

#[derive(Clone, Copy)]
enum Algorithm {
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

fn algorithm(name: &str) -> Result<Algorithm, CalcError> {
    Ok(match name.to_ascii_uppercase().as_str() {
        "MD5" => Algorithm::Md5,
        "SHA1" => Algorithm::Sha1,
        "SHA224" => Algorithm::Sha224,
        "SHA256" => Algorithm::Sha256,
        "SHA384" => Algorithm::Sha384,
        "SHA512" | "" => Algorithm::Sha512,
        _ => return Err(UNKNOWN_NAME),
    })
}

/// `CryptDigest`.
pub fn digest(name: &str, data: &[u8]) -> Result<Vec<u8>, CalcError> {
    Ok(match algorithm(name)? {
        Algorithm::Md5 => Md5::digest(data).to_vec(),
        Algorithm::Sha1 => Sha1::digest(data).to_vec(),
        Algorithm::Sha224 => Sha224::digest(data).to_vec(),
        Algorithm::Sha256 => Sha256::digest(data).to_vec(),
        Algorithm::Sha384 => Sha384::digest(data).to_vec(),
        Algorithm::Sha512 => Sha512::digest(data).to_vec(),
    })
}

/// `CryptAuthCode`: HMAC of `data` under `key`. With an empty key the
/// engine returns the data itself (oracle: `CryptAuthCode ( "abc" ; "MD5" ;
/// "" )` is `abc`).
pub fn hmac(name: &str, key: &[u8], data: &[u8]) -> Result<Vec<u8>, CalcError> {
    fn run<M: Mac + hmac::digest::KeyInit>(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut mac = <M as hmac::digest::KeyInit>::new_from_slice(key)
            .expect("HMAC takes keys of any length");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }
    let algorithm = algorithm(name)?;
    if key.is_empty() {
        return Ok(data.to_vec());
    }
    Ok(match algorithm {
        Algorithm::Md5 => run::<Hmac<Md5>>(key, data),
        Algorithm::Sha1 => run::<Hmac<Sha1>>(key, data),
        Algorithm::Sha224 => run::<Hmac<Sha224>>(key, data),
        Algorithm::Sha256 => run::<Hmac<Sha256>>(key, data),
        Algorithm::Sha384 => run::<Hmac<Sha384>>(key, data),
        Algorithm::Sha512 => run::<Hmac<Sha512>>(key, data),
    })
}

/// The PKCS#1 v1.5 padding scheme and digest of `data` for an algorithm.
fn scheme(algorithm: Algorithm, data: &[u8]) -> (Pkcs1v15Sign, Vec<u8>) {
    match algorithm {
        Algorithm::Md5 => (Pkcs1v15Sign::new::<Md5>(), Md5::digest(data).to_vec()),
        Algorithm::Sha1 => (Pkcs1v15Sign::new::<Sha1>(), Sha1::digest(data).to_vec()),
        Algorithm::Sha224 => (Pkcs1v15Sign::new::<Sha224>(), Sha224::digest(data).to_vec()),
        Algorithm::Sha256 => (Pkcs1v15Sign::new::<Sha256>(), Sha256::digest(data).to_vec()),
        Algorithm::Sha384 => (Pkcs1v15Sign::new::<Sha384>(), Sha384::digest(data).to_vec()),
        Algorithm::Sha512 => (Pkcs1v15Sign::new::<Sha512>(), Sha512::digest(data).to_vec()),
    }
}

/// `CryptGenerateSignature`; `Ok(None)` (`?` without an error) for an
/// empty key.
pub fn generate_signature(
    data: &[u8],
    name: &str,
    private_key: &str,
    password: &str,
) -> Result<Option<Vec<u8>>, CalcError> {
    let algorithm = algorithm(name)?;
    if private_key.is_empty() {
        return Ok(None);
    }
    let key = private_key_from_pem(private_key, password.as_bytes()).ok_or(BAD_KEY)?;
    let (padding, hashed) = scheme(algorithm, data);
    key.sign(padding, &hashed).map(Some).map_err(|_| BAD_KEY)
}

/// `CryptVerifySignature`: whether `signature` signs `data`; a signature
/// that is not one (wrong length, garbage) is simply false. `Ok(None)` (`?`
/// without an error) for an empty key or signature.
pub fn verify_signature(
    data: &[u8],
    name: &str,
    public_key: &str,
    signature: &[u8],
) -> Result<Option<bool>, CalcError> {
    let algorithm = algorithm(name)?;
    if public_key.is_empty() || signature.is_empty() {
        return Ok(None);
    }
    let key = public_key_from_pem(public_key).ok_or(BAD_KEY)?;
    let (padding, hashed) = scheme(algorithm, data);
    Ok(Some(key.verify(padding, &hashed, signature).is_ok()))
}

/// A PEM block: its label, its `Name: value` headers and its DER bytes.
struct Pem {
    label: String,
    headers: Vec<(String, String)>,
    der: Vec<u8>,
}

/// Reads the first PEM block of `text`; lines may end with CR, LF or both.
fn pem(text: &str) -> Option<Pem> {
    let mut lines = text
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty());
    let label = lines
        .by_ref()
        .find_map(|l| l.strip_prefix("-----BEGIN ")?.strip_suffix("-----"))?
        .to_string();
    let end = format!("-----END {label}-----");
    let mut headers = Vec::new();
    let mut body = String::new();
    for line in lines {
        if line == end {
            let der = encodings::base64_decode(&body)?;
            return Some(Pem {
                label,
                headers,
                der,
            });
        }
        match line.split_once(':') {
            Some((name, value)) if body.is_empty() => {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
            _ => body.push_str(line),
        }
    }
    None
}

fn private_key_from_pem(text: &str, password: &[u8]) -> Option<RsaPrivateKey> {
    let pem = pem(text)?;
    match pem.label.as_str() {
        "RSA PRIVATE KEY" => {
            let encrypted = pem
                .headers
                .iter()
                .any(|(n, v)| n == "Proc-Type" && v.ends_with("ENCRYPTED"));
            let der = if encrypted {
                let dek = &pem.headers.iter().find(|(n, _)| n == "DEK-Info")?.1;
                legacy_decrypt(dek, password, &pem.der)?
            } else {
                pem.der
            };
            RsaPrivateKey::from_pkcs1_der(&der).ok()
        }
        "PRIVATE KEY" => RsaPrivateKey::from_pkcs8_der(&pem.der).ok(),
        "ENCRYPTED PRIVATE KEY" => {
            let info = EncryptedPrivateKeyInfo::try_from(pem.der.as_slice()).ok()?;
            let document = info.decrypt(password).ok()?;
            RsaPrivateKey::from_pkcs8_der(document.as_bytes()).ok()
        }
        _ => None,
    }
}

fn public_key_from_pem(text: &str) -> Option<RsaPublicKey> {
    let pem = pem(text)?;
    match pem.label.as_str() {
        "PUBLIC KEY" => RsaPublicKey::from_public_key_der(&pem.der).ok(),
        "RSA PUBLIC KEY" => RsaPublicKey::from_pkcs1_der(&pem.der).ok(),
        _ => None,
    }
}

/// OpenSSL's traditional PEM encryption (`DEK-Info: <cipher>,<hex IV>`):
/// the key is `EVP_BytesToKey` with MD5, one round, salted with the IV's
/// first 8 bytes; the body is CBC with PKCS#7 padding.
fn legacy_decrypt(dek_info: &str, password: &[u8], der: &[u8]) -> Option<Vec<u8>> {
    let (cipher, iv_hex) = dek_info.split_once(',')?;
    let iv = encodings::hex_decode(iv_hex.trim())?;
    let salt = iv.get(..8)?;
    let key_len = match cipher.trim().to_ascii_uppercase().as_str() {
        "DES-EDE3-CBC" => 24,
        "DES-CBC" => 8,
        "AES-128-CBC" => 16,
        "AES-192-CBC" => 24,
        "AES-256-CBC" => 32,
        _ => return None,
    };
    let mut key = Vec::with_capacity(key_len + 16);
    let mut block: Vec<u8> = Vec::new();
    while key.len() < key_len {
        let mut md5 = Md5::new();
        md5.update(&block);
        md5.update(password);
        md5.update(salt);
        block = md5.finalize().to_vec();
        key.extend_from_slice(&block);
    }
    key.truncate(key_len);
    let mut buf = der.to_vec();
    fn run<C: BlockDecryptMut + KeyIvInit>(key: &[u8], iv: &[u8], buf: &mut [u8]) -> Option<usize> {
        let cipher = C::new_from_slices(key, iv).ok()?;
        cipher
            .decrypt_padded_mut::<Pkcs7>(buf)
            .ok()
            .map(|plain| plain.len())
    }
    let len = match key_len {
        8 => run::<cbc::Decryptor<des::Des>>(&key, &iv, &mut buf),
        24 if cipher.trim().to_ascii_uppercase().starts_with("DES") => {
            run::<cbc::Decryptor<des::TdesEde3>>(&key, &iv, &mut buf)
        }
        16 => run::<cbc::Decryptor<aes::Aes128>>(&key, &iv, &mut buf),
        24 => run::<cbc::Decryptor<aes::Aes192>>(&key, &iv, &mut buf),
        _ => run::<cbc::Decryptor<aes::Aes256>>(&key, &iv, &mut buf),
    }?;
    buf.truncate(len);
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        encodings::hex_encode(bytes)
    }

    #[test]
    fn digests_and_hmacs() {
        assert_eq!(
            hex(&digest("md5", b"abc").unwrap()),
            "900150983CD24FB0D6963F7D28E17F72"
        );
        assert_eq!(
            digest("", b"abc").unwrap(),
            digest("SHA512", b"abc").unwrap()
        );
        assert!(digest("SHA-256", b"abc").is_err());
        assert!(digest(" SHA256", b"abc").is_err());
        assert_eq!(
            hex(&hmac("SHA256", b"key", b"abc").unwrap()),
            "9C196E32DC0175F86F4B1CB89289D6619DE6BEE699E4C378E68309ED97A1A6AB"
        );
        assert_eq!(hmac("MD5", b"", b"abc").unwrap(), b"abc");
        assert!(hmac("SHA-1", b"", b"abc").is_err());
    }

    /// The unencrypted key formats sign alike and verify (the engine's
    /// signatures for an OpenSSL key are in the fixture, checked against
    /// its public keys; encrypted keys were checked against OpenSSL output
    /// when this was written, their scrypt is too slow for a debug test).
    #[test]
    fn key_formats() {
        use rsa::pkcs1::{EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding};
        use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey};
        let mut rng = rsa::rand_core::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 1024).unwrap();
        // FileMaker text ends lines with CR.
        let cr = |s: &str| s.replace('\n', "\r");
        let pkcs1 = cr(&key.to_pkcs1_pem(LineEnding::LF).unwrap());
        let pkcs8 = cr(&key.to_pkcs8_pem(LineEnding::LF).unwrap());
        let public = key.to_public_key();
        let spki = cr(&public.to_public_key_pem(LineEnding::LF).unwrap());
        let rsa_public = cr(&public.to_pkcs1_pem(LineEnding::LF).unwrap());
        let sig = generate_signature(b"abc", "SHA256", &pkcs1, "")
            .unwrap()
            .unwrap();
        assert_eq!(
            generate_signature(b"abc", "sha256", &pkcs8, "x")
                .unwrap()
                .unwrap(),
            sig
        );
        for public in [&spki, &rsa_public] {
            assert_eq!(
                verify_signature(b"abc", "SHA256", public, &sig),
                Ok(Some(true))
            );
            assert_eq!(
                verify_signature(b"abd", "SHA256", public, &sig),
                Ok(Some(false))
            );
            assert_eq!(
                verify_signature(b"abc", "SHA256", public, b"junk"),
                Ok(Some(false))
            );
            assert_eq!(verify_signature(b"abc", "SHA256", public, b""), Ok(None));
        }
        assert_eq!(
            verify_signature(b"abc", "SHA256", &pkcs1, &sig),
            Err(BAD_KEY)
        );
        assert_eq!(
            verify_signature(b"abc", "xyz", "junk", &sig),
            Err(UNKNOWN_NAME)
        );
        assert_eq!(generate_signature(b"abc", "SHA256", "", ""), Ok(None));
    }

    #[test]
    fn pem_blocks() {
        let text = "junk\r-----BEGIN RSA PRIVATE KEY-----\rProc-Type: 4,ENCRYPTED\r\
                    DEK-Info: DES-EDE3-CBC,0011223344556677\r\rQUJD\rREVG\r-----END RSA PRIVATE KEY-----\r";
        let pem = pem(text).unwrap();
        assert_eq!(pem.label, "RSA PRIVATE KEY");
        assert_eq!(pem.headers.len(), 2);
        assert_eq!(pem.der, b"ABCDEF");
        assert!(super::pem("-----BEGIN X-----\rQUJD\r").is_none());
        assert!(private_key_from_pem("junk", b"").is_none());
    }
}
