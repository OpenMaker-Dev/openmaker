//! Number literals in their stored decimal form (`Draco::FixPt`,
//! calc-encoding.md 2.2; calc-values.md 2.1-2.2), the source literal parser
//! and the display text. Arithmetic belongs to `om-calc`.

use crate::codec::{Reader, Result};
use crate::error::DecodeErrorKind;

/// A stored number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Number {
    /// Tag 0: an empty number.
    Empty,
    /// Tag 1: `?` (`kNaNResult`).
    NotANumber,
    /// Tag 2.
    Decimal(Decimal),
}

/// A sign-magnitude decimal in base 10^9 words, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decimal {
    pub negative: bool,
    /// Integer words, most significant first, each 0..=999,999,999.
    pub whole: Vec<u32>,
    /// Fraction words: digits 1-9, 10-18, ... after the point.
    pub fraction: Vec<u32>,
    /// Display precision (`mPrecision - mExtraPrecision`): 16 unless the
    /// literal was written with more fractional digits.
    pub precision: u16,
}

/// Guard digits FileMaker holds beyond the display precision.
const EXTRA_PRECISION: u16 = 2;

/// How many fraction words hold `precision` digits plus the guard digits,
/// as `FixPt::ReadAsPolish` reads them.
fn fraction_words(precision: u16) -> usize {
    let p = (precision as i16).unsigned_abs().clamp(16, 800);
    usize::from((p + EXTRA_PRECISION).div_ceil(9))
}

/// The format word of a number token in the formatted stream (4.1).
pub const FORMAT_FLOAT: u16 = 0x0020;
/// Written with an exponent.
pub const FORMAT_SCIENTIFIC: u16 = 0x0040;

impl Number {
    pub(crate) fn read(r: &mut Reader<'_>) -> Result<Self> {
        match r.u8()? {
            0 => Ok(Self::Empty),
            1 => Ok(Self::NotANumber),
            2 => {
                let count = r.u16()? as i16;
                let precision = r.u16()?;
                let whole = (0..count.unsigned_abs())
                    .map(|_| r.u32())
                    .collect::<Result<_>>()?;
                let fraction = (0..fraction_words(precision))
                    .map(|_| r.u32())
                    .collect::<Result<_>>()?;
                Ok(Self::Decimal(Decimal {
                    negative: count < 0,
                    whole,
                    fraction,
                    precision,
                }))
            }
            _ => Err(r.error(DecodeErrorKind::Malformed("number"))),
        }
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::Empty => out.push(0),
            Self::NotANumber => out.push(1),
            Self::Decimal(d) => {
                out.push(2);
                let count = d.whole.len() as i16;
                let count = if d.negative { -count } else { count };
                out.extend_from_slice(&count.to_be_bytes());
                out.extend_from_slice(&d.precision.to_be_bytes());
                for word in d.whole.iter().chain(&d.fraction) {
                    out.extend_from_slice(&word.to_be_bytes());
                }
            }
        }
    }
}

impl Decimal {
    /// Parse a source literal (`DBCalcScan::GetNumber`, 6.1): digits, an
    /// optional point and digits, an optional `E`, sign and digits. A
    /// literal holds exactly the digits written, with precision 16 or the
    /// number of fractional digits if more (calc-values.md 2.1). `None` when
    /// FileMaker cannot evaluate it (error 1218): no digits, a point with no
    /// digit after it, an exponent without digits, or a magnitude of 10^800
    /// or more, or below 10^-799.
    pub fn parse_literal(text: &str) -> Option<Self> {
        let (mantissa, exponent) = match text.find(['e', 'E']) {
            Some(i) => (&text[..i], Some(&text[i + 1..])),
            None => (text, None),
        };
        let (int_part, frac_part) = match mantissa.split_once('.') {
            Some((i, f)) => {
                if f.is_empty() {
                    return None;
                }
                (i, f)
            }
            None => (mantissa, ""),
        };
        let digits_ok = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if !digits_ok(int_part) || !digits_ok(frac_part) || int_part.len() + frac_part.len() == 0 {
            return None;
        }
        let exponent: i64 = match exponent {
            None => 0,
            Some(e) => {
                let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
                if digits.is_empty() || !digits_ok(digits) {
                    return None;
                }
                let value = digits.parse::<i64>().unwrap_or(i64::MAX / 2).min(100_000);
                if e.starts_with('-') {
                    -value
                } else {
                    value
                }
            }
        };
        // All digits, and the position of the point counted from their start.
        let all: Vec<u8> = int_part
            .bytes()
            .chain(frac_part.bytes())
            .map(|b| b - b'0')
            .collect();
        let point = int_part.len() as i64 + exponent;
        let first = all.iter().position(|&d| d != 0);
        if let Some(first) = first {
            // Magnitude 10^(point - first - 1).
            let magnitude = point - first as i64 - 1;
            if !(-799..800).contains(&magnitude) {
                return None;
            }
        }
        let mut int_digits = Vec::new();
        let mut frac_digits = Vec::new();
        for (i, &d) in all.iter().enumerate() {
            if (i as i64) < point {
                int_digits.push(d);
            } else {
                frac_digits.push(d);
            }
        }
        // Zeros the exponent moves in on either side of the written digits.
        if point > all.len() as i64 {
            int_digits.extend(std::iter::repeat(0).take((point - all.len() as i64) as usize));
        }
        if point < 0 {
            let mut padded = vec![0; (-point) as usize];
            padded.extend(frac_digits);
            frac_digits = padded;
        }
        let precision = (frac_digits.len() as u16).max(16);
        Some(Self::from_digits(&int_digits, &frac_digits, precision))
    }

