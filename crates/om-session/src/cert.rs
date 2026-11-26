//! Just enough X.509 to check a TLS handshake signature and to show a
//! certificate.
//!
//! FileMaker Pro presents an X.509 version 1 client certificate, and
//! FileMaker Server's default server certificate is version 1 too:
//! certificate path libraries (webpki) refuse to parse either. OpenMaker
//! Server does not validate client certificate chains (om-server's `tls`),
//! and a client connects to a server certificate the user permitted by its
//! fingerprint ([`crate::tls`]), so both need only the certificate's public
//! key, its names and its validity, read here from the DER encoding (RFC
//! 5280 section 4.1). Certificates come from the peer: everything here is
//! bounded and returns `None` or replacement text rather than failing.

use rustls::SignatureScheme;

/// The public key of a certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicKey<'a> {
    /// The DER `RSAPublicKey` (modulus and exponent) and the modulus size in bits.
    Rsa {
        der: &'a [u8],
        bits: usize,
    },
    /// An uncompressed P-256 point.
    EcP256(&'a [u8]),
    /// An uncompressed P-384 point.
    EcP384(&'a [u8]),
    Ed25519(&'a [u8]),
    /// A key type this server cannot verify, with its algorithm OID bytes.
    Other(&'a [u8]),
}

/// The fields of a certificate OpenMaker uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Certificate<'a> {
    /// The X.509 version (1, 2 or 3).
    pub version: u8,
    pub key: PublicKey<'a>,
    /// The issuer's and the subject's names: the contents of their DER
    /// `Name` ([`describe_name`]).
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    /// When the certificate becomes valid and when it expires, if the
    /// validity reads as two times ([`Time::describe`]).
    pub validity: Option<(Time<'a>, Time<'a>)>,
    /// The whole DER `SubjectPublicKeyInfo`, as a TLS library verifies a
    /// raw public key's signatures.
    pub spki: &'a [u8],
}

/// A certificate time: a DER `UTCTime` or `GeneralizedTime` as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Time<'a> {
    tag: u8,
    text: &'a [u8],
}

const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
const OID_ED25519: &[u8] = &[0x2b, 0x65, 0x70];

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const INTEGER: u8 = 0x02;
const BIT_STRING: u8 = 0x03;
const OID: u8 = 0x06;
const VERSION_TAG: u8 = 0xa0;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;

/// Split one DER TLV off `input`: `(tag, contents, rest)`.
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = match first {
        0x00..=0x7f => (usize::from(first), rest),
        0x81..=0x84 => {
            let n = usize::from(first & 0x7f);
            let bytes = rest.get(..n)?;
            let len = bytes
                .iter()
                .fold(0usize, |acc, &b| acc << 8 | usize::from(b));
            (len, &rest[n..])
        }
        _ => return None,
    };
    let contents = rest.get(..len)?;
    Some((tag, contents, &rest[len..]))
}

/// Split a TLV with the expected `tag`.
fn expect(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (got, contents, rest) = tlv(input)?;
    (got == tag).then_some((contents, rest))
}

/// Parse the version and public key of a DER certificate.
pub fn parse(der: &[u8]) -> Option<Certificate<'_>> {
    let (cert, _) = expect(der, SEQUENCE)?;
    let (tbs, _) = expect(cert, SEQUENCE)?;

    // version [0] EXPLICIT INTEGER DEFAULT v1 (encoded value 0 = v1).
    let (version, rest) = match tlv(tbs)? {
        (VERSION_TAG, explicit, rest) => {
            let (value, _) = expect(explicit, INTEGER)?;
            (value.last().copied()?.checked_add(1)?, rest)
        }
        _ => (1, tbs),
    };
    let (_serial, rest) = expect(rest, INTEGER)?;
    let (_signature, rest) = expect(rest, SEQUENCE)?;
    let (issuer, rest) = expect(rest, SEQUENCE)?;
    let (validity, rest) = expect(rest, SEQUENCE)?;
    let (subject, rest) = expect(rest, SEQUENCE)?;
    let spki_der = rest;
    let (spki, after) = expect(rest, SEQUENCE)?;
    let spki_der = &spki_der[..spki_der.len() - after.len()];
    let validity = (|| {
        let (from_tag, from, rest) = tlv(validity)?;
        let (until_tag, until, _) = tlv(rest)?;
        let time =
            |tag, text| matches!(tag, UTC_TIME | GENERALIZED_TIME).then_some(Time { tag, text });
        Some((time(from_tag, from)?, time(until_tag, until)?))
    })();

    let (algorithm, rest) = expect(spki, SEQUENCE)?;
    let (key_bits, _) = expect(rest, BIT_STRING)?;
    // The first BIT STRING byte counts unused bits; keys use whole bytes.
    let (&0, key) = key_bits.split_first()? else {
        return None;
    };
    let (oid, params) = expect(algorithm, OID)?;
    let key = match oid {
        OID_RSA => {
            let (rsa, _) = expect(key, SEQUENCE)?;
            let (modulus, _) = expect(rsa, INTEGER)?;
            let significant = modulus.iter().skip_while(|&&b| b == 0).count();
            PublicKey::Rsa {
                der: key,
                bits: significant * 8,
            }
        }
        OID_EC => match expect(params, OID)?.0 {
            OID_P256 => PublicKey::EcP256(key),
            OID_P384 => PublicKey::EcP384(key),
            curve => PublicKey::Other(curve),
        },
        OID_ED25519 => PublicKey::Ed25519(key),
        other => PublicKey::Other(other),
    };
    Some(Certificate {
        version,
        key,
        issuer,
        subject,
        validity,
        spki: spki_der,
    })
}

