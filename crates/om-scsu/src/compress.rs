//! FileMaker's SCSU compressor.
//!
//! SCSU (UTS #6) lets an encoder choose among many valid byte streams for the
//! same text; FileMaker's choices are fixed by its compressor, and writers that
//! must produce the bytes FileMaker stores (record values, catalog names,
//! formulas) follow them exactly. Spec: `docs/re/calc-encoding.md` 2.4, checked
//! byte for byte on every distinct string of the formula corpus.

use crate::{DEFAULT_DYNAMIC, SC0, SCU, SD0, SDX, SQ0, SQU, UC0, UD0, UDX, UQU};

/// The static windows FileMaker quotes from with `SQ3`..`SQ7`, besides the
/// `SQ1`/`SQ2` ranges below U+0180.
const STATIC_QUOTES: [(u32, u8); 5] = [
    (0x0300, 3),
    (0x2000, 4),
    (0x2080, 5),
    (0x2100, 6),
    (0x3000, 7),
];

/// Characters a dynamic window can hold: U+0000..U+33FF and U+E000..U+FFFF.
fn compressible(unit: u32) -> bool {
    unit <= 0x33FF || (0xE000..=0xFFFF).contains(&unit)
}

fn in_window(offset: u32, c: u32) -> bool {
    (offset..offset + 0x80).contains(&c)
}

/// How FileMaker defines a window for `c`.
enum Definition {
    /// `SD`/`UD` with a one-byte offset index.
    Index(u8, u32),
    /// `SDX`/`UDX` for a supplementary character: the window bits without
    /// the window number, and the offset.
    Extended(u16, u32),
}

/// The window FileMaker defines for `c` (2.4): the fixed UTS #6 windows for
/// six scripts, else the half-block holding `c`. `None` for U+3400..U+DFFF,
/// which no window covers.
fn definition(c: u32) -> Option<Definition> {
    Some(match c {
        0x0250..=0x02CF => Definition::Index(0xFA, 0x0250),
        0x0370..=0x03EF => Definition::Index(0xFB, 0x0370),
        0x0530..=0x05AF => Definition::Index(0xFC, 0x0530),
        // Hiragana 3040-30BF first: U+30A0..U+30BF, in both fixed windows,
        // gets the Hiragana one.
        0x3040..=0x30BF => Definition::Index(0xFD, 0x3040),
        0x30C0..=0x311F => Definition::Index(0xFE, 0x30A0),
        0xFF60..=0xFFDF => Definition::Index(0xFF, 0xFF60),
        0..=0x33FF => Definition::Index((c >> 7) as u8, c & 0xFF80),
        0xE000..=0xFFFF => Definition::Index(((c - 0xAC00) >> 7) as u8, c & 0xFF80),
        0x10000.. => {
            let base = c - 0x10000;
            Definition::Extended((base >> 7) as u16, 0x10000 + (base & !0x7F))
        }
        _ => return None,
    })
}

struct Compressor<'a> {
    units: &'a [u16],
    windows: [u32; 8],
    active: usize,
    /// The window the next definition replaces: 3, 4, ..., 7, 0, 1, ...
    next: usize,
    out: Vec<u8>,
}

