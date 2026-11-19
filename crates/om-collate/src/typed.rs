//! Index keys of number, date, time and timestamp values (spec
//! `docs/re/query-program.md` 7.3), from the text FileMaker stores for them.
//!
//! Each form makes byte order equal value order. Only the forms the spec
//! establishes are built: non-negative integers, whole seconds, and dates
//! with a four-digit year in the order the caller names. Anything else
//! returns `None`.

/// First byte of a number key.
const NUMBER: u8 = 0x80;
/// Bit 23 of a date key's three bytes.
const DATE: u32 = 0x80_0000;

/// The key of a number: `80`, a u8 count of integer digits, then the digits
/// as packed nibbles each holding `digit + 1`, the last byte padded with a
/// zero nibble. `0` is `80 00`. Only non-negative integers are supported.
pub fn number_key(text: &str) -> Option<Vec<u8>> {
    let digits = text.trim();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits = digits.trim_start_matches('0').as_bytes();
    let count = u8::try_from(digits.len()).ok()?;
    let mut key = vec![NUMBER, count];
    for pair in digits.chunks(2) {
        let high = pair[0] - b'0' + 1;
        let low = pair.get(1).map_or(0, |d| d - b'0' + 1);
        key.push(high << 4 | low);
    }
    Some(key)
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

/// Seconds since midnight of a time stored as `h:mm[:ss]` with an optional
/// `AM`/`PM`, or 24-hour.
fn parse_time(text: &str) -> Option<u32> {
    let text = text.trim();
    let (clock, meridiem) = match text.rsplit_once(' ') {
        Some((clock, m)) if m.eq_ignore_ascii_case("AM") => (clock, Some(false)),
        Some((clock, m)) if m.eq_ignore_ascii_case("PM") => (clock, Some(true)),
        _ => (text, None),
    };
    let mut parts = clock.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = match parts.next() {
        Some(s) => s.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() || minute > 59 || second > 59 {
        return None;
    }
    let hour = match meridiem {
        Some(pm) if (1..=12).contains(&hour) => hour % 12 + if pm { 12 } else { 0 },
        Some(_) => return None,
        None if hour <= 23 => hour,
        None => return None,
    };
    Some(hour * 3600 + minute * 60 + second)
}

/// The key of a date stored in `order`: its three bytes, then the time key
/// of midnight (`80 00`).
pub fn date_key(text: &str, order: DateOrder) -> Option<Vec<u8>> {
    let mut key = parse_date(text, order)?.to_vec();
    key.extend([NUMBER, 0]);
    Some(key)
}

/// The key of a time: the number key of its seconds since midnight.
pub fn time_key(text: &str) -> Option<Vec<u8>> {
    number_key(&parse_time(text)?.to_string())
}

/// The key of a timestamp whose date is stored in `order`: the date's three
/// bytes, then the time's key.
pub fn timestamp_key(text: &str, order: DateOrder) -> Option<Vec<u8>> {
    let (date, time) = text.trim().split_once(' ')?;
    let mut key = parse_date(date, order)?.to_vec();
    key.extend(time_key(time)?);
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
        assert_eq!(number_key("-3"), None);
        assert_eq!(number_key("1.5"), None);
        assert_eq!(number_key(""), None);
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
        assert_eq!(hex(&time_key("9:26:57 PM").unwrap()), "8005883280");
        assert_eq!(hex(&time_key("21:26:57").unwrap()), "8005883280");
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
    }
}
