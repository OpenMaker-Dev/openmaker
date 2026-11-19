//! FileMaker's wire string codec: SCSU compression with a per-byte XOR mask.
//!
//! FileMaker transmits strings as SCSU-compressed UTF-16 (Unicode Technical
//! Standard #6, "A Standard Compression Scheme for Unicode") with every emitted
//! byte XORed by [`MASK`] (`0x5A`). On the wire a string field is the masked
//! SCSU byte run plus a length, framed three ways (see [`Framing`]).
//!
//! The decoder accepts any SCSU stream; the encoder is FileMaker's own
//! compressor, so text written through it has the bytes FileMaker writes
//! (formulas, record values, catalog names), for every script.
//!
//! Spec: `docs/re/network-protocol.md` sections 4.2 and 9 (framing) and
//! `docs/re/calc-encoding.md` 2.4 (the compressor). SCSU itself is the public
//! UTS #6 standard, implemented here from that standard.

mod compress;

/// Byte mask XORed onto every SCSU byte on the wire.
pub const MASK: u8 = 0x5A;

/// How a string field carries its length alongside the masked SCSU bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// `[masked SCSU bytes][u32 big-endian byte count]` - stack `Push(unistring)`.
    TrailingU32,
    /// `[1-byte count][masked SCSU bytes]` - `PushUnistring` in identity payloads.
    LeadingU8,
    /// `[masked SCSU bytes][1-byte count]` - `Push(uchar const*)`, count < 256.
    TrailingU8,
}

/// Error decoding a FileMaker string field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The declared length ran past the end of the input.
    Truncated,
    /// The declared length exceeds what the framing byte can hold.
    LengthTooLarge,
    /// The SCSU byte stream was malformed (bad surrogate, short sequence).
    MalformedScsu,
}

// --- SCSU constants (UTS #6) -------------------------------------------------

// Single-byte-mode control codes.
const SQ0: u8 = 0x01; // SQ0..SQ7: quote from dynamic window n for one char
const SDX: u8 = 0x0B; // define extended dynamic window
const SQU: u8 = 0x0E; // quote one Unicode character (two bytes follow)
const SCU: u8 = 0x0F; // change to Unicode mode
const SC0: u8 = 0x10; // SC0..SC7: change to dynamic window n
const SD0: u8 = 0x18; // SD0..SD7: define dynamic window n

// Unicode-mode control codes.
const UC0: u8 = 0xE0; // UC0..UC7: change to dynamic window n
const UD0: u8 = 0xE8; // UD0..UD7: define dynamic window n
const UQU: u8 = 0xF0; // quote one Unicode character
const UDX: u8 = 0xF1; // define extended dynamic window (UTS #6 Table 7)

/// Offsets used when a window is defined by a one-byte index (UTS #6 table).
const WINDOW_OFFSETS: [u32; 256] = build_window_offsets();

const fn build_window_offsets() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 1usize;
    while i < 0x68 {
        table[i] = i as u32 * 0x80;
        i += 1;
    }
    // 0x68..0xA8: offset = value*0x80 + 0xAC00 (skips the CJK block), per UTS #6
    // Table 4. (0x68 -> U+E000, not U+AC00.)
    let mut j = 0x68usize;
    while j < 0xA8 {
        table[j] = j as u32 * 0x80 + 0xAC00;
        j += 1;
    }
    // Reserved values 0x00, 0xA8..0xF8 stay 0; special fixed offsets (UTS #6 Table 4):
    table[0xF9] = 0x00C0;
    table[0xFA] = 0x0250;
    table[0xFB] = 0x0370;
    table[0xFC] = 0x0530;
    table[0xFD] = 0x3040;
    table[0xFE] = 0x30A0;
    table[0xFF] = 0xFF60;
    table
}

/// Initial positions of the eight dynamic windows (UTS #6 default).
const STATIC_OFFSETS: [u32; 8] = [
    0x0000, 0x0080, 0x0100, 0x0300, 0x2000, 0x2080, 0x2100, 0x3000,
];
const DEFAULT_DYNAMIC: [u32; 8] = [
    0x0080, 0x00C0, 0x0400, 0x0600, 0x0900, 0x3040, 0x30A0, 0xFF00,
];

/// Decode a masked SCSU byte run (length already stripped) to a `String`.
pub fn decode_scsu(masked: &[u8]) -> Result<String, DecodeError> {
    let (units, _) = expand(masked, None, masked.len())?;
    String::from_utf16(&units).map_err(|_| DecodeError::MalformedScsu)
}

