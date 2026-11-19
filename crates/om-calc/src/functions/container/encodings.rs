//! Base64, hexadecimal and character encodings for the container functions
//! (spec `docs/re/calc-functions.md` 5.4, the Claris pages and the container
//! oracle answers in 9).
//!
//! Base64 and hex are reproduced exactly. For `TextEncode` and `TextDecode`
//! the single-byte encodings use tables read from the engine
//! ([`tables`]), `utf-8` is exact, and the double-byte encodings go through
//! [`encoding_rs`] with the engine's differences applied: `shift_jis` is
//! JIS X 0208 without the Microsoft and NEC extensions (`0x5C` is `¥`),
//! `gb18030` has no single-byte `€`. A character an encoding lacks is
//! written as its compatibility form (NFKC: `－` as `-`, `™` as `TM`) when
//! that encodes, else left out. The engine transliterates further (`€` as
//! `EUR`, `①` as `(1)`, `é` as `´e` in `shift_jis`); that table is not
//! reproduced (known deviation).

use super::{FOREIGN_CHARSET, UNKNOWN_NAME};
use crate::error::CalcError;
use encoding_rs::{DecoderResult, EncoderResult};
use icu_normalizer::ComposingNormalizerBorrowed;

mod tables;

// ----- Base64 -------------------------------------------------------------

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 in the layout of an RFC (the `Base64EncodeRFC` page): 1421 is
/// 64-character lines, 2045 76-character lines, each line ended with CRLF,
/// the last one too; 4880 is 2045's layout plus a line `=` and the Base64
/// of the data's CRC-24; 3548, 4648 and every other number add no breaks.
/// Empty data still gets its line end (`"\r\n"`) and CRC line (oracle).
pub fn base64_encode_rfc(rfc: u32, data: &[u8]) -> String {
    let body = base64(data);
    match rfc {
        1421 => lines(&body, 64),
        2045 => lines(&body, 76),
        4880 => {
            let crc = crc24(data).to_be_bytes();
            let mut out = lines(&body, 76);
            out.push('=');
            out.push_str(&base64(&crc[1..]));
            out.push_str("\r\n");
            out
        }
        _ => body,
    }
}

/// The length of [`base64_encode_rfc`]'s text for `n` bytes, known before
/// it is built.
pub fn base64_len(rfc: u32, n: usize) -> usize {
    let body = n.div_ceil(3).saturating_mul(4);
    let lines = |width: usize| body + body.div_ceil(width).max(1) * 2;
    match rfc {
        1421 => lines(64),
        2045 => lines(76),
        // The CRC line: `=`, four characters and CR LF.
        4880 => lines(76) + 7,
        _ => body,
    }
}

/// Base64 without line breaks (RFC 4648 alphabet, `=` padding).
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `text` cut into lines of `width` characters, each ended with CRLF; empty
/// text is one empty line.
fn lines(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / width * 2 + 2);
    let mut rest = text;
    loop {
        let (line, tail) = rest.split_at(width.min(rest.len()));
        out.push_str(line);
        out.push_str("\r\n");
        rest = tail;
        if rest.is_empty() {
            return out;
        }
    }
}

/// The OpenPGP CRC-24 (RFC 4880 6.1).
fn crc24(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xB7_04CE;
    for &byte in data {
        crc ^= u32::from(byte) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x100_0000 != 0 {
                crc ^= 0x186_4CFB;
            }
        }
    }
    crc & 0xFF_FFFF
}

