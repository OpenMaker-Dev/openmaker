//! The keys a client builds when the host has no index yet
//! (`om_query::keys`), checked against the literal keys of
//! `docs/re/query-program.md` 7.1-7.3 rather than against the code that
//! builds OpenMaker Server's own indexes, which shares them; and client
//! finds (`om_query::find::ClientFind`) against the host's SimpleFind over
//! indexes holding those literal keys.

use std::borrow::Cow;
use std::collections::BTreeSet;

use om_collate::DateOrder;
use om_hbam::slot::uint_segment;
use om_hbam::{Catalog, DataType, Field, FieldKind, OwnedNode, RidList, Table};
use om_query::find::{self, ClientFind, Criterion, FindContext, IndexKind};
use om_query::keys::{record_value_keys, value_keys, word_keys, TextKeys};
use om_query::{Context, Source};

const TABLE: u64 = 0x8001;
const FIELD: u64 = 1;

fn hex(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .flat_map(|word| {
            (0..word.len())
                .step_by(2)
                .map(move |i| u8::from_str_radix(&word[i..i + 2], 16).unwrap())
        })
        .collect()
}

fn keys(form: TextKeys, value: &str) -> Vec<Vec<u8>> {
    value_keys(DataType::Text, value, DateOrder::MonthDayYear, form)
}

/// 7.1 (test project's First Name index) and 7.2 (empty text, case, the
/// space's weight, and the forms of the other index languages).
#[test]
fn text_keys_are_the_specs() {
    let english = TextKeys::of_language(21).unwrap();
    assert_eq!(english, TextKeys::Collated);
    assert_eq!(TextKeys::of_language(77), Some(TextKeys::Collated));
    for (value, key) in [
        ("Test", "01 1433 126b 1410 1433"),
        ("Hello", "01 12d3 126b 1330 1330 138e"),
        ("Ima", "01 12ec 135f 120f"),
        ("t", "01 1433"),
        ("T", "01 1433"),
        ("", "01"),
        ("a b", "01 120f 020a 1225"),
        ("x", "01 1497"),
    ] {
        assert_eq!(keys(english, value), [hex(key)], "{value:?}");
    }
    // System (1) and Unicode standard (3): the English form plus 00 00.
    for language in [1, 3] {
        let form = TextKeys::of_language(language).unwrap();
        assert_eq!(keys(form, "Test"), [hex("01 1433 126b 1410 1433 0000")]);
    }
    // Unicode raw (2): 01 and the UTF-16BE code units, case kept.
    let raw = TextKeys::of_language(2).unwrap();
    assert_eq!(keys(raw, "Test"), [hex("01 0054 0065 0073 0074")]);
    assert_ne!(keys(raw, "A"), keys(raw, "a"));
    // Language 0: no key.
    assert!(keys(TextKeys::of_language(0).unwrap(), "Test").is_empty());
    // The localized templates' languages tailor the table (German keys `ä`
    // as `a e`), which is not established: no form.
    for language in [20, 24, 25, 30, 31, 34, 39, 41, 45, 76] {
        assert_eq!(TextKeys::of_language(language), None, "{language}");
    }
    // German's literal example in 7.2: ä expands to the weights of a e.
    // English's accent-blind key is different, so it is not a substitute.
    assert_ne!(keys(english, "ä"), [hex("01 120f 126b")]);
}

/// 7.3: number keys, and date keys read in the file's date order.
#[test]
fn typed_keys_are_the_specs() {
    let number = |value| {
        value_keys(
            DataType::Number,
            value,
            DateOrder::MonthDayYear,
            TextKeys::Collated,
        )
    };
    for (value, key) in [
        ("0", "80 00"),
        ("1", "80 01 20"),
        ("2", "80 01 30"),
        ("9", "80 01 a0"),
        ("10", "80 02 21"),
        ("12", "80 02 23"),
        ("250000", "80 06 36 11 11"),
        ("698047", "80 06 7a 91 58"),
        ("4000002", "80 07 51 11 11 30"),
    ] {
        assert_eq!(number(value), [hex(key)], "{value}");
    }
    let date = |value, order| value_keys(DataType::Date, value, order, TextKeys::Collated);
    for (month_first, day_first, key) in [
        ("2/17/2014", "17/2/2014", "9f 78 91 80 00"),
        ("8/2/2015", "2/8/2015", "9f 7e 02 80 00"),
        ("10/9/2015", "9/10/2015", "9f 7e 89 80 00"),
    ] {
        assert_eq!(date(month_first, DateOrder::MonthDayYear), [hex(key)]);
        assert_eq!(date(day_first, DateOrder::DayMonthYear), [hex(key)]);
    }
    assert_eq!(
        date("2014/2/17", DateOrder::YearMonthDay),
        [hex("9f 78 91 80 00")]
    );
    assert_eq!(
        value_keys(
            DataType::Time,
            "21:26:57",
            DateOrder::MonthDayYear,
            TextKeys::Collated
        ),
        [hex("80 00 00 80 05 88 32 80")]
    );
    for (value, order) in [
        ("10/27/2026 21:26:57", DateOrder::MonthDayYear),
        ("27/10/2026 21:26:57", DateOrder::DayMonthYear),
        ("2026/10/27 21:26:57", DateOrder::YearMonthDay),
    ] {
        assert_eq!(
            value_keys(DataType::Timestamp, value, order, TextKeys::Collated),
            [hex("9f aa 9b 80 05 88 32 80")]
        );
    }
}