/// [`decode_scsu`] for a secret, such as the password File Options store:
/// the working code units and the text are each sized once so that they
/// never move (a byte yields at most two code units, a code unit at most
/// three UTF-8 bytes), the code units are wiped before they are dropped,
/// and the text is wiped when it is.
pub fn decode_scsu_secret(masked: &[u8]) -> Result<zeroize::Zeroizing<String>, DecodeError> {
    use zeroize::Zeroize;
    let (mut units, _) = expand(masked, None, masked.len() * 2)?;
    let mut text = zeroize::Zeroizing::new(String::with_capacity(units.len() * 3));
    let mut malformed = false;
    for c in char::decode_utf16(units.iter().copied()) {
        match c {
            Ok(c) => text.push(c),
            Err(_) => {
                malformed = true;
                break;
            }
        }
    }
    units.zeroize();
    if malformed {
        return Err(DecodeError::MalformedScsu);
    }
    Ok(text)
}

/// Decode masked SCSU that carries a UTF-16 unit count instead of a byte
/// length (text in stored formulas, calc-encoding.md 2.3): expand until
/// `units` code units are produced. Returns the text and the bytes consumed.
pub fn decode_scsu_units(masked: &[u8], units: usize) -> Result<(String, usize), DecodeError> {
    let (text, used) = expand(masked, Some(units), masked.len())?;
    match text.len().cmp(&units) {
        std::cmp::Ordering::Less => return Err(DecodeError::Truncated),
        // The last byte produced a surrogate pair across the declared count.
        std::cmp::Ordering::Greater => return Err(DecodeError::MalformedScsu),
        std::cmp::Ordering::Equal => {}
    }
    let text = String::from_utf16(&text).map_err(|_| DecodeError::MalformedScsu)?;
    Ok((text, used))
}

/// Expand masked SCSU to UTF-16, stopping after `limit` units when given.
fn expand(
    bytes: &[u8],
    limit: Option<usize>,
    reserve: usize,
) -> Result<(Vec<u16>, usize), DecodeError> {
    // A declared count can exceed what the input holds, so reserve only for
    // the bytes present (`reserve`); a byte that yields a surrogate pair
    // grows the buffer beyond it.
    let mut units: Vec<u16> = Vec::with_capacity(limit.map_or(reserve, |n| n.min(reserve)));
    let mut dynamic = DEFAULT_DYNAMIC;
    let mut window = 0usize;
    let mut i = 0usize;
    let mut unicode_mode = false;
    let byte = |i: usize| {
        bytes
            .get(i)
            .map(|b| b ^ MASK)
            .ok_or(DecodeError::MalformedScsu)
    };

    while i < bytes.len() && limit.map_or(true, |n| units.len() < n) {
        let b = bytes[i] ^ MASK;
        i += 1;
        if !unicode_mode {
            match b {
                0x00 | 0x09 | 0x0A | 0x0D | 0x20..=0x7F => units.push(b as u16),
                SQ0..=0x08 => {
                    let w = (b - SQ0) as usize;
                    let c = byte(i)?;
                    i += 1;
                    push_from_window(&mut units, c, STATIC_OFFSETS[w], dynamic[w]);
                }
                SQU => {
                    let hi = byte(i)?;
                    let lo = byte(i + 1)?;
                    i += 2;
                    units.push(u16::from_be_bytes([hi, lo]));
                }
                SCU => unicode_mode = true,
                SC0..=0x17 => window = (b - SC0) as usize,
                SD0..=0x1F => {
                    let idx = byte(i)?;
                    i += 1;
                    window = (b - SD0) as usize;
                    dynamic[window] = WINDOW_OFFSETS[idx as usize];
                }
                SDX => {
                    let hi = byte(i)?;
                    let lo = byte(i + 1)?;
                    i += 2;
                    window = (hi >> 5) as usize;
                    dynamic[window] = 0x10000 + (((((hi & 0x1F) as u32) << 8) | lo as u32) * 0x80);
                }
                _ => {
                    // 0x80..=0xFF: emit from the current dynamic window.
                    push_from_window(&mut units, b, 0, dynamic[window]);
                }
            }
        } else {
            match b {
                UC0..=0xE7 => {
                    window = (b - UC0) as usize;
                    unicode_mode = false;
                }
                UD0..=0xEF => {
                    let idx = byte(i)?;
                    i += 1;
                    window = (b - UD0) as usize;
                    dynamic[window] = WINDOW_OFFSETS[idx as usize];
                    unicode_mode = false;
                }
                UQU => {
                    let hi = byte(i)?;
                    let lo = byte(i + 1)?;
                    i += 2;
                    units.push(u16::from_be_bytes([hi, lo]));
                }
                UDX => {
                    let hi = byte(i)?;
                    let lo = byte(i + 1)?;
                    i += 2;
                    window = (hi >> 5) as usize;
                    dynamic[window] = 0x10000 + (((((hi & 0x1F) as u32) << 8) | lo as u32) * 0x80);
                    unicode_mode = false;
                }
                _ => {
                    // Two bytes form one UTF-16 code unit directly.
                    let lo = byte(i)?;
                    i += 1;
                    units.push(u16::from_be_bytes([b, lo]));
                }
            }
        }
    }
    Ok((units, i))
}

