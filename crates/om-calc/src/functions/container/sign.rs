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
use rsa::pkcs1::{RsaPrivateKeyRef, RsaPublicKeyRef, Version};
use rsa::pkcs8::der::asn1::{AsUintRef, Null};
use rsa::pkcs8::der::Decode;
use rsa::pkcs8::spki::AlgorithmIdentifierRef;
use rsa::pkcs8::{EncryptedPrivateKeyInfoRef, PrivateKeyInfoRef, SubjectPublicKeyInfoRef};
use rsa::{BoxedUint, Pkcs1v15Sign, PrivateKey, PublicKey};
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
            private_key_from_pkcs1(&der)
        }
        "PRIVATE KEY" => private_key_from_pkcs8(&pem.der),
        "ENCRYPTED PRIVATE KEY" => {
            let document = EncryptedPrivateKeyInfoRef::from_der(&pem.der)
                .ok()?
                .decrypt(password)
                .ok()?;
            private_key_from_pkcs8(document.as_bytes())
        }
        _ => None,
    }
}

fn public_key_from_pem(text: &str) -> Option<PublicKey> {
    let pem = pem(text)?;
    match pem.label.as_str() {
        "PUBLIC KEY" => {
            let info = SubjectPublicKeyInfoRef::from_der(&pem.der).ok()?;
            rsa_algorithm(&info.algorithm)?;
            public_key_from_pkcs1(info.subject_public_key.as_bytes()?)
        }
        "RSA PUBLIC KEY" => public_key_from_pkcs1(&pem.der),
        _ => None,
    }
}

// The keys are read from their components here rather than by `rsa`'s
// decoders, whose policies refuse keys `rsa` 0.9 read and OpenSSL signs
// and verifies with: public keys below 1,024 bits, and private keys whose
// primes are not each about half the modulus. These policies are fixed,
// not chosen per key, so that reading a key does not depend on the sizes
// of its secret primes (#29).

/// Public moduli read: any size up to `rsa`'s largest, 8,192 bits (`rsa`
/// 0.9 had no smallest and stopped at 4,096).
const PUBLIC_KEY_BITS: std::ops::RangeInclusive<usize> = 1..=PublicKey::MAX_SIZE;

/// `rsaEncryption` with NULL parameters, as `rsa` 0.9 required.
fn rsa_algorithm(algorithm: &AlgorithmIdentifierRef<'_>) -> Option<()> {
    let null = algorithm.parameters_any().ok()? == Null.into();
    (algorithm.oid == rsa::pkcs1::ALGORITHM_OID && null).then_some(())
}

/// An unsigned big-endian integer `bits` wide.
fn uint(bytes: &[u8], bits: u32) -> Option<BoxedUint> {
    BoxedUint::from_be_slice(bytes, bits).ok()
}

/// The bits of `bytes`, whole bytes.
fn bits(bytes: &[u8]) -> Option<u32> {
    u32::try_from(bytes.len()).ok()?.checked_mul(8)
}

/// A two-prime PKCS#1 `RSAPrivateKey`, its primes stored at the modulus's
/// width whatever their sizes.
fn private_key_from_pkcs1(der: &[u8]) -> Option<PrivateKey> {
    let key = RsaPrivateKeyRef::from_der(der).ok()?;
    if key.version() != Version::TwoPrime {
        return None;
    }
    let modulus = key.modulus.as_uint_ref().as_bytes();
    let width = bits(modulus)?;
    let exponent = key.public_exponent.as_uint_ref().as_bytes();
    let n = uint(modulus, width)?;
    let e = uint(exponent, bits(exponent)?)?;
    let d = uint(key.private_exponent.as_uint_ref().as_bytes(), width)?;
    let p = uint(key.prime1.as_uint_ref().as_bytes(), width)?;
    let q = uint(key.prime2.as_uint_ref().as_bytes(), width)?;
    PrivateKey::from_components_with_wide_primes(n, e, d, vec![p, q]).ok()
}

/// A PKCS#8 `PrivateKeyInfo` holding an RSA key.
fn private_key_from_pkcs8(der: &[u8]) -> Option<PrivateKey> {
    let info = PrivateKeyInfoRef::from_der(der).ok()?;
    rsa_algorithm(&info.algorithm)?;
    private_key_from_pkcs1(info.private_key.as_bytes())
}

