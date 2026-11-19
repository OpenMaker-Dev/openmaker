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
use om_query::find::{self, ClientFind, IndexKind};
use om_query::keys::{record_value_keys, value_keys, TextKeys};
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
        [hex("80 05 88 32 80")]
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

/// What `criterion` finds over records holding `texts`: by the host's
/// SimpleFind over a value index holding the spec's literal keys `index`
/// (key, records), and by a client find keyed in `language`'s form.
fn host_and_client(
    texts: &[&str],
    index: &[(&str, &[u64])],
    criterion: &str,
    language: u32,
) -> (Vec<u64>, Option<Vec<u64>>) {
    let records: RidList = (1..=texts.len() as u64).collect();
    let finders: Vec<Vec<u8>> = find::finders(
        TABLE,
        FIELD,
        DataType::Text,
        criterion,
        DateOrder::MonthDayYear,
    )
    .unwrap()
    .into_iter()
    .map(|f| f.bytes)
    .collect();
    let mut root = OwnedNode::default();
    let table = uint_segment(TABLE);
    root.put(&[&table, &[13]], records.to_node(Vec::new()));
    let entries = index
        .iter()
        .map(|(key, ids)| ids.iter().copied().collect::<RidList>().to_node(hex(key)))
        .collect();
    root.put(
        &[&table, &[1], &uint_segment(FIELD), &[0]],
        OwnedNode::list(Vec::new(), entries),
    );
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
    assert_eq!(client.reads(), [(FIELD, IndexKind::Value)]);
    for (i, text) in texts.iter().enumerate() {
        let keys: BTreeSet<Vec<u8>> = record_value_keys(
            DataType::Text,
            &[text.to_string()],
            DateOrder::MonthDayYear,
            form,
        );
        client.add(i as u64 + 1, &[keys]).unwrap();
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