/// The SHA-256 fingerprint of a DER certificate, the one a permitted
/// certificate is kept by.
pub fn sha256(der: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, der);
    let mut out = [0; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// A fingerprint as `openssl x509 -fingerprint` prints one: uppercase
/// hexadecimal pairs joined by colons.
pub fn fingerprint_text(fingerprint: &[u8]) -> String {
    fingerprint
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The most characters a name's value, and a whole name, is shown with.
const MAX_VALUE_CHARS: usize = 256;
const MAX_NAME_CHARS: usize = 1024;

/// A name as RFC 4514 writes one, most specific part first:
/// `CN=host, O=Company, C=US`. Attributes it does not know are written by
/// their OID; a value it cannot read is written as `?`.
pub fn describe_name(name: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut rest = name;
    while let Some((SET, mut set, after)) = tlv(rest) {
        rest = after;
        while let Some((SEQUENCE, attribute, after)) = tlv(set) {
            set = after;
            let Some((oid, value)) = expect(attribute, OID) else {
                continue;
            };
            let text =
                tlv(value).map_or_else(|| "?".to_owned(), |(tag, bytes, _)| string(tag, bytes));
            parts.push(format!("{}={text}", attribute_label(oid)));
        }
    }
    parts.reverse();
    bounded(parts.join(", "), MAX_NAME_CHARS)
}

/// The short name of a naming attribute, or its OID in dotted form.
fn attribute_label(oid: &[u8]) -> String {
    match oid {
        [0x55, 0x04, 0x03] => "CN".into(),
        [0x55, 0x04, 0x06] => "C".into(),
        [0x55, 0x04, 0x07] => "L".into(),
        [0x55, 0x04, 0x08] => "ST".into(),
        [0x55, 0x04, 0x0a] => "O".into(),
        [0x55, 0x04, 0x0b] => "OU".into(),
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x01] => "emailAddress".into(),
        [0x09, 0x92, 0x26, 0x89, 0x93, 0xf2, 0x2c, 0x64, 0x01, 0x19] => "DC".into(),
        other => dotted(other),
    }
}

/// An OID's dotted form, or `?` for a malformed one.
fn dotted(oid: &[u8]) -> String {
    let mut arcs: Vec<u64> = Vec::new();
    let mut value: u64 = 0;
    for &byte in oid {
        value = match value.checked_mul(128) {
            Some(shifted) => shifted | u64::from(byte & 0x7f),
            None => return "?".into(),
        };
        if byte & 0x80 == 0 {
            if arcs.is_empty() {
                let first = (value / 40).min(2);
                arcs.extend([first, value - first * 40]);
            } else {
                arcs.push(value);
            }
            value = 0;
        }
    }
    if arcs.is_empty() || oid.last().is_some_and(|byte| byte & 0x80 != 0) {
        return "?".into();
    }
    arcs.iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// A directory string's text: UTF-8, printable, IA5 and Teletex strings
/// (read as Latin-1), and BMP strings (UTF-16). Control characters show as
/// `?`, and long values are cut.
fn string(tag: u8, bytes: &[u8]) -> String {
    let text: String = match tag {
        0x0c | 0x13 | 0x16 => String::from_utf8_lossy(bytes).into_owned(),
        0x14 => bytes.iter().map(|&b| char::from(b)).collect(),
        0x1e => char::decode_utf16(
            bytes
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]])),
        )
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect(),
        _ => return "?".into(),
    };
    bounded(text, MAX_VALUE_CHARS)
}

/// `text` with control characters as `?`, cut to `max` characters.
fn bounded(text: String, max: usize) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .take(max)
        .collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