fn push_from_window(units: &mut Vec<u16>, byte: u8, static_off: u32, dynamic_off: u32) {
    if byte < 0x80 {
        // Quote from a static window.
        let scalar = static_off + byte as u32;
        push_scalar(units, scalar);
    } else {
        let scalar = dynamic_off + (byte - 0x80) as u32;
        push_scalar(units, scalar);
    }
}

fn push_scalar(units: &mut Vec<u16>, scalar: u32) {
    if let Some(c) = char::from_u32(scalar) {
        let mut buf = [0u16; 2];
        units.extend_from_slice(c.encode_utf16(&mut buf));
    }
}

/// Encode `text` as masked SCSU, byte for byte as FileMaker's compressor
/// does (calc-encoding.md 2.4).
pub fn encode_scsu(text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = compress::compress(&units);
    for b in &mut out {
        *b ^= MASK;
    }
    out
}

/// Decode a string stored with its UTF-16 unit count in front, as FileMaker's
/// stored records write strings (`SwapOStream`; file-metadata.md 0, the
/// same count as formula text in calc-encoding.md 2.3): one byte below
/// `0x80`, two bytes with `0x8000` set below `0x4000`, `c2` and a u16, or
/// `c4` and a u32, then the masked SCSU bytes. Returns the text and the
/// bytes consumed.
pub fn decode_counted(input: &[u8]) -> Result<(String, usize), DecodeError> {
    let byte = |i: usize| input.get(i).copied().ok_or(DecodeError::Truncated);
    let (units, header) = match byte(0)? {
        n @ 0..=0x7f => (usize::from(n), 1),
        n @ 0x80..=0xbf => (usize::from(n & 0x3f) << 8 | usize::from(byte(1)?), 2),
        0xc2 => (usize::from(byte(1)?) << 8 | usize::from(byte(2)?), 3),
        0xc4 => {
            let word = input.get(1..5).ok_or(DecodeError::Truncated)?;
            (u32::from_be_bytes(word.try_into().unwrap()) as usize, 5)
        }
        _ => return Err(DecodeError::MalformedScsu),
    };
    if units == 0 {
        return Ok((String::new(), header));
    }
    let (text, used) = decode_scsu_units(&input[header..], units)?;
    Ok((text, header + used))
}

/// Encode a string as [`decode_counted`] reads it.
pub fn encode_counted(text: &str) -> Vec<u8> {
    let units = text.encode_utf16().count();
    let mut out = match units {
        0..=0x7f => vec![units as u8],
        0x80..=0x3fff => (0x8000 | units as u16).to_be_bytes().to_vec(),
        0x4000..=0xffff => [&[0xc2][..], &(units as u16).to_be_bytes()].concat(),
        _ => [&[0xc4][..], &(units as u32).to_be_bytes()].concat(),
    };
    out.extend(encode_scsu(text));
    out
}

/// Decode one framed string field, returning the text and bytes consumed.
pub fn decode_field(input: &[u8], framing: Framing) -> Result<(String, usize), DecodeError> {
    match framing {
        Framing::LeadingU8 => {
            let len = *input.first().ok_or(DecodeError::Truncated)? as usize;
            let body = input.get(1..1 + len).ok_or(DecodeError::Truncated)?;
            Ok((decode_scsu(body)?, 1 + len))
        }
        Framing::TrailingU8 => {
            if input.is_empty() {
                return Err(DecodeError::Truncated);
            }
            let len = input[input.len() - 1] as usize;
            let body = input.get(..len).ok_or(DecodeError::Truncated)?;
            Ok((decode_scsu(body)?, len + 1))
        }
        Framing::TrailingU32 => {
            // `[masked SCSU bytes][u32 BE byte count]`: the length is the last
            // four bytes, the body the bytes before it. The whole slice is the
            // field (RCTransaction pops it from the end of the buffer).
            let split = input.len().checked_sub(4).ok_or(DecodeError::Truncated)?;
            let len = u32::from_be_bytes(input[split..].try_into().unwrap()) as usize;
            if len != split {
                return Err(DecodeError::Truncated);
            }
            Ok((decode_scsu(&input[..split])?, input.len()))
        }
    }
}

