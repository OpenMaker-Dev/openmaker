//! Missing-index value lists, over synthetic loopback files only.
use super::*;

fn put(root: &mut OwnedNode, field: u64, id: u64, value: &str) {
    root.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &serial::encode(id),
            &uint_segment(field),
        ],
        text_node(&[], value),
    );
}

fn list(root: &mut OwnedNode, occurrence: u64, field: u64) {
    let key = |id| {
        let bytes = uint_segment(id);
        [vec![bytes.len() as u8 + 1, bytes.len() as u8], bytes].concat()
    };
    let mut definition = vec![1, 0, 0];
    definition.extend(key(occurrence));
    definition.extend(key(field));
    definition.extend([0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    definition.extend(om_scsu::encode_counted(" "));
    root.put(
        &[&[0x21], &[5], &[5], &[2]],
        OwnedNode::leaf(Vec::new(), definition),
    );
}

fn literal_index(root: &mut OwnedNode, field: u64, entries: &[(&[u8], &[u64])]) {
    for (key, ids) in entries {
        root.put(
            &[&uint_segment(TABLE), &[1], &uint_segment(field), &[0], key],
            ids.iter().copied().collect::<RidList>().to_node(Vec::new()),
        );
    }
    root.put(
        &[&uint_segment(TABLE), &[1], &uint_segment(field), &[0x10]],
        OwnedNode::leaf(
            Vec::new(),
            [2u64.to_be_bytes(), (entries.len() as u64).to_be_bytes()].concat(),
        ),
    );
}

fn entry_path_handles_missing_and_warm_indexes(field_list: bool) {
    for never_build in [true, false] {
        let tag = format!("vl-entry-{field_list}-{never_build}");
        let file = file_with(&tag, &[], |root| {
            if never_build {
                // Unlike with_late_index_builds (which affects finders
                // only), this prevents CreateValueListEx from building
                // an index. Its response stays literally empty.
                fixture::add_field(root, TABLE, 90, "Never indexed", 1, 1, 0, 0x4000_0000, 0);
                put(root, 90, 1, "Write tests");
                put(root, 90, 2, "Ship");
            }
            list(root, OCCURRENCE, if never_build { 90 } else { NAME });
        });
        let dir = record_dir(&tag);
        let (port, ca) = host_with(&file.0, Some(dir.clone()), Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        let mut get = || {
            if field_list {
                provider.value_list(5)
            } else {
                provider.index_values(tasks, if never_build { "Never indexed" } else { "Name" })
            }
        };
        assert_eq!(get().unwrap(), ["Ship", "Write tests"]);
        let cold = row_downloads(&dir);
        assert_eq!(cold.get(&TABLE), Some(&2));
        assert_eq!(get().unwrap(), ["Ship", "Write tests"]);
        if never_build {
            assert_eq!(
                row_downloads(&dir).get(&TABLE),
                Some(&4),
                "an index that stays absent needs another bounded scan"
            );
        } else {
            assert_eq!(
                row_downloads(&dir),
                cold,
                "warm list must use CreateValueListEx"
            );
        }
        provider.close();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn field_value_lists_handle_never_built_and_warm_indexes() {
    entry_path_handles_missing_and_warm_indexes(true);
}

#[test]
fn insert_from_index_handles_never_built_and_warm_indexes() {
    entry_path_handles_missing_and_warm_indexes(false);
}

#[test]
fn missing_index_agrees_with_literal_native_keys_and_lowest_record_display() {
    for indexed in [false, true] {
        let file = file_with(if indexed { "vl-native" } else { "vl-cold" }, &[], |root| {
            put(root, NAME, 1, "Tést\rHello\nTest\r\n");
            put(root, NAME, 2, "test\nIma");
            list(root, OCCURRENCE, NAME);
            if indexed {
                literal_index(
                    root,
                    NAME,
                    &[
                        (&[1], &[]),
                        (
                            &[
                                1, 0x12, 0xd3, 0x12, 0x6b, 0x13, 0x30, 0x13, 0x30, 0x13, 0x8e,
                            ],
                            &[1],
                        ),
                        (&[1, 0x12, 0xec, 0x13, 0x5f, 0x12, 0x0f], &[2]),
                        (
                            &[1, 0x14, 0x33, 0x12, 0x6b, 0x14, 0x10, 0x14, 0x33],
                            &[1, 2],
                        ),
                    ],
                );
            }
        });
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        assert_eq!(provider.value_list(5).unwrap(), ["Hello", "Ima", "Tést"]);
        provider.close();
    }
}

#[test]
fn repetitions_include_256_and_multiline_values_but_exclude_empty_keys() {
    let file = file_with("vl-repetitions", &[], |root| {
        const FIELD: u64 = 250;
        fixture::add_field(root, TABLE, FIELD, "Repeated", 1, 1, 0, 0, 0);
        fixture::set_field_repetitions(root, TABLE, FIELD, 256);
        put(root, FIELD, 1, "\r\n");
        put(root, FIELD * 256 + 2, 1, "Tést\rHello\nTest");
        put(root, FIELD * 256 + 256, 2, "Ima\nHello\n\u{0301}");
        list(root, OCCURRENCE, FIELD);
    });
    let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca);
    assert_eq!(provider.value_list(5).unwrap(), ["Hello", "Ima", "Tést"]);
    provider.close();
}

#[test]
fn text_forms_use_declared_language_and_unestablished_languages_fail() {
    for (language, expected) in [
        (0, vec![]),
        // System and Unicode standard keep accents (query-program.md 7.2).
        (1, vec!["a", "Test", "tést"]),
        (2, vec!["Test", "a", "tést"]),
        (3, vec!["a", "Test", "tést"]),
        (21, vec!["a", "Test"]),
        (77, vec!["a", "Test"]),
    ] {
        let file = file_with(&format!("vl-language-{language}"), &[], |root| {
            fixture::set_field_language(root, TABLE, NAME, language);
            put(root, NAME, 1, "Test\ra");
            put(root, NAME, 2, "tést");
        });
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        assert_eq!(
            provider.index_values(tasks, "Name").unwrap(),
            expected,
            "language {language}"
        );
        provider.close();
    }
    let file = file_with("vl-unsupported-language", &[], |root| {
        fixture::set_field_language(root, TABLE, NAME, 25)
    });
    let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    let error = provider.index_values(tasks, "Name").unwrap_err();
    assert_eq!(error.code, Some(3));
    assert!(error.message.contains("index language (25)"));
    provider.close();
}

#[test]
fn established_typed_keys_and_file_date_orders_preserve_index_order() {
    for (kind, order, first, second, expected) in [
        (
            2,
            om_collate::DateOrder::MonthDayYear,
            "12",
            "2",
            vec!["2", "12"],
        ),
        (
            3,
            om_collate::DateOrder::DayMonthYear,
            "02/08/2015",
            "17/02/2014",
            vec!["17/02/2014", "02/08/2015"],
        ),
        (
            3,
            om_collate::DateOrder::YearMonthDay,
            "2015-08-02",
            "2014-02-17",
            vec!["2014-02-17", "2015-08-02"],
        ),
        (
            4,
            om_collate::DateOrder::MonthDayYear,
            "21:26:57",
            "00:00:00",
            vec!["00:00:00", "21:26:57"],
        ),
        (
            5,
            om_collate::DateOrder::DayMonthYear,
            "28/10/2026 11:21:33",
            "27/10/2026 21:26:57",
            vec!["27/10/2026 21:26:57", "28/10/2026 11:21:33"],
        ),
    ] {
        let file = file_with(&format!("vl-typed-{kind}-{order:?}"), &[], |root| {
            fixture::add_field(root, TABLE, 90, "Typed", 1, kind, 0, 0, 0);
            let mut locale = om_calc::Locale::fr_fr();
            locale.date_order = order;
            root.put(
                &[&[2], &[13]],
                OwnedNode::leaf(Vec::new(), locale.to_record()),
            );
            put(root, 90, 1, first);
            put(root, 90, 2, second);
        });
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        for attempt in 0..2 {
            assert_eq!(
                provider.index_values(tasks, "Typed").unwrap(),
                expected,
                "attempt {attempt}"
            );
        }
        provider.close();
    }
}

#[test]
fn fallback_rejects_unread_values_instead_of_skipping() {
    // Text that is no value.
    for (i, (kind, value)) in [
        (2, "N/A"),
        (3, "not a date"),
        (5, "not a timestamp"),
        (6, "data"),
    ]
    .into_iter()
    .enumerate()
    {
        let file = file_with(&format!("vl-unsupported-{i}"), &[], |root| {
            fixture::add_field(root, TABLE, 90, "Unknown", 1, kind, 0, 0, 0);
            put(root, 90, 1, value);
        });
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        assert_eq!(
            provider.index_values(tasks, "Unknown").unwrap_err().code,
            Some(3)
        );
        provider.close();
    }
    // Negative and fractional numbers, numbers among other characters and
    // fractions of a second have FileMaker's keys (query-program.md 7.3).
    // A number is read with the file's separators, so the file records
    // its locale.
    for (i, (kind, value)) in [(2, "-2"), (2, "2.5"), (2, "1,5"), (4, "12:00:00.5")]
        .into_iter()
        .enumerate()
    {
        let file = file_with(&format!("vl-keyed-{i}"), &[], |root| {
            fixture::add_field(root, TABLE, 90, "Keyed", 1, kind, 0, 0, 0);
            root.put(
                &[&[2], &[13]],
                OwnedNode::leaf(Vec::new(), om_calc::Locale::en_us().to_record()),
            );
            put(root, 90, 1, value);
        });
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        assert_eq!(provider.index_values(tasks, "Keyed").unwrap(), [value]);
        provider.close();
    }
}

#[test]
fn field_identity_is_base_table_and_field_id_and_bad_references_are_errors() {
    let file = file_with("vl-identity", &[], |root| {
        list(root, fixture::NOTES_OCCURRENCE, fixture::BODY);
        // Same numeric field id as Tasks::Name, different base table.
    });
    let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca);
    assert_eq!(provider.value_list(5).unwrap(), ["Hi"]);
    let tasks = layout(&mut provider, "Tasks");
    assert_eq!(
        provider.index_values(tasks, "missing").unwrap_err().code,
        Some(102)
    );
    provider.close();
    let file = file_with("vl-bad-field", &[], |root| list(root, OCCURRENCE, 999));
    let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca);
    assert_eq!(provider.value_list(5).unwrap_err().code, Some(102));
    provider.close();
}

#[test]
fn scan_crosses_batches_and_later_keys_and_display_ties_remain_deterministic() {
    let rows: Vec<_> = (0..1001)
        .map(|i| (format!("{:04}", 1001 - i), String::new(), String::new()))
        .collect();
    let file = file_with("vl-batches", &rows, |root| {
        put(root, NAME, 1, "Z");
        put(root, NAME, 2, "z");
    });
    let dir = record_dir("vl-batches");
    let (port, ca) = host_with(&file.0, Some(dir.clone()), Server::with_late_index_builds);
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    let values = provider.index_values(tasks, "Name").unwrap();
    assert_eq!(values.len(), 1002);
    assert_eq!(values.first().unwrap(), "0001");
    assert_eq!(values.last().unwrap(), "Z");
    assert_eq!(row_downloads(&dir).get(&TABLE), Some(&1003));
    provider.close();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn empty_fields_have_no_entries_and_existing_empty_index_uses_fast_path() {
    for indexed in [false, true] {
        let file = file_with(
            if indexed {
                "vl-empty-warm"
            } else {
                "vl-empty-cold"
            },
            &[],
            |root| {
                put(root, NAME, 1, "\r\n");
                put(root, NAME, 2, "");
                if indexed {
                    literal_index(root, NAME, &[(&[1], &[1, 2])]);
                }
            },
        );
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        assert!(provider.index_values(tasks, "Name").unwrap().is_empty());
        provider.close();
    }
}

#[test]
fn missing_locale_global_and_unstored_fields_fail_explicitly() {
    for case in [0, 2, 3, 4] {
        let file = file_with(
            &format!("vl-unsupported-metadata-{case}"),
            &[],
            |root| match case {
                0 => {
                    root.remove(&[&[2], &[13]]);
                }
                2 | 3 => fixture::add_field(
                    root,
                    TABLE,
                    90,
                    "Unavailable",
                    1,
                    1,
                    0,
                    if case == 2 { 0x10000 } else { 0x20000 },
                    0,
                ),
                _ => {
                    let mut locale = om_calc::Locale::en_us().to_record();
                    locale[24] = 3;
                    root.put(&[&[2], &[13]], OwnedNode::leaf(Vec::new(), locale));
                }
            },
        );
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        let field = match case {
            0 | 4 => "Due",
            _ => "Unavailable",
        };
        assert_eq!(
            provider.index_values(tasks, field).unwrap_err().code,
            Some(3),
            "case {case}"
        );
        provider.close();
    }
}

/// #340: a field of 5,000 values, two of them long values that share the
/// 219-byte key an index holds for both (query-program.md 7.1). Without an
/// index the client scans the records; once the host has built it, the
/// host's list passes one reply value and comes back cut, and the client
/// reads the index and its records. Both give every value, the long ones
/// apart, in index order.
#[test]
fn a_field_of_thousands_of_values_lists_every_value() {
    let values: Vec<String> = (0..5000).map(|i| format!("Value {i:05}")).collect();
    let rows: Vec<_> = values
        .iter()
        .map(|value| (value.clone(), String::new(), String::new()))
        .collect();
    let (first, second) = ("a".repeat(120), format!("{}b", "a".repeat(129)));
    let file = file_with("vl-large", &rows, |root| {
        put(root, NAME, 1, &first);
        put(root, NAME, 2, &second);
    });
    let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    let expected: Vec<String> = [first, second].into_iter().chain(values).collect();
    for attempt in ["scan", "index"] {
        assert_eq!(
            provider.index_values(tasks, "Name").unwrap(),
            expected,
            "{attempt}"
        );
    }
    provider.close();
}

#[test]
fn malformed_record_text_is_an_error_not_a_missing_value() {
    let file = file_with("vl-bad-text", &[], |root| {
        root.put(
            &[&uint_segment(TABLE), &[5], &[1], &uint_segment(NAME)],
            OwnedNode::leaf(Vec::new(), vec![0x0e ^ 0x5a]),
        ); // truncated SQU
    });
    let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    assert!(provider.index_values(tasks, "Name").is_err());
    provider.close();
}

// The test host omits only the QueryProgram slot terminator.
fn assert_unterminated_host_reply_is_an_error(field_list: bool) {
    let file = file_with(
        &format!("review-malformed-host-{field_list}"),
        &[],
        |root| {
            fixture::add_field(root, TABLE, 90, "Review", 1, 1, 0, 0x4000_0000, 0);
            put(root, 90, 1, "Test");
            put(root, 90, 2, "TEST");
            list(root, OCCURRENCE, 90);
            literal_index(
                root,
                90,
                &[(
                    &[1, 0x14, 0x33, 0x12, 0x6b, 0x14, 0x10, 0x14, 0x33],
                    &[1, 2],
                )],
            );
        },
    );
    let (port, ca) = host_with(&file.0, None, Server::with_unterminated_query_replies);
    let mut p = open(port, &ca);
    let tasks = layout(&mut p, "Tasks");
    let result = if field_list {
        p.value_list(5)
    } else {
        p.index_values(tasks, "Review")
    };
    p.close();
    assert!(
        result.is_err(),
        "unterminated QueryProgram reply must be an error, got {result:?}"
    );
}
#[test]
fn unterminated_host_reply_value_list_is_an_error() {
    assert_unterminated_host_reply_is_an_error(true);
}
#[test]
fn unterminated_host_reply_insert_from_index_is_an_error() {
    assert_unterminated_host_reply_is_an_error(false);
}
