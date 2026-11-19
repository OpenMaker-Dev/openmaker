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
//!
//! The private-key operation is `rsa` 0.10's: constant-time arithmetic,
//! blinded with the operating system's random numbers. 0.9's leaked the
//! key through its timing (RUSTSEC-2023-0071), which a client can measure
//! when OpenMaker Server signs in a script (#29).

use super::{encodings, UNKNOWN_NAME};
use crate::error::CalcError;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use md5::Md5;
use rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::{Pkcs1v15Sign, PrivateKey, PublicKey};
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

fn private_key_from_pem(text: &str, password: &[u8]) -> Option<PrivateKey> {
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
            PrivateKey::from_pkcs1_der(&der).ok()
        }
        "PRIVATE KEY" => PrivateKey::from_pkcs8_der(&pem.der).ok(),
        "ENCRYPTED PRIVATE KEY" => PrivateKey::from_pkcs8_encrypted_der(&pem.der, password).ok(),
        _ => None,
    }
}

fn public_key_from_pem(text: &str) -> Option<PublicKey> {
    let pem = pem(text)?;
    match pem.label.as_str() {
        "PUBLIC KEY" => PublicKey::from_public_key_der(&pem.der).ok(),
        "RSA PUBLIC KEY" => PublicKey::from_pkcs1_der(&pem.der).ok(),
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

    /// A 1024-bit key made with OpenSSL for these tests (the engine's
    /// signatures for another OpenSSL key are in the fixture, checked
    /// against its public key), in each format the functions read.
    const PKCS1_PRIVATE: &str = "\
-----BEGIN RSA PRIVATE KEY-----\n\
MIICXQIBAAKBgQCrW8Wu2elo457F+N/hEMNGylPdK5+XKgljD9rsr/HryrElFANk\n\
NKZObPQPn9+kZcamIC791Vyf6vexyn71kTwcYX0BRk0xDO24UIyJGgZZ6zLqH6wC\n\
Xb0Ij9vFA9zKWwziXTUDtJfx3g+ONmUjuzY7MReAy5fomhWyTbz1HbMLjQIDAQAB\n\
AoGAa+/NsFrAxHoEK82dL8aASpntg3YT+4LWENkO3GHjb9PoC2P00uy7CNFmVmUs\n\
2P/mxgOltevsgSrfRNU5bXNDOdCVQznJQrt04Pj2FxCmIx7E/TccN2Y9cQ00Y1/l\n\
/1tDsBR3YGc0rrnLeLE1W2P6dNZB9I1/JgGq4SLOkpzWr10CQQDi7g0ouiG1FwNv\n\
XQP+hK6Xpnt/PYkTcnbTisUJzqUKedr8nFo5tWkgI/lwONpNgsWkfJbtUbbCpmO/\n\
CWtws/ivAkEAwU9PTos8m+BtfXd7GAONhE2knyUp8+57ng6aqBIITyXR7S95hV71\n\
n4N4Ed+I2rdkWsJy2rj/gxpH2wxuqPBWgwJASR7FVLMs+0nQDIkfKVqdWzYEowoz\n\
6fU+/oi/VpqkKUz8V9sVY3Tly7prvFJFXSZfAtI2W3+x8ORmTX7+hJEo9wJBAK3v\n\
BpTNg4NdtCaoUwGQcM0Sb4Uik1UAyAm3akC/uOo+RVz80oLGposYZ/jtAN6LFKj1\n\
PRLAUmdMRks6LHeliLECQQDNO4Vt2EsnEYJ/61rh3dDbLbg8I+UolkbLw3soXjlV\n\
E8udXarD25UZ6HW9jymYLdO5gv5CgMkjlnFcvwALSdyW\n\
-----END RSA PRIVATE KEY-----\n";
    const PKCS8_PRIVATE: &str = "\
-----BEGIN PRIVATE KEY-----\n\
MIICdwIBADANBgkqhkiG9w0BAQEFAASCAmEwggJdAgEAAoGBAKtbxa7Z6WjjnsX4\n\
3+EQw0bKU90rn5cqCWMP2uyv8evKsSUUA2Q0pk5s9A+f36RlxqYgLv3VXJ/q97HK\n\
fvWRPBxhfQFGTTEM7bhQjIkaBlnrMuofrAJdvQiP28UD3MpbDOJdNQO0l/HeD442\n\
ZSO7NjsxF4DLl+iaFbJNvPUdswuNAgMBAAECgYBr782wWsDEegQrzZ0vxoBKme2D\n\
dhP7gtYQ2Q7cYeNv0+gLY/TS7LsI0WZWZSzY/+bGA6W16+yBKt9E1Tltc0M50JVD\n\
OclCu3Tg+PYXEKYjHsT9Nxw3Zj1xDTRjX+X/W0OwFHdgZzSuuct4sTVbY/p01kH0\n\
jX8mAarhIs6SnNavXQJBAOLuDSi6IbUXA29dA/6Erpeme389iRNydtOKxQnOpQp5\n\
2vycWjm1aSAj+XA42k2CxaR8lu1RtsKmY78Ja3Cz+K8CQQDBT09Oizyb4G19d3sY\n\
A42ETaSfJSnz7nueDpqoEghPJdHtL3mFXvWfg3gR34jat2RawnLauP+DGkfbDG6o\n\
8FaDAkBJHsVUsyz7SdAMiR8pWp1bNgSjCjPp9T7+iL9WmqQpTPxX2xVjdOXLumu8\n\
UkVdJl8C0jZbf7Hw5GZNfv6EkSj3AkEAre8GlM2Dg120JqhTAZBwzRJvhSKTVQDI\n\
CbdqQL+46j5FXPzSgsamixhn+O0A3osUqPU9EsBSZ0xGSzosd6WIsQJBAM07hW3Y\n\
SycRgn/rWuHd0NstuDwj5SiWRsvDeyheOVUTy51dqsPblRnodb2PKZgt07mC/kKA\n\
ySOWcVy/AAtJ3JY=\n\
-----END PRIVATE KEY-----\n";
    const SPKI_PUBLIC: &str = "\
-----BEGIN PUBLIC KEY-----\n\
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQCrW8Wu2elo457F+N/hEMNGylPd\n\
K5+XKgljD9rsr/HryrElFANkNKZObPQPn9+kZcamIC791Vyf6vexyn71kTwcYX0B\n\
Rk0xDO24UIyJGgZZ6zLqH6wCXb0Ij9vFA9zKWwziXTUDtJfx3g+ONmUjuzY7MReA\n\
y5fomhWyTbz1HbMLjQIDAQAB\n\
-----END PUBLIC KEY-----\n";
    const PKCS1_PUBLIC: &str = "\
-----BEGIN RSA PUBLIC KEY-----\n\
MIGJAoGBAKtbxa7Z6WjjnsX43+EQw0bKU90rn5cqCWMP2uyv8evKsSUUA2Q0pk5s\n\
9A+f36RlxqYgLv3VXJ/q97HKfvWRPBxhfQFGTTEM7bhQjIkaBlnrMuofrAJdvQiP\n\
28UD3MpbDOJdNQO0l/HeD442ZSO7NjsxF4DLl+iaFbJNvPUdswuNAgMBAAE=\n\
-----END RSA PUBLIC KEY-----\n";
    /// `openssl dgst -sha256 -sign` of `abc` with that key, base64.
    const ABC_SHA256: &str = "ISQf5prtj9BSWeqg5Rfgbfg1mt3BWIdp3aWyNfkEyqDwblip5Rkvp2Mx8wqzuUrhRZPDJuFZq21lBO3B2LGfg+n/SYzVb4LqABs1G/SafC5WLXXN12oTN44OhuCAg1i9hhm5A71x6CkGl6zEhi9t3GsD4Tan6D89QSXPatIa+LQ=";

    /// The unencrypted key formats sign as OpenSSL does and verify
    /// (encrypted keys were checked against OpenSSL output when this was
    /// written; their scrypt is too slow for a debug test).
    #[test]
    fn key_formats() {
        // FileMaker text ends lines with CR.
        let cr = |s: &str| s.replace('\n', "\r");
        let (pkcs1, pkcs8) = (cr(PKCS1_PRIVATE), cr(PKCS8_PRIVATE));
        let (spki, rsa_public) = (cr(SPKI_PUBLIC), cr(PKCS1_PUBLIC));
        let sig = generate_signature(b"abc", "SHA256", &pkcs1, "")
            .unwrap()
            .unwrap();
        assert_eq!(Some(sig.clone()), encodings::base64_decode(ABC_SHA256));
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