/// A file whose one text field (index language `language`) has a value
/// index holding `index`, for the host's SimpleFind to read.
struct Host {
    root: OwnedNode,
    language: u32,
}

impl Source for Host {
    fn catalog(&self) -> om_hbam::Result<Catalog> {
        let mut definition = vec![0; 26];
        definition[0] = 1;
        definition[1] = 1;
        definition[4..8].copy_from_slice(&self.language.to_be_bytes());
        definition[24..26].copy_from_slice(&1u16.to_be_bytes());
        Ok(Catalog {
            tables: vec![Table {
                id: TABLE,
                name: "Table".into(),
                fields: vec![Field {
                    id: FIELD,
                    name: "Text".into(),
                    kind: FieldKind::Normal,
                    data_type: DataType::Text,
                    options: definition,
                }],
            }],
            ..Catalog::default()
        })
    }

    fn node(&self, path: &[&[u8]]) -> om_hbam::Result<Option<Cow<'_, OwnedNode>>> {
        Ok(self.root.at(path).map(Cow::Borrowed))
    }
}

/// What `criterion` finds over records holding `texts` (one word each): by
/// the host's SimpleFind over value and word indexes holding the spec's
/// literal keys `index` (key, records), and by a client find keyed in
/// `language`'s form. The criterion is keyed as a find on a field indexed
/// in `language` keys it, English where OpenMaker builds no form.
fn host_and_client(
    texts: &[&str],
    index: &[(&str, &[u64])],
    criterion: &str,
    language: u32,
) -> (Vec<u64>, Option<Vec<u64>>) {
    let records: RidList = (1..=texts.len() as u64).collect();
    let form = TextKeys::of_language(language).unwrap_or(TextKeys::Collated);
    let context = FindContext::new(DateOrder::MonthDayYear, None).with_text_keys(form);
    let criterion = Criterion::parse(criterion, DataType::Text, &context)
        .unwrap()
        .unwrap();
    let plan = find::plan(TABLE, FIELD, &criterion);
    assert!(plan.exact);
    let finders: Vec<Vec<u8>> = plan.finders.into_iter().map(|f| f.bytes).collect();
    let mut root = OwnedNode::default();
    let table = uint_segment(TABLE);
    root.put(&[&table, &[13]], records.to_node(Vec::new()));
    let entries: Vec<OwnedNode> = index
        .iter()
        .map(|(key, ids)| ids.iter().copied().collect::<RidList>().to_node(hex(key)))
        .collect();
    for kind in [0, 1] {
        root.put(
            &[&table, &[1], &uint_segment(FIELD), &[kind]],
            OwnedNode::list(Vec::new(), entries.clone()),
        );
    }
    let program = find::program(1, TABLE, &finders, None);
    let context = Context {
        file_id: 1,
        connection_key: None,
    };
    let outcome = om_query::execute(&Host { root, language }, &program, &context);
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    let host = find::found(&outcome.slots).unwrap().iter().collect();

    let Some(form) = TextKeys::of_language(language) else {
        return (host, None);
    };
    let mut client = ClientFind::new(&[finders], usize::MAX).unwrap();
    let reads = client.reads().to_vec();
    for (i, text) in texts.iter().enumerate() {
        let texts = [text.to_string()];
        let keys: Vec<BTreeSet<Vec<u8>>> = reads
            .iter()
            .map(|(field, index)| {
                assert_eq!(*field, FIELD);
                match index {
                    IndexKind::Value => {
                        record_value_keys(DataType::Text, &texts, DateOrder::MonthDayYear, form)
                    }
                    IndexKind::Word => word_keys(&texts, form),
                }
            })
            .collect();
        client.add(i as u64 + 1, &keys).unwrap();
    }
    let client = client.finish().remove(0).iter().collect();
    (host, Some(client))
}

