//! Number to text, `FixPt::GetText` (spec `docs/re/calc-values.md` 2.7).

use super::{mag, Number, Repr};

impl Number {
    /// The text `&`, text functions and `GetAsText` see, with `decimal` as
    /// the decimal separator (the locale's): rounded half away from zero to
    /// the display precision; `0` for zero (never `-0`); plain unless the
    /// integer part is at least 10^9 with its lowest nine digits zero, or
    /// 0 < |x| < 10^-9, which use the scientific form `1.5E+20`. Empty is
    /// the empty string and `?` is `?`.
    pub fn to_text_with(&self, decimal: char) -> String {
        self.text_with(decimal, true)
    }

    /// [`Self::to_text_with`] with every digit written out, never the
    /// scientific form: `1000000000`, not `1.0E+9`, which a find criterion
    /// reads as another number.
    pub fn to_plain_text_with(&self, decimal: char) -> String {
        self.text_with(decimal, false)
    }

    fn text_with(&self, decimal: char, scientific_form: bool) -> String {
        let fin = match &self.0 {
            Repr::Empty => return String::new(),
            Repr::NaN => return "?".to_string(),
            Repr::Fin(f) => f.rounded(i64::from(f.precision)),
        };
        if fin.is_zero() {
            return "0".to_string();
        }
        let digits = mag::to_digits(&fin.coef);
        let scale = fin.scale as usize;
        let (int_part, frac_part) = if digits.len() > scale {
            let (i, f) = digits.split_at(digits.len() - scale);
            (i.to_string(), f.to_string())
        } else {
            (String::new(), format!("{digits:0>scale$}"))
        };
        let frac_part = frac_part.trim_end_matches('0');
        let sign = if fin.neg { "-" } else { "" };
        let scientific = scientific_form
            && if int_part.len() >= 10 {
                int_part.ends_with("000000000")
            } else {
                int_part.is_empty() && frac_part.len() >= 10 && frac_part.starts_with("000000000")
            };
        if !scientific {
            let mut out = format!("{sign}{int_part}");
            if !frac_part.is_empty() {
                out.push(decimal);
                out.push_str(frac_part);
            }
            return out;
        }
        // Significant digits and the decimal exponent of the first one. A
        // large number's mantissa is held in clamp(e + p, 16, 800)
        // fractional digits, whole words, so beyond 800 it keeps 801 of
        // them, truncated (calc-values.md 14.8: `Exp(1900)` shows 802
        // significant digits).
        let all = format!("{int_part}{frac_part}");
        let lead = all.len() - all.trim_start_matches('0').len();
        let exponent = int_part.len() as i64 - lead as i64 - 1;
        let mut significant = &all[lead..];
        if exponent > 0 {
            let held = (exponent + i64::from(fin.precision)).clamp(16, 800) as usize;
            let keep = 1 + 9 * held.div_ceil(9);
            significant = &significant[..significant.len().min(keep)];
        }
        let significant = significant.trim_end_matches('0');
        let (first, rest) = significant.split_at(1);
        let rest = if rest.is_empty() { "0" } else { rest };
        let exp_sign = if exponent < 0 { '-' } else { '+' };
        format!(
            "{sign}{first}{decimal}{rest}E{exp_sign}{}",
            exponent.unsigned_abs()
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::number::Number;

    fn show(s: &str) -> String {
        match s.strip_prefix('-') {
            Some(rest) => Number::from_literal(rest, '.').unwrap().neg(),
            None => Number::from_literal(s, '.').unwrap(),
        }
        .to_text_with('.')
    }

    #[test]
    fn plain_and_scientific_forms() {
        assert_eq!(show("1000000"), "1000000");
        assert_eq!(show("1000000000"), "1.0E+9");
        assert_eq!(show("1500000000"), "1500000000");
        assert_eq!(show("1000000000.5"), "1.0000000005E+9");
        assert_eq!(show("12345678901000000"), "12345678901000000");
        assert_eq!(show("0.000000001"), ".000000001");
        assert_eq!(show("0.0000000001"), "1.0E-10");
        assert_eq!(show("0.00000000099"), "9.9E-10");
        assert_eq!(show("-0.5"), "-.5");
        assert_eq!(show("-0.0"), "0");
        assert_eq!(show("1.50"), "1.5");
        assert_eq!(show("0.0000000000000012345"), "1.2345E-15");
        assert_eq!(
            show("123456789000000000000.5"),
            "1.234567890000000000005E+20"
        );
        assert_eq!(
            Number::from_literal("1,5", ',').unwrap().to_text_with(','),
            "1,5"
        );
        // Written out in full for a find criterion.
        let plain = |s: &str| {
            Number::from_literal(s, '.')
                .unwrap()
                .to_plain_text_with('.')
        };
        assert_eq!(plain("1000000000"), "1000000000");
        assert_eq!(plain("0.0000000001"), ".0000000001");
        assert_eq!(plain("1500000000"), "1500000000");
    }

    #[test]
    fn display_rounds_to_the_precision() {
        assert_eq!(show("0.12345678901234567"), ".12345678901234567");
        let third = Number::from_i64(1).div(&Number::from_i64(3), 16).unwrap();
        assert_eq!(third.to_text_with('.'), ".3333333333333333");
        assert_eq!(Number::empty().to_text_with('.'), "");
        assert_eq!(Number::nan().to_text_with('.'), "?");
    }
}
