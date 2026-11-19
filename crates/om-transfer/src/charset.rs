//! The character sets of exported and imported files (script-steps.md
//! 11.3, 11.4), numbered as the stored specs number them, and FileMaker
//! Server's conversions (oracle x01, x02):
//!
//! - Export: Unicode (UTF-16) is little-endian with a byte order mark,
//!   UTF-8 has none; a character the target cannot hold is dropped, except
//!   that "€" is "EUR" in Macintosh and Big-5, and a letter with a diacritic
//!   the target lacks is written as the mark then the base letter (`"i` for
//!   ï, `'e` for é in Big-5).
//! - Import: a UTF-16 byte order mark decides the byte order whatever the
//!   import's character set; UTF-16 without one is little-endian; a UTF-8
//!   byte order mark is skipped but does not change the character set.

use encoding_rs::Encoding;
use icu_normalizer::DecomposingNormalizerBorrowed;

/// Export character sets (`TExpCharacterSet`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExportCharset {
    /// 0 "Windows (ANSI)": Windows-1252.
    Windows,
    /// 1 "ASCII (DOS)": code page 850.
    Dos,
    /// 2 "Macintosh": Mac OS Roman.
    Macintosh,
    /// 3 "Unicode (UTF-16)".
    Utf16,
    /// 4 "Unicode (UTF-8)".
    Utf8,
    /// 6 "Japanese (Shift-JIS)".
    ShiftJis,
    /// 8 "Simplified Chinese (GB)": GB 18030.
    Gb,
    /// 9 "Traditional Chinese (Big-5)".
    Big5,
    /// 10 "Korean (EUC-KR)".
    EucKr,
}

impl ExportCharset {
    pub fn from_stored(v: u8) -> Option<ExportCharset> {
        Some(match v {
            0 => ExportCharset::Windows,
            1 => ExportCharset::Dos,
            2 => ExportCharset::Macintosh,
            3 => ExportCharset::Utf16,
            4 => ExportCharset::Utf8,
            6 => ExportCharset::ShiftJis,
            8 => ExportCharset::Gb,
            9 => ExportCharset::Big5,
            10 => ExportCharset::EucKr,
            _ => return None,
        })
    }

    pub fn stored(self) -> u8 {
        match self {
            ExportCharset::Windows => 0,
            ExportCharset::Dos => 1,
            ExportCharset::Macintosh => 2,
            ExportCharset::Utf16 => 3,
            ExportCharset::Utf8 => 4,
            ExportCharset::ShiftJis => 6,
            ExportCharset::Gb => 8,
            ExportCharset::Big5 => 9,
            ExportCharset::EucKr => 10,
        }
    }

