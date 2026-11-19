//! The formats a file stores and calculations convert with (spec
//! `docs/re/calc-values.md` 9.3, `docs/re/file-metadata.md` 2):
//! separators, date and time formats, day, month and quarter names, the
//! long date, and the collation language.
//!
//! [`Locale::from_record`] decodes a file's locale record `[2, 0d]`, the
//! one decoder every host uses; [`Locale::elements_json`] rebuilds
//! `Get ( FileLocaleElements )` from it. The constructors decode the
//! records of FileMaker's shipped Contacts templates, one per language
//! they ship in.

mod contacts;
mod elements;
mod record;

use std::collections::BTreeMap;
use std::sync::OnceLock;

pub use om_collate::DateOrder;

/// The language catalog (`Locale::Type`; file-metadata.md 2.4).
const LOCALE_TYPES: &str = include_str!("../../../../docs/re/catalogs/locale-types.csv");

/// One `Locale::Type`: its name, language and ISO 639-1 code.
struct TypeInfo {
    name: &'static str,
    language: &'static str,
    iso: &'static str,
}

fn types() -> &'static BTreeMap<u16, TypeInfo> {
    static TYPES: OnceLock<BTreeMap<u16, TypeInfo>> = OnceLock::new();
    TYPES.get_or_init(|| {
        LOCALE_TYPES
            .lines()
            .skip(1)
            .filter_map(|line| {
                let mut columns = line.split(',');
                let id = columns.next()?.parse().ok()?;
                let name = columns.next()?;
                let language = columns.next()?;
                let iso = columns.next()?;
                Some((
                    id,
                    TypeInfo {
                        name,
                        language,
                        iso,
                    },
                ))
            })
            .collect()
    })
}

/// A file's language: the collation tailoring of text comparison and the
/// `LocaleID` `IDNum` (`Locale::Type`) FileMaker reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    English,
    French,
    German,
    Swedish,
    Japanese,
    /// Spanish (Modern): `ñ` is a letter of its own after `n`.
    Spanish,
    /// Chinese (Pinyin): ideographs by pinyin (`text::cjk`).
    Chinese,
    /// Korean: Hangul by jamo, Hanja by reading (`text::cjk`).
    Korean,
    /// Another `IDNum`; collates as English. Italian (30), Dutch (20) and
    /// Portuguese (34) files do (calc-values.md 13, value-core answers).
    Other(u16),
}

impl Language {
    /// `Get(FileLocaleElements)` `LocaleID.IDNum`.
    pub fn id(self) -> u16 {
        match self {
            Language::English => 21,
            Language::French => 24,
            Language::German => 25,
            Language::Japanese => 31,
            Language::Swedish => 41,
            Language::Spanish => 39,
            Language::Chinese => 45,
            Language::Korean => 76,
            Language::Other(id) => id,
        }
    }

    pub fn from_id(id: u16) -> Language {
        match id {
            21 => Language::English,
            24 => Language::French,
            25 => Language::German,
            31 => Language::Japanese,
            41 => Language::Swedish,
            39 => Language::Spanish,
            45 => Language::Chinese,
            76 => Language::Korean,
            other => Language::Other(other),
        }
    }

    fn info(self) -> Option<&'static TypeInfo> {
        types().get(&self.id())
    }

    /// `LocaleID.IDStr`, the type's name (`Chinese` for 45); empty for an
    /// id the catalog does not list.
    pub fn type_name(self) -> &'static str {
        self.info().map_or("", |t| t.name)
    }

    /// `LocaleID.LID`, the type's language (`Simplified Chinese` for 45).
    pub fn language_name(self) -> &'static str {
        self.info().map_or("", |t| t.language)
    }

    /// `LocaleID.ISOLangCode`: `en` for every type without a code of its
    /// own.
    pub fn iso639_1(self) -> &'static str {
        self.info().map_or("en", |t| t.iso)
    }

    /// `LocaleID.Name`, the language's display name. Observed for the 11
    /// Contacts languages; others fall back to the language (file-metadata.md
    /// 10, U2).
    pub fn display_name(self) -> &'static str {
        match self.id() {
            39 => "Spanish (Modern)",
            45 => "Chinese (Pinyin)",
            _ => match self.language_name() {
                "" => self.type_name(),
                language => language,
            },
        }
    }
}