#[test]
fn client_finds_agree_with_finds_on_the_specs_keys() {
    // English: case is ignored, so "Test" and "test" share a key.
    let english = [("01 1433 126b 1410 1433", &[1, 2][..]), ("01", &[3])];
    let texts = ["Test", "test", ""];
    for (criterion, found) in [("!", &[1, 2][..]), ("=", &[3]), ("*", &[1, 2])] {
        let answers = host_and_client(&texts, &english, criterion, 21);
        assert_eq!(
            answers,
            (found.to_vec(), Some(found.to_vec())),
            "English {criterion}"
        );
    }
    // Unicode raw: case is kept, so nothing is a duplicate.
    let raw = [("01 0041", &[1][..]), ("01 0061", &[2])];
    let answers = host_and_client(&["A", "a"], &raw, "!", 2);
    assert_eq!(answers, (vec![], Some(vec![])), "Unicode raw !");
    let answers = host_and_client(&["A", "a"], &raw, "*", 2);
    assert_eq!(answers, (vec![1, 2], Some(vec![1, 2])), "Unicode raw *");
    // A criterion on a Unicode raw field is keyed in its code units, as
    // FileMaker Pro 26's blank file keys its PrimaryKey (7.2); case is
    // kept (whether FileMaker's raw finds keep it is unmeasured).
    let uuids = [
        "0E9E1BC2-B39D-4563-9841-4A4BFA860376",
        "0e9e1bc2-b39d-4563-9841-4a4bfa860376",
    ];
    let raw_key = |text: &str| {
        std::iter::once("01".to_string())
            .chain(text.encode_utf16().map(|u| format!("{u:04x}")))
            .collect::<String>()
    };
    let (upper, lower) = (raw_key(uuids[0]), raw_key(uuids[1]));
    let raw = [(upper.as_str(), &[1][..]), (lower.as_str(), &[2])];
    for (criterion, found) in [
        (format!("=={}", uuids[0]), vec![1]),
        (format!("=={}", uuids[1]), vec![2]),
        ("0E9E".to_string(), vec![1]),
    ] {
        let answers = host_and_client(&uuids, &raw, &criterion, 2);
        assert_eq!(
            answers,
            (found.clone(), Some(found)),
            "Unicode raw {criterion}"
        );
    }
    // Unicode standard keeps accents (catalogs/index-key-vectors.tsv):
    // `==` finds only the accented word, a prefix both, since its key is
    // the primary weights, which come before `00 00` and the accents in
    // every word that starts so (OpenMaker's reading; unmeasured).
    let padded = [
        ("01 126b 13c8 1453 12ec 13b3 126b 0000", &[2][..]),
        (
            "01 126b 13c8 1453 12ec 13b3 126b 0000 01 13 01 01 01 01 01",
            &[1],
        ),
    ];
    let texts = ["\u{e9}quipe", "equipe"];
    for (criterion, found) in [
        ("==\u{e9}quipe", vec![1]),
        ("==equipe", vec![2]),
        ("equ", vec![1, 2]),
    ] {
        let answers = host_and_client(&texts, &padded, criterion, 3);
        assert_eq!(
            answers,
            (found.clone(), Some(found)),
            "Unicode standard {criterion}"
        );
    }
    // System: the English form plus 00 00.
    let system = [("01 1433 126b 1410 1433 0000", &[1, 2][..])];
    let answers = host_and_client(&["Test", "TEST"], &system, "!", 1);
    assert_eq!(answers, (vec![1, 2], Some(vec![1, 2])), "System !");
    // German's ä / ae share this literal spec key. Until that tailoring is
    // implemented, decline to build keys instead of answering with English.
    let german = [("01 120f 126b", &[1, 2][..])];
    let answers = host_and_client(&["ä", "ae"], &german, "!", 25);
    assert_eq!(
        answers,
        (vec![1, 2], None),
        "German ! is unsupported on the client"
    );
}

/// A row of `docs/re/catalogs/index-key-vectors.tsv`: the keys FileMaker
/// Server's engine wrote for one value (query-program.md 7.1-7.3).
struct Vector {
    kind: &'static str,
    language: &'static str,
    value: String,
    value_entries: BTreeSet<Vec<u8>>,
    word_entries: BTreeSet<Vec<u8>>,
}

