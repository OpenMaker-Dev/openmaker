//! The constant tables of `FixPt` (spec `docs/re/calc-values.md` 14.1),
//! computed once from their definitions with integer arithmetic:
//!
//! | Constant | Fractional digits | Last digit |
//! |---|---|---|
//! | ln 10 | 801 | truncated |
//! | 1 / ln 10 | 801 | truncated |
//! | ln 2 | 1,953 | truncated |
//! | 1 / ln 2 | 801 | rounded |
//! | π | 720 | rounded |
//!
//! The series ln 2 = 2 atanh(1/3), ln 10 = 3 ln 2 + 2 atanh(1/9) and
//! π = 16 atan(1/5) − 4 atan(1/239) are summed in fixed point with
//! [`EXTRA`] digits beyond the longest table; every truncated division of
//! the series is off by less than one unit there, so the sums are off by a
//! few tens of thousands of units at most. A table is only cut where the
//! [`CHECK`] digits after the cut are not all 0 or all 9 (nor at a rounding
//! tie), which proves its digits are those of the exact value.

use super::mag;
use std::cmp::Ordering;
use std::sync::OnceLock;

/// Fixed-point digits beyond the longest table.
const EXTRA: u32 = 40;
/// Digits after a cut that must not all be 0 or all 9.
const CHECK: u32 = 30;
/// The longest table (ln 2).
const LONGEST: u32 = 1953;

/// A constant as the integer value × 10^`digits`.
pub(super) struct Table {
    pub(super) coef: Vec<u32>,
    pub(super) digits: u32,
}

pub(super) struct Tables {
    pub(super) ln10: Table,
    pub(super) ln10_inv: Table,
    pub(super) ln2: Table,
    pub(super) ln2_inv: Table,
    pub(super) pi: Table,
}

/// The tables, computed on first use.
pub(super) fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(compute)
}

fn compute() -> Tables {
    let n = LONGEST + EXTRA;
    let ln2 = mag::add(&atanh_inv(3, n), &atanh_inv(3, n));
    let ln10 = mag::add(
        &mag::mul_small(&ln2, 3),
        &mag::add(&atanh_inv(9, n), &atanh_inv(9, n)),
    );
    let pi = mag::sub(
        &mag::mul_small(&atan_inv(5, n), 16),
        &mag::mul_small(&atan_inv(239, n), 4),
    );
    // 1 / x at n digits: 10^(2n) / (x × 10^n).
    let inverse = |x: &[u32]| mag::divmod(&mag::pow10(2 * n), x).0;
    Tables {
        ln10: cut(&ln10, n, 801, false),
        ln10_inv: cut(&inverse(&ln10), n, 801, false),
        ln2: cut(&ln2, n, 1953, false),
        ln2_inv: cut(&inverse(&ln2), n, 801, true),
        pi: cut(&pi, n, 720, true),
    }
}

/// atanh(1/q) × 10^n = Σ 10^n / ((2k + 1) q^(2k+1)), each division
/// truncated.
fn atanh_inv(q: u32, n: u32) -> Vec<u32> {
    series(q, n, false)
}

/// atan(1/q) × 10^n, the same series with alternating signs.
fn atan_inv(q: u32, n: u32) -> Vec<u32> {
    series(q, n, true)
}

fn series(q: u32, n: u32, alternate: bool) -> Vec<u32> {
    let mut power = mag::divmod_small(&mag::pow10(n), q).0;
    let (mut plus, mut minus) = (power.clone(), Vec::new());
    let mut k = 1u32;
    loop {
        power = mag::divmod_small(&power, q * q).0;
        if power.is_empty() {
            break;
        }
        let term = mag::divmod_small(&power, 2 * k + 1).0;
        if alternate && k % 2 == 1 {
            minus = mag::add(&minus, &term);
        } else {
            plus = mag::add(&plus, &term);
        }
        k += 1;
    }
    mag::sub(&plus, &minus)
}

/// The value x / 10^n cut to `digits` fractional digits, truncated or
/// rounded half up. The approximation is off by less than one unit of the
/// CHECK digits after the cut, so those digits, unless they are all 0 or all
/// 9 (or straddle a rounding tie), prove the cut is that of the exact value.
fn cut(x: &[u32], n: u32, digits: u32, rounded: bool) -> Table {
    let unit = mag::pow10(CHECK);
    let window = mag::shr_digits(x, n - digits - CHECK).0;
    let (coef, rest) = mag::divmod(&window, &unit);
    let near = |v: &[u32]| rest == v || mag::add(&rest, &[1]) == v;
    assert!(
        !rest.is_empty() && !near(&unit),
        "constant digits {digits}: the series is too close to a cut"
    );
    let half = mag::mul_small(&mag::pow10(CHECK - 1), 5);
    let coef = if !rounded {
        coef
    } else {
        assert!(!near(&half), "constant digits {digits}: a rounding tie");
        if mag::cmp(&rest, &half) == Ordering::Less {
            coef
        } else {
            mag::add(&coef, &[1])
        }
    };
    Table { coef, digits }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(t: &Table, n: usize) -> String {
        let digits = mag::to_digits(&t.coef);
        digits[..n].to_string()
    }

    #[test]
    fn tables_start_with_the_known_digits() {
        let t = tables();
        assert_eq!(
            prefix(&t.ln10, 40),
            "2302585092994045684017991454684364207601"
        );
        assert_eq!(
            prefix(&t.ln10_inv, 40),
            "4342944819032518276511289189166050822943"
        );
        assert_eq!(
            prefix(&t.ln2, 40),
            "6931471805599453094172321214581765680755"
        );
        assert_eq!(
            prefix(&t.ln2_inv, 40),
            "1442695040888963407359924681001892137426"
        );
        assert_eq!(
            prefix(&t.pi, 40),
            "3141592653589793238462643383279502884197"
        );
        // Digit counts: the integer digit plus the fractional ones (1/ln 10
        // and ln 2 have none, their tables start after the point).
        assert_eq!(mag::digit_count(&t.ln10.coef), 802);
        assert_eq!(mag::digit_count(&t.ln10_inv.coef), 801);
        assert_eq!(mag::digit_count(&t.ln2.coef), 1953);
        assert_eq!(mag::digit_count(&t.ln2_inv.coef), 802);
        assert_eq!(mag::digit_count(&t.pi.coef), 721);
    }
}