/// The long date format (file-metadata.md 2.3): four
/// `International::DateElement` codes (0 empty, 1 day, 3 day name, 4 month,
/// 6 month name, 8 year, ...) and the separators before, between and after
/// them; an empty element ends the format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LongDate {
    pub elements: [u8; 4],
    pub separators: [String; 5],
}

/// A file's formats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locale {
    pub language: Language,
    /// `Num.Decimal`: the decimal separator of number text and literals.
    pub decimal_separator: char,
    /// `Num.1000s`; a space also accepts U+00A0 and U+202F.
    pub thousands_separator: char,
    /// `Num.Lead0`: `0.5`, not `.5`, in number display formats.
    pub leading_zero: bool,
    /// `Date.DateOrderName`.
    pub date_order: DateOrder,
    /// `Date.Sep`, the separator dates display with.
    pub date_separator: char,
    /// `Date.DateNums.0d`: days shown with two digits.
    pub pad_day: bool,
    /// `Date.DateNums.0m`: months shown with two digits.
    pub pad_month: bool,
    /// `Date.DateNums.YYyy`: four-digit years in short date formats
    /// (calculation text always shows four).
    pub show_century: bool,
    /// The long date (`Date.<order>.ElementArray`).
    pub long_date: LongDate,
    /// `Time.Sep`.
    pub time_separator: char,
    /// `Time.12h`: times of day shown on a 12-hour clock.
    pub twelve_hour: bool,
    /// `Time.HMS.0h`: hours shown with two digits.
    pub pad_hour: bool,
    /// `Time.NightDay.amStr` and `pmStr`, leading space included (` AM`);
    /// empty when the locale has none, and then AM/PM text is not accepted.
    pub am: String,
    pub pm: String,
    /// `Time.NightDay.12hSuffix` false: the AM/PM marker comes before the
    /// time (Korean: ` 오후1:05:09`).
    pub am_pm_before: bool,
    /// `Time.HMS.24h`: a suffix for 24-hour times (empty in every file).
    pub suffix_24h: String,
    /// `DaysOfWeek.NameList`, Sunday first.
    pub day_names: [String; 7],
    pub day_abbreviations: [String; 7],
    /// `Months.NameList`, January first.
    pub month_names: [String; 12],
    pub month_abbreviations: [String; 12],
    /// `Quarters.NameList` and `AbbrvList`.
    pub quarter_names: [String; 4],
    pub quarter_abbreviations: [String; 4],
    /// `DMQ.1stDayOfWeek`: 1 Sunday, 2 Monday.
    pub first_day_of_week: u8,
    /// `Text.Sep`: the list separator (`,` or `;`).
    pub list_separator: char,
    /// `Text.SQuotLead`, `Text.SQuotTrail`.
    pub smart_quotes: [char; 2],
    /// `Currency.Symbol`, and `Currency.Leading` (before the amount).
    pub currency_symbol: String,
    pub currency_leading: bool,
    /// `Misc.Metric`.
    pub metric: bool,
}

/// A shipped Contacts template's record, decoded once.
fn contacts(index: usize, record: &str) -> Locale {
    static DECODED: [OnceLock<Locale>; 11] = [const { OnceLock::new() }; 11];
    DECODED[index]
        .get_or_init(|| {
            let bytes: Vec<u8> = (0..record.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&record[i..i + 2], 16).expect("hex"))
                .collect();
            Locale::from_record(&bytes).expect("a shipped locale record")
        })
        .clone()
}

