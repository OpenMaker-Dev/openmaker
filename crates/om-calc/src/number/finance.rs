//! `FV`, `PV`, `PMT` and `NPV` (spec `docs/re/calc-values.md` 14.6).
//!
//! `FV(pmt; r; n)`, `PMT` and `PV` evaluate into a result of the payment's
//! precision and guard digits (the evaluation precision when that is
//! larger; the rate's and the periods' precisions do not count), widened
//! four digits: the power (1 + r)^±n by the rules of `^` at that width
//! (so p + 14 digits inside it), then the subtraction, the product with the
//! payment and the division by r at p + 4, and the result rounded at its
//! held digits. `NPV` sums v / (1 + r)^k over the payments, each power and
//! quotient at the payment's precision.

use super::fixpt::{add3, add_into, div, mul, sub3, Fx};
use super::transcend::power_fx;
use super::{fixpt, Number};
use crate::error::CalcError;

impl Number {
    /// `FV(payment; rate; periods)`: payment ((1 + r)^n - 1) / r, or
    /// payment n when r is 0. Empty counts as 0 and `?` gives `?`. Error 14
    /// when (1 + r)^n reaches 10^405 or a negative 1 + r has a fractional
    /// n.
    pub fn future_value(
        payment: &Number,
        rate: &Number,
        periods: &Number,
        precision: u16,
    ) -> Result<Number, CalcError> {
        annuity(Annuity::Future, payment, rate, periods, precision)
    }

    /// `PV(payment; rate; periods)`: payment (1 - (1 + r)^-n) / r, or
    /// payment n when r is 0. Error 15 when (1 + r)^n vanishes at the
    /// working digits (`PV(100;-.5;500)`).
    pub fn present_value(
        payment: &Number,
        rate: &Number,
        periods: &Number,
        precision: u16,
    ) -> Result<Number, CalcError> {
        annuity(Annuity::Present, payment, rate, periods, precision)
    }

    /// `PMT(principal; rate; term)`: principal r / (1 - (1 + r)^-n), or
    /// principal / n when r is 0; error 15 when the divisor is 0
    /// (`PMT(1000;.1;0)`, `PMT(1000;0;0)`).
    pub fn payment(
        principal: &Number,
        rate: &Number,
        term: &Number,
        precision: u16,
    ) -> Result<Number, CalcError> {
        annuity(Annuity::Payment, principal, rate, term, precision)
    }