/// `Base64Decode`'s reading (oracle): characters outside the Base64
/// alphabet (spaces, line breaks, `!`, `-`, `_`, `é`) are skipped, the data
/// ends at the first `=`, and the digits before it must not leave a lone
/// digit in the last group. Without a `=` they must also fill whole groups
/// (`QQ` fails, `QQ=` is `A`). Extra bits in a final partial group are
/// ignored (`QR==` is `A`). `None` is error 17.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut digits = Vec::with_capacity(text.len());
    let mut padded = false;
    for c in text.chars() {
        if c == '=' {
            padded = true;
            break;
        }
        if let Some(d) = ALPHABET.iter().position(|&a| char::from(a) == c) {
            digits.push(d as u32);
        }
    }
    match digits.len() % 4 {
        1 => return None,
        2 | 3 if !padded => return None,
        _ => {}
    }
    let mut out = Vec::with_capacity(digits.len() / 4 * 3 + 2);
    for group in digits.chunks(4) {
        let n = group
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &d)| acc | d << (18 - 6 * i));
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..group.len()]);
    }
    Some(out)
}

// ----- Hexadecimal --------------------------------------------------------

/// `HexEncode`: upper-case hex digits.
pub fn hex_encode(data: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(data.len() * 2);
    for &b in data {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    out
}

/// `HexDecode`'s reading (oracle): hex digits of either case; spaces, tabs,
/// CR and LF are skipped anywhere before the last digit (`"4 6"` is `F`);
/// the digit count must be even. Whitespace after the last digit is
/// accepted only when the digits make a whole number of 16-bit units
/// (`"4641 "` decodes, `"46 "` and `"464142 "` fail). Anything else
/// (`0x`, `:`, `-`, a no-break space) fails. `None` is error 17.
pub fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let mut nibbles = Vec::with_capacity(text.len());
    let mut trailing_space = false;
    for c in text.chars() {
        match c {
            ' ' | '\t' | '\r' | '\n' => trailing_space = true,
            _ => {
                nibbles.push(c.to_digit(16)? as u8);
                trailing_space = false;
            }
        }
    }
    if nibbles.len() % 2 == 1 || (trailing_space && nibbles.len() % 4 != 0) {
        return None;
    }
    Some(nibbles.chunks(2).map(|p| p[0] << 4 | p[1]).collect())
}

// ----- Text and line breaks -----------------------------------------------

/// Text as `Base64Encode` reads it: UTF-8 with every line break (CR, LF,
/// CRLF, U+2028, U+2029) written as CRLF (oracle: `"a¶b"` encodes
/// `61 0D 0A 62`, LF then CR gives two CRLFs). The other functions read
/// text as plain UTF-8.
pub fn crlf_lines(text: &str) -> String {
    replace_breaks(text, "\r\n", true)
}

/// Decoded bytes as FileMaker text: CRLF and LF fold to the return
/// character CR (oracle: `0D 0D 0A 0A` reads as three returns).
pub fn internal_returns(text: &str) -> String {
    replace_breaks(text, "\r", false)
}

/// Replaces CR, LF and CRLF (and the Unicode separators when `unicode`)
/// with `with`.
fn replace_breaks(text: &str, with: &str, unicode: bool) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                out.push_str(with);
            }
            '\n' => out.push_str(with),
            '\u{2028}' | '\u{2029}' if unicode => out.push_str(with),
            other => out.push(other),
        }
    }
    out
}

/// `TextEncode`'s `lineEndings` (the Claris page): 2 CR, 3 LF, 4 CRLF;
/// 1 and any other number leave the text as it is. A change turns every
/// CR, LF, CRLF, U+2028 and U+2029 into the chosen ending (oracle).
fn line_endings(text: &str, mode: i64) -> String {
    match mode {
        2 => replace_breaks(text, "\r", true),
        3 => replace_breaks(text, "\n", true),
        4 => replace_breaks(text, "\r\n", true),
        _ => text.to_string(),
    }
}

// ----- Character encodings ------------------------------------------------

