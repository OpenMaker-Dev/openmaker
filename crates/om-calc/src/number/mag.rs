//! Unsigned integers in base 10^9 words, least significant word first: the
//! word layout of `Draco::FixPt` (spec `docs/re/calc-values.md` 2.1). Every
//! function takes and returns trimmed vectors (no high zero words; zero is
//! the empty vector).

use std::cmp::Ordering;

/// The word base.
pub(crate) const BASE: u64 = 1_000_000_000;
/// Decimal digits per word.
pub(crate) const WORD_DIGITS: u32 = 9;

pub(crate) fn trim(mut v: Vec<u32>) -> Vec<u32> {
    while v.last() == Some(&0) {
        v.pop();
    }
    v
}

pub(crate) fn from_u64(mut n: u64) -> Vec<u32> {
    let mut v = Vec::new();
    while n > 0 {
        v.push((n % BASE) as u32);
        n /= BASE;
    }
    v
}

/// The value if it fits in a `u64`.
pub(crate) fn to_u64(v: &[u32]) -> Option<u64> {
    let mut n: u64 = 0;
    for &w in v.iter().rev() {
        n = n.checked_mul(BASE)?.checked_add(u64::from(w))?;
    }
    Some(n)
}

pub(crate) fn cmp(a: &[u32], b: &[u32]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().rev().cmp(b.iter().rev()))
}

pub(crate) fn add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(long.len() + 1);
    let mut carry = 0u64;
    for (i, &w) in long.iter().enumerate() {
        let sum = u64::from(w) + u64::from(short.get(i).copied().unwrap_or(0)) + carry;
        out.push((sum % BASE) as u32);
        carry = sum / BASE;
    }
    if carry > 0 {
        out.push(carry as u32);
    }
    out
}

/// `a - b`; `a` must not be less than `b`.
pub(crate) fn sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    debug_assert!(cmp(a, b) != Ordering::Less);
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for (i, &w) in a.iter().enumerate() {
        let mut d = i64::from(w) - i64::from(b.get(i).copied().unwrap_or(0)) - borrow;
        borrow = 0;
        if d < 0 {
            d += BASE as i64;
            borrow = 1;
        }
        out.push(d as u32);
    }
    trim(out)
}

pub(crate) fn mul_small(a: &[u32], m: u64) -> Vec<u32> {
    debug_assert!(m < BASE * 4);
    let mut out = Vec::with_capacity(a.len() + 2);
    let mut carry = 0u64;
    for &w in a {
        let p = u64::from(w) * m + carry;
        out.push((p % BASE) as u32);
        carry = p / BASE;
    }
    while carry > 0 {
        out.push((carry % BASE) as u32);
        carry /= BASE;
    }
    trim(out)
}

/// The full product.
pub(crate) fn mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut acc = vec![0u64; a.len() + b.len() + 1];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &y) in b.iter().enumerate() {
            let t = acc[i + j] + u64::from(x) * u64::from(y) + carry;
            acc[i + j] = t % BASE;
            carry = t / BASE;
        }
        let mut k = i + b.len();
        while carry > 0 {
            let t = acc[k] + carry;
            acc[k] = t % BASE;
            carry = t / BASE;
            k += 1;
        }
    }
    trim(acc.into_iter().map(|w| w as u32).collect())
}

/// Quotient and remainder by a single word.
pub(crate) fn divmod_small(a: &[u32], d: u32) -> (Vec<u32>, u32) {
    debug_assert!(d > 0);
    let mut q = vec![0u32; a.len()];
    let mut rem = 0u64;
    for i in (0..a.len()).rev() {
        let cur = rem * BASE + u64::from(a[i]);
        q[i] = (cur / u64::from(d)) as u32;
        rem = cur % u64::from(d);
    }
    (trim(q), rem as u32)
}

/// Quotient and remainder (Knuth's algorithm D in base 10^9).
pub(crate) fn divmod(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    assert!(!b.is_empty(), "division by zero magnitude");
    if cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = divmod_small(a, b[0]);
        return (q, from_u64(u64::from(r)));
    }
    // Normalize so the divisor's top word is at least BASE / 2.
    let factor = BASE / (u64::from(*b.last().unwrap()) + 1);
    let u_norm = mul_small(a, factor);
    let v = mul_small(b, factor);
    let n = v.len();
    let mut u = u_norm;
    u.resize(a.len() + 1, 0);
    let m = u.len() - n;
    let mut q = vec![0u32; m];
    let v_top = u64::from(v[n - 1]);
    let v_next = u64::from(v[n - 2]);
    for j in (0..m).rev() {
        let top = u64::from(u[j + n]) * BASE + u64::from(u[j + n - 1]);
        let mut qhat = top / v_top;
        let mut rhat = top % v_top;
        while qhat >= BASE || qhat * v_next > rhat * BASE + u64::from(u[j + n - 2]) {
            qhat -= 1;
            rhat += v_top;
            if rhat >= BASE {
                break;
            }
        }
        // u[j..=j+n] -= qhat * v
        let mut borrow = 0i64;
        let mut carry = 0u64;
        for i in 0..n {
            let p = qhat * u64::from(v[i]) + carry;
            carry = p / BASE;
            let t = i64::from(u[i + j]) - (p % BASE) as i64 - borrow;
            if t < 0 {
                u[i + j] = (t + BASE as i64) as u32;
                borrow = 1;
            } else {
                u[i + j] = t as u32;
                borrow = 0;
            }
        }
        let t = i64::from(u[j + n]) - carry as i64 - borrow;
        if t < 0 {
            u[j + n] = (t + BASE as i64) as u32;
            // qhat was one too large: add v back.
            qhat -= 1;
            let mut c = 0u64;
            for i in 0..n {
                let s = u64::from(u[i + j]) + u64::from(v[i]) + c;
                u[i + j] = (s % BASE) as u32;
                c = s / BASE;
            }
            u[j + n] = ((u64::from(u[j + n]) + c) % BASE) as u32;
        } else {
            u[j + n] = t as u32;
        }
        q[j] = qhat as u32;
    }
    u.truncate(n);
    let (r, rest) = divmod_small(&trim(u), factor as u32);
    debug_assert_eq!(rest, 0);
    (trim(q), r)
}