    /// `NPV(payments; rate)`: the sum of payment k / (1 + r)^k for k from
    /// 1, each power and quotient at the payment's precision and guard
    /// digits (the evaluation precision when that is larger), the sum
    /// exact. Payments must be numbers (the caller leaves out empty and
    /// digitless values); a `?` payment or rate gives `?`; no payment gives
    /// 0. An empty rate is 0 and 1 + r = 0 is error 15.
    pub fn net_present_value(
        payments: &[Number],
        rate: &Number,
        precision: u16,
    ) -> Result<Number, CalcError> {
        let Some(mut base) = rate.fx_operand(precision) else {
            return Ok(Number::nan());
        };
        add_into(&mut base, &Fx::int(1));
        let mut sum = Fx::int(0);
        let mut k = Fx::int(0);
        for payment in payments {
            let Some(v) = payment.fx_operand(precision) else {
                return Ok(Number::nan());
            };
            add_into(&mut k, &Fx::int(1));
            let power = power_fx(&base, &k, v.disp, v.extra)?;
            let term = div(&v, &power, &Fx::int_at(0, v.disp, v.extra))?;
            add_into(&mut sum, &term);
        }
        Ok(Number::from_fx(sum))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Annuity {
    Future,
    Present,
    Payment,
}

fn annuity(
    kind: Annuity,
    amount: &Number,
    rate: &Number,
    periods: &Number,
    precision: u16,
) -> Result<Number, CalcError> {
    let (Some(amount), Some(rate), Some(n)) = (
        amount.fx_operand(precision),
        rate.fx_operand(precision),
        periods.fx_operand(precision),
    ) else {
        return Ok(Number::nan());
    };
    let p = amount.disp;
    let mut result = Fx::int_at(0, p, amount.extra);
    result.widen(12, 796, 4);
    let like = Fx::like(0, &result);
    result = match kind {
        Annuity::Future if rate.is_zero() => mul(&amount, &n, &result),
        Annuity::Future => {
            let base = add3(&rate, &Fx::int(1));
            let power = power_fx(&base, &n, like.disp, like.extra)?;
            let t = sub3(&power, &Fx::int(1), &like);
            let t = mul(&amount, &t, &like);
            div(&t, &rate, &result)?
        }
        Annuity::Present | Annuity::Payment => {
            // A(r, n) = (1 - (1 + r)^-n) / r, or n when r is 0.
            let a = if rate.is_zero() {
                n
            } else {
                let base = add3(&rate, &Fx::int(1));
                let power = power_fx(&base, &n.negated(), like.disp, like.extra)?;
                let t = sub3(&Fx::int(1), &power, &Fx::int(0));
                div(&t, &rate, &like)?
            };
            if kind == Annuity::Present {
                mul(&amount, &a, &result)
            } else {
                div(&amount, &a, &result)?
            }
        }
    };
    result.round(u32::from(result.extra) + u32::from(p));
    result.set_disp(fixpt::clamp_p(i64::from(p)));
    Ok(Number::from_fx(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> Number {
        match s.strip_prefix('-') {
            Some(rest) => Number::from_literal(rest, '.').unwrap().neg(),
            None => Number::from_literal(s, '.').unwrap(),
        }
    }

    fn held_digits(n: &Number) -> String {
        n.mul(&lit("1000000000000000000"), 16).to_text_with('.')
    }

    #[test]
    fn annuities_follow_section_14() {
        let r = lit(".11").div(&lit("12"), 16).unwrap();
        let fv = Number::future_value(&lit("50"), &r, &lit("60"), 16).unwrap();
        // The exact value is ...008506572; FileMaker holds ...008506.
        assert_eq!(held_digits(&fv), "3975903984291350008506");
        let r = lit(".05").div(&lit("12"), 16).unwrap();
        let pmt = Number::payment(&lit("1000"), &r, &lit("12"), 16).unwrap();
        assert_eq!(pmt.to_text_with('.'), "85.6074817884671147");
        let pv = Number::present_value(&lit("500"), &lit(".05"), &lit("5"), 16).unwrap();
        assert_eq!(held_digits(&pv), "2164738335315409682572");
    }

    #[test]
    fn annuity_edges() {
        let zero = lit("0");
        let pmt = Number::payment(&lit("1000"), &zero, &lit("12"), 16).unwrap();
        assert_eq!(pmt.to_text_with('.'), "83.3333333333333333");
        assert_eq!(
            Number::payment(&lit("100"), &lit(".1"), &zero, 16).unwrap_err(),
            CalcError::DIVIDE_BY_ZERO
        );
        assert_eq!(
            Number::present_value(&lit("100"), &lit("-.5"), &lit("500"), 16).unwrap_err(),
            CalcError::DIVIDE_BY_ZERO
        );
        assert_eq!(
            Number::future_value(&lit("100"), &lit("1"), &lit("2000"), 16).unwrap_err(),
            CalcError::OUT_OF_RANGE
        );
        let pv = Number::present_value(&lit("100"), &lit("1"), &lit("2000"), 16).unwrap();
        assert_eq!(pv.to_text_with('.'), "100");
        // The payment's precision counts, the rate's does not.
        let pmt =
            Number::payment(&lit("1.0000000000000000001"), &lit(".1"), &lit("3"), 16).unwrap();
        assert_eq!(pmt.to_text_with('.'), ".4021148036253776435");
        let pmt = Number::payment(&lit("1"), &lit(".1000000000000000000"), &lit("3"), 16).unwrap();
        assert_eq!(pmt.to_text_with('.'), ".4021148036253776");
    }

    #[test]
    fn net_present_value() {
        let ids: Vec<Number> = [7, 1, 3, 12, 5, 6, 8, 11, 9, 2, 10, 4]
            .iter()
            .map(|&v| Number::from_i64(v))
            .collect();
        let npv = |rate: &Number| Number::net_present_value(&ids, rate, 16).unwrap();
        let third = lit("1").div(&lit("3"), 16).unwrap();
        assert_eq!(held_digits(&npv(&third)), "16635953664779663103");
        assert_eq!(npv(&lit(".1")).to_text_with('.'), "42.7359122453516229");
        assert_eq!(npv(&Number::empty()).to_text_with('.'), "78");
        assert_eq!(npv(&lit("-2")).to_text_with('.'), "-6");
        assert_eq!(
            Number::net_present_value(&ids, &lit("-1"), 16).unwrap_err(),
            CalcError::DIVIDE_BY_ZERO
        );
        assert_eq!(
            Number::net_present_value(&[], &lit(".1"), 16)
                .unwrap()
                .to_text_with('.'),
            "0"
        );
        // Claris examples.
        let loan: Vec<Number> = [-2000, 600, 300, 500, 700, 400]
            .iter()
            .map(|&v| Number::from_i64(v))
            .collect();
        let npv = Number::net_present_value(&loan, &lit(".05"), 16).unwrap();
        assert_eq!(npv.to_text_with('.'), "156.9127744508813684");
    }
}