    /// Build from decimal digits (most significant first).
    fn from_digits(int_digits: &[u8], frac_digits: &[u8], precision: u16) -> Self {
        let int_digits = {
            let lead = int_digits.iter().take_while(|&&d| d == 0).count();
            &int_digits[lead..]
        };
        let mut whole = Vec::new();
        let mut end = int_digits.len();
        while end > 0 {
            let start = end.saturating_sub(9);
            whole.push(
                int_digits[start..end]
                    .iter()
                    .fold(0, |acc, &d| acc * 10 + u32::from(d)),
            );
            end = start;
        }
        if whole.is_empty() {
            whole.push(0);
        }
        whole.reverse();
        let mut fraction = vec![0u32; fraction_words(precision)];
        for (i, &d) in frac_digits.iter().take(fraction.len() * 9).enumerate() {
            fraction[i / 9] += u32::from(d) * 10u32.pow(8 - (i % 9) as u32);
        }
        Self {
            negative: false,
            whole,
            fraction,
            precision,
        }
    }

    /// Integer digits without leading zeros (empty for zero) and fractional
    /// digits without trailing zeros.
    fn digits(&self) -> (String, String) {
        let mut int = String::new();
        for word in &self.whole {
            if !int.is_empty() {
                int.push_str(&format!("{word:09}"));
            } else if *word != 0 {
                int = word.to_string();
            }
        }
        let mut frac: String = self.fraction.iter().map(|w| format!("{w:09}")).collect();
        frac.truncate(frac.trim_end_matches('0').len());
        (int, frac)
    }

    /// The literal as FileMaker displays it in a formula
    /// (`FixPt::GetFormattedText` with the stored format word, 4.2): plain
    /// decimal without a leading zero before the point (`.5`), or with
    /// [`FORMAT_SCIENTIFIC`] one digit, the point, the other digits (`0` if
    /// none) and a signed exponent (`1.5E+20`), as calc-values.md 2.7
    /// describes number text.
    pub fn display(&self, format: u16) -> String {
        let (int, frac) = self.digits();
        let sign = if self.negative { "-" } else { "" };
        if int.is_empty() && frac.is_empty() {
            return "0".into();
        }
        if format & FORMAT_SCIENTIFIC == 0 {
            return if frac.is_empty() {
                format!("{sign}{int}")
            } else {
                format!("{sign}{int}.{frac}")
            };
        }
        let all = format!("{int}{frac}");
        let first = all.find(|c| c != '0').unwrap_or(0);
        let exponent = int.len() as i64 - first as i64 - 1;
        let significant = all[first..].trim_end_matches('0');
        let (lead, rest) = significant.split_at(1);
        let rest = if rest.is_empty() { "0" } else { rest };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{lead}.{rest}E{exp_sign}{}", exponent.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(d: &Decimal) -> Vec<u8> {
        let mut out = Vec::new();
        Number::Decimal(d.clone()).write(&mut out);
        out
    }

    #[test]
    fn one_is_one_whole_word_and_two_fraction_words() {
        // calc-encoding.md 2.2: `1` = `02 0001 0010 00000001 00000000 00000000`.
        let one = Decimal::parse_literal("1").unwrap();
        let bytes = stored(&one);
        assert_eq!(bytes, [2, 0, 1, 0, 16, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        let mut r = Reader::new(&bytes);
        assert_eq!(Number::read(&mut r).unwrap(), Number::Decimal(one));
        assert_eq!(Decimal::parse_literal("584").unwrap().whole, [0x248]);
    }

    #[test]
    fn literals_hold_the_digits_written() {
        let d = Decimal::parse_literal("1234567890.5").unwrap();
        assert_eq!(d.whole, [1, 234_567_890]);
        assert_eq!(d.fraction, [500_000_000, 0]);
        let d = Decimal::parse_literal("0.12345678901234567").unwrap();
        assert_eq!((d.precision, d.fraction.len()), (17, 3));
        let d = Decimal::parse_literal("1.5E3").unwrap();
        assert_eq!(d.whole, [1500]);
        let d = Decimal::parse_literal("25E-2").unwrap();
        assert_eq!(d.fraction[0], 250_000_000);
    }

    #[test]
    fn invalid_literals_are_rejected() {
        for text in ["5.", "1e800", "1e-800", "1E", "1E+", "."] {
            assert_eq!(Decimal::parse_literal(text), None, "{text}");
        }
        assert!(Decimal::parse_literal("1e799").is_some());
    }

    #[test]
    fn display_matches_number_text() {
        let show = |t: &str, f| Decimal::parse_literal(t).unwrap().display(f);
        assert_eq!(show("0.5", FORMAT_FLOAT), ".5");
        assert_eq!(show("10", FORMAT_FLOAT), "10");
        assert_eq!(show("1000000000", FORMAT_FLOAT), "1000000000");
        assert_eq!(show("0", FORMAT_FLOAT), "0");
        assert_eq!(show("1E20", FORMAT_SCIENTIFIC), "1.0E+20");
        assert_eq!(show("1.5e-3", FORMAT_SCIENTIFIC), "1.5E-3");
    }
}
