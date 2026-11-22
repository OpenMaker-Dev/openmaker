//! Text to number, `FixPt::Set(unistring, Locale)` (spec
//! `docs/re/calc-values.md` 2.8), and formula number literals (2.2, 7.1).
//! The reading itself is [`om_collate::number_text`], which a number
//! field's index keys share.

use om_collate::number_text::{self, ReadNumber};

use super::{mag, Fin, Number, DEFAULT_PRECISION, GUARD, MAX_PRECISION};
use crate::error::CalcError;

/// Builds the number `0.digits[..point]digits[point..] * 10^exponent` with
/// `frac` digits written after the point.
fn assemble(neg: bool, digits: &[u8], frac: u64, exponent: i64) -> Number {
    let ascii: Vec<u8> = digits.iter().map(|d| d + b'0').collect();
    let coef = mag::from_digits(&ascii);
    let scale = frac as i64 - exponent;
    let precision = scale.clamp(i64::from(DEFAULT_PRECISION), i64::from(MAX_PRECISION)) as u16;
    let fin = if scale >= 0 {
        Fin::new(neg, coef, scale as u32, precision)
    } else {
        Fin::new(neg, mag::shl_digits(&coef, (-scale) as u32), 0, precision)
    };
    Number::from_fin(fin.truncated(u32::from(precision) + GUARD))
}

/// The number `read` describes.
fn number_of(read: ReadNumber) -> Number {
    match read {
        ReadNumber::Empty => Number::empty(),
        ReadNumber::Invalid => Number::nan(),
        ReadNumber::Value {
            negative,
            digits,
            frac,
            exponent,
        } => assemble(negative, &digits, frac, exponent),
    }
}

impl Number {
    /// A number literal of formula text (`1.5`, `.5`, `1e3`, `1,5` in a
    /// locale whose decimal separator is `,`): p = max(16, fractional digits
    /// written), all digits kept. `5.` and an exponent of 800 or more in
    /// magnitude are error 1218.
    pub fn from_literal(token: &str, decimal: char) -> Result<Number, CalcError> {
        number_text::read_number_literal(token, decimal)
            .map(number_of)
            .ok_or(CalcError::CANNOT_EVALUATE_NUMBER)
    }

    /// Text to number (spec 2.8, and the value-core oracle answers in
    /// calc-values.md 13; [`number_text::read_number`] has the rules). No
    /// digits gives the empty number; exactly `?` gives `?`, and so does a
    /// pure exponent of 800 or more.
    pub fn parse_text(text: &str, decimal: char, thousands: char) -> Number {
        number_of(number_text::read_number(text, decimal, thousands))
    }

    /// The number a text is when it is one in the "pure" form of
    /// [`Number::parse_text`] (`1.5`, `-3`, `1e3`, `(12)`, `1,000`), `None`
    /// when it is not (`12abc`, `abc`, `$5`): what a field validated as
    /// "Strict data type: Numeric only" accepts (script-runtime.md 13.21).
    pub fn parse_pure_text(text: &str, decimal: char, thousands: char) -> Option<Number> {
        number_text::read_pure_number(text, decimal, thousands).map(number_of)
    }
}

#[cfg(test)]
mod tests {
    use crate::error::CalcError;
    use crate::number::Number;

    fn en(s: &str) -> String {
        Number::parse_text(s, '.', ',').to_text_with('.')
    }

    fn de(s: &str) -> String {
        Number::parse_text(s, ',', '.').to_text_with(',')
    }

    fn sv(s: &str) -> String {
        Number::parse_text(s, ',', ' ').to_text_with(',')
    }

    #[test]
    fn spec_table_en_us() {
        for (text, number) in [
            ("abc12d3.4", "123.4"),
            ("1.2.3", "1.23"),
            ("1-2", "12"),
            ("a-1", "-1"),
            ("--12", "-12"),
            ("12-", "12"),
            ("(12)", "-12"),
            ("$1,234.56", "1234.56"),
            ("1,5", "15"),
            ("1.234,56", "1.23456"),
            ("1.5e2x", "1.52"),
            ("1e3e4", "134"),
            ("1/2/2003", "122003"),
            ("12:30", "1230"),
            ("5.", "5"),
            (".5", ".5"),
            ("１２３", "123"),
            ("١٢٣", ""),
            ("¼", ""),
            ("abc", ""),
            (" ", ""),
            ("-", ""),
            (".", ""),
            ("?", "?"),
            ("1e800", "?"),
            ("0.000000000000000000001", "1.0E-21"),
            ("1e3", "1000"),
            ("1e-3", ".001"),
            ("1,5e3", "15000"),
            (" 12 ", "12"),
            ("+-1", "1"),
            ("-+1", "-1"),
            ("(1)2", "-12"),
            ("1(2)", "12"),
            ("(a)1", "1"),
            ("((1))", "-1"),
            ("−5", "-5"),
            ("1e3 ", "1000"),
            ("1e3\t", "13"),
            ("\t1e3", "1000"),
            ("1e+", "1"),
            ("5.e1", "50"),
            (".5e1", "5"),
            ("1,234.5e2", "123450"),
        ] {
            assert_eq!(en(text), number, "{text:?}");
        }
    }

