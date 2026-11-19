//! `Get ( FileLocaleElements )` and `Get ( SystemLocaleElements )`
//! (`DBFileUser::GetLocaleElementsAsJSON`, file-metadata.md 2.4): the
//! locale as JSON, keys in byte order, `\r` line breaks, one tab per level
//! and ` : ` between a key and its value.

use super::{DateOrder, Locale};

/// A JSON value as FileMaker lays it out.
enum Json {
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<String>),
    Object(Vec<(&'static str, Json)>),
}

fn object(mut entries: Vec<(&'static str, Json)>) -> Json {
    entries.sort_by(|a, b| a.0.cmp(b.0));
    Json::Object(entries)
}

fn text(s: impl Into<String>) -> Json {
    Json::Str(s.into())
}

fn list<'a>(items: impl IntoIterator<Item = &'a String>) -> Json {
    Json::List(items.into_iter().cloned().collect())
}

fn quoted(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn tabs(depth: usize, out: &mut String) {
    out.extend(std::iter::repeat('\t').take(depth));
}

/// Write `value` whose opening bracket sits at `depth` tabs.
fn write(value: &Json, depth: usize, out: &mut String) {
    match value {
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(n) => out.push_str(&n.to_string()),
        Json::Str(s) => quoted(s, out),
        Json::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                out.push_str(if i == 0 { "\r" } else { ",\r" });
                tabs(depth + 1, out);
                quoted(item, out);
            }
            out.push('\r');
            tabs(depth, out);
            out.push(']');
        }
        Json::Object(entries) => {
            out.push('{');
            for (i, (key, value)) in entries.iter().enumerate() {
                out.push_str(if i == 0 { "\r" } else { ",\r" });
                tabs(depth + 1, out);
                quoted(key, out);
                out.push_str(" : ");
                if matches!(value, Json::List(_) | Json::Object(_)) {
                    out.push('\r');
                    tabs(depth + 1, out);
                }
                write(value, depth + 1, out);
            }
            out.push('\r');
            tabs(depth, out);
            out.push('}');
        }
    }
}

/// A long date element as `ElementArray.NameList` names it; the codes no
/// file uses are not known (file-metadata.md 10, U3) and read as empty.
fn element_name(code: u8) -> &'static str {
    match code {
        1 => "D#",
        3 => "D$",
        4 => "M#",
        6 => "M$",
        8 => "YYYY#",
        13 => "ReignY",
        _ => "",
    }
}

