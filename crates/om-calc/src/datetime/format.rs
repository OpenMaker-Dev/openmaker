//! Display of dates, times and timestamps (spec `docs/re/calc-values.md`
//! 4.5).
//!
//! Dates follow the locale's field order and padding with four-digit years,
//! and the separator they were typed with (`1-2-2003`), else the locale's.
//! Elapsed times are `h:mm:ss` with unbounded hours, padded per the locale
//! (`01:02:03` in de-DE); times of day use the locale's clock and omit zero
//! seconds (`1:05 PM`, `13:05`, `0:05` in ja-JP, ` 오후1:05` in ko-KR). Fractional seconds show as
//! many digits as needed, up to six, after the locale's decimal separator.

use super::{Date, Time, TimeStyle, Timestamp, MICROS_PER_SECOND};
use crate::locale::{DateOrder, Locale};

fn pad2(n: u32, pad: bool) -> String {
    if pad {
        format!("{n:02}")
    } else {
        n.to_string()
    }
}

/// `.5` for half a second: the fraction without trailing zeros, or empty.
fn fraction(micros: u128, decimal: char) -> String {
    if micros == 0 {
        return String::new();
    }
    let digits = format!("{micros:06}");
    format!("{decimal}{}", digits.trim_end_matches('0'))
}

impl Date {
    /// The display text in `locale`.
    pub fn format(self, locale: &Locale) -> String {
        let (y, m, d) = self.ymd();
        let sep = self.sep.unwrap_or(locale.date_separator);
        let year = format!("{y:04}");
        let month = pad2(m, locale.pad_month);
        let day = pad2(d, locale.pad_day);
        match locale.date_order {
            DateOrder::MonthDayYear => format!("{month}{sep}{day}{sep}{year}"),
            DateOrder::DayMonthYear => format!("{day}{sep}{month}{sep}{year}"),
            DateOrder::YearMonthDay => format!("{year}{sep}{month}{sep}{day}"),
        }
    }
}

/// An elapsed time: optional `-`, hours, minutes, seconds, fraction.
fn elapsed(micros: i128, locale: &Locale) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let total = micros.unsigned_abs();
    let seconds = total / MICROS_PER_SECOND as u128;
    let frac = total % MICROS_PER_SECOND as u128;
    // Hours are 32-bit: the one time with 2^31 of them, `Time(-2147483648;
    // 0;0)`, shows `-0:00:00`.
    let hours = seconds / 3600 % (1 << 31);
    let hours = if locale.pad_hour {
        format!("{hours:02}")
    } else {
        hours.to_string()
    };
    let sep = locale.time_separator;
    format!(
        "{sign}{hours}{sep}{:02}{sep}{:02}{}",
        seconds / 60 % 60,
        seconds % 60,
        fraction(frac, locale.decimal_separator)
    )
}

/// A time of day (0 <= micros < 24 h) on the locale's clock.
fn time_of_day(micros: i128, style: TimeStyle, locale: &Locale) -> String {
    let total = micros.rem_euclid(super::SECONDS_PER_DAY * MICROS_PER_SECOND) as u128;
    let seconds = total / MICROS_PER_SECOND as u128;
    let frac = total % MICROS_PER_SECOND as u128;
    let hour = (seconds / 3600) as u32;
    let minute = seconds / 60 % 60;
    let second = seconds % 60;
    let sep = locale.time_separator;
    let (shown_hour, suffix) = if locale.twelve_hour {
        let h12 = if hour % 12 == 0 { 12 } else { hour % 12 };
        let marker = if hour < 12 { &locale.am } else { &locale.pm };
        (h12, marker.as_str())
    } else {
        (hour, "")
    };
    let hour_text = pad2(shown_hour, locale.pad_hour);
    let mut out = if style == TimeStyle::HourOnly
        && locale.twelve_hour
        && minute == 0
        && second == 0
        && frac == 0
    {
        hour_text
    } else {
        let mut out = format!("{hour_text}{sep}{minute:02}");
        if second != 0 || frac != 0 {
            out.push_str(&format!(
                "{sep}{second:02}{}",
                fraction(frac, locale.decimal_separator)
            ));
        }
        out
    };
    // Korean puts the marker, with its own leading space, before the time
    // (file-metadata.md 2.2: ` 오후1:05:09`).
    if locale.am_pm_before {
        out.insert_str(0, suffix);
    } else {
        out.push_str(suffix);
    }
    out
}

impl Time {
    /// The display text in `locale`.
    pub fn format(self, locale: &Locale) -> String {
        match self.style {
            TimeStyle::Elapsed => elapsed(self.micros, locale),
            style => time_of_day(self.micros, style, locale),
        }
    }
}