impl Time<'_> {
    /// The time as `2027-11-25 20:22:31 UTC`, or as written when it does
    /// not read as a UTC time.
    pub fn describe(&self) -> String {
        let digits = |range: std::ops::Range<usize>| -> Option<u32> {
            let part = self.text.get(range)?;
            if !part.iter().all(u8::is_ascii_digit) {
                return None;
            }
            std::str::from_utf8(part).ok()?.parse().ok()
        };
        let (year, at) = match (self.tag, self.text.len()) {
            // YYMMDDHHMMSSZ; RFC 5280: YY of 50 and above is 19YY.
            (UTC_TIME, 13) => (
                digits(0..2).map(|yy| if yy >= 50 { 1900 + yy } else { 2000 + yy }),
                2,
            ),
            // YYYYMMDDHHMMSSZ.
            (GENERALIZED_TIME, 15) => (digits(0..4), 4),
            _ => (None, 0),
        };
        let parts = (|| {
            Some((
                year?,
                digits(at..at + 2)?,
                digits(at + 2..at + 4)?,
                digits(at + 4..at + 6)?,
                digits(at + 6..at + 8)?,
                digits(at + 8..at + 10)?,
            ))
        })();
        match parts {
            Some((year, month, day, hour, minute, second)) if self.text.last() == Some(&b'Z') => {
                format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
            }
            _ => bounded(String::from_utf8_lossy(self.text).into_owned(), 32),
        }
    }
}

impl PublicKey<'_> {
    /// A short description for logs, e.g. `RSA 2048-bit`.
    pub fn describe(&self) -> String {
        match self {
            PublicKey::Rsa { bits, .. } => format!("RSA {bits}-bit"),
            PublicKey::EcP256(_) => "ECDSA P-256".into(),
            PublicKey::EcP384(_) => "ECDSA P-384".into(),
            PublicKey::Ed25519(_) => "Ed25519".into(),
            PublicKey::Other(oid) => format!("unsupported key (OID {oid:02x?})"),
        }
    }

    /// Verify a handshake `signature` over `message` made with `scheme`.
    pub fn verify(&self, scheme: SignatureScheme, message: &[u8], signature: &[u8]) -> bool {
        use ring::signature as sig;
        let (algorithm, key): (&dyn sig::VerificationAlgorithm, &[u8]) = match (scheme, *self) {
            (SignatureScheme::ED25519, PublicKey::Ed25519(k)) => (&sig::ED25519, k),
            (SignatureScheme::ECDSA_NISTP256_SHA256, PublicKey::EcP256(k)) => {
                (&sig::ECDSA_P256_SHA256_ASN1, k)
            }
            (SignatureScheme::ECDSA_NISTP384_SHA384, PublicKey::EcP384(k)) => {
                (&sig::ECDSA_P384_SHA384_ASN1, k)
            }
            (SignatureScheme::RSA_PSS_SHA256, PublicKey::Rsa { der, .. }) => {
                (&sig::RSA_PSS_2048_8192_SHA256, der)
            }
            (SignatureScheme::RSA_PSS_SHA384, PublicKey::Rsa { der, .. }) => {
                (&sig::RSA_PSS_2048_8192_SHA384, der)
            }
            (SignatureScheme::RSA_PSS_SHA512, PublicKey::Rsa { der, .. }) => {
                (&sig::RSA_PSS_2048_8192_SHA512, der)
            }
            (SignatureScheme::RSA_PKCS1_SHA256, PublicKey::Rsa { der, .. }) => {
                (&sig::RSA_PKCS1_1024_8192_SHA256_FOR_LEGACY_USE_ONLY, der)
            }
            (SignatureScheme::RSA_PKCS1_SHA384, PublicKey::Rsa { der, .. }) => {
                (&sig::RSA_PKCS1_2048_8192_SHA384, der)
            }
            (SignatureScheme::RSA_PKCS1_SHA512, PublicKey::Rsa { der, .. }) => {
                (&sig::RSA_PKCS1_1024_8192_SHA512_FOR_LEGACY_USE_ONLY, der)
            }
            (SignatureScheme::RSA_PKCS1_SHA1, PublicKey::Rsa { der, .. }) => {
                (&sig::RSA_PKCS1_1024_8192_SHA1_FOR_LEGACY_USE_ONLY, der)
            }
            _ => return false,
        };
        sig::UnparsedPublicKey::new(algorithm, key)
            .verify(message, signature)
            .is_ok()
    }
}

