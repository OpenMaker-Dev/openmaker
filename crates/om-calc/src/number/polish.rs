//! The serialized number of formula streams, `FixPt::WriteAsPolish` /
//! `ReadAsPolish` (spec `docs/re/calc-values.md` 2.2).
//!
//! A tag byte (0 empty, 1 `?`, 2 value), then for a value: the integer word
//! count as a big-endian i16, negated for negative numbers; the display
//! precision as a big-endian u16; the integer words and then the fraction
//! words, most significant first, each a big-endian u32 below 10^9. The
//! stream does not count fraction words: a number of precision p carries
//! ceil((p + 2) / 9) of them, so an integer at the default precision takes
//! 1 + 2 + 2 + 3 x 4 = 17 bytes (the count is the spec's inference, to be
//! confirmed on the formula corpus by `calc-encoding.md`).

use super::{mag, Fin, Number, Repr, GUARD};
use std::fmt;

/// Why a stored number could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolishError {
    /// The bytes end inside the number.
    Truncated,
    /// The tag byte is not 0, 1 or 2.
    BadTag(u8),
    /// A word is 10^9 or more.
    BadWord(u32),
}

impl fmt::Display for PolishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolishError::Truncated => write!(f, "stored number truncated"),
            PolishError::BadTag(t) => write!(f, "stored number tag {t}"),
            PolishError::BadWord(w) => write!(f, "stored number word {w} out of range"),
        }
    }
}

impl std::error::Error for PolishError {}

/// Fraction words a number of display precision `precision` carries.
fn fraction_words(precision: u16) -> usize {
    (u32::from(precision) + GUARD).div_ceil(mag::WORD_DIGITS) as usize
}

impl Number {
    /// Appends the stored form.
    pub fn write_polish(&self, out: &mut Vec<u8>) {
        let fin = match &self.0 {
            Repr::Empty => return out.push(0),
            Repr::NaN => return out.push(1),
            Repr::Fin(f) => f,
        };
        out.push(2);
        let frac_count = fraction_words(fin.precision);
        let fin = fin.truncated(frac_count as u32 * mag::WORD_DIGITS);
        let aligned = fin.coef_at(frac_count as u32 * mag::WORD_DIGITS);
        let int: Vec<u32> = aligned
            .get(frac_count..)
            .map(<[u32]>::to_vec)
            .unwrap_or_default();
        let int_count = int.len().max(1);
        let count = int_count as i16;
        out.extend_from_slice(&(if fin.neg { -count } else { count }).to_be_bytes());
        out.extend_from_slice(&fin.precision.to_be_bytes());
        for i in (0..int_count).rev() {
            out.extend_from_slice(&int.get(i).copied().unwrap_or(0).to_be_bytes());
        }
        for i in (0..frac_count).rev() {
            out.extend_from_slice(&aligned.get(i).copied().unwrap_or(0).to_be_bytes());
        }
    }

    /// Reads a stored number from the start of `bytes`; returns it and the
    /// number of bytes used.
    pub fn read_polish(bytes: &[u8]) -> Result<(Number, usize), PolishError> {
        let tag = *bytes.first().ok_or(PolishError::Truncated)?;
        match tag {
            0 => return Ok((Number::empty(), 1)),
            1 => return Ok((Number::nan(), 1)),
            2 => {}
            other => return Err(PolishError::BadTag(other)),
        }
        let header = bytes.get(1..5).ok_or(PolishError::Truncated)?;
        let count = i16::from_be_bytes([header[0], header[1]]);
        let precision = u16::from_be_bytes([header[2], header[3]]);
        let int_count = usize::from(count.unsigned_abs());
        let frac_count = fraction_words(precision);
        let total = int_count + frac_count;
        let body = bytes.get(5..5 + 4 * total).ok_or(PolishError::Truncated)?;
        let mut words = Vec::with_capacity(total);
        for chunk in body.chunks_exact(4).rev() {
            let w = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if u64::from(w) >= mag::BASE {
                return Err(PolishError::BadWord(w));
            }
            words.push(w);
        }
        let fin = Fin::new(
            count < 0,
            words,
            frac_count as u32 * mag::WORD_DIGITS,
            precision,
        );
        Ok((Number::from_fin(fin), 5 + 4 * total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn an_integer_literal_is_17_bytes() {
        let mut out = Vec::new();
        Number::from_i64(5).write_polish(&mut out);
        assert_eq!(hex(&out), "0200010010000000050000000000000000");
        assert_eq!(out.len(), 17);
    }

    #[test]
    fn round_trips() {
        for text in [
            "123456789012.5",
            "0.125",
            "1e-30",
            "123456789012345678901234567890",
        ] {
            let n = Number::from_literal(text, '.').unwrap().neg();
            let mut out = Vec::new();
            n.write_polish(&mut out);
            let (back, used) = Number::read_polish(&out).unwrap();
            assert_eq!(used, out.len());
            assert_eq!(back.to_text_with('.'), n.to_text_with('.'));
            assert_eq!(back.precision(), n.precision());
        }
        assert!(Number::read_polish(&[0]).unwrap().0.is_empty());
        assert!(Number::read_polish(&[1]).unwrap().0.is_nan());
        assert_eq!(
            Number::read_polish(&[2, 0]).unwrap_err(),
            PolishError::Truncated
        );
        assert_eq!(
            Number::read_polish(&[9]).unwrap_err(),
            PolishError::BadTag(9)
        );
    }
}