    /// The name FileMaker's dialogs and XML show.
    pub fn name(self) -> &'static str {
        match self {
            ExportCharset::Windows => "Windows (ANSI)",
            ExportCharset::Dos => "ASCII (DOS)",
            ExportCharset::Macintosh => "Macintosh",
            ExportCharset::Utf16 => "Unicode (UTF-16)",
            ExportCharset::Utf8 => "Unicode (UTF-8)",
            ExportCharset::ShiftJis => "Japanese (Shift-JIS)",
            ExportCharset::Gb => "Simplified Chinese (GB)",
            ExportCharset::Big5 => "Traditional Chinese (Big-5)",
            ExportCharset::EucKr => "Korean (EUC-KR)",
        }
    }

    pub const ALL: [ExportCharset; 9] = [
        ExportCharset::Windows,
        ExportCharset::Dos,
        ExportCharset::Macintosh,
        ExportCharset::Utf16,
        ExportCharset::Utf8,
        ExportCharset::ShiftJis,
        ExportCharset::Gb,
        ExportCharset::Big5,
        ExportCharset::EucKr,
    ];

    fn table(self) -> Table {
        match self {
            ExportCharset::Windows => Table::Rs(encoding_rs::WINDOWS_1252),
            ExportCharset::Dos => Table::Cp850,
            ExportCharset::Macintosh => Table::Rs(encoding_rs::MACINTOSH),
            ExportCharset::Utf16 => Table::Utf16Le,
            ExportCharset::Utf8 => Table::Utf8,
            ExportCharset::ShiftJis => Table::Rs(encoding_rs::SHIFT_JIS),
            ExportCharset::Gb => Table::Rs(encoding_rs::GB18030),
            ExportCharset::Big5 => Table::Rs(encoding_rs::BIG5),
            ExportCharset::EucKr => Table::Rs(encoding_rs::EUC_KR),
        }
    }

    /// The bytes that start a file in this character set.
    pub fn preamble(self) -> &'static [u8] {
        match self {
            ExportCharset::Utf16 => &[0xff, 0xfe],
            _ => &[],
        }
    }

    /// Encode text, dropping what the character set cannot hold (module
    /// docs).
    pub fn encode(self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len());
        self.encode_into(text, &mut out);
        out
    }

    pub fn encode_into(self, text: &str, out: &mut Vec<u8>) {
        match self.table() {
            Table::Utf8 => out.extend(text.as_bytes()),
            Table::Utf16Le => {
                for unit in text.encode_utf16() {
                    out.extend(unit.to_le_bytes());
                }
            }
            table => {
                for c in text.chars() {
                    if self == ExportCharset::Macintosh {
                        // FileMaker's Mac OS Roman table is the one from
                        // before the euro: 0xDB is the currency sign, 0xBD
                        // the ohm sign.
                        match c {
                            '¤' => {
                                out.push(0xdb);
                                continue;
                            }
                            '\u{2126}' => {
                                out.push(0xbd);
                                continue;
                            }
                            '€' | 'Ω' => {
                                self.fallback(table, c, out);
                                continue;
                            }
                            _ => {}
                        }
                    }
                    // Nor has its Big-5 table a euro sign.
                    let euro = c == '€' && self == ExportCharset::Big5;
                    if euro || !table.push(c, out) {
                        self.fallback(table, c, out);
                    }
                }
            }
        }
    }

    /// A character the table lacks.
    fn fallback(self, table: Table, c: char, out: &mut Vec<u8>) {
        if c == '€' && matches!(self, ExportCharset::Macintosh | ExportCharset::Big5) {
            out.extend(b"EUR");
            return;
        }
        if c == '–' && self == ExportCharset::EucKr {
            out.push(b'-');
            return;
        }
        if !matches!(self, ExportCharset::Big5 | ExportCharset::EucKr) {
            return;
        }
        // A letter with marks: the marks (as spacing characters), then the
        // base letter.
        let decomposed: String = DecomposingNormalizerBorrowed::new_nfd()
            .normalize(c.encode_utf8(&mut [0; 4]))
            .into_owned();
        let mut chars = decomposed.chars();
        let Some(base) = chars.next() else {
            return;
        };
        let marks: Vec<char> = chars.collect();
        if marks.is_empty() {
            return;
        }
        for mark in marks {
            let spacing = match mark {
                '\u{0308}' => '"',
                '\u{0301}' if self == ExportCharset::EucKr => '´',
                '\u{0301}' => '\'',
                '\u{0300}' => '`',
                '\u{0302}' => '^',
                '\u{0303}' => '~',
                _ => continue,
            };
            if !table.push(spacing, out) {
                out.push(b'?');
            }
        }
        table.push(base, out);
    }
}

/// Import character sets (`FMImportOptions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImportCharset {
    /// 0 "Windows ANSI".
    Windows,
    /// 1 "DOS".
    Dos,
    /// 2 "Mac Roman".
    MacRoman,
    /// 3 "Unicode": UTF-16, little-endian without a byte order mark.
    Utf16,
    /// 4 "Unicode (big endian)".
    Utf16Be,
    /// 5 "UTF-8".
    Utf8,
    /// 6 "Shift JIS".
    ShiftJis,
    /// 7 "Simplified Chinese (GB)".
    Gb,
    /// 8 "Traditional Chinese (Big-5)".
    Big5,
    /// 9 "Korean (EUC-KR)".
    EucKr,
}

impl ImportCharset {
    pub fn from_stored(v: u8) -> Option<ImportCharset> {
        Some(match v {
            0 => ImportCharset::Windows,
            1 => ImportCharset::Dos,
            2 => ImportCharset::MacRoman,
            3 => ImportCharset::Utf16,
            4 => ImportCharset::Utf16Be,
            5 => ImportCharset::Utf8,
            6 => ImportCharset::ShiftJis,
            7 => ImportCharset::Gb,
            8 => ImportCharset::Big5,
            9 => ImportCharset::EucKr,
            _ => return None,
        })
    }