impl Timestamp {
    /// The display text in `locale`: the date, a space, and the time of day
    /// (or the elapsed form for a time a declared result converted,
    /// `1/1/0001 1:00:00`).
    pub fn format(self, locale: &Locale) -> String {
        let date = self.date().format(locale);
        let time = self.time_of_day().micros();
        let time = match self.style {
            TimeStyle::Elapsed => elapsed(time, locale),
            style => time_of_day(time, style, locale),
        };
        format!("{date} {time}")
    }
}

#[cfg(test)]
mod tests {
    use crate::datetime::{Date, Time, TimeStyle, Timestamp};
    use crate::locale::Locale;
    use crate::number::Number;

    fn ts(y: i64, m: u32, d: u32, h: i64, min: i64, s: &str) -> Timestamp {
        let time = Time::from_hms(
            &Number::from_i64(h),
            &Number::from_i64(min),
            &Number::from_literal(s, '.').unwrap(),
        )
        .unwrap();
        Timestamp::from_date_time(Date::from_ymd(y, m, d).unwrap(), time).unwrap()
    }

    #[test]
    fn dates_by_locale() {
        let d = Date::from_ymd(2003, 1, 2).unwrap();
        assert_eq!(d.format(&Locale::en_us()), "1/2/2003");
        assert_eq!(d.format(&Locale::de_de()), "02.01.2003");
        assert_eq!(d.format(&Locale::fr_fr()), "02/01/2003");
        assert_eq!(d.format(&Locale::sv_se()), "2003-01-02");
        assert_eq!(d.format(&Locale::ja_jp()), "2003/01/02");
        assert_eq!(
            d.with_separator(Some('-')).format(&Locale::en_us()),
            "1-2-2003"
        );
        assert_eq!(
            Date::from_ymd(99, 1, 1).unwrap().format(&Locale::en_us()),
            "1/1/0099"
        );
    }

    #[test]
    fn times_by_locale() {
        let t = |secs: &str| Time::from_seconds(&Number::from_literal(secs, '.').unwrap()).unwrap();
        assert_eq!(t("3723").format(&Locale::en_us()), "1:02:03");
        assert_eq!(t("3723").format(&Locale::de_de()), "01:02:03");
        assert_eq!(t("1.5").format(&Locale::de_de()), "00:00:01,5");
        assert_eq!(t("3600000000").format(&Locale::en_us()), "1000000:00:00");
        let neg = Time::from_seconds(&Number::from_i64(-1)).unwrap();
        assert_eq!(neg.format(&Locale::en_us()), "-0:00:01");
        assert_eq!(neg.format(&Locale::de_de()), "-00:00:01");
        let tod = Time::from_seconds(&Number::from_i64(13 * 3600 + 30 * 60))
            .unwrap()
            .with_style(TimeStyle::TimeOfDay);
        assert_eq!(tod.format(&Locale::en_us()), "1:30 PM");
        assert_eq!(tod.format(&Locale::ja_jp()), "13:30");
        let hour = Time::from_seconds(&Number::from_i64(13 * 3600))
            .unwrap()
            .with_style(TimeStyle::HourOnly);
        assert_eq!(hour.format(&Locale::en_us()), "1 PM");
    }

    #[test]
    fn timestamps_by_locale() {
        let a = ts(2003, 1, 2, 13, 5, "7");
        assert_eq!(a.format(&Locale::en_us()), "1/2/2003 1:05:07 PM");
        assert_eq!(a.format(&Locale::de_de()), "02.01.2003 13:05:07");
        assert_eq!(a.format(&Locale::ja_jp()), "2003/01/02 13:05:07");
        let b = ts(2003, 1, 2, 0, 0, "0");
        assert_eq!(b.format(&Locale::en_us()), "1/2/2003 12:00 AM");
        assert_eq!(b.format(&Locale::de_de()), "02.01.2003 00:00");
        assert_eq!(b.format(&Locale::ja_jp()), "2003/01/02 0:00");
        let c = ts(2001, 1, 1, 0, 0, "0.123456");
        assert_eq!(c.format(&Locale::en_us()), "1/1/2001 12:00:00.123456 AM");
        // FileMaker Server in the Korean Contacts template.
        let ko = Locale::ko_kr();
        assert_eq!(
            ts(2014, 2, 17, 13, 5, "9").format(&ko),
            "2014-2-17  오후1:05:09"
        );
        let morning = ts(2014, 2, 17, 1, 5, "0").time_of_day();
        assert_eq!(morning.format(&ko), " 오전1:05");
    }
}