    #[test]
    fn parentheses_and_the_decimal_separator() {
        for (text, number) in [
            (".(22)", ".22"),
            ("(.22)", "-.22"),
            ("(.)22", ".22"),
            (".-22", ".22"),
            ("-.22", "-.22"),
            ("(.(22))", ".22"),
            ("(1(2))", "12"),
            ("(1)(2)", "-12"),
            ("((1)2)", "-12"),
            ("(a)1)", "1"),
            ("((a)1)", "1"),
            ("(a.)1)", ".1"),
            ("(.1)2)", "-.12"),
            ("1(.2)", "1.2"),
            (",(22)", "-22"),
            (",-22", "-22"),
            ("(1e2)", "-100"),
            ("((1e2))", "-100"),
            ("( 1e2 )", "-100"),
            ("(1e2", "100"),
            ("1e2)", "100"),
            ("-(1e3)", "-1000"),
            ("(-1e3)", "-13"),
            ("(+1e2)", "-12"),
            ("(1e2)x", "-12"),
            ("(\t1e2)", "-12"),
            ("\t(1e2)", "-100"),
            ("(1e800)", "?"),
            ("（1e2）", "-100"),
            ("1．5", "1.5"),
            ("．(22)", ".22"),
            ("－12", "-12"),
            ("‐1", "-1"),
            ("﹙1﹚", "-1"),
            ("⁽1⁾", "1"),
            ("1ｅ2", "100"),
        ] {
            assert_eq!(en(text), number, "{text:?}");
        }
        assert_eq!(de(",(22)"), ",22");
        assert_eq!(de(".(22)"), "-22");
        assert_eq!(de("１，５"), "1,5");
        assert_eq!(de("(.5e1)"), "-51");
    }

    #[test]
    fn comma_locales() {
        assert_eq!(de("1,5"), "1,5");
        assert_eq!(de("1.5"), "15");
        assert_eq!(de("1.234,56"), "1234,56");
        assert_eq!(de("1,234.56"), "1,23456");
        assert_eq!(de("1,5e3"), "1500");
        assert_eq!(sv("1e3 "), "13");
        assert_eq!(sv("1 000,5"), "1000,5");
        assert_eq!(sv("1\u{a0}000,5"), "1000,5");
        assert_eq!(sv(",5"), ",5");
        assert_eq!(sv("1.5e-20"), "1520");
    }

    #[test]
    fn precision_follows_the_written_digits() {
        let n = Number::parse_text("0.1234567890123456789", '.', ',');
        assert_eq!(n.precision(), 19);
        let n = Number::parse_text("1.0000000000000000000e2", '.', ',');
        assert_eq!(n.precision(), 17);
        assert_eq!(Number::parse_text("1e-20", '.', ',').precision(), 20);
    }

    #[test]
    fn literals() {
        let lit = |s: &str| Number::from_literal(s, '.').map(|n| n.to_text_with('.'));
        assert_eq!(lit("1.50").unwrap(), "1.5");
        assert_eq!(lit("0001").unwrap(), "1");
        assert_eq!(lit(".5").unwrap(), ".5");
        assert_eq!(lit("1e-400").unwrap(), "1.0E-400");
        assert_eq!(lit("10e799").unwrap(), "1.0E+800");
        assert_eq!(lit("1e0799").unwrap(), "1.0E+799");
        assert_eq!(lit("5.").unwrap_err(), CalcError::CANNOT_EVALUATE_NUMBER);
        assert_eq!(lit("1e800").unwrap_err(), CalcError::CANNOT_EVALUATE_NUMBER);
        assert_eq!(
            lit("0.5e800").unwrap_err(),
            CalcError::CANNOT_EVALUATE_NUMBER
        );
        assert_eq!(
            lit("100e-801").unwrap_err(),
            CalcError::CANNOT_EVALUATE_NUMBER
        );
        assert_eq!(lit("0.1e-799").unwrap(), "1.0E-800");
        assert_eq!(
            Number::from_literal("1,5", ',').unwrap().to_text_with(','),
            "1,5"
        );
    }
}