/// The encodings of the `TextEncode` page, by name (any case).
#[derive(Clone, Copy)]
enum Charset {
    Utf8,
    SingleByte(&'static [u16; 128]),
    ShiftJis,
    Gb18030,
    Other(&'static encoding_rs::Encoding),
}

fn charset(name: &str) -> Result<Charset, CalcError> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "utf-8" => Charset::Utf8,
        "iso-8859-1" => Charset::SingleByte(&tables::ISO_8859_1),
        "windows-1251" => Charset::SingleByte(&tables::WINDOWS_1251),
        "windows-1252" => Charset::SingleByte(&tables::WINDOWS_1252),
        "macintosh" => Charset::SingleByte(&tables::MACINTOSH),
        "shift_jis" => Charset::ShiftJis,
        "gb18030" => Charset::Gb18030,
        "euc-kr" => Charset::Other(encoding_rs::EUC_KR),
        "big5" => Charset::Other(encoding_rs::BIG5),
        _ => return Err(UNKNOWN_NAME),
    })
}

/// `TextEncode`: the text with its line endings changed, in the named
/// encoding; characters the encoding lacks are left out. Error 4 for a name
/// not on the Claris page (`utf8`, `latin1`, `utf-16`, empty).
pub fn text_encode(text: &str, encoding: &str, mode: i64) -> Result<Vec<u8>, CalcError> {
    let charset = charset(encoding)?;
    let text = line_endings(text, mode);
    let mut out = Vec::with_capacity(text.len());
    let nfkc = ComposingNormalizerBorrowed::new_nfkc();
    for c in text.chars() {
        if encode_char(charset, c, &mut out) {
            continue;
        }
        // A character the encoding lacks: its compatibility form (`－` as
        // `-`, `￠` as `¢`, `™` as `TM`), else nothing.
        let mut buf = [0; 4];
        let compat = nfkc.normalize(c.encode_utf8(&mut buf));
        if compat.chars().ne(std::iter::once(c)) {
            for k in compat.chars() {
                encode_char(charset, k, &mut out);
            }
        }
    }
    Ok(out)
}

/// Appends `c` in `charset`; false when the encoding lacks it.
fn encode_char(charset: Charset, c: char, out: &mut Vec<u8>) -> bool {
    let before = out.len();
    match charset {
        Charset::Utf8 => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        Charset::SingleByte(table) => {
            if (c as u32) < 0x80 {
                out.push(c as u8);
            } else if let Some(i) = table
                .iter()
                .position(|&m| m != 0 && u32::from(m) == c as u32)
            {
                out.push(0x80 + i as u8);
            }
        }
        Charset::ShiftJis => shift_jis_encode(c, out),
        Charset::Gb18030 => encode_with(encoding_rs::GB18030, c, out),
        Charset::Other(encoding) => encode_with(encoding, c, out),
    }
    out.len() > before
}

/// Appends `c` in `encoding`, or nothing when the encoding lacks it.
fn encode_with(encoding: &'static encoding_rs::Encoding, c: char, out: &mut Vec<u8>) {
    let mut encoder = encoding.new_encoder();
    let mut buf = [0u8; 8];
    let (result, _, written) =
        encoder.encode_from_utf8_without_replacement(c.encode_utf8(&mut [0; 4]), &mut buf, true);
    if let EncoderResult::InputEmpty = result {
        out.extend_from_slice(&buf[..written]);
    }
}

/// `shift_jis` as the engine has it (oracle): JIS X 0201 in the single
/// bytes (`0x5C` `¥`, `0x7E` `‾`, no backslash or tilde), half-width
/// katakana, JIS X 0208 without the NEC and IBM rows, and the user-defined
/// rows (`0xF040` is U+E000). The Microsoft forms of [`JIS_VARIANTS`] are
/// not encoded as such.
fn shift_jis_encode(c: char, out: &mut Vec<u8>) {
    match c {
        '\\' | '~' => {}
        '¥' => out.push(0x5C),
        '‾' => out.push(0x7E),
        _ if (c as u32) < 0x80 => out.push(c as u8),
        _ if JIS_VARIANTS.iter().any(|&(_, microsoft)| microsoft == c) => {}
        _ => {
            let c = JIS_VARIANTS
                .iter()
                .find(|&&(jis, _)| jis == c)
                .map_or(c, |&(_, microsoft)| microsoft);
            let mut bytes = Vec::with_capacity(2);
            encode_with(encoding_rs::SHIFT_JIS, c, &mut bytes);
            if bytes.len() == 1 || bytes.first().is_some_and(|&lead| jis_lead(lead)) {
                out.extend_from_slice(&bytes);
            }
        }
    }
}