    pub fn stored(self) -> u8 {
        match self {
            ImportCharset::Windows => 0,
            ImportCharset::Dos => 1,
            ImportCharset::MacRoman => 2,
            ImportCharset::Utf16 => 3,
            ImportCharset::Utf16Be => 4,
            ImportCharset::Utf8 => 5,
            ImportCharset::ShiftJis => 6,
            ImportCharset::Gb => 7,
            ImportCharset::Big5 => 8,
            ImportCharset::EucKr => 9,
        }
    }

    /// The name FileMaker's dialogs and XML show.
    pub fn name(self) -> &'static str {
        match self {
            ImportCharset::Windows => "Windows ANSI",
            ImportCharset::Dos => "DOS",
            ImportCharset::MacRoman => "Mac Roman",
            ImportCharset::Utf16 => "Unicode",
            ImportCharset::Utf16Be => "Unicode (big endian)",
            ImportCharset::Utf8 => "UTF-8",
            ImportCharset::ShiftJis => "Shift JIS",
            ImportCharset::Gb => "Simplified Chinese (GB)",
            ImportCharset::Big5 => "Traditional Chinese (Big-5)",
            ImportCharset::EucKr => "Korean (EUC-KR)",
        }
    }

    pub const ALL: [ImportCharset; 10] = [
        ImportCharset::Windows,
        ImportCharset::Dos,
        ImportCharset::MacRoman,
        ImportCharset::Utf16,
        ImportCharset::Utf16Be,
        ImportCharset::Utf8,
        ImportCharset::ShiftJis,
        ImportCharset::Gb,
        ImportCharset::Big5,
        ImportCharset::EucKr,
    ];

    /// Decode a whole file (module docs for byte order marks). Bytes that
    /// are not text in the character set become U+FFFD.
    pub fn decode(self, bytes: &[u8]) -> String {
        if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
            return utf16(rest, false);
        }
        if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
            return utf16(rest, true);
        }
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        match self {
            ImportCharset::Utf16 => utf16(bytes, false),
            ImportCharset::Utf16Be => utf16(bytes, true),
            ImportCharset::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            ImportCharset::Dos => bytes.iter().map(|&b| cp850_char(b)).collect(),
            ImportCharset::Windows => rs(encoding_rs::WINDOWS_1252, bytes),
            ImportCharset::MacRoman => rs(encoding_rs::MACINTOSH, bytes),
            ImportCharset::ShiftJis => rs(encoding_rs::SHIFT_JIS, bytes),
            ImportCharset::Gb => rs(encoding_rs::GB18030, bytes),
            ImportCharset::Big5 => rs(encoding_rs::BIG5, bytes),
            ImportCharset::EucKr => rs(encoding_rs::EUC_KR, bytes),
        }
    }
}

fn rs(encoding: &'static Encoding, bytes: &[u8]) -> String {
    encoding.decode_without_bom_handling(bytes).0.into_owned()
}

fn utf16(bytes: &[u8], big_endian: bool) -> String {
    let units = bytes.chunks_exact(2).map(|p| {
        if big_endian {
            u16::from_be_bytes([p[0], p[1]])
        } else {
            u16::from_le_bytes([p[0], p[1]])
        }
    });
    char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{fffd}'))
        .collect()
}

#[derive(Clone, Copy)]
enum Table {
    Rs(&'static Encoding),
    Cp850,
    Utf8,
    Utf16Le,
}

impl Table {
    /// Append `c`; `false` when the table has no code for it.
    fn push(self, c: char, out: &mut Vec<u8>) -> bool {
        match self {
            Table::Utf8 => {
                out.extend(c.encode_utf8(&mut [0; 4]).as_bytes());
                true
            }
            Table::Utf16Le => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    out.extend(unit.to_le_bytes());
                }
                true
            }
            Table::Cp850 => match cp850_byte(c) {
                Some(b) => {
                    out.push(b);
                    true
                }
                None => false,
            },
            Table::Rs(encoding) => {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                let (bytes, _, unmappable) = encoding.encode(s);
                if unmappable {
                    false
                } else {
                    out.extend(bytes.iter());
                    true
                }
            }
        }
    }
}

