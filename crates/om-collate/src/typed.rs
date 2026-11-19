//! Index keys of number, date, time and timestamp values (spec
//! `docs/re/query-program.md` 7.3), from the text FileMaker stores for them.
//!
//! Each form makes byte order equal value order. The forms are FileMaker's:
//! decimal numbers of either sign, dates with a four-digit year in the order
//! the caller names, times of either sign with fractions of a second, and
//! timestamps. Text these functions do not read returns `None`; which of it
//! FileMaker reads as a value (digits among other text, spec 7.3) and which
//! it lists as invalid is the caller's to decide.

/// Bit 23 of a date key's three bytes.
const DATE: u32 = 0x80_0000;

/// The most digits a number key holds (spec 7.3, from bin).
const MAX_DIGITS: usize = 200;

/// The key of a decimal number stored as an optional `-` (or U+2212), digits
/// and at most one `.` (spec 7.3): a big-endian u16 head, then one nibble
/// per digit, integer digits first, without leading integer zeros or
/// trailing fraction zeros, at most 200 of them.
///
/// - Zero (`0`, `-0`, `0.0`) is `80 00`.
/// - A positive number's head is 0x8000 + the count of integer digits kept;
///   each digit is `digit + 1`, the last byte padded with a `0` nibble
///   (`1` = `80 01 20`, `0.5` = `80 00 60`).
/// - A negative number's head is 0x7FFF - that count; each digit is
///   `14 - digit`, then an `f` nibble, the last byte padded with `f`
///   (`-1` = `7f fe df`, `-12` = `7f fd dc ff`), so a longer magnitude
///   sorts first.
pub fn number_key(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    let (negative, magnitude) = match text
        .strip_prefix('-')
        .or_else(|| text.strip_prefix('\u{2212}'))
    {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (integer, fraction) = magnitude.split_once('.').unwrap_or((magnitude, ""));
    if integer.is_empty() && fraction.is_empty()
        || !integer
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some(decimal_key(
        negative,
        integer.trim_start_matches('0'),
        fraction.trim_end_matches('0'),
    ))
}

/// The number key of the decimal `integer.fraction` (ASCII digits, the
/// integer without leading zeros, the fraction without trailing zeros).
fn decimal_key(negative: bool, integer: &str, fraction: &str) -> Vec<u8> {
    let digits: Vec<u8> = integer
        .bytes()
        .chain(fraction.bytes())
        .take(MAX_DIGITS)
        .map(|b| b - b'0')
        .collect();
    // Zero by the whole number, not by the digits kept: `0.` and 200 zeros
    // then `1` keys its 200 zero digits.
    if integer.bytes().chain(fraction.bytes()).all(|b| b == b'0') {
        return vec![0x80, 0x00];
    }
    // At most 200, so both heads fit.
    let count = integer.len().min(MAX_DIGITS) as u16;
    if !negative {
        let mut key = (0x8000 + count).to_be_bytes().to_vec();
        for pair in digits.chunks(2) {
            key.push((pair[0] + 1) << 4 | pair.get(1).map_or(0, |d| d + 1));
        }
        return key;
    }
    let mut key = (0x7FFF - count).to_be_bytes().to_vec();
    let mut nibbles: Vec<u8> = digits.iter().map(|d| 14 - d).collect();
    nibbles.push(0xF);
    for pair in nibbles.chunks(2) {
        key.push(pair[0] << 4 | pair.get(1).copied().unwrap_or(0xF));
    }
    key
}

/// The three bytes of a date: `0x800000 | year << 10 | month << 6 | day`.
fn date_bytes(year: u32, month: u32, day: u32) -> Option<[u8; 3]> {
    if !(1..=4000).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let value = DATE | year << 10 | month << 6 | day;
    let bytes = value.to_be_bytes();
    Some([bytes[1], bytes[2], bytes[3]])
}

/// The order of a stored date's parts, a property of the file's locale:
/// test project and the English and Spanish samples store
/// `month/day/year`, the French, Italian and German samples `day/month/year`,
/// the Japanese `year/month/day`. Where a file records its order is not yet
/// known.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DateOrder {
    #[default]
    MonthDayYear,
    DayMonthYear,
    YearMonthDay,
}

/// A date stored as three numbers separated by `/`, `.` or `-` in `order`,
/// with a four-digit year.
fn parse_date(text: &str, order: DateOrder) -> Option<[u8; 3]> {
    let parts: Vec<&str> = text.trim().split(['/', '.', '-']).collect();
    let [a, b, c] = parts.as_slice() else {
        return None;
    };
    let (year, month, day) = match order {
        DateOrder::MonthDayYear => (c, a, b),
        DateOrder::DayMonthYear => (c, b, a),
        DateOrder::YearMonthDay => (a, b, c),
    };
    if year.len() != 4 {
        return None;
    }
    date_bytes(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?)
}