/// The schemes [`PublicKey::verify`] supports, most preferred first.
pub const SCHEMES: &[SignatureScheme] = &[
    SignatureScheme::ED25519,
    SignatureScheme::ECDSA_NISTP256_SHA256,
    SignatureScheme::ECDSA_NISTP384_SHA384,
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::RSA_PKCS1_SHA256,
    SignatureScheme::RSA_PKCS1_SHA384,
    SignatureScheme::RSA_PKCS1_SHA512,
    SignatureScheme::RSA_PKCS1_SHA1,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v3_ecdsa_and_verifies_its_signature() {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["client".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let parsed = parse(cert.der()).unwrap();
        assert_eq!(parsed.version, 3);
        assert!(matches!(parsed.key, PublicKey::EcP256(_)));

        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = key.serialize_der();
        let signer = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            &pkcs8,
            &rng,
        )
        .unwrap();
        let signature = signer.sign(&rng, b"handshake").unwrap();
        let scheme = SignatureScheme::ECDSA_NISTP256_SHA256;
        assert!(parsed.key.verify(scheme, b"handshake", signature.as_ref()));
        assert!(!parsed.key.verify(scheme, b"tampered", signature.as_ref()));
        assert!(!parsed.key.verify(
            SignatureScheme::RSA_PSS_SHA256,
            b"handshake",
            signature.as_ref()
        ));
    }

    #[test]
    fn parses_v1_rsa_certificate() {
        // An OpenSSL `req -x509` style v1 certificate, as FileMaker Server's
        // default certificate is: no [0] version field. Built by hand: tbs =
        // serial, sigalg, issuer, validity, subject, spki.
        fn der(tag: u8, contents: &[u8]) -> Vec<u8> {
            let mut out = vec![tag];
            match contents.len() {
                n @ 0..=0x7f => out.push(n as u8),
                n @ 0x80..=0xff => out.extend([0x81, n as u8]),
                n => out.extend([0x82, (n >> 8) as u8, n as u8]),
            }
            out.extend_from_slice(contents);
            out
        }
        let mut modulus = vec![0x00, 0xc0];
        modulus.extend([0x11; 255]); // 2048-bit modulus with a leading zero
        let rsa_key = der(
            SEQUENCE,
            &[der(INTEGER, &modulus), der(INTEGER, &[1, 0, 1])].concat(),
        );
        let mut bits = vec![0];
        bits.extend(&rsa_key);
        let spki = der(
            SEQUENCE,
            &[
                der(SEQUENCE, &[der(OID, OID_RSA), vec![0x05, 0x00]].concat()),
                der(BIT_STRING, &bits),
            ]
            .concat(),
        );
        // Names as RDNs of one attribute each, the country first.
        let attribute = |oid: &[u8], tag: u8, value: &[u8]| {
            der(
                SET,
                &der(SEQUENCE, &[der(OID, oid), der(tag, value)].concat()),
            )
        };
        let country = attribute(&[0x55, 0x04, 0x06], 0x13, b"US");
        let issuer = der(
            SEQUENCE,
            &[
                country.clone(),
                attribute(&[0x55, 0x04, 0x03], 0x13, b"Example Root\x07"),
            ]
            .concat(),
        );
        let subject = der(
            SEQUENCE,
            &[
                country,
                attribute(&[0x55, 0x04, 0x0a], 0x0c, "Exämple".as_bytes()),
                attribute(
                    &[0x55, 0x04, 0x03],
                    0x1e,
                    &[0, b'h', 0, b'o', 0, b's', 0, b't'],
                ),
                attribute(&[0x2a, 0x03], 0x13, b"x"),
            ]
            .concat(),
        );
        let validity = der(
            SEQUENCE,
            &[
                der(UTC_TIME, b"491125202231Z"),
                der(GENERALIZED_TIME, b"20271125202231Z"),
            ]
            .concat(),
        );
        let tbs = der(
            SEQUENCE,
            &[
                der(INTEGER, &[7]),
                der(SEQUENCE, &[]),
                issuer,
                validity,
                subject,
                spki,
            ]
            .concat(),
        );
        let cert = der(
            SEQUENCE,
            &[tbs, der(SEQUENCE, &[]), der(BIT_STRING, &[0])].concat(),
        );
        let parsed = parse(&cert).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(
            parsed.key,
            PublicKey::Rsa {
                der: &rsa_key,
                bits: 2048
            }
        );
        assert_eq!(parsed.key.describe(), "RSA 2048-bit");
        // Most specific first; a control character and an attribute it does
        // not know are shown, not dropped.
        assert_eq!(describe_name(parsed.issuer), "CN=Example Root?, C=US");
        assert_eq!(
            describe_name(parsed.subject),
            "1.2.3=x, CN=host, O=Exämple, C=US"
        );
        let (from, until) = parsed.validity.unwrap();
        assert_eq!(from.describe(), "2049-11-25 20:22:31 UTC");
        assert_eq!(until.describe(), "2027-11-25 20:22:31 UTC");
        assert_eq!(fingerprint_text(&sha256(b"abc"))[..11], *"BA:78:16:BF");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]).is_none());
        assert!(parse(&[0x30, 0x03, 0x02, 0x01, 0x00]).is_none());
    }
}