/// A PKCS#1 `RSAPublicKey`.
fn public_key_from_pkcs1(der: &[u8]) -> Option<PublicKey> {
    let key = RsaPublicKeyRef::from_der(der).ok()?;
    let modulus = key.modulus.as_uint_ref().as_bytes();
    let exponent = key.public_exponent.as_uint_ref().as_bytes();
    let n = uint(modulus, bits(modulus)?)?;
    let e = uint(exponent, bits(exponent)?)?;
    PublicKey::new_with_size_limits(n, e, PUBLIC_KEY_BITS).ok()
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

    /// Keys `rsa` 0.9 read and OpenSSL signs and verifies with, which
    /// `rsa` 0.10's own decoders refuse (#29 review): a 512-bit key, and a
    /// 1,040-bit key of a 640-bit and a 400-bit prime (`openssl rsa -check`
    /// passes). The signatures are OpenSSL's, of `abc` with SHA256.
    const SMALL_PRIVATE: &str = "\
-----BEGIN RSA PRIVATE KEY-----\n\
MIIBOwIBAAJBAK14aLCy7h5oIIn+XbqeVxVxVjQPNmAZedTzNld53FEKJi2G10zh\n\
L/SVMEY3v0988+imtzhjmUR02HzFie8fCLkCAwEAAQJAPu11HNRr6P/g/e4WVkJm\n\
WNL6cHwsP4Q+W++2gNkmAQdSkeOnhL1x0n1O6yY64q+dIGLLr+RsFxwyqP6WWHA7\n\
AQIhANcBw0oWvFoMO3+Lp6eZQSN1W/aOrE4AHWtZ+tIJzcERAiEAzotLtZvpzIpE\n\
svEqhyFDShCdQbi5uk/yRgj05KiJTSkCIGN5VFIF3NTIjgp/oWygMgvms0qKTdu0\n\
n+fX9HySvpgRAiEAorv1pt11VIcMocCKNXXrwotcm8+j5jvfM3cTHKX9qCECIQCh\n\
+9XZ2qqtmtluvfPySX3DewepMnVEwSp5eduDW/kIMQ==\n\
-----END RSA PRIVATE KEY-----\n";
    const SMALL_PUBLIC: &str = "\
-----BEGIN PUBLIC KEY-----\n\
MFwwDQYJKoZIhvcNAQEBBQADSwAwSAJBAK14aLCy7h5oIIn+XbqeVxVxVjQPNmAZ\n\
edTzNld53FEKJi2G10zhL/SVMEY3v0988+imtzhjmUR02HzFie8fCLkCAwEAAQ==\n\
-----END PUBLIC KEY-----\n";
    const SMALL_ABC_SHA256: &str =
        "OAPfoaaDLKg2C7EnVCRUSzy+jxfyElxWzhNc6PDhh16t2HRraHvH4s8YI+60n0gWinsBj5hx+VRHAbCLAndMPg==";
    const UNBALANCED_PRIVATE: &str = "\
-----BEGIN RSA PRIVATE KEY-----\n\
MIICdgIBAAKBgwCgOfdbvFH8/50wQ3mJ7z1XD8hVtSl9bnRD7BUT7piCO4ya/mQa\n\
pm6Vgx7PK1iz5RH85d3DKiz7nWHeTKQ1YdQlw4XHdFSsEnCs7rTTH+OIhoSBd1zT\n\
pl1RdPBwV5eoaDoM+ppe54kak5PyP7+OLsreR6yXBiCi44KZOJwJRSyg9njRAgMB\n\
AAECgYJWg9Yvn3j5zh/XHqfytc5+QxReSwrTIp/+yOv8rjmFIJhkXHY6+aD+dCVf\n\
ob0srcTPs/aI/qKU5uwlp8T91XIB28fy6ZGXXAIx5IDn/4yLks0UTdrLgP0/fD4l\n\
u8FX+sbqbwymRlNuvw+kFF6nJtjhbVD8TlwS6jA4UyfiQ7zKaIoBAlEAyImI/c+2\n\
owm2hm9efNPO1qG+2tUqbNAojaArjcUT/310KordR14Z+Nnmbg6n1ll2uq7FnKfh\n\
2Z++sdOwZIQ5rt9dsKZJUoUfwZuHmddAUDECMwDMilbf8r4Kb/ocHSF/dYkoB/9a\n\
+Pey/El24D0EWzbnLZk2TLBtuGylnhw5EI7tHxwqoQJRAJVKNm7ldIzZjUV1oX0C\n\
XchRL3lQGj5ZlOciZ2oqXmHkblfBe0xHFMS6yx4B0Rb1ETuXIswl+y/9CYTvGDEs\n\
cpjp9qCsmodRTbvfYkq4AUvxAjMAu2mSWlZf3M5nO/VRKSl3x880IZX31+8kHSEv\n\
p1swfjggp0kzawVNv56Uu5C5dPlJZKECUQCZtxTB2GGi0YmYEuGiPxnyGPruwG0f\n\
5JIetjztLYwEQhEBek7+f+PiOGsDPhUFIy9DKSNkxvL7p3rW2N9Q/L23RKERLgfP\n\
JL/muXQG1uYEwQ==\n\
-----END RSA PRIVATE KEY-----\n";
    const UNBALANCED_ABC_SHA256: &str = "QimFMCd/6k84Vze3emSoZyWgKBYGHl1ncVid4+gB/jZwTdXs5OVIkE8QRHn9N3bMVADs4o7lSZ10ryAKV+d5EEZdoVC4L5wmD4L2dA785/HWu9VSJNAq1cXf3Gok6QxYPwl2UDFIexbqcOi0aqZnZzSLnVLmw4kYcJsI+NZWsWtNqA==";

    #[test]
    fn small_and_unbalanced_keys() {
        let cr = |s: &str| s.replace('\n', "\r");
        for (private, expected) in [
            (SMALL_PRIVATE, SMALL_ABC_SHA256),
            (UNBALANCED_PRIVATE, UNBALANCED_ABC_SHA256),
        ] {
            let sig = generate_signature(b"abc", "SHA256", &cr(private), "");
            assert_eq!(sig, Ok(encodings::base64_decode(expected)));
        }
        let sig = encodings::base64_decode(SMALL_ABC_SHA256).unwrap();
        assert_eq!(
            verify_signature(b"abc", "SHA256", &cr(SMALL_PUBLIC), &sig),
            Ok(Some(true))
        );
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
