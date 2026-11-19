//! Credential obscuring and password hashing for FileMaker login.
//!
//! Login credential fields are obscured inside TLS by XORing their SCSU-encoded
//! bytes with a PBKDF2-HMAC-SHA1 keystream. The keystream is keyed by a per-login
//! `PasswordHash` derived from a nonce (not from the account password) and salted
//! with the 16-byte connection key from the `Connect` reply. Spec:
//! `docs/re/network-protocol.md` sections 4.3 and 5.4.
//!
//! Status: the primitives are verified against RFC 6070 PBKDF2-HMAC-SHA1
//! vectors, and the full login chain (nonce text -> pepper mix -> PasswordHash
//! -> per-field keystream) is verified against known-answer vectors produced by
//! calling the shipped FileMaker library with synthetic inputs. The Unicode
//! decomposition applied before hashing is identity for the ASCII decimal nonce
//! text, which is the only text the login path hashes.

use sha1::Sha1;
use sha2::{Sha256, Sha384, Sha512};

pub mod account;
pub mod decompose;

/// The fixed 30-byte pepper mixed into every `ComputePasswordHash` input
/// (spec 4.3): ASCII `"FileMaker, Inc."`, a NUL, then 14 fixed bytes.
pub const PEPPER: [u8; 30] = [
    0x46, 0x69, 0x6c, 0x65, 0x4d, 0x61, 0x6b, 0x65, 0x72, 0x2c, 0x20, 0x49, 0x6e, 0x63, 0x2e, 0x00,
    0xca, 0x3c, 0x23, 0x90, 0xa5, 0x5b, 0x71, 0x74, 0xb6, 0x1d, 0xf5, 0xb2, 0x6d, 0x00,
];

/// Length of the login `PasswordHash` (SHA1 digest size).
pub const LOGIN_HASH_LEN: usize = 20;

/// Bytes of the connection key used as the `PasswordHash` salt (spec 4.3).
pub const LOGIN_HASH_SALT_LEN: usize = 10;

/// Hash algorithm selector used by FileMaker's password hashing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgo {
    Sha1 = 1,
    Sha256 = 2,
    Sha384 = 3,
    Sha512 = 4,
}

impl HashAlgo {
    /// Digest length in bytes; also the derived-key length of the hash.
    pub fn digest_len(self) -> usize {
        match self {
            HashAlgo::Sha1 => 20,
            HashAlgo::Sha256 => 32,
            HashAlgo::Sha384 => 48,
            HashAlgo::Sha512 => 64,
        }
    }
}

/// The SHA-256 digest of `data`: OpenMaker's content revisions of catalog
/// members (`om_protocol::authoring`), not a FileMaker hash.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    Sha256::digest(data).into()
}

/// PBKDF2-HMAC-SHA1 into a buffer of `out.len()` bytes.
pub fn pbkdf2_hmac_sha1(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    pbkdf2::pbkdf2_hmac::<Sha1>(password, salt, iterations, out);
}

/// Mix `pepper` into UTF-16 `text`, producing the PBKDF2 password buffer.
///
/// Spec 4.3: for each code unit `cu[i]`, emit the two big-endian bytes of
/// `cu[i] XOR ((pepper[(2i) mod n] << 8) | pepper[(2i+1) mod n])`, where `n` is
/// the pepper length. The result is `2 * text.len()` bytes.
pub fn pepper_mix(text: &[u16], pepper: &[u8]) -> Vec<u8> {
    let n = pepper.len();
    let mut out = Vec::with_capacity(text.len() * 2);
    for (i, &cu) in text.iter().enumerate() {
        let mask = if n == 0 {
            0
        } else {
            (u16::from(pepper[(2 * i) % n]) << 8) | u16::from(pepper[(2 * i + 1) % n])
        };
        out.extend_from_slice(&(cu ^ mask).to_be_bytes());
    }
    out
}

/// FileMaker's `ComputePasswordHash`: PBKDF2-HMAC over the pepper-mixed text.
///
/// Spec 4.4: `text` is UTF-16 code units after FileMaker's decomposition
/// ([`decompose::hash_decompose`], which callers apply; identity for ASCII),
/// mixed with `pepper` by [`pepper_mix`], then `PBKDF2-HMAC(pass = mixed,
/// salt, iterations, md = algo)` with a derived-key length of the digest
/// size. The mixed bytes are erased when done.
pub fn compute_password_hash(
    text: &[u16],
    salt: &[u8],
    pepper: &[u8],
    iterations: u32,
    algo: HashAlgo,
) -> Vec<u8> {
    let mixed = zeroize::Zeroizing::new(pepper_mix(text, pepper));
    let mut out = vec![0u8; algo.digest_len()];
    match algo {
        HashAlgo::Sha1 => pbkdf2::pbkdf2_hmac::<Sha1>(&mixed, salt, iterations, &mut out),
        HashAlgo::Sha256 => pbkdf2::pbkdf2_hmac::<Sha256>(&mixed, salt, iterations, &mut out),
        HashAlgo::Sha384 => pbkdf2::pbkdf2_hmac::<Sha384>(&mixed, salt, iterations, &mut out),
        HashAlgo::Sha512 => pbkdf2::pbkdf2_hmac::<Sha512>(&mixed, salt, iterations, &mut out),
    }
    out
}

