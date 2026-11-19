//! The variance behind `Variance`, `VarianceP`, `StDev` and `StDevP` (spec
//! `docs/re/calc-functions.md` 5.7 and the aggregate oracle answers at the
//! end of its section 9).

use super::{Fin, Number, DEFAULT_PRECISION, GUARD};
use crate::error::CalcError;

impl Number {
    /// The sample (`population` false) or population variance of finite
    /// values: the exact sum of squared deviations from the exact mean over
    /// n - 1 or n, rounded half up at `precision` + 2 digits, at exactly
    /// `precision` (the values' own precisions do not count, though all
    /// their digits do: `Variance(1/3;2/3)*10^18` = 55555555555555556).
    /// Error 15 for a single value, for the population variance too
    /// (`VarianceP(5;"")`). Empty and `?` values must be left out by the
    /// caller; `None` when none remain.
    pub fn variance(
        values: &[Number],
        population: bool,
        precision: u16,
    ) -> Option<Result<Number, CalcError>> {
        let values: Vec<&Fin> = values.iter().filter_map(Number::fin).collect();
        let n = values.len() as i64;
        if n == 0 {
            return None;
        }
        if n == 1 {
            return Some(Err(CalcError::DIVIDE_BY_ZERO));
        }
        // n Σx² - (Σx)² over n (n - 1) or n², all exact until the division.
        let p = DEFAULT_PRECISION;
        let zero = Fin::zero(p);
        let sum = values.iter().fold(zero.clone(), |s, x| s.add_exact(x, p));
        let squares = values
            .iter()
            .fold(zero, |s, x| s.add_exact(&x.mul_exact(x, p), p));
        let count = Fin::from_i64(n, p);
        let spread = count
            .mul_exact(&squares, p)
            .add_exact(&sum.mul_exact(&sum, p).negated(), p);
        let divisor = Fin::from_i64(n * if population { n } else { n - 1 }, p);
        let variance = spread.div_rounded(&divisor, u32::from(precision) + GUARD, precision);
        Some(Ok(Number::checked(variance)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> Number {
        Number::from_literal(s, '.').unwrap()
    }

    fn held(n: &Number) -> String {
        n.mul(&lit("1000000000000000000"), 16).to_text_with('.')
    }

    #[test]
    fn exact_deviations_rounded_once() {
        let third = lit("1").div(&lit("3"), 16).unwrap();
        let two_thirds = lit("2").div(&lit("3"), 16).unwrap();
        let both = [third, two_thirds];
        let sample = Number::variance(&both, false, 16).unwrap().unwrap();
        assert_eq!(held(&sample), "55555555555555556");
        let population = Number::variance(&both, true, 16).unwrap().unwrap();
        assert_eq!(held(&population), "27777777777777778");
        let small = [lit("1"), lit("2"), lit("4")];
        let p = Number::variance(&small, true, 16).unwrap().unwrap();
        assert_eq!(held(&p), "1555555555555555556");
        // The values' precision does not carry over.
        let wide = [lit("1.0000000000000000001"), lit("2")];
        let v = Number::variance(&wide, false, 16).unwrap().unwrap();
        assert_eq!(v.to_text_with('.'), ".5");
        assert!(Number::variance(&[], false, 16).is_none());
        assert_eq!(
            Number::variance(&[lit("5")], true, 16)
                .unwrap()
                .unwrap_err(),
            CalcError::DIVIDE_BY_ZERO
        );
    }
}
