//! The file locale record `[2, 0d]` (`Draco::Locale::Write`,
//! file-metadata.md 2.1): the file's `Locale::Type`, flag bits, separators,
//! date order and long date, AM/PM strings, currency, and the day, month and
//! quarter names. Strings carry a UTF-16 unit count and masked SCSU.

use super::{DateOrder, Language, Locale, LongDate};

/// Flag bits (field 3).
const HOUR_LEADING_ZERO: u32 = 0x8000_0000;
const DAY_LEADING_ZERO: u32 = 0x4000_0000;
const MONTH_LEADING_ZERO: u32 = 0x2000_0000;
const SHOW_CENTURY: u32 = 0x1000_0000;
const TWELVE_HOUR: u32 = 0x0800_0000;
const NUMBER_LEADING_ZERO: u32 = 0x0200_0000;
const METRIC: u32 = 0x0100_0000;
const CURRENCY_LEADING: u32 = 0x0080_0000;
/// Set: the AM/PM marker comes before the time.
const AM_PM_BEFORE: u32 = 0x0040_0000;

/// The format version byte written after the time separator.
const FORMAT_VERSION: u8 = 2;

/// A big-endian reader over the record.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let out = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(out)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    /// A UTF-16 code unit as a character (separators and quotes).
    fn char(&mut self) -> Option<char> {
        char::from_u32(u32::from(self.u16()?))
    }

    fn string(&mut self) -> Option<String> {
        let (text, used) = om_scsu::decode_counted(self.bytes.get(self.at..)?).ok()?;
        self.at += used;
        Some(text)
    }

    fn strings<const N: usize>(&mut self) -> Option<[String; N]> {
        let mut out: [String; N] = std::array::from_fn(|_| String::new());
        for slot in &mut out {
            *slot = self.string()?;
        }
        Some(out)
    }
}

fn order_code(order: DateOrder) -> u8 {
    match order {
        DateOrder::MonthDayYear => 0,
        DateOrder::DayMonthYear => 1,
        DateOrder::YearMonthDay => 2,
    }
}

impl Locale {
    /// Decode a file's locale record `[2, 0d]`; `None` when it is
    /// malformed. Date orders 3-5 (MYD, DYM, YDM; in no file) read as the
    /// order with the same first element.
    pub fn from_record(bytes: &[u8]) -> Option<Locale> {
        let mut r = Reader { bytes, at: 0 };
        let language = Language::from_id(u16::try_from(r.u32()?).unwrap_or(u16::MAX));
        r.u8()?;
        let flags = r.u32()?;
        let list_separator = r.char()?;
        let smart_quotes = [r.char()?, r.char()?];
        let thousands_separator = r.char()?;
        let decimal_separator = r.char()?;
        let date_separator = r.char()?;
        let time_separator = r.char()?;
        r.u8()?;
        let date_order = match r.u8()? {
            0 | 3 => DateOrder::MonthDayYear,
            1 | 4 => DateOrder::DayMonthYear,
            2 | 5 => DateOrder::YearMonthDay,
            _ => return None,
        };
        let elements: [u8; 4] = r.take(4)?.try_into().ok()?;
        let am = r.string()?;
        let pm = r.string()?;
        let suffix_24h = r.string()?;
        let currency_symbol = r.string()?;
        // `+` and `-`, written as constants.
        r.string()?;
        r.string()?;
        let day_names: [String; 7] = r.strings()?;
        let day_abbreviations = r.strings()?;
        let month_names = r.strings()?;
        let month_abbreviations = r.strings()?;
        let quarter_names = r.strings()?;
        let quarter_abbreviations = r.strings()?;
        let separators = r.strings()?;
        // `International::GetFirstDayOfWeek`: Sunday first when the first
        // day name is English's or Japanese's Sunday.
        let first_day_of_week = if matches!(day_names[0].as_str(), "Sunday" | "日曜日") {
            1
        } else {
            2
        };
        Some(Locale {
            language,
            decimal_separator,
            thousands_separator,
            leading_zero: flags & NUMBER_LEADING_ZERO != 0,
            date_order,
            date_separator,
            pad_day: flags & DAY_LEADING_ZERO != 0,
            pad_month: flags & MONTH_LEADING_ZERO != 0,
            show_century: flags & SHOW_CENTURY != 0,
            long_date: LongDate {
                elements,
                separators,
            },
            time_separator,
            twelve_hour: flags & TWELVE_HOUR != 0,
            pad_hour: flags & HOUR_LEADING_ZERO != 0,
            am,
            pm,
            am_pm_before: flags & AM_PM_BEFORE != 0,
            suffix_24h,
            day_names,
            day_abbreviations,
            month_names,
            month_abbreviations,
            quarter_names,
            quarter_abbreviations,
            first_day_of_week,
            list_separator,
            smart_quotes,
            currency_symbol,
            currency_leading: flags & CURRENCY_LEADING != 0,
            metric: flags & METRIC != 0,
        })
    }

