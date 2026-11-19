//! Just enough X.509 to check a client's TLS handshake signature.
//!
//! FileMaker Pro presents an X.509 version 1 certificate, which certificate
//! path libraries (webpki) refuse to parse. The server does not validate client
//! certificate chains anyway (see `tls`), so it only needs the certificate's
//! public key, read here from the DER encoding (RFC 5280 section 4.1).

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

/// The fields of a certificate the server uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Certificate<'a> {
    /// The X.509 version (1, 2 or 3).
    pub version: u8,
    pub key: PublicKey<'a>,
}

const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
const OID_ED25519: &[u8] = &[0x2b, 0x65, 0x70];

const SEQUENCE: u8 = 0x30;
const INTEGER: u8 = 0x02;
const BIT_STRING: u8 = 0x03;
const OID: u8 = 0x06;
const VERSION_TAG: u8 = 0xa0;

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
    let (_issuer, rest) = expect(rest, SEQUENCE)?;
    let (_validity, rest) = expect(rest, SEQUENCE)?;
    let (_subject, rest) = expect(rest, SEQUENCE)?;
    let (spki, _) = expect(rest, SEQUENCE)?;

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
    Some(Certificate { version, key })
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
        // An OpenSSL `req -x509` style v1 certificate: no [0] version field.
        // Built by hand: tbs = serial, sigalg, issuer, validity, subject, spki.
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
        let name = der(SEQUENCE, &[]);
        let tbs = der(
            SEQUENCE,
            &[
                der(INTEGER, &[7]),
                der(SEQUENCE, &[]),
                name.clone(),
                der(SEQUENCE, &[]),
                name,
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
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[]).is_none());
        assert!(parse(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]).is_none());
        assert!(parse(&[0x30, 0x03, 0x02, 0x01, 0x00]).is_none());
    }
}