/// Characters JIS X 0208 and the Microsoft code page (which
/// [`encoding_rs`] implements) map differently: the engine has the JIS
/// ones (oracle: `0x8160` is `〜` U+301C, `0x817C` is `−` U+2212, and `～`
/// U+FF5E does not encode).
const JIS_VARIANTS: [(char, char); 6] = [
    ('\u{301C}', '\u{FF5E}'),
    ('\u{2016}', '\u{2225}'),
    ('\u{2212}', '\u{FF0D}'),
    ('\u{00A2}', '\u{FFE0}'),
    ('\u{00A3}', '\u{FFE1}'),
    ('\u{00AC}', '\u{FFE2}'),
];

/// Lead bytes of double-byte characters in the engine's Shift_JIS: JIS X
/// 0208 and the user-defined rows `0xF0`-`0xF9`, without the rows the
/// Microsoft code page adds (`0x87` NEC specials, `0xED`/`0xEE`
/// NEC-selected IBM, `0xFA`-`0xFC` IBM).
fn jis_lead(b: u8) -> bool {
    matches!(b, 0x81..=0x86 | 0x88..=0x9F | 0xE0..=0xEC | 0xEF..=0xF9)
}

/// `TextDecode`: bytes in the named encoding as text. Bytes that are not a
/// character in it are skipped (`c3 41` in `utf-8` is `A`, an unmapped
/// byte of a single-byte code page disappears); a sequence cut short by the
/// end of the data is error 17. CRLF and LF become the return character.
pub fn text_decode(bytes: &[u8], encoding: &str) -> Result<String, CalcError> {
    let text = match charset(encoding)? {
        Charset::Utf8 => decode_with(encoding_rs::UTF_8, bytes)?,
        Charset::SingleByte(table) => bytes
            .iter()
            .filter_map(|&b| match b {
                0..=0x7F => Some(char::from(b)),
                _ => char::from_u32(u32::from(table[usize::from(b - 0x80)])).filter(|&c| c != '\0'),
            })
            .collect(),
        Charset::ShiftJis => shift_jis_decode(bytes)?,
        Charset::Gb18030 => gb18030_decode(bytes)?,
        Charset::Other(encoding) => decode_with(encoding, bytes)?,
    };
    Ok(internal_returns(&text))
}

/// Decodes with `encoding`, skipping malformed sequences; a sequence the
/// end of the data cuts short is error 17.
fn decode_with(
    encoding: &'static encoding_rs::Encoding,
    bytes: &[u8],
) -> Result<String, CalcError> {
    let mut decoder = encoding.new_decoder_without_bom_handling();
    let mut out = String::with_capacity(bytes.len() * 3 + 16);
    let mut rest = bytes;
    loop {
        let (result, read) = decoder.decode_to_string_without_replacement(rest, &mut out, false);
        rest = &rest[read..];
        match result {
            DecoderResult::InputEmpty => break,
            DecoderResult::Malformed(..) => {}
            DecoderResult::OutputFull => out.reserve(rest.len() * 3 + 16),
        }
    }
    match decoder
        .decode_to_string_without_replacement(&[], &mut out, true)
        .0
    {
        DecoderResult::Malformed(..) => Err(FOREIGN_CHARSET),
        _ => Ok(out),
    }
}