/// `10^k`.
pub(crate) fn pow10(k: u32) -> Vec<u32> {
    let mut v = vec![0u32; (k / WORD_DIGITS) as usize];
    v.push(10u32.pow(k % WORD_DIGITS));
    v
}

/// `a * 10^k`.
pub(crate) fn shl_digits(a: &[u32], k: u32) -> Vec<u32> {
    if a.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; (k / WORD_DIGITS) as usize];
    out.extend_from_slice(a);
    mul_small(&out, u64::from(10u32.pow(k % WORD_DIGITS)))
}

/// `a / 10^k` truncated, the first dropped digit, and whether any digit after
/// that one was non-zero.
pub(crate) fn shr_digits(a: &[u32], k: u32) -> (Vec<u32>, u32, bool) {
    if k == 0 {
        return (a.to_vec(), 0, false);
    }
    let words = (k / WORD_DIGITS) as usize;
    let digits = k % WORD_DIGITS;
    let low = &a[..words.min(a.len())];
    let high: Vec<u32> = a.get(words..).map(<[u32]>::to_vec).unwrap_or_default();
    if digits == 0 {
        // The first dropped digit is the top digit of the highest dropped word.
        let (top, rest) = match low.split_last() {
            Some((top, rest)) => (*top, rest),
            None => (0, &[][..]),
        };
        let sticky = top % 100_000_000 != 0 || rest.iter().any(|&w| w != 0);
        (high, top / 100_000_000, sticky)
    } else {
        let div = 10u32.pow(digits);
        let (q, r) = divmod_small(&high, div);
        let sticky = r % (div / 10) != 0 || low.iter().any(|&w| w != 0);
        (q, r / (div / 10), sticky)
    }
}

/// Number of decimal digits (0 for zero).
pub(crate) fn digit_count(a: &[u32]) -> u32 {
    match a.last() {
        None => 0,
        Some(&top) => (a.len() as u32 - 1) * WORD_DIGITS + top.ilog10() + 1,
    }
}

/// Decimal digits, most significant first ("0" for zero).
pub(crate) fn to_digits(a: &[u32]) -> String {
    match a.split_last() {
        None => "0".to_string(),
        Some((top, rest)) => {
            let mut s = top.to_string();
            for w in rest.iter().rev() {
                s.push_str(&format!("{w:09}"));
            }
            s
        }
    }
}

/// From ASCII decimal digits.
pub(crate) fn from_digits(digits: &[u8]) -> Vec<u32> {
    let mut v = Vec::with_capacity(digits.len() / 9 + 1);
    let mut end = digits.len();
    while end > 0 {
        let start = end.saturating_sub(WORD_DIGITS as usize);
        let mut w = 0u32;
        for &d in &digits[start..end] {
            w = w * 10 + u32::from(d - b'0');
        }
        v.push(w);
        end = start;
    }
    trim(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Vec<u32> {
        from_digits(s.as_bytes())
    }

    #[test]
    fn digits_round_trip() {
        assert_eq!(to_digits(&n("0")), "0");
        assert_eq!(to_digits(&n("000123")), "123");
        let big = "123456789012345678901234567890";
        assert_eq!(to_digits(&n(big)), big);
        assert_eq!(digit_count(&n(big)), 30);
        assert_eq!(digit_count(&n("1000000000")), 10);
    }

    #[test]
    fn arithmetic() {
        let a = n("999999999999999999999");
        let b = n("1");
        assert_eq!(to_digits(&add(&a, &b)), "1000000000000000000000");
        assert_eq!(to_digits(&sub(&add(&a, &b), &b)), to_digits(&a));
        assert_eq!(
            to_digits(&mul(&n("123456789012345678"), &n("987654321098765432"))),
            "121932631137021794322511812221002896"
        );
        let (q, r) = divmod(
            &n("121932631137021794322511812221002897"),
            &n("987654321098765432"),
        );
        assert_eq!(to_digits(&q), "123456789012345678");
        assert_eq!(to_digits(&r), "1");
        let (q, r) = divmod(
            &n("1000000000000000000000000000000"),
            &n("3000000000000000007"),
        );
        assert_eq!(to_digits(&q), "333333333333");
        assert_eq!(to_digits(&r), "999997666666666669");
    }

    #[test]
    fn decimal_shifts() {
        assert_eq!(to_digits(&shl_digits(&n("123"), 10)), "1230000000000");
        let (q, first, sticky) = shr_digits(&n("123456789012"), 4);
        assert_eq!(
            (to_digits(&q).as_str(), first, sticky),
            ("12345678", 9, true)
        );
        let (q, first, sticky) = shr_digits(&n("1500000000000000000"), 18);
        assert_eq!((to_digits(&q).as_str(), first, sticky), ("1", 5, false));
        let (q, first, sticky) = shr_digits(&n("1000000001"), 9);
        assert_eq!((to_digits(&q).as_str(), first, sticky), ("1", 0, true));
    }
}