fn vectors() -> Vec<Vector> {
    include_str!("../../../docs/re/catalogs/index-key-vectors.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let columns: Vec<&str> = line.split('\t').collect();
            Vector {
                kind: columns[0],
                language: columns[1],
                value: vector_value(columns[2]),
                value_entries: columns[3].split_whitespace().map(vector_key).collect(),
                word_entries: columns[4].split_whitespace().map(vector_key).collect(),
            }
        })
        .collect()
}

/// A value as the table writes it: `\r \n \t \\`, `\u{hex}`, and `c{n}`
/// for `n` copies of `c`.
fn vector_value(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    let until_brace = |chars: &mut std::str::Chars| -> String {
        chars.by_ref().take_while(|&c| c != '}').collect()
    };
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    chars.next();
                    let hex = until_brace(&mut chars);
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
                }
                other => out.extend(other),
            },
            '{' => {
                let count: usize = until_brace(&mut chars).parse().unwrap();
                let last = out.chars().last().unwrap();
                out.extend(std::iter::repeat_n(last, count - 1));
            }
            other => out.push(other),
        }
    }
    out
}

/// A key as the table writes it: hex, `hhhh{n}` for `n` copies of a
/// 2-byte weight.
fn vector_key(text: &str) -> Vec<u8> {
    let mut digits = String::new();
    let mut parts = text.split('{');
    digits.push_str(parts.next().unwrap());
    for part in parts {
        let (count, rest) = part.split_once('}').unwrap();
        let weight = digits[digits.len() - 4..].to_string();
        for _ in 1..count.parse::<usize>().unwrap() {
            digits.push_str(&weight);
        }
        digits.push_str(rest);
    }
    hex(&digits)
}