/// `shift_jis` decoding (oracle): ASCII with `0x5C` `¥` and `0x7E` `‾`,
/// half-width katakana, and JIS X 0208 pairs (`0x8160` `〜`, `0x817C` `−`);
/// a byte that cannot start a character there (`0x80`, `0xA0`, `0x87`,
/// `0xFD`) is skipped, and so is an unassigned pair; a lead byte at the end
/// is error 17.
fn shift_jis_decode(bytes: &[u8]) -> Result<String, CalcError> {
    let mut out = String::with_capacity(bytes.len() * 2);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            0x5C => out.push('¥'),
            0x7E => out.push('‾'),
            0..=0x7F => out.push(char::from(b)),
            0xA1..=0xDF => out.extend(char::from_u32(0xFF61 + u32::from(b - 0xA1))),
            _ if jis_lead(b) => {
                let Some(&trail) = bytes.get(i + 1) else {
                    return Err(FOREIGN_CHARSET);
                };
                if !matches!(trail, 0x40..=0x7E | 0x80..=0xFC) {
                    i += 1;
                    continue;
                }
                let (text, malformed) =
                    encoding_rs::SHIFT_JIS.decode_without_bom_handling(&bytes[i..i + 2]);
                if !malformed {
                    out.extend(text.chars().map(|c| {
                        JIS_VARIANTS
                            .iter()
                            .find(|&&(_, microsoft)| microsoft == c)
                            .map_or(c, |&(jis, _)| jis)
                    }));
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    Ok(out)
}

/// `gb18030` decoding: as [`encoding_rs`], except that a lone `0x80` (and
/// `0xFF`) is no character and is skipped (oracle; the WHATWG decoder reads
/// `0x80` as `€`).
fn gb18030_decode(bytes: &[u8]) -> Result<String, CalcError> {
    let mut out = String::with_capacity(bytes.len() * 2);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let len = match b {
            0..=0x7F => 1,
            0x80 | 0xFF => {
                i += 1;
                continue;
            }
            _ => match bytes.get(i + 1) {
                None => return Err(FOREIGN_CHARSET),
                Some(0x30..=0x39) => 4,
                Some(_) => 2,
            },
        };
        let Some(seq) = bytes.get(i..i + len) else {
            return Err(FOREIGN_CHARSET);
        };
        let (text, malformed) = encoding_rs::GB18030.decode_without_bom_handling(seq);
        if malformed {
            // Skip the lead byte and read on from the next one.
            i += 1;
            continue;
        }
        out.push_str(&text);
        i += len;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        hex_decode(s).unwrap()
    }

    #[test]
    fn base64_layouts() {
        let hundred = b"0123456789".repeat(10);
        assert_eq!(base64_encode_rfc(4648, b"Black"), "QmxhY2s=");
        assert_eq!(base64_encode_rfc(2045, b"Black"), "QmxhY2s=\r\n");
        assert_eq!(base64_encode_rfc(4880, b"Black"), "QmxhY2s=\r\n=Yb2R\r\n");
        assert_eq!(base64_encode_rfc(4880, b"a"), "YQ==\r\n=8lcT\r\n");
        assert_eq!(base64_encode_rfc(4880, b""), "\r\n=twTO\r\n");
        assert_eq!(base64_encode_rfc(1421, b""), "\r\n");
        let rfc1421 = base64_encode_rfc(1421, &hundred);
        assert_eq!(
            rfc1421.split("\r\n").map(str::len).collect::<Vec<_>>(),
            [64, 64, 8, 0]
        );
        assert!(base64_encode_rfc(4880, &hundred).ends_with("=HDvi\r\n"));
    }

    #[test]
    fn base64_reading() {
        let ok = |s: &str| base64_decode(s).unwrap();
        assert_eq!(ok("QmxhY2s="), b"Black");
        assert_eq!(ok("Qmxh Y2s=\r"), b"Black");
        assert_eq!(ok("QU!JD"), b"ABC");
        assert_eq!(ok("QQ=QQ"), b"A");
        assert_eq!(ok("QR=="), b"A");
        assert_eq!(ok("===="), b"");
        assert_eq!(ok("QUJDQQ="), b"ABCA");
        for bad in ["QQ", "QUJ", "Q=", "Q===", "-_8=", "QU!J", "12"] {
            assert!(base64_decode(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn hex_reading() {
        assert_eq!(hex("46696c654D616B6572"), b"FileMaker");
        assert_eq!(hex(" 4 6"), b"F");
        assert_eq!(hex("466 9"), b"Fi");
        assert_eq!(hex("4641 "), b"FA");
        assert_eq!(hex("46414243 "), b"FABC");
        for bad in [
            "466",
            "46 ",
            "464142 ",
            "46 46 46 ",
            "0x46",
            "46:69",
            "4G",
            "46\u{a0}69",
        ] {
            assert!(hex_decode(bad).is_none(), "{bad:?}");
        }
        assert_eq!(hex_encode(&[0, 0xff, 0x10]), "00FF10");
    }

    #[test]
    fn line_breaks() {
        assert_eq!(crlf_lines("a\rb\n\rc\u{2028}"), "a\r\nb\r\n\r\nc\r\n");
        assert_eq!(internal_returns("\r\r\n\n"), "\r\r\r");
        let mixed = "a\nb\r\nc\rd";
        assert_eq!(line_endings(mixed, 1), mixed);
        assert_eq!(line_endings(mixed, 3), "a\nb\nc\nd");
        assert_eq!(line_endings(mixed, 4), "a\r\nb\r\nc\r\nd");
        assert_eq!(line_endings(mixed, -3), mixed);
    }

    #[test]
    fn character_encodings() {
        let enc = |t: &str, e: &str| hex_encode(&text_encode(t, e, 1).unwrap());
        assert_eq!(enc("aé", "UTF-8"), "61C3A9");
        assert_eq!(enc("aé", "iso-8859-1"), "61E9");
        assert_eq!(enc("aé", "macintosh"), "618E");
        assert_eq!(enc("aжb", "windows-1251"), "61E662");
        assert_eq!(enc("Ω日", "iso-8859-1"), "");
        assert_eq!(enc("™…ﬁ", "iso-8859-1"), "544D2E2E2E6669");
        assert_eq!(enc("－￠\u{2225}～", "shift_jis"), "2D8191");
        assert_eq!(enc("日本語ｱ", "shift_jis"), "93FA967B8CEAB1");
        assert_eq!(enc("〜−¥~", "shift_jis"), "8160817C5C");
        assert_eq!(enc("한국어", "euc-kr"), "C7D1B1B9BEEE");
        assert_eq!(enc("中文€", "gb18030"), "D6D0CEC4A2E3");
        assert_eq!(enc("😀", "gb18030"), "9439FC36");
        assert_eq!(enc("中文", "big5"), "A4A4A4E5");
        assert!(text_encode("a", "utf8", 1).is_err());

        let dec = |h: &str, e: &str| text_decode(&hex(h), e);
        assert_eq!(dec("c341", "utf-8").unwrap(), "A");
        assert_eq!(dec("eda080", "utf-8").unwrap(), "");
        assert!(dec("c3", "utf-8").is_err());
        assert_eq!(dec("80818d8f909d9f", "windows-1252").unwrap(), "€Ÿ");
        assert_eq!(dec("8e80db", "macintosh").unwrap(), "éÄ¤");
        assert_eq!(dec("5c7e", "shift_jis").unwrap(), "¥‾");
        assert_eq!(
            dec("93fa967b8cea815f8160817c", "shift_jis").unwrap(),
            "日本語＼〜−"
        );
        assert_eq!(dec("8740", "shift_jis").unwrap(), "@");
        assert_eq!(dec("ff", "shift_jis").unwrap(), "");
        assert!(dec("81", "shift_jis").is_err());
        assert!(dec("c7d1b1b9be", "euc-kr").is_err());
        assert_eq!(dec("d6d0cec4a2e3", "gb18030").unwrap(), "中文€");
        assert_eq!(dec("80", "gb18030").unwrap(), "");
        assert_eq!(dec("80", "big5").unwrap(), "");
        assert_eq!(dec("0d0d0a0a", "utf-8").unwrap(), "\r\r\r");
    }
}