/// The login nonce: `(~open_nonce + internal_creation) mod 2^32` (spec 4.3).
///
/// `open_nonce` is the Open (op78) reply's per-file nonce (`file[0x3ac]`);
/// `internal_creation` is the file's `mInternalCreation` from its HBAM.
pub fn login_nonce(open_nonce: u32, internal_creation: u32) -> u32 {
    (!open_nonce).wrapping_add(internal_creation)
}

/// Derive the per-login `PasswordHash` that keys credential obscuring.
///
/// Spec 4.3: `ComputePasswordHash(text = NumToString(nonce), salt =
/// conn_key[0..10], pepper = PEPPER, iter = 1, SHA1)`. `NumToString` is the
/// plain unsigned decimal form of `nonce`.
pub fn password_hash_for_login(nonce: u32, conn_key: &[u8; 16]) -> [u8; LOGIN_HASH_LEN] {
    let text: Vec<u16> = nonce.to_string().encode_utf16().collect();
    let hash = compute_password_hash(
        &text,
        &conn_key[..LOGIN_HASH_SALT_LEN],
        &PEPPER,
        1,
        HashAlgo::Sha1,
    );
    let mut out = [0u8; LOGIN_HASH_LEN];
    out.copy_from_slice(&hash);
    out
}

/// Obscure one `PushEncrypt` field in place.
///
/// Mirrors `Encrypt_ObscureBytes(field, len, conn_key, 16, password_hash)`
/// (spec 4.3): PBKDF2-HMAC-SHA1 with `password_hash` as the key, the 16-byte
/// connection key as the salt, one iteration, and a derived-key length equal to
/// the field's length; the result is XORed onto the field's SCSU (XOR 0x5A)
/// bytes. Each field restarts the keystream, so call this once per field.
pub fn obscure_field(
    plain_scsu: &mut [u8],
    password_hash: &[u8; LOGIN_HASH_LEN],
    conn_key: &[u8; 16],
) {
    if plain_scsu.is_empty() {
        return;
    }
    let mut keystream = vec![0u8; plain_scsu.len()];
    pbkdf2_hmac_sha1(password_hash, conn_key, 1, &mut keystream);
    for (b, k) in plain_scsu.iter_mut().zip(keystream) {
        *b ^= k;
    }
}

/// XOR is its own inverse, so de-obscuring is the same operation.
pub fn deobscure_field(
    field: &mut [u8],
    password_hash: &[u8; LOGIN_HASH_LEN],
    conn_key: &[u8; 16],
) {
    obscure_field(field, password_hash, conn_key);
}