impl Locale {
    /// The English (United States) file formats.
    pub fn en_us() -> Locale {
        contacts(0, contacts::EN_US)
    }

    /// German (Germany).
    pub fn de_de() -> Locale {
        contacts(1, contacts::DE_DE)
    }

    /// French (France); the thousands separator is U+00A0.
    pub fn fr_fr() -> Locale {
        contacts(2, contacts::FR_FR)
    }

    /// Swedish (Sweden).
    pub fn sv_se() -> Locale {
        contacts(3, contacts::SV_SE)
    }

    /// Japanese (Japan). Times of day use a 24-hour clock; `午前`/`午後`
    /// are accepted after a time.
    pub fn ja_jp() -> Locale {
        contacts(4, contacts::JA_JP)
    }

    /// Spanish (Modern), the Contacts template's `es`.
    pub fn es_es() -> Locale {
        contacts(5, contacts::ES_ES)
    }

    /// Italian; times use `.` between hours and minutes.
    pub fn it_it() -> Locale {
        contacts(6, contacts::IT_IT)
    }

    /// Dutch.
    pub fn nl_nl() -> Locale {
        contacts(7, contacts::NL_NL)
    }

    /// Portuguese.
    pub fn pt_pt() -> Locale {
        contacts(8, contacts::PT_PT)
    }

    /// Korean: a 12-hour clock with the AM/PM marker before the time.
    pub fn ko_kr() -> Locale {
        contacts(9, contacts::KO_KR)
    }

    /// Chinese (Pinyin). The month names are FileMaker's, October included
    /// (`一月`).
    pub fn zh_cn() -> Locale {
        contacts(10, contacts::ZH_CN)
    }

    /// The constructor for a BCP 47 tag of the observed locales.
    pub fn for_tag(tag: &str) -> Option<Locale> {
        match tag {
            "en-US" => Some(Locale::en_us()),
            "de-DE" => Some(Locale::de_de()),
            "fr-FR" => Some(Locale::fr_fr()),
            "sv-SE" => Some(Locale::sv_se()),
            "ja-JP" => Some(Locale::ja_jp()),
            "es-ES" => Some(Locale::es_es()),
            "it-IT" => Some(Locale::it_it()),
            "nl-NL" => Some(Locale::nl_nl()),
            "pt-PT" => Some(Locale::pt_pt()),
            "ko-KR" => Some(Locale::ko_kr()),
            "zh-CN" => Some(Locale::zh_cn()),
            _ => None,
        }
    }

    /// The AM and PM markers without surrounding spaces, when the locale
    /// has them.
    pub(crate) fn am_pm(&self) -> Option<(&str, &str)> {
        let (am, pm) = (self.am.trim(), self.pm.trim());
        (!am.is_empty() && !pm.is_empty()).then_some((am, pm))
    }
}