/// A time stored as `[-]h:mm[:ss[.fraction]]`, 24-hour or with `AM`/`PM`:
/// whether it is negative, its whole seconds since midnight and the digits
/// of its fraction of a second. `max_hour` bounds a 24-hour time's hours (a
/// time field holds any number of hours; a timestamp's time a day's).
fn parse_time(text: &str, max_hour: u64) -> Option<(bool, u64, &str)> {
    let text = text.trim();
    let (clock, meridiem) = match text.rsplit_once(' ') {
        Some((clock, m)) if m.eq_ignore_ascii_case("AM") => (clock.trim_end(), Some(false)),
        Some((clock, m)) if m.eq_ignore_ascii_case("PM") => (clock.trim_end(), Some(true)),
        _ => (text, None),
    };
    let (negative, clock) = match clock.strip_prefix('-') {
        Some(rest) if meridiem.is_none() => (true, rest),
        Some(_) => return None,
        None => (false, clock),
    };
    let number = |part: &str| -> Option<u64> {
        // Up to 12 digits keeps every sum below 2^64.
        (!part.is_empty() && part.len() <= 12 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    let mut parts = clock.split(':');
    let hour = number(parts.next()?)?;
    let minute = number(parts.next()?)?;
    let (second, fraction) = match parts.next() {
        Some(s) => {
            let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
            if !fraction.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (number(whole)?, fraction)
        }
        None => (0, ""),
    };
    if parts.next().is_some() || minute > 59 || second > 59 {
        return None;
    }
    let hour = match meridiem {
        Some(pm) if (1..=12).contains(&hour) => hour % 12 + if pm { 12 } else { 0 },
        Some(_) => return None,
        None if hour <= max_hour => hour,
        None => return None,
    };
    Some((negative, hour * 3600 + minute * 60 + second, fraction))
}

/// The number key of a time's seconds since midnight, fraction included.
fn seconds_key((negative, seconds, fraction): (bool, u64, &str)) -> Vec<u8> {
    let integer = if seconds == 0 {
        String::new()
    } else {
        seconds.to_string()
    };
    decimal_key(negative, &integer, fraction.trim_end_matches('0'))
}

/// The key of a date stored in `order`: its three bytes, then the time key
/// of midnight (`80 00`).
pub fn date_key(text: &str, order: DateOrder) -> Option<Vec<u8>> {
    let mut key = parse_date(text, order)?.to_vec();
    key.extend([0x80, 0x00]);
    Some(key)
}

/// The key of a time (spec 7.3): the date part of year 0, `80 00 00`, then
/// the number key of its seconds since midnight, fraction and sign included
/// (`21:26:57.5` = `80 00 00 80 05 88 32 86`, `-01:00:00` = `80 00 00 7f fb
/// b8 ee ff`).
pub fn time_key(text: &str) -> Option<Vec<u8>> {
    let mut key = vec![0x80, 0x00, 0x00];
    key.extend(seconds_key(parse_time(text, u64::MAX)?));
    Some(key)
}

/// The key of a timestamp whose date is stored in `order`: the date's three
/// bytes, then the number key of its time's seconds since midnight, a
/// fraction included. A negative time or one past the day is not read.
pub fn timestamp_key(text: &str, order: DateOrder) -> Option<Vec<u8>> {
    let (date, time) = text.trim().split_once(' ')?;
    let time = parse_time(time, 23).filter(|(negative, _, _)| !negative)?;
    let mut key = parse_date(date, order)?.to_vec();
    key.extend(seconds_key(time));
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn numbers_match_the_spec_examples() {
        for (value, key) in [
            ("0", "8000"),
            ("1", "800120"),
            ("2", "800130"),
            ("9", "8001a0"),
            ("10", "800221"),
            ("12", "800223"),
            ("250000", "8006361111"),
            ("698047", "80067a9158"),
            ("4000002", "800751111130"),
            ("007", "800180"),
        ] {
            assert_eq!(hex(&number_key(value).unwrap()), key, "{value}");
        }
        // The 200-digit cut keeps a small fraction's leading zeros.
        let tiny = format!("0.{}1", "0".repeat(200));
        assert_eq!(
            number_key(&tiny).unwrap(),
            [&[0x80, 0][..], &[0x11; 100]].concat()
        );
        // The probe's other numbers are in catalogs/index-key-vectors.tsv,
        // checked in om-query; text these functions do not read:
        for value in ["", "-", ".", "abc", "1.2.3", "1e3", "+5"] {
            assert_eq!(number_key(value), None, "{value}");
        }
    }

    #[test]
    fn dates_times_and_timestamps_match_the_spec_examples() {
        let us = DateOrder::MonthDayYear;
        assert_eq!(hex(&date_key("2/17/2014", us).unwrap()), "9f78918000");
        assert_eq!(hex(&date_key("8/2/2015", us).unwrap()), "9f7e028000");
        assert_eq!(hex(&date_key("10/9/2015", us).unwrap()), "9f7e898000");
        assert_eq!(
            date_key("17/02/2014", DateOrder::DayMonthYear),
            date_key("2/17/2014", us)
        );
        assert_eq!(
            date_key("2014/02/17", DateOrder::YearMonthDay),
            date_key("2/17/2014", us)
        );
        assert_eq!(hex(&time_key("9:26:57 PM").unwrap()), "8000008005883280");
        assert_eq!(hex(&time_key("21:26:57").unwrap()), "8000008005883280");
        // test project's CreationTimestamp values (live:proj_ts).
        assert_eq!(
            hex(&timestamp_key("10/27/2026 9:26:57 PM", us).unwrap()),
            "9faa9b8005883280"
        );
        assert_eq!(
            hex(&timestamp_key("10/28/2026 11:21:33 AM", us).unwrap()),
            "9faa9c8005519a40"
        );
        assert_eq!(date_key("2014-02-17", us), None);
        assert_eq!(time_key("13:00 PM"), None);
        assert_eq!(time_key("-1:00 PM"), None);
        assert_eq!(time_key("1:60"), None);
        assert_eq!(time_key(&format!("{}:00", "9".repeat(13))), None);
        assert_eq!(timestamp_key("10/27/2026 -1:00:00", us), None);
        assert_eq!(timestamp_key("10/27/2026 24:00:00", us), None);
    }
}