    /// The record [`Locale::from_record`] reads, as FileMaker writes it.
    /// Characters outside the Basic Multilingual Plane do not fit a
    /// separator and are written as U+FFFD.
    pub fn to_record(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(u32::from(self.language.id()).to_be_bytes());
        out.push(0);
        let flags = [
            (self.pad_hour, HOUR_LEADING_ZERO),
            (self.pad_day, DAY_LEADING_ZERO),
            (self.pad_month, MONTH_LEADING_ZERO),
            (self.show_century, SHOW_CENTURY),
            (self.twelve_hour, TWELVE_HOUR),
            (self.leading_zero, NUMBER_LEADING_ZERO),
            (self.metric, METRIC),
            (self.currency_leading, CURRENCY_LEADING),
            (self.am_pm_before, AM_PM_BEFORE),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .fold(0u32, |acc, (_, bit)| acc | bit);
        out.extend(flags.to_be_bytes());
        for c in [
            self.list_separator,
            self.smart_quotes[0],
            self.smart_quotes[1],
            self.thousands_separator,
            self.decimal_separator,
            self.date_separator,
            self.time_separator,
        ] {
            let unit = u16::try_from(u32::from(c)).unwrap_or(0xfffd);
            out.extend(unit.to_be_bytes());
        }
        out.push(FORMAT_VERSION);
        out.push(order_code(self.date_order));
        out.extend(self.long_date.elements);
        let strings = [&self.am, &self.pm, &self.suffix_24h, &self.currency_symbol]
            .into_iter()
            .map(String::as_str)
            .chain(["+", "-"])
            .chain(self.day_names.iter().map(String::as_str))
            .chain(self.day_abbreviations.iter().map(String::as_str))
            .chain(self.month_names.iter().map(String::as_str))
            .chain(self.month_abbreviations.iter().map(String::as_str))
            .chain(self.quarter_names.iter().map(String::as_str))
            .chain(self.quarter_abbreviations.iter().map(String::as_str))
            .chain(self.long_date.separators.iter().map(String::as_str));
        for text in strings {
            out.extend(om_scsu::encode_counted(text));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::contacts;
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn the_shipped_records_re_encode_byte_for_byte() {
        for record in [
            contacts::EN_US,
            contacts::DE_DE,
            contacts::FR_FR,
            contacts::SV_SE,
            contacts::JA_JP,
            contacts::ES_ES,
            contacts::IT_IT,
            contacts::NL_NL,
            contacts::PT_PT,
            contacts::KO_KR,
            contacts::ZH_CN,
        ] {
            let bytes = hex(record);
            let locale = Locale::from_record(&bytes).unwrap();
            assert_eq!(locale.to_record(), bytes, "{:?}", locale.language);
        }
    }

    #[test]
    fn the_english_record_reads_as_specified() {
        let en = Locale::from_record(&hex(contacts::EN_US)).unwrap();
        assert_eq!(en.language, Language::English);
        assert_eq!(
            (en.am.as_str(), en.pm.as_str(), en.currency_symbol.as_str()),
            (" AM", " PM", "$")
        );
        assert!(en.twelve_hour && en.leading_zero && en.currency_leading);
        assert!(!en.pad_day && !en.show_century && !en.metric && !en.am_pm_before);
        assert_eq!(en.smart_quotes, ['“', '”']);
        assert_eq!(en.long_date.separators, ["", ", ", " ", ", ", ""]);
        assert_eq!(en.quarter_abbreviations, ["Q1", "Q2", "Q3", "Q4"]);
        assert_eq!(en.day_abbreviations[0], "Sun");
        assert_eq!(en.month_names[11], "December");
        // A truncated record is refused.
        assert!(Locale::from_record(&hex(contacts::EN_US)[..40]).is_none());
        assert!(Locale::from_record(&[]).is_none());
    }
}