/// Encode one framed string field.
pub fn encode_field(text: &str, framing: Framing) -> Result<Vec<u8>, DecodeError> {
    let body = encode_scsu(text);
    let mut out = Vec::with_capacity(body.len() + 4);
    match framing {
        Framing::LeadingU8 => {
            let len = u8::try_from(body.len()).map_err(|_| DecodeError::LengthTooLarge)?;
            out.push(len);
            out.extend_from_slice(&body);
        }
        Framing::TrailingU8 => {
            let len = u8::try_from(body.len()).map_err(|_| DecodeError::LengthTooLarge)?;
            out.extend_from_slice(&body);
            out.push(len);
        }
        Framing::TrailingU32 => {
            out.extend_from_slice(&body);
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vectors taken from the s03 capture (docs/re/network-protocol.md 5.2).
    // These are server-name/version fields, not secrets.

    #[test]
    fn counted_strings_carry_their_unit_count() {
        // ` AM` and `Sunday` from an English file's locale record.
        let am = [0x03, 0x7a, 0x1b, 0x17, 0xff];
        assert_eq!(decode_counted(&am).unwrap(), (" AM".to_string(), 4));
        assert_eq!(decode_counted(&[0x00, 0x01]).unwrap(), (String::new(), 1));
        for text in [
            "",
            "Sunday",
            "日曜日",
            &"x".repeat(200),
            &"y".repeat(0x4000),
        ] {
            let encoded = encode_counted(text);
            assert_eq!(
                decode_counted(&encoded).unwrap(),
                (text.to_string(), encoded.len())
            );
        }
        assert_eq!(encode_counted(&"x".repeat(200))[..2], [0x80, 0xc8]);
        assert_eq!(encode_counted(&"y".repeat(0x4000))[..3], [0xc2, 0x40, 0x00]);
        assert_eq!(decode_counted(&[0x05, 0x7a]), Err(DecodeError::Truncated));
    }

    #[test]
    fn decodes_captured_server_name() {
        // On-wire field: leading length 0x06 then masked SCSU of the server's
        // six-character name (here "server", standing in for the captured one).
        let field = [0x06, 0x29, 0x3f, 0x28, 0x2c, 0x3f, 0x28];
        let (text, used) = decode_field(&field, Framing::LeadingU8).unwrap();
        assert_eq!(text, "server");
        assert_eq!(used, 7);
    }

    #[test]
    fn decodes_captured_version_string() {
        let field = [
            0x0d, 0x09, 0x3f, 0x28, 0x2c, 0x3f, 0x28, 0x7a, 0x68, 0x6c, 0x74, 0x6a, 0x74, 0x69,
        ];
        let (text, _) = decode_field(&field, Framing::LeadingU8).unwrap();
        assert_eq!(text, "Server 26.0.3");
    }

    /// A secret decodes to the same text, outside ASCII and the BMP too.
    #[test]
    fn secrets_decode_as_text_does() {
        for text in ["", "om157pass", "café ünï", "日本語", "😀 emoji"] {
            let masked = encode_scsu(text);
            assert_eq!(
                *decode_scsu_secret(&masked).unwrap(),
                decode_scsu(&masked).unwrap()
            );
        }
    }

    #[test]
    fn ascii_round_trips_all_framings() {
        for framing in [
            Framing::LeadingU8,
            Framing::TrailingU8,
            Framing::TrailingU32,
        ] {
            let encoded = encode_field("admin", framing).unwrap();
            let (text, used) = decode_field(&encoded, framing).unwrap();
            assert_eq!(text, "admin");
            assert_eq!(used, encoded.len());
        }
    }

    #[test]
    fn latin1_round_trips() {
        let s = "café";
        let encoded = encode_scsu(s);
        assert_eq!(decode_scsu(&encoded).unwrap(), s);
    }

    #[test]
    fn non_latin_round_trips_via_squote() {
        let s = "Zürich 東京";
        let encoded = encode_scsu(s);
        assert_eq!(decode_scsu(&encoded).unwrap(), s);
    }

    #[test]
    fn empty_string() {
        let encoded = encode_field("", Framing::LeadingU8).unwrap();
        assert_eq!(encoded, vec![0x00]);
        let (text, used) = decode_field(&encoded, Framing::LeadingU8).unwrap();
        assert_eq!(text, "");
        assert_eq!(used, 1);
    }

    #[test]
    fn truncated_is_error() {
        let field = [0x06, 0x3c, 0x37];
        assert_eq!(
            decode_field(&field, Framing::LeadingU8),
            Err(DecodeError::Truncated)
        );
    }
}

#[cfg(test)]
mod uts6_tests {
    use super::*;

    #[test]
    fn unit_counts_must_match_exactly() {
        // A supplementary character is two UTF-16 units; a declared count of
        // one ends inside its surrogate pair.
        let emoji = encode_scsu("😀");
        assert_eq!(decode_scsu_units(&emoji, 2).unwrap().0, "😀");
        assert!(decode_scsu_units(&emoji, 1).is_err());
        assert!(decode_scsu_units(&emoji, 3).is_err());
    }

    #[test]
    fn window_offset_table_matches_uts6() {
        // UTS #6 Table 4: 0x68 -> U+E000 (skips the CJK block), 0xFF -> U+FF60.
        assert_eq!(WINDOW_OFFSETS[0x68], 0xE000);
        assert_eq!(WINDOW_OFFSETS[0xA7], 0xA7 * 0x80 + 0xAC00);
        assert_eq!(WINDOW_OFFSETS[0xFF], 0xFF60);
        assert_eq!(WINDOW_OFFSETS[0x01], 0x80);
    }

    #[test]
    fn decodes_filemaker_host_sentinels() {
        // The captured Open host field's form (XOR-masked, on the wire), with
        // the dialed host name replaced. FileMaker wraps host/path strings in
        // U+E000 sentinels via an SD3 window define.
        let host: &[u8] = &[
            0x41, 0x32, 0xda, 0x3c, 0x37, 0x29, 0x74, 0x3f, 0x22, 0x3b, 0x37, 0x2a, 0x36, 0x3f,
            0x74, 0x39, 0x35, 0x37, 0xda,
        ];
        assert_eq!(
            decode_scsu(host).unwrap(),
            "\u{E000}fms.example.com\u{E000}"
        );
    }

    #[test]
    fn encodes_host_sentinels_as_filemaker_does() {
        // FileMaker's own form of the Open host field (above): it defines
        // window 3 at U+E000 (`SD3 68`) rather than quoting the sentinels.
        let host: &[u8] = &[
            0x41, 0x32, 0xda, 0x3c, 0x37, 0x29, 0x74, 0x3f, 0x22, 0x3b, 0x37, 0x2a, 0x36, 0x3f,
            0x74, 0x39, 0x35, 0x37, 0xda,
        ];
        assert_eq!(encode_scsu("\u{E000}fms.example.com\u{E000}"), host);
    }

    #[test]
    fn latin1_text_is_one_byte_per_character() {
        // Writers sending ASCII and Latin-1 (account names, paths, record
        // values) get the bytes the earlier single-byte encoder produced;
        // C0 controls other than tab, LF and CR are quoted with SQ0.
        for c in (0x20..=0xFFu32)
            .chain([0, 9, 10, 13])
            .filter_map(char::from_u32)
        {
            assert_eq!(encode_scsu(&c.to_string()), [c as u8 ^ MASK], "{c:?}");
        }
        assert_eq!(encode_scsu("\u{1}"), [SQ0 ^ MASK, 1 ^ MASK]);
    }

    #[test]
    fn non_latin_text_uses_filemaker_windows() {
        // Greek defines the fixed window FB (U+0370) in window 3; CJK
        // ideographs, which no window holds, go through Unicode mode.
        assert_eq!(
            encode_scsu("Ωa")
                .iter()
                .map(|b| b ^ MASK)
                .collect::<Vec<_>>(),
            [SD0 + 3, 0xFB, 0x80 + 0x39, b'a']
        );
        let cjk: Vec<u8> = encode_scsu("東京").iter().map(|b| b ^ MASK).collect();
        assert_eq!(cjk, [SCU, 0x67, 0x71, 0x4E, 0xAC]);
        for s in [
            "Zürich 東京",
            "日本語のテキスト",
            "Привет",
            "😀 a",
            "\u{E000}x",
        ] {
            assert_eq!(decode_scsu(&encode_scsu(s)).unwrap(), s);
        }
    }

    #[test]
    fn udx_supplementary_round_trips() {
        // Unicode mode (SCU=0x0F), UDX=0xF1 defining an extended window, then a
        // char from it. Build via the encoder's SQU path and confirm decode.
        let s = "\u{10000}";
        assert_eq!(decode_scsu(&encode_scsu(s)).unwrap(), s);
    }
}