/// Code page 850, bytes 0x80-0xff.
const CP850: [u16; 128] = [
    0x00c7, 0x00fc, 0x00e9, 0x00e2, 0x00e4, 0x00e0, 0x00e5, 0x00e7, 0x00ea, 0x00eb, 0x00e8, 0x00ef,
    0x00ee, 0x00ec, 0x00c4, 0x00c5, 0x00c9, 0x00e6, 0x00c6, 0x00f4, 0x00f6, 0x00f2, 0x00fb, 0x00f9,
    0x00ff, 0x00d6, 0x00dc, 0x00f8, 0x00a3, 0x00d8, 0x00d7, 0x0192, 0x00e1, 0x00ed, 0x00f3, 0x00fa,
    0x00f1, 0x00d1, 0x00aa, 0x00ba, 0x00bf, 0x00ae, 0x00ac, 0x00bd, 0x00bc, 0x00a1, 0x00ab, 0x00bb,
    0x2591, 0x2592, 0x2593, 0x2502, 0x2524, 0x00c1, 0x00c2, 0x00c0, 0x00a9, 0x2563, 0x2551, 0x2557,
    0x255d, 0x00a2, 0x00a5, 0x2510, 0x2514, 0x2534, 0x252c, 0x251c, 0x2500, 0x253c, 0x00e3, 0x00c3,
    0x255a, 0x2554, 0x2569, 0x2566, 0x2560, 0x2550, 0x256c, 0x00a4, 0x00f0, 0x00d0, 0x00ca, 0x00cb,
    0x00c8, 0x0131, 0x00cd, 0x00ce, 0x00cf, 0x2518, 0x250c, 0x2588, 0x2584, 0x00a6, 0x00cc, 0x2580,
    0x00d3, 0x00df, 0x00d4, 0x00d2, 0x00f5, 0x00d5, 0x00b5, 0x00fe, 0x00de, 0x00da, 0x00db, 0x00d9,
    0x00fd, 0x00dd, 0x00af, 0x00b4, 0x00ad, 0x00b1, 0x2017, 0x00be, 0x00b6, 0x00a7, 0x00f7, 0x00b8,
    0x00b0, 0x00a8, 0x00b7, 0x00b9, 0x00b3, 0x00b2, 0x25a0, 0x00a0,
];

fn cp850_char(b: u8) -> char {
    if b < 0x80 {
        char::from(b)
    } else {
        char::from_u32(u32::from(CP850[usize::from(b - 0x80)])).unwrap_or('\u{fffd}')
    }
}

fn cp850_byte(c: char) -> Option<u8> {
    if (c as u32) < 0x80 {
        return Some(c as u8);
    }
    CP850
        .iter()
        .position(|&u| u32::from(u) == c as u32)
        .map(|i| 0x80 + i as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_conversions_drop_or_spell_what_a_set_lacks() {
        let web = "naïve – €100 😀 日本";
        // oracle x01: x_tabs_c0, c1, c2, c8, c10 (Website column)
        assert_eq!(
            ExportCharset::Windows.encode(web),
            b"na\xefve \x96 \x80100  "
        );
        assert_eq!(ExportCharset::Dos.encode(web), b"na\x8bve  100  ");
        assert_eq!(
            ExportCharset::Macintosh.encode(web),
            b"na\x95ve \xd0 EUR100  "
        );
        assert_eq!(
            ExportCharset::Gb.encode(web),
            b"na\x810\x8a7ve \xa8C \xa2\xe3100 \x949\xfc6 \xc8\xd5\xb1\xbe"
        );
        assert_eq!(ExportCharset::EucKr.encode("naïve"), b"na\"ive".to_vec());
        assert_eq!(ExportCharset::Big5.encode("é€"), b"'eEUR");
        assert_eq!(ExportCharset::EucKr.encode("é"), b"\xa2\xa5e");
        assert_eq!(ExportCharset::Utf16.encode("A€"), b"A\x00\xac\x20");
    }

    #[test]
    fn imports_follow_byte_order_marks() {
        let le = [0xff, 0xfe, b'a', 0, b'b', 0];
        assert_eq!(ImportCharset::Utf16Be.decode(&le), "ab");
        assert_eq!(ImportCharset::Utf16.decode(&[b'a', 0]), "a");
        assert_eq!(ImportCharset::Utf16.decode(&[0xfe, 0xff, 0, b'a']), "a");
        // A UTF-8 mark is skipped; the bytes stay Windows text.
        assert_eq!(ImportCharset::Windows.decode(b"\xef\xbb\xbf\xc3\x9c"), "Ãœ");
        assert_eq!(ImportCharset::Dos.decode(b"caf\x82"), "café");
        assert_eq!(ImportCharset::MacRoman.decode(b"caf\x8e \xd0"), "café –");
    }
}