/// Every English, Unicode standard and Unicode raw text row of the vector
/// table: the value-index keys OpenMaker builds are FileMaker's. System (1)
/// and English international (77) key as Unicode standard and English and
/// have no rows of their own. Whether a record is also listed under
/// `ff 80` (a long key, 7.1) follows rules OpenMaker does not model yet
/// ([#177](https://github.com/OpenMaker-Dev/openmaker/issues/177)), so that
/// entry is left out here.
#[test]
fn text_keys_are_filemakers_vectors() {
    let too_long = om_collate::TOO_LONG_KEY.to_vec();
    let mut checked = 0;
    let mut mismatches = Vec::new();
    for vector in vectors().iter().filter(|v| v.kind == "text") {
        let language: u32 = vector.language.parse().unwrap();
        let Some(form @ (TextKeys::Collated | TextKeys::CollatedPadded | TextKeys::Raw)) =
            TextKeys::of_language(language)
        else {
            continue;
        };
        let ours = om_query::keys::record_value_entries(
            DataType::Text,
            std::slice::from_ref(&vector.value),
            DateOrder::MonthDayYear,
            form,
        );
        let without_flag = |keys: &BTreeSet<Vec<u8>>| {
            keys.iter()
                .filter(|key| **key != too_long)
                .cloned()
                .collect::<BTreeSet<_>>()
        };
        checked += 1;
        if without_flag(&ours) != without_flag(&vector.value_entries) {
            mismatches.push(format!("{language} {:?}", vector.value));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    assert_eq!(checked, 364);
}

/// The Japanese (31) rows of the vector table (query-program.md 7.2,
/// "Japanese"; #299): each kana row's weights, and the value- and word-index
/// keys of every text row. A value holding a kanji outside JIS X 0208
/// level 1 has no key, its order not being derived: `豈` (U+F900), and the
/// row with `丂` (JIS X 0212) and `㐀` (an extension). Nor does the row of
/// sound and iteration marks, whose U+309A keys in a way not established.
/// `ff 80` is left out
/// as for the other forms
/// ([#177](https://github.com/OpenMaker-Dev/openmaker/issues/177)).
#[test]
fn japanese_keys_are_filemakers_vectors() {
    let japanese = TextKeys::Japanese;
    let mut kana = 0;
    for vector in vectors().iter().filter(|v| v.kind == "kana") {
        let weights = om_collate::japanese_key(&vector.value).unwrap()[1..].to_vec();
        assert_eq!(
            BTreeSet::from([weights]),
            vector.value_entries,
            "{:?}",
            vector.value
        );
        kana += 1;
    }
    assert_eq!(kana, 236);
    let too_long = om_collate::TOO_LONG_KEY.to_vec();
    let without_flag = |keys: &BTreeSet<Vec<u8>>| {
        keys.iter()
            .filter(|key| **key != too_long)
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    let (mut keyed, mut keyless, mut mismatches) = (0, Vec::new(), Vec::new());
    for vector in vectors()
        .iter()
        .filter(|v| v.kind == "text" && v.language == "31")
    {
        let lines = vector.value.split(['\r', '\n']).filter(|l| !l.is_empty());
        if lines.clone().any(|line| japanese.key(line).is_none()) {
            keyless.push(vector.value.clone());
            continue;
        }
        let value = std::slice::from_ref(&vector.value);
        let ours = om_query::keys::record_value_entries(
            DataType::Text,
            value,
            DateOrder::MonthDayYear,
            japanese,
        );
        let words = om_query::keys::word_entries(value, japanese);
        if without_flag(&ours) != without_flag(&vector.value_entries)
            || without_flag(&words) != without_flag(&vector.word_entries)
        {
            mismatches.push(vector.value.clone());
        }
        keyed += 1;
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    assert_eq!(
        keyless,
        [
            // The compatibility ideograph U+F900, as the probe holds it.
            "\u{f900}",
            "゛゜\u{3099}\u{309a}ー々〆〇〻",
            "丂㐀一二三十百千万円年月日本語"
        ]
    );
    assert_eq!(keyed, 22);
}

/// The word-index keys of every English and Unicode standard text row of
/// the vector table (7.1, word breaking): OpenMaker breaks words where
/// FileMaker does. `ff 80` is left out as in the value index
/// ([#177](https://github.com/OpenMaker-Dev/openmaker/issues/177)); Unicode
/// raw breaks words by rules of its own, not built
/// ([#395](https://github.com/OpenMaker-Dev/openmaker/issues/395)).
#[test]
fn word_keys_are_filemakers_vectors() {
    let too_long = om_collate::TOO_LONG_KEY.to_vec();
    let mut checked = 0;
    let mut mismatches = Vec::new();
    for vector in vectors().iter().filter(|v| v.kind == "text") {
        let language: u32 = vector.language.parse().unwrap();
        let Some(form @ (TextKeys::Collated | TextKeys::CollatedPadded)) =
            TextKeys::of_language(language)
        else {
            continue;
        };
        let without_flag = |keys: BTreeSet<Vec<u8>>| {
            keys.into_iter()
                .filter(|key| *key != too_long)
                .collect::<BTreeSet<_>>()
        };
        let ours = om_query::keys::word_entries(std::slice::from_ref(&vector.value), form);
        checked += 1;
        if without_flag(ours) != without_flag(vector.word_entries.clone()) {
            mismatches.push(format!("{language} {:?}", vector.value));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    assert_eq!(checked, 242);
}

/// Every number, time, timestamp and date row of the vector table: the
/// value-index entries OpenMaker builds for the stored text are FileMaker's
/// (7.3), empty values under `01` and values FileMaker cannot read under
/// `fe ff`. The probe file stores dates month first.
#[test]
fn typed_keys_are_filemakers_vectors() {
    // Numbers FileMaker reads from digits among other characters, which
    // OpenMaker does not key yet
    // ([#246](https://github.com/OpenMaker-Dev/openmaker/issues/246)).
    const MIXED: [&str; 17] = [
        "1e3", "1,234.5", "12abc34", "5-", "1.5.2", "1.0E+20", "(5)", "$5", "5%", "+5", "1 000",
        "1/2", "e5", "12.5e2", "1E400", "1e-5", "1.0E+400",
    ];
    let mut checked = 0;
    let mut mismatches = Vec::new();
    for vector in vectors() {
        let data_type = match vector.kind {
            "number" => DataType::Number,
            "time" => DataType::Time,
            "timestamp" => DataType::Timestamp,
            "date" => DataType::Date,
            _ => continue,
        };
        // "the value as set (stored the text when it differs)"
        let stored = match vector.value.split_once(" (stored ") {
            Some((_, stored)) => stored.strip_suffix(')').unwrap().to_string(),
            None => vector.value.clone(),
        };
        let ours = om_query::keys::record_value_entries(
            data_type,
            std::slice::from_ref(&stored),
            DateOrder::MonthDayYear,
            TextKeys::Collated,
        );
        checked += 1;
        if MIXED.contains(&stored.as_str()) {
            assert!(ours.is_empty(), "{stored:?} is keyed now: update #246");
        } else if ours != vector.value_entries {
            mismatches.push(format!("{} {stored:?}", vector.kind));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    assert_eq!(checked, 88);
    // Every record without a number is indexed under `01` (7.3).
    for data_type in [DataType::Number, DataType::Time, DataType::Date] {
        assert_eq!(
            record_value_keys(
                data_type,
                &[String::new()],
                DateOrder::MonthDayYear,
                TextKeys::Collated
            ),
            [vec![0x01]].into()
        );
    }
}