impl Default for Locale {
    fn default() -> Locale {
        Locale::en_us()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eleven_locales() {
        for tag in [
            "en-US", "de-DE", "fr-FR", "sv-SE", "ja-JP", "es-ES", "it-IT", "nl-NL", "pt-PT",
            "ko-KR", "zh-CN",
        ] {
            let locale = Locale::for_tag(tag).unwrap();
            assert_eq!(Language::from_id(locale.language.id()), locale.language);
            assert_eq!(locale.language.iso639_1(), &tag[..2], "{tag}");
        }
        assert_eq!(Locale::en_us().am_pm(), Some(("AM", "PM")));
        assert_eq!(Locale::de_de().am_pm(), None);
        assert_eq!(Locale::ja_jp().am_pm(), Some(("午前", "午後")));
        assert_eq!(Locale::de_de().day_names[4], "Donnerstag");
        assert_eq!(Locale::ja_jp().month_names[0], "1月");
        assert_eq!(Locale::zh_cn().month_names[9], "一月");
        assert_eq!(Locale::fr_fr().thousands_separator, '\u{a0}');
        assert_eq!(Locale::it_it().time_separator, '.');
    }

    /// What the constructors held before they decoded the templates'
    /// records (FileMaker's `Get ( FileLocaleElements )`).
    #[test]
    fn the_records_give_the_observed_formats() {
        let expect = [
            (
                "en-US",
                '.',
                DateOrder::MonthDayYear,
                '/',
                false,
                false,
                true,
                1,
                ',',
            ),
            (
                "de-DE",
                ',',
                DateOrder::DayMonthYear,
                '.',
                true,
                true,
                false,
                2,
                ';',
            ),
            (
                "fr-FR",
                ',',
                DateOrder::DayMonthYear,
                '/',
                true,
                true,
                false,
                2,
                ';',
            ),
            (
                "sv-SE",
                ',',
                DateOrder::YearMonthDay,
                '-',
                true,
                true,
                false,
                2,
                ';',
            ),
            (
                "ja-JP",
                '.',
                DateOrder::YearMonthDay,
                '/',
                true,
                false,
                false,
                1,
                ',',
            ),
            (
                "es-ES",
                ',',
                DateOrder::DayMonthYear,
                '/',
                true,
                false,
                false,
                2,
                ';',
            ),
            (
                "it-IT",
                ',',
                DateOrder::DayMonthYear,
                '/',
                true,
                false,
                false,
                2,
                ';',
            ),
            (
                "nl-NL",
                ',',
                DateOrder::DayMonthYear,
                '-',
                false,
                false,
                false,
                2,
                ';',
            ),
            (
                "pt-PT",
                ',',
                DateOrder::DayMonthYear,
                '-',
                true,
                false,
                false,
                2,
                ';',
            ),
            (
                "ko-KR",
                '.',
                DateOrder::YearMonthDay,
                '-',
                false,
                false,
                true,
                2,
                ',',
            ),
            (
                "zh-CN",
                '.',
                DateOrder::YearMonthDay,
                '/',
                false,
                false,
                false,
                2,
                ',',
            ),
        ];
        for (tag, decimal, order, sep, pad, pad_hour, twelve, first, list) in expect {
            let l = Locale::for_tag(tag).unwrap();
            assert_eq!(l.decimal_separator, decimal, "{tag}");
            assert_eq!(l.date_order, order, "{tag}");
            assert_eq!(l.date_separator, sep, "{tag}");
            assert_eq!((l.pad_day, l.pad_month), (pad, pad), "{tag}");
            assert_eq!(l.pad_hour, pad_hour, "{tag}");
            assert_eq!(l.twelve_hour, twelve, "{tag}");
            assert_eq!(l.first_day_of_week, first, "{tag}");
            assert_eq!(l.list_separator, list, "{tag}");
        }
        let ko = Locale::ko_kr();
        assert!(ko.am_pm_before);
        assert_eq!((ko.am.as_str(), ko.pm.as_str()), (" 오전", " 오후"));
        assert_eq!(Locale::en_us().quarter_names[0], "1st Quarter");
        assert_eq!(Locale::en_us().long_date.elements, [3, 6, 1, 8]);
    }

    #[test]
    fn languages_come_from_the_catalog() {
        let zh = Language::from_id(45);
        assert_eq!(
            (zh.type_name(), zh.language_name(), zh.iso639_1()),
            ("Chinese", "Simplified Chinese", "zh")
        );
        assert_eq!(zh.display_name(), "Chinese (Pinyin)");
        assert_eq!(Language::Spanish.display_name(), "Spanish (Modern)");
        assert_eq!(Language::from_id(19).display_name(), "Danish");
        assert_eq!(Language::from_id(19).iso639_1(), "en");
        assert_eq!(Language::from_id(4).display_name(), "Catalog");
        assert_eq!(Language::from_id(9999).iso639_1(), "en");
    }
}