impl Locale {
    /// `Get ( FileLocaleElements )` for the file's locale (`active`), or
    /// `Get ( SystemLocaleElements )` for the system's (`Misc.Active`
    /// false).
    pub fn elements_json(&self, active: bool) -> String {
        let (order_id, order_name) = match self.date_order {
            DateOrder::MonthDayYear => (0, "MDY"),
            DateOrder::DayMonthYear => (1, "DMY"),
            DateOrder::YearMonthDay => (2, "YMD"),
        };
        // Elements 1, 2, 3, then 0, up to the first empty one, with the
        // separators after each.
        let long = &self.long_date;
        let shown: Vec<usize> = [1, 2, 3, 0]
            .into_iter()
            .take_while(|&i| long.elements[i] != 0)
            .collect();
        let names = shown
            .iter()
            .map(|&i| element_name(long.elements[i]).to_string())
            .collect();
        let separators = (1..=shown.len())
            .map(|i| long.separators[i].clone())
            .collect();
        let date = object(vec![
            (
                "DMQ",
                object(vec![
                    ("1stDayOfWeek", Json::Int(i64::from(self.first_day_of_week))),
                    (
                        "DaysOfWeek",
                        object(vec![
                            ("AbbrvList", list(&self.day_abbreviations)),
                            ("NameList", list(&self.day_names)),
                        ]),
                    ),
                    (
                        "Months",
                        object(vec![
                            ("AbbrvList", list(&self.month_abbreviations)),
                            ("NameList", list(&self.month_names)),
                        ]),
                    ),
                    (
                        "Quarters",
                        object(vec![
                            ("AbbrvList", list(&self.quarter_abbreviations)),
                            ("NameList", list(&self.quarter_names)),
                        ]),
                    ),
                ]),
            ),
            (
                "DateNums",
                object(vec![
                    ("0d", Json::Bool(self.pad_day)),
                    ("0m", Json::Bool(self.pad_month)),
                    ("YYyy", Json::Bool(self.show_century)),
                ]),
            ),
            ("DateOrderID", Json::Int(order_id)),
            ("DateOrderName", text(order_name)),
            (
                order_name,
                object(vec![
                    (
                        "ElementArray",
                        object(vec![
                            ("NameList", Json::List(names)),
                            ("SepList", Json::List(separators)),
                        ]),
                    ),
                    ("MustUseLocalesSep", Json::Bool(false)),
                ]),
            ),
            ("Sep", text(self.date_separator)),
        ]);
        let language = self.language;
        let root = object(vec![
            ("APIVers", Json::Int(1)),
            (
                "Currency",
                object(vec![
                    ("Leading", Json::Bool(self.currency_leading)),
                    ("Symbol", text(&*self.currency_symbol)),
                ]),
            ),
            ("Date", date),
            (
                "LocaleID",
                object(vec![
                    ("IDNum", Json::Int(i64::from(language.id()))),
                    ("IDStr", text(language.type_name())),
                    ("ISOLangCode", text(language.iso639_1())),
                    ("LID", text(language.language_name())),
                    ("Name", text(language.display_name())),
                ]),
            ),
            (
                "Misc",
                object(vec![
                    ("Active", Json::Bool(active)),
                    ("Metric", Json::Bool(self.metric)),
                ]),
            ),
            (
                "Num",
                object(vec![
                    ("1000s", text(self.thousands_separator)),
                    ("Decimal", text(self.decimal_separator)),
                    ("Lead0", Json::Bool(self.leading_zero)),
                ]),
            ),
            (
                "Text",
                object(vec![
                    ("SQuotLead", text(self.smart_quotes[0])),
                    ("SQuotTrail", text(self.smart_quotes[1])),
                    ("Sep", text(self.list_separator)),
                ]),
            ),
            (
                "Time",
                object(vec![
                    ("12h", Json::Bool(self.twelve_hour)),
                    (
                        "HMS",
                        object(vec![
                            ("0h", Json::Bool(self.pad_hour)),
                            ("24h", text(&*self.suffix_24h)),
                            ("Seconds", Json::Bool(false)),
                        ]),
                    ),
                    (
                        "NightDay",
                        object(vec![
                            ("12hSuffix", Json::Bool(!self.am_pm_before)),
                            ("amStr", text(&*self.am)),
                            ("pmStr", text(&*self.pm)),
                        ]),
                    ),
                    ("Sep", text(self.time_separator)),
                ]),
            ),
        ]);
        let mut out = String::new();
        write(&root, 0, &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FileMaker Server's own `Get ( FileLocaleElements )` in the Contacts
    /// templates (`tools/fms-calc-harness`, 2026-10-29), with the harness's
    /// `\r` and `\t` escapes.
    const ENGINE: [(&str, &str); 4] = [
        (
            "en-US",
            include_str!("../../tests/data/locale-elements-en.txt"),
        ),
        (
            "de-DE",
            include_str!("../../tests/data/locale-elements-de.txt"),
        ),
        (
            "ja-JP",
            include_str!("../../tests/data/locale-elements-ja.txt"),
        ),
        (
            "ko-KR",
            include_str!("../../tests/data/locale-elements-ko.txt"),
        ),
    ];

    #[test]
    fn the_json_is_the_engines() {
        for (tag, expected) in ENGINE {
            let expected = expected
                .trim_end()
                .replace("\\r", "\r")
                .replace("\\t", "\t");
            let json = Locale::for_tag(tag).unwrap().elements_json(true);
            assert_eq!(json, expected, "{tag}");
        }
        assert!(Locale::en_us()
            .elements_json(false)
            .contains("\"Active\" : false"));
    }
}