impl Compressor<'_> {
    fn unit(&self, i: usize) -> Option<u32> {
        self.units.get(i).map(|&u| u32::from(u))
    }

    /// The character at `i` and its length in units; a valid surrogate pair
    /// is one character.
    fn char_at(&self, i: usize) -> (u32, usize) {
        let u = u32::from(self.units[i]);
        if (0xD800..0xDC00).contains(&u) {
            if let Some(low) = self.unit(i + 1).filter(|l| (0xDC00..0xE000).contains(l)) {
                return (0x10000 + ((u - 0xD800) << 10) + (low - 0xDC00), 2);
            }
        }
        (u, 1)
    }

    fn window_of(&self, c: u32) -> Option<usize> {
        (0..8).find(|&w| in_window(self.windows[w], c))
    }

    /// Define a window for `c` in the next slot with the single-byte (`SD`,
    /// `SDX`) or Unicode-mode (`UD`, `UDX`) tag; the window becomes active.
    fn define(&mut self, definition: Definition, unicode: bool) {
        let n = self.next;
        match definition {
            Definition::Index(index, offset) => {
                self.out.push(if unicode { UD0 } else { SD0 } + n as u8);
                self.out.push(index);
                self.windows[n] = offset;
            }
            Definition::Extended(bits, offset) => {
                self.out.push(if unicode { UDX } else { SDX });
                self.out
                    .extend_from_slice(&((n as u16) << 13 | bits).to_be_bytes());
                self.windows[n] = offset;
            }
        }
        self.active = n;
        self.next = (n + 1) % 8;
    }

    fn run(mut self) -> Vec<u8> {
        let mut i = 0;
        let mut unicode = false;
        while i < self.units.len() {
            if unicode {
                let u = u32::from(self.units[i]);
                if compressible(u) && self.unit(i + 1).is_some_and(compressible) {
                    self.leave_unicode(i);
                    unicode = false;
                    continue;
                }
                if (0xE000..=0xF2FF).contains(&u) {
                    self.out.push(UQU);
                }
                self.out.extend_from_slice(&self.units[i].to_be_bytes());
                i += 1;
                continue;
            }
            let (c, len) = self.char_at(i);
            if (1..=31).contains(&c) && !matches!(c, 9 | 10 | 13) {
                self.out.extend_from_slice(&[SQ0, c as u8]);
            } else if c < 0x80 {
                self.out.push(c as u8);
            } else if in_window(self.windows[self.active], c) {
                self.out.push((0x80 + c - self.windows[self.active]) as u8);
            } else if let Some(w) = self.window_of(c) {
                let byte = (0x80 + c - self.windows[w]) as u8;
                let next_in_active = self
                    .unit(i + len)
                    .is_some_and(|n| in_window(self.windows[self.active], n));
                if next_in_active {
                    self.out.extend_from_slice(&[SQ0 + w as u8, byte]);
                } else {
                    self.out.extend_from_slice(&[SC0 + w as u8, byte]);
                    self.active = w;
                }
            } else if c < 0x100 {
                self.out.extend_from_slice(&[SQ0 + 1, (c - 0x80) as u8]);
            } else if c < 0x180 {
                self.out.extend_from_slice(&[SQ0 + 2, (c - 0x100) as u8]);
            } else if let Some(&(base, w)) = STATIC_QUOTES.iter().find(|(b, _)| in_window(*b, c)) {
                self.out.extend_from_slice(&[SQ0 + w, (c - base) as u8]);
            } else if let Some(definition) = definition(c) {
                // Define a window, then encode `c` again from it.
                self.define(definition, false);
                continue;
            } else {
                // U+3400..U+DFFF (or a lone surrogate): quote it when the text
                // ends here or returns to compressible text at once, else
                // switch to Unicode mode.
                let quote = i + 1 == self.units.len()
                    || (self.unit(i + 1).is_some_and(compressible)
                        && self.unit(i + 2).is_some_and(compressible));
                if quote {
                    self.out.push(SQU);
                    self.out.extend_from_slice(&self.units[i].to_be_bytes());
                } else {
                    self.out.push(SCU);
                    unicode = true;
                    continue;
                }
            }
            i += len;
        }
        self.out
    }

    /// Return to single-byte mode before the compressible unit at `i`,
    /// selecting the window of the first non-ASCII unit of the compressible
    /// run that starts there.
    fn leave_unicode(&mut self, i: usize) {
        let run = self.units[i..]
            .iter()
            .map(|&u| u32::from(u))
            .take_while(|&u| compressible(u));
        let mut target = 0;
        for u in run {
            target = u;
            if u > 0x7F {
                break;
            }
        }
        if target <= 0x7F {
            self.out.push(UC0 + self.active as u8);
        } else if let Some(w) = self.window_of(target) {
            self.out.push(UC0 + w as u8);
            self.active = w;
        } else if let Some(definition) = definition(target) {
            self.define(definition, true);
        }
    }
}

/// Compress UTF-16 text the way FileMaker does (unmasked bytes).
pub(crate) fn compress(units: &[u16]) -> Vec<u8> {
    Compressor {
        units,
        windows: DEFAULT_DYNAMIC,
        active: 0,
        next: 3,
        out: Vec::with_capacity(units.len()),
    }
    .run()
}