/// The 16-byte access key a query program instruction carries at `[1, i, 7]`
/// (spec `docs/re/query-program.md` 2.4):
/// `MD5(BE32(instruction) || connection key || BE32(file id))`.
///
/// The client computes it with the connection key from the Connect reply;
/// the server recomputes it and refuses an instruction whose key differs.
pub fn query_access_key(instruction: u32, conn_key: &[u8; 16], file_id: u32) -> [u8; 16] {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(instruction.to_be_bytes());
    hasher.update(conn_key);
    hasher.update(file_id.to_be_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6070 PBKDF2-HMAC-SHA1 test vectors: confidence in the primitive.
    #[test]
    fn rfc6070_vectors() {
        let mut out = [0u8; 20];
        pbkdf2_hmac_sha1(b"password", b"salt", 1, &mut out);
        assert_eq!(
            out,
            [
                0x0c, 0x60, 0xc8, 0x0f, 0x96, 0x1f, 0x0e, 0x71, 0xf3, 0xa9, 0xb5, 0x24, 0xaf, 0x60,
                0x12, 0x06, 0x2f, 0xe0, 0x37, 0xa6,
            ]
        );

        pbkdf2_hmac_sha1(b"password", b"salt", 2, &mut out);
        assert_eq!(
            out,
            [
                0xea, 0x6c, 0x01, 0x4d, 0xc7, 0x2d, 0x6f, 0x8c, 0xcd, 0x1e, 0xd9, 0x2a, 0xce, 0x1d,
                0x41, 0xf0, 0xd8, 0xde, 0x89, 0x57,
            ]
        );

        let mut long = [0u8; 25];
        pbkdf2_hmac_sha1(
            b"passwordPASSWORDpassword",
            b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
            4096,
            &mut long,
        );
        assert_eq!(
            long,
            [
                0x3d, 0x2e, 0xec, 0x4f, 0xe4, 0x1c, 0x84, 0x9b, 0x80, 0xc8, 0xd8, 0x36, 0x62, 0xc0,
                0xe4, 0x4a, 0x8b, 0x29, 0x1a, 0x96, 0x4c, 0xf2, 0xf0, 0x70, 0x38,
            ]
        );
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn pepper_mix_xors_pepper_pairs_big_endian() {
        // "12": '1'=0x0031 ^ 0x4669 ("Fi") = 0x4658; '2'=0x0032 ^ 0x6c65 ("le") = 0x6c57.
        let text: Vec<u16> = "12".encode_utf16().collect();
        assert_eq!(pepper_mix(&text, &PEPPER), [0x46, 0x58, 0x6c, 0x57]);
        // Code unit 15 wraps: pepper indices 30 and 31 are 0 and 1 again.
        let long: Vec<u16> = vec![0; 16];
        let mixed = pepper_mix(&long, &PEPPER);
        assert_eq!(&mixed[28..30], &[0x6d, 0x00]);
        assert_eq!(&mixed[30..32], &[0x46, 0x69]);
    }

    #[test]
    fn login_nonce_wraps() {
        assert_eq!(login_nonce(0, 0), u32::MAX);
        assert_eq!(login_nonce(u32::MAX, 5), 5);
        assert_eq!(login_nonce(1, 1), 0xFFFF_FFFF);
        assert_eq!(login_nonce(0x0000_0010, 0x0000_0020), 0x0000_000F);
    }

    /// Known-answer vectors from the shipped FileMaker library: `NumToString`,
    /// `ComputePasswordHash` (connKey[0..10], pepper, 1, SHA1), then
    /// `Encrypt_ObscureBytes` over 24 zero bytes (= the raw keystream), with the
    /// synthetic connection key 00 01 .. 0f.
    #[test]
    fn login_keystream_matches_shipped_library() {
        let conn_key: [u8; 16] = core::array::from_fn(|i| i as u8);
        let vectors = [
            (0u32, "dcf786c9979a1cdf1f0eea53a29537b14440f91adaafbdcb"),
            (
                123_456_789,
                "a8ba46ee69fa1e3da8ccd4b85ef650b4aebe4ac7d10466cc",
            ),
            (
                4_294_967_295,
                "227c89846b237f907849ac33a8136ad1f4adb0b3cfb610fd",
            ),
            (
                2_882_400_018,
                "d82dfb7922fc048a825c3fa4a8748d57708295f105dd0894",
            ),
        ];
        for (nonce, expected) in vectors {
            let hash = password_hash_for_login(nonce, &conn_key);
            let mut field = [0u8; 24];
            obscure_field(&mut field, &hash, &conn_key);
            assert_eq!(field.to_vec(), unhex(expected), "nonce {nonce}");
        }
    }

    #[test]
    fn password_hash_for_login_intermediate() {
        // Independent PBKDF2 computation of the same chain for nonce 123456789.
        let conn_key: [u8; 16] = core::array::from_fn(|i| i as u8);
        assert_eq!(
            password_hash_for_login(123_456_789, &conn_key).to_vec(),
            unhex("4fcf433ab8ce8cc998631c490a029672af1a002f")
        );
    }

    #[test]
    fn fields_restart_the_keystream() {
        // Two fields of different lengths share the same keystream prefix.
        let conn_key = [9u8; 16];
        let hash = password_hash_for_login(42, &conn_key);
        let mut short = [0u8; 6];
        let mut long = [0u8; 19];
        obscure_field(&mut short, &hash, &conn_key);
        obscure_field(&mut long, &hash, &conn_key);
        assert_eq!(short, long[..6]);
    }

    #[test]
    fn obscure_is_reversible() {
        let key = [7u8; 16];
        let hash = [0x11u8; 20];
        let original = b"secret account name".to_vec();
        let mut data = original.clone();
        obscure_field(&mut data, &hash, &key);
        assert_ne!(data, original, "obscured bytes should differ");
        deobscure_field(&mut data, &hash, &key);
        assert_eq!(data, original);
    }

    #[test]
    fn compute_password_hash_sizes() {
        let text: Vec<u16> = "1".encode_utf16().collect();
        for algo in [
            HashAlgo::Sha1,
            HashAlgo::Sha256,
            HashAlgo::Sha384,
            HashAlgo::Sha512,
        ] {
            let h = compute_password_hash(&text, &[0u8; 10], &PEPPER, 1, algo);
            assert_eq!(h.len(), algo.digest_len());
        }
    }

    #[test]
    fn query_access_key_is_md5_of_instruction_key_and_file() {
        let key: [u8; 16] = core::array::from_fn(|i| i as u8);
        assert_eq!(
            query_access_key(1, &key, 0x84),
            hex16("e5f0d965740512eed0074d175f68abc9")
        );
        assert_eq!(
            query_access_key(3, &key, 0x166),
            hex16("1721e46be2f595153a5b10dbdd0538fd")
        );
    }

    fn hex16(s: &str) -> [u8; 16] {
        core::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
    }
}
