//! Synthetic loopback request counts and cache-coherence checks for lazy
//! related sets. No native files or external services are involved.
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;

use fixture::{BODY, NAME, NOTES, NOTES_LAYOUT, NOTES_OCCURRENCE, OCCURRENCE, SETTING, TABLE};
use om_data::host::{CalcData, CalcNeed};
use om_data::{DataProvider, FieldChanges};
use om_hbam::{serial, slot::uint_segment, OwnedNode};
use related::Host;

const FILE: &str = "Lazy.fmp12";
const ALL: u64 = 0xd0_0040;
const REMOTE: u64 = 0xd0_0041;

fn put(root: &mut OwnedNode, table: u64, record: u64, field: u64, text: &str) {
    root.put(
        &[
            &uint_segment(table),
            &[5],
            &serial::encode(record),
            &uint_segment(field),
        ],
        om_scripts::text_node(&[], text),
    );
}

fn need(record: u64, to: u64) -> CalcNeed {
    CalcNeed::RelatedIds {
        from: OCCURRENCE,
        record,
        to,
    }
}

/// No related records, as an expected list of ids: typed, so that the
/// comparison does not depend on which `PartialEq` impls the build links.
const NO_IDS: [u64; 0] = [];

fn ids(data: &CalcData, record: u64, to: u64) -> Vec<u64> {
    data.related_ids[&(OCCURRENCE, record, to)].to_vec()
}

fn changes(field: &str, text: &str) -> FieldChanges {
    [(field.into(), Some(text.into()))].into()
}

fn matching(field: u64) -> OwnedNode {
    let mut root = fixture::tree(&[], true);
    put(&mut root, NOTES, 1, BODY, "Write tests");
    fixture::set_field_options(&mut root, NOTES, BODY, 0x8000_0000);
    fixture::add_relationship(
        &mut root,
        9,
        (OCCURRENCE, field),
        0,
        (NOTES_OCCURRENCE, BODY),
    );
    root
}

#[test]
fn cartesian_ids_then_visible_batches_avoid_offscreen_bodies_and_reuse_rows() {
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=1000);
    fixture::add_occurrence(&mut root, ALL, "All Tasks", TABLE);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 6, (ALL, NAME));
    let host = Host::new("s4-cartesian", vec![(FILE, root)]);
    let mut lazy = host.open(FILE);
    lazy.calc_schema().unwrap();
    let before = host.captures.row_downloads().len();
    let answer = lazy.calc_data(&[need(1, ALL)]).unwrap();
    assert_eq!(ids(&answer, 1, ALL), (1..=1000).collect::<Vec<_>>());
    assert!(answer.records.is_empty());
    assert!(answer.has(&need(1, ALL)));
    assert!(!answer.has(&CalcNeed::Related {
        from: OCCURRENCE,
        record: 1,
        to: ALL
    }));
    assert_eq!(host.captures.row_downloads().len(), before);
    let rows = |records: &[u64]| {
        records
            .iter()
            .map(|&record| CalcNeed::Record {
                table: TABLE,
                record,
            })
            .collect::<Vec<_>>()
    };
    lazy.calc_data(&rows(&[10, 11])).unwrap();
    assert_eq!(
        host.captures.row_downloads()[before..],
        [(FILE.into(), TABLE, 2)]
    );
    lazy.calc_data(&rows(&[11, 12])).unwrap();
    let after = host.captures.row_downloads().len();
    assert_eq!(
        host.captures.row_downloads()[before..],
        [(FILE.into(), TABLE, 1), (FILE.into(), TABLE, 2)]
    );
    lazy.calc_data(&[need(1, ALL)]).unwrap();
    lazy.calc_data(&rows(&[10, 11, 12])).unwrap();
    assert_eq!(
        host.captures.row_downloads().len(),
        after,
        "both IDs and rows survive later batches"
    );

    // Differential control: the old public need still brings every body.
    let mut eager = host.open(FILE);
    let legacy = CalcNeed::Related {
        from: OCCURRENCE,
        record: 1,
        to: ALL,
    };
    let answer = eager.calc_data(std::slice::from_ref(&legacy)).unwrap();
    assert!(answer.has(&legacy));
    assert_eq!(answer.records.len(), 1000);
    let downloads = host.captures.row_downloads();
    let eager_rows: usize = downloads.iter().map(|(_, _, n)| n).sum::<usize>() - 3;
    assert_eq!(eager_rows, 1000);
    println!("S4 Cartesian: IDs 0 bodies; lazy 2 visible batches / 3 bodies; eager {} batches / {eager_rows} bodies", downloads.len() - after);
    let generation = lazy.calc_data(&[need(1, ALL)]).unwrap().generation;
    lazy.use_window(2);
    assert_eq!(
        lazy.calc_data(&[need(1, ALL)]).unwrap().generation,
        generation,
        "switching found-set context must not invalidate another window's data generation"
    );
    lazy.use_window(1);
    assert_eq!(lazy.calc_data(&rows(&[12])).unwrap().generation, generation);
}

#[test]
fn indexed_then_cartesian_reuses_candidates_and_lookups_between_parents() {
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=1000);
    put(&mut root, TABLE, 2, NAME, "Write tests");
    put(&mut root, NOTES, 1, BODY, "Write tests");
    fixture::set_field_options(&mut root, NOTES, BODY, 0x8000_0000);
    fixture::add_occurrence(&mut root, ALL, "All Tasks", TABLE);
    fixture::add_relationship(
        &mut root,
        9,
        (OCCURRENCE, NAME),
        0,
        (NOTES_OCCURRENCE, BODY),
    );
    fixture::add_relationship(&mut root, 10, (NOTES_OCCURRENCE, BODY), 6, (ALL, NAME));
    for warm in [false, true] {
        let host = Host::new(
            if warm {
                "s4-mixed-warm"
            } else {
                "s4-mixed-cold"
            },
            vec![(FILE, root.clone())],
        );
        let mut provider = host.open(FILE);
        if warm {
            let cold = provider
                .calc_data(&[CalcNeed::Lookup {
                    table: NOTES,
                    field: BODY,
                    text: "Write tests".into(),
                }])
                .unwrap();
            assert!(cold.lookups.values().all(Option::is_none));
        }
        let first = provider.calc_data(&[need(1, ALL)]).unwrap();
        assert_eq!(ids(&first, 1, ALL).len(), 1000);
        let finds = host
            .captures
            .transactions()
            .iter()
            .filter(|(_, t)| t[0] == 86)
            .count();
        assert!(finds > 0);
        provider
            .calc_data(&[CalcNeed::Record {
                table: TABLE,
                record: 100,
            }])
            .unwrap();
        let second = provider.calc_data(&[need(2, ALL)]).unwrap();
        assert_eq!(ids(&first, 1, ALL), ids(&second, 2, ALL));
        provider
            .calc_data(&[CalcNeed::Record {
                table: TABLE,
                record: 101,
            }])
            .unwrap();
        assert_eq!(
            host.captures
                .transactions()
                .iter()
                .filter(|(_, t)| t[0] == 86)
                .count(),
            if warm { finds } else { finds + 1 },
            "successful lookup data is reused; an unavailable cold index is retried"
        );
        assert_eq!(
            host.captures
                .row_downloads()
                .iter()
                .filter(|(_, table, _)| *table == NOTES)
                .map(|(_, _, n)| n)
                .sum::<usize>(),
            1,
            "the intermediate candidate must not download twice"
        );
        assert_eq!(
            host.captures
                .row_downloads()
                .iter()
                .filter(|(_, table, _)| *table == TABLE)
                .map(|(_, _, n)| n)
                .sum::<usize>(),
            4,
            "two parents and two visible rows, no offscreen final bodies"
        );
        if warm {
            println!("S4 mixed indexed/Cartesian after index warmup: 1 lookup, 1 intermediate body, 2 parent bodies, 2 visible bodies across 4 batches");
        }
    }
}

/// A text key holding characters a find reads as operators (a paper's
/// `50#` weight, a decimal width) is looked up on the value index by its
/// own key, not by scanning the related table (#1068): the order's notes
/// cost one find and their own rows, not all 500 notes. A note another
/// client changes to match, or away from it, shows at the next read.
#[test]
fn a_key_with_find_operators_reads_the_index_not_the_table() {
    // Notes validate Body to at most 10 characters.
    const KEY: &str = "50# 25.5in";
    let mut root = matching(NAME);
    for note in 2..=500 {
        put(&mut root, NOTES, note, BODY, &format!("Stock {note}.0"));
    }
    put(&mut root, NOTES, 7, BODY, KEY);
    // Text `=` ignores case, as the value index does.
    put(&mut root, NOTES, 300, BODY, "50# 25.5IN");
    put(&mut root, NOTES, 301, BODY, "50# 25.50");
    put(&mut root, TABLE, 1, NAME, KEY);
    let notes: om_hbam::RidList = (1..=500).collect();
    root.put(&[&uint_segment(NOTES), &[0x0d]], notes.to_node(Vec::new()));
    let host = Host::new("s4-operator-key", vec![(FILE, root)]);
    let mut provider = host.open(FILE);
    // The host builds a value index the first time a find needs it.
    provider
        .calc_data(&[CalcNeed::Lookup {
            table: NOTES,
            field: BODY,
            text: "Write tests".into(),
        }])
        .unwrap();
    let finds = || {
        host.captures
            .with_opcode(om_protocol::Opcode::QueryProgram)
            .len()
    };
    let note_rows = || -> usize {
        host.captures
            .row_downloads()
            .iter()
            .filter(|(_, table, _)| *table == NOTES)
            .map(|(_, _, n)| n)
            .sum()
    };
    let (finds_before, rows_before) = (finds(), note_rows());
    let related = ids(
        &provider.calc_data(&[need(1, NOTES_OCCURRENCE)]).unwrap(),
        1,
        NOTES_OCCURRENCE,
    );
    println!(
        "operator key: {} finds, {} note rows downloaded",
        finds() - finds_before,
        note_rows() - rows_before
    );
    assert_eq!(related, [7, 300]);
    assert_eq!(finds() - finds_before, 1);
    assert_eq!(note_rows() - rows_before, 2, "only the candidates' rows");

    let mut other = host.open(FILE);
    other
        .update_record(NOTES_LAYOUT as u32, 7, changes("Body", "Stock 7.0"))
        .unwrap();
    other
        .update_record(NOTES_LAYOUT as u32, 301, changes("Body", "50# 25.5In"))
        .unwrap();
    drop(other);
    assert_eq!(
        ids(
            &provider.calc_data(&[need(1, NOTES_OCCURRENCE)]).unwrap(),
            1,
            NOTES_OCCURRENCE
        ),
        [300, 301]
    );
}

/// An indexed relationship's related set is put in creation order, the
/// record ids' order (file-metadata.md 4.3), by sorting what the index
/// gives: neither one record's read nor a batch of records' reads the
/// related table's whole record list (#1100). Each set equals its own
/// match, in ascending id order.
#[test]
fn indexed_related_sets_read_no_record_list() {
    let mut root = matching(NAME);
    for note in 2..=300 {
        put(&mut root, NOTES, note, BODY, &format!("Note {}", note % 7));
    }
    let notes: om_hbam::RidList = (1..=300).collect();
    root.put(&[&uint_segment(NOTES), &[0x0d]], notes.to_node(Vec::new()));
    fixture::add_tasks(&mut root, 3..=20);
    for task in 1..=20 {
        put(&mut root, TABLE, task, NAME, &format!("Note {}", task % 7));
    }
    let host = Host::new("s4-no-record-list", vec![(FILE, root)]);
    let mut provider = host.open(FILE);
    // The host builds a value index the first time a find needs it.
    provider
        .calc_data(&[CalcNeed::Lookup {
            table: NOTES,
            field: BODY,
            text: "Write tests".into(),
        }])
        .unwrap();
    let lists = || {
        host.captures
            .with_opcode(om_protocol::Opcode::DownloadWithLock)
            .iter()
            .filter(|(_, t)| {
                om_protocol::parse_download_request(t)
                    .is_ok_and(|d| d.key_path == [uint_segment(NOTES), vec![0x0d]])
            })
            .count()
    };
    let expected =
        |task: u64| -> Vec<u64> { (2..=300).filter(|note| note % 7 == task % 7).collect() };
    let before = lists();
    let one = provider.calc_data(&[need(1, NOTES_OCCURRENCE)]).unwrap();
    assert_eq!(ids(&one, 1, NOTES_OCCURRENCE), expected(1));
    let many: Vec<CalcNeed> = (2..=20).map(|task| need(task, NOTES_OCCURRENCE)).collect();
    let batch = provider.calc_data(&many).unwrap();
    for task in 2..=20 {
        assert_eq!(
            ids(&batch, task, NOTES_OCCURRENCE),
            expected(task),
            "task {task}"
        );
    }
    assert_eq!(lists() - before, 0, "Notes' record list was read");
}

/// A record the open transaction deleted is not related, although the
/// host's index still lists it: the record list the page read keeps
/// without it still filters what the index gives (#1100).
#[test]
fn a_record_the_transaction_deleted_is_not_related() {
    let mut root = fixture::tree(&[], true);
    put(&mut root, TABLE, 1, NAME, "Same");
    put(&mut root, TABLE, 2, NAME, "Same");
    fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
    fixture::add_occurrence(&mut root, ALL, "Same Name", TABLE);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (ALL, NAME));
    let host = Host::new("s4-held-deletion", vec![(FILE, root)]);
    let mut provider = host.open(FILE);
    // The host builds a value index the first time a find needs it.
    provider
        .calc_data(&[CalcNeed::Lookup {
            table: TABLE,
            field: NAME,
            text: "Write tests".into(),
        }])
        .unwrap();
    let same =
        |p: &mut om_provider::NativeProvider| ids(&p.calc_data(&[need(1, ALL)]).unwrap(), 1, ALL);
    assert_eq!(same(&mut provider), [1, 2]);
    provider.begin_transaction(Default::default()).unwrap();
    provider.delete_record(fixture::LAYOUT as u32, 2).unwrap();
    provider
        .records_with_summary(fixture::LAYOUT as u32, 0, 25, &[])
        .unwrap();
    assert_eq!(same(&mut provider), [1]);
    provider.revert_transaction().unwrap();
}

#[test]
fn globals_and_related_table_commit_create_delete_rebuild_membership() {
    let host = Host::new("s4-global-writes", vec![(FILE, matching(SETTING))]);
    let mut provider = host.open(FILE);
    let read = |p: &mut om_provider::NativeProvider| {
        ids(
            &p.calc_data(&[need(1, NOTES_OCCURRENCE)]).unwrap(),
            1,
            NOTES_OCCURRENCE,
        )
    };
    provider
        .set_global(TABLE, SETTING, 1, "Write tests")
        .unwrap();
    assert_eq!(read(&mut provider), [1]);
    provider.set_global(TABLE, SETTING, 1, "Changed").unwrap();
    assert_eq!(read(&mut provider), NO_IDS);
    provider
        .update_record(NOTES_LAYOUT as u32, 1, changes("Body", "Changed"))
        .unwrap();
    assert_eq!(read(&mut provider), [1]);
    let created = provider
        .create_record(NOTES_LAYOUT as u32, changes("Body", "Changed"))
        .unwrap();
    assert_eq!(read(&mut provider), [1, created.id as u64]);
    provider.delete_related_record(NOTES, 1).unwrap();
    assert_eq!(read(&mut provider), [created.id as u64]);
    provider.delete_related_record(NOTES, created.id).unwrap();
    assert_eq!(read(&mut provider), NO_IDS);
}

#[test]
fn parent_key_transaction_commit_and_page_context_refresh_read_new_data() {
    let host = Host::new("s4-context", vec![(FILE, matching(NAME))]);
    let mut provider = host.open(FILE);
    let read = |p: &mut om_provider::NativeProvider| {
        ids(
            &p.calc_data(&[need(1, NOTES_OCCURRENCE)]).unwrap(),
            1,
            NOTES_OCCURRENCE,
        )
    };
    assert_eq!(read(&mut provider), [1]);
    provider
        .update_record(1, 1, changes("Name", "Changed"))
        .unwrap();
    assert_eq!(read(&mut provider), NO_IDS);
    provider
        .begin_transaction(om_data::runtime::TransactionOptions::default())
        .unwrap();
    provider
        .update_record(NOTES_LAYOUT as u32, 1, changes("Body", "Changed"))
        .unwrap();
    assert_eq!(
        read(&mut provider),
        NO_IDS,
        "a held edit is not committed data"
    );
    provider.commit_transaction(false).unwrap();
    assert_eq!(read(&mut provider), [1]);
    let mut other = host.open(FILE);
    other
        .update_record(NOTES_LAYOUT as u32, 1, changes("Body", "Elsewhere"))
        .unwrap();
    provider.records_with_calculations(1, 0, 1, &[]).unwrap();
    assert_eq!(read(&mut provider), NO_IDS);
    other
        .update_record(NOTES_LAYOUT as u32, 1, changes("Body", "Changed"))
        .unwrap();
    provider.use_window(2);
    assert_eq!(read(&mut provider), [1]);
    other
        .update_record(NOTES_LAYOUT as u32, 1, changes("Body", "Elsewhere"))
        .unwrap();
    provider.database_schema().unwrap();
    assert_eq!(
        read(&mut provider),
        NO_IDS,
        "schema refresh drops reads under the previous graph"
    );
}

#[test]
fn related_owning_file_notifications_rebuild_cached_ids() {
    let mut root = fixture::tree(&[], true);
    let mut child = fixture::tree(&[], true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Child");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    fixture::set_field_options(&mut child, TABLE, NAME, 0x8000_0000);
    let host = Host::new("s4-notify", vec![(FILE, root), ("Child.fmp12", child)]);
    let mut reader = host.open(FILE);
    let schema = reader.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    assert_eq!(
        ids(&reader.calc_data(&[need(1, REMOTE)]).unwrap(), 1, REMOTE),
        [1]
    );
    // Both windows have old IDs. Only one will read the change-bearing
    // batch; later replies must also invalidate the other window's cache.
    let old = reader.calc_data(&[need(1, REMOTE)]).unwrap();
    let mut window = old.clone();
    let mut second_window = old;
    let mut writer = host.open("Child.fmp12");
    writer
        .update_record(1, 1, changes("Name", "Changed"))
        .unwrap();
    // An ordinary op92 reply on the owning connection carries the real
    // change notification. Ask for both that row and the previously cached
    // set in the same batch: it must not mix old IDs with fresh bodies.
    let answer = reader
        .calc_data(&[need(1, REMOTE), CalcNeed::Record { table, record: 99 }])
        .unwrap();
    assert_eq!(ids(&answer, 1, REMOTE), NO_IDS);
    assert!(answer.records.contains_key(&(table, 99)));
    assert_ne!(window.generation, answer.generation);
    window.merge(answer);
    assert_eq!(&*window.related_ids[&(OCCURRENCE, 1, REMOTE)], &NO_IDS);
    let late = reader
        .calc_data(&[CalcNeed::Record { table, record: 99 }])
        .unwrap();
    second_window.merge(late);
    assert!(
        !second_window.has(&need(1, REMOTE)),
        "a later reply must evict the second window's old IDs too"
    );
}

#[test]
fn unavailable_ids_are_not_cached_as_empty_or_complete() {
    let mut root = fixture::tree(&[], true);
    related::reference(&mut root, (REMOTE, "Missing"), 1, "Not hosted");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 6, (REMOTE, NAME));
    let host = Host::new("s4-unavailable", vec![(FILE, root)]);
    let mut provider = host.open(FILE);
    for _ in 0..2 {
        let result = provider
            .calc_data(&[
                need(1, REMOTE),
                CalcNeed::Record {
                    table: TABLE,
                    record: 1,
                },
            ])
            .unwrap();
        assert!(result.records.contains_key(&(TABLE, 1)));
        assert!(!result.has(&need(1, REMOTE)));
        assert!(!result.related_ids.contains_key(&(OCCURRENCE, 1, REMOTE)));
    }
}

#[test]
fn a_lost_shared_connection_answers_nothing_from_the_cache() {
    // Root and Child share one connection (#740): losing it fails Root's
    // and Child's reads alike, and the IDs cached for Child's set are not
    // answered as if they were still there.
    let mut root = fixture::tree(&[], true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Child");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    let host = Host::new(
        "s4-lost-cache",
        vec![(FILE, root), ("Child.fmp12", fixture::tree(&[], true))],
    );
    let mut reader = host.open(FILE);
    let schema = reader.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    assert_eq!(
        ids(&reader.calc_data(&[need(1, REMOTE)]).unwrap(), 1, REMOTE),
        [1]
    );
    let clients = host.handle.clients();
    assert_eq!(clients.len(), 1, "one connection for Root and Child");
    assert!(host.handle.disconnect_client(clients[0].id));
    let lost = |result: Result<CalcData, om_data::DataError>| match result {
        Ok(data) => panic!(
            "answered after the loss: {:?}",
            data.records.keys().collect::<Vec<_>>()
        ),
        Err(error) => assert!(
            error
                .message
                .contains("the connection to the host was lost"),
            "{error:?}"
        ),
    };
    lost(reader.calc_data(&[
        need(1, REMOTE),
        CalcNeed::Record {
            table: TABLE,
            record: 1,
        },
        CalcNeed::Record { table, record: 99 },
    ]));
    lost(reader.calc_data(&[need(1, REMOTE)]));
}

fn volatile_related_sort(page_seed: bool) {
    use fixture::{KEY, NOTE_COUNT, TASK};
    use om_data::specs::{SortEntry, SortSpec, SpecField};
    use om_hbam::RidList;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const SORT_NOTES: u64 = 0xd0_0042;
    let millis = || {
        62_135_596_800_000
            + SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
    };
    let mut root = fixture::tree(&[], true);
    fixture::add_calculated(&mut root);
    for (record, task, body) in [(1, "Write tests", "A"), (2, "Ship", "Z")] {
        put(&mut root, NOTES, record, TASK, task);
        put(&mut root, NOTES, record, BODY, body);
    }
    root.put(
        &[&uint_segment(NOTES), &[0x0d]],
        (1..=2).collect::<RidList>().to_node(Vec::new()),
    );
    fixture::add_occurrence(&mut root, ALL, "All Tasks", TABLE);
    fixture::add_occurrence(&mut root, SORT_NOTES, "Sort Notes", NOTES);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 6, (ALL, NAME));
    fixture::add_relationship(&mut root, 10, (ALL, KEY), 0, (SORT_NOTES, TASK));
    let mut names = fixture::calculated_names();
    names.add_occurrence(ALL, "All Tasks", TABLE);
    names.add_occurrence(SORT_NOTES, "Sort Notes", NOTES);
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, ALL),
        KEY,
        5,
        "If ( Get ( CurrentTimeUTCMilliseconds ) < GetAsNumber ( Setting ); Name; If ( Name = \"Write tests\"; \"Ship\"; \"Write tests\" ) )",
    );
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        NOTE_COUNT,
        5,
        "Count ( All Tasks::Name )",
    );
    let mut definition = related::key(OCCURRENCE);
    definition.extend(related::key(ALL));
    definition.extend(
        SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(SORT_NOTES),
                    field: BODY,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            }],
        }
        .encode(),
    );
    definition.extend(
        SortSpec {
            keep_sorted: false,
            blanks_last: false,
            fields: vec![],
        }
        .encode(),
    );
    definition.extend(om_data::meta::relation_option::SORT_LEFT_TO_RIGHT.to_be_bytes());
    root.put(
        &[&[3], &[0xfb], &[5], &[9], &[2]],
        OwnedNode::leaf(Vec::new(), definition),
    );
    let host = Host::new(
        if page_seed {
            "s4-related-sort-page"
        } else {
            "s4-related-sort-batch"
        },
        vec![(FILE, root)],
    );
    let mut provider = host.open(FILE);
    provider.calc_schema().unwrap();
    // Start the before/after interval after opening and schema hydration.
    // Only the clock changes once the ordered set has been warmed.
    let deadline = millis() + 3000;
    provider
        .set_global(TABLE, SETTING, 1, &deadline.to_string())
        .unwrap();
    if page_seed {
        let page = provider
            .records_with_calculations(1, 0, 1, &["Notes".into()])
            .unwrap();
        let field = page.fields.iter().position(|f| f.name == "Notes").unwrap();
        assert_eq!(page.records[0].values[field].as_deref(), Some("2"));
    }
    assert_eq!(
        ids(&provider.calc_data(&[need(1, ALL)]).unwrap(), 1, ALL),
        [1, 2]
    );
    let now = millis();
    assert!(now < deadline, "fixture failed to observe its before phase");
    std::thread::sleep(Duration::from_millis((deadline - now + 25) as u64));
    let direct = provider
        .calc_data(&[
            CalcNeed::RelatedIds {
                from: ALL,
                record: 1,
                to: SORT_NOTES,
            },
            CalcNeed::RelatedIds {
                from: ALL,
                record: 2,
                to: SORT_NOTES,
            },
        ])
        .unwrap();
    assert_eq!(direct.related_ids[&(ALL, 1, SORT_NOTES)].as_ref(), &[2]);
    assert_eq!(direct.related_ids[&(ALL, 2, SORT_NOTES)].as_ref(), &[1]);
    let mut fresh = host.open(FILE);
    fresh
        .set_global(TABLE, SETTING, 1, &deadline.to_string())
        .unwrap();
    assert_eq!(
        ids(&fresh.calc_data(&[need(1, ALL)]).unwrap(), 1, ALL),
        [2, 1]
    );
    let before = host.captures.row_downloads().len();
    assert_eq!(
        ids(&provider.calc_data(&[need(1, ALL)]).unwrap(), 1, ALL),
        [2, 1],
        "a stored sort field reached through a volatile relationship must be resolved again",
    );
    assert_eq!(
        host.captures.row_downloads().len(),
        before,
        "recomputing the order still reuses downloaded candidate and sort-field rows",
    );
}

#[test]
fn related_field_sort_after_batch_snapshot() {
    volatile_related_sort(false);
}

#[test]
fn related_field_sort_after_initial_page_snapshot() {
    volatile_related_sort(true);
}

#[test]
fn refresh_window_rereads_warm_owning_rows_without_reopening_files() {
    let mut root = fixture::tree(&[], true);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Child");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 6, (REMOTE, NAME));
    let host = Host::new(
        "refresh-rows",
        vec![(FILE, root), ("Child.fmp12", fixture::tree(&[], true))],
    );
    let mut reader = host.open(FILE);
    let schema = reader.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let row = CalcNeed::Record { table, record: 1 };
    let mut window = reader.calc_data(&[need(1, REMOTE), row.clone()]).unwrap();
    let generation = window.generation;
    let opened = host.captures.opens_and_logins();
    let mut writer = host.open("Child.fmp12");
    writer
        .update_record(1, 1, changes("Name", "Fresh remote"))
        .unwrap();
    // Reusing warm data first takes the host's notifications with one
    // HostTimeStamp (op82), so the change shows without a refresh (#1067).
    let warm = reader.calc_data(std::slice::from_ref(&row)).unwrap();
    assert_ne!(
        warm.generation, generation,
        "the warm read takes the change's notification"
    );
    assert!(
        matches!(warm.records[&(table, 1)].as_ref().unwrap().get(NAME, 1),
        Some(om_data::calc::Value::Text(text)) if text.as_str() == "Fresh remote")
    );
    let connections = host.captures.opens_and_logins();
    assert_ne!(
        opened, connections,
        "only the independent writer opened a connection"
    );
    // External flush is harmless without ODBC. Ordinary refresh still expires
    // hosted peer row bodies, including under imported client-only table IDs.
    let downloads = host.captures.row_downloads().len();
    reader.refresh_window(false, true).unwrap();
    let fresh = reader.calc_data(std::slice::from_ref(&row)).unwrap();
    assert!(
        host.captures.row_downloads().len() > downloads,
        "refresh reads the warm row again"
    );
    assert_ne!(fresh.generation, warm.generation);
    assert!(
        matches!(fresh.records[&(table, 1)].as_ref().unwrap().get(NAME, 1),
        Some(om_data::calc::Value::Text(text)) if text.as_str() == "Fresh remote")
    );
    window.merge(fresh);
    assert!(
        !window.has(&need(1, REMOTE)),
        "a new provider generation replaces older client answers"
    );
    assert!(std::sync::Arc::ptr_eq(
        &schema,
        &reader.calc_schema().unwrap().unwrap()
    ));
    assert_eq!(host.captures.opens_and_logins(), connections);
}

#[test]
fn refresh_window_flushes_join_candidates_but_keeps_found_order_and_held_edits() {
    let host = Host::new("refresh-joins", vec![(FILE, matching(NAME))]);
    let mut reader = host.open(FILE);
    let query = need(1, NOTES_OCCURRENCE);
    assert_eq!(
        ids(
            &reader.calc_data(std::slice::from_ref(&query)).unwrap(),
            1,
            NOTES_OCCURRENCE
        ),
        [1]
    );
    let downloads = host.captures.row_downloads().len();
    reader.refresh_window(false, true).unwrap();
    assert_eq!(
        ids(
            &reader.calc_data(std::slice::from_ref(&query)).unwrap(),
            1,
            NOTES_OCCURRENCE
        ),
        [1]
    );
    assert_eq!(
        host.captures.row_downloads().len(),
        downloads,
        "ordinary/external refresh can reuse native join IDs"
    );
    let mut writer = host.open(FILE);
    writer
        .update_record(NOTES_LAYOUT as u32, 1, changes("Body", "Off"))
        .unwrap();
    // The warm read matches against the scan it kept, so it first takes the
    // host's notifications with one HostTimeStamp (op82) and sees the
    // membership change without a refresh (#804).
    assert_eq!(
        ids(
            &reader.calc_data(std::slice::from_ref(&query)).unwrap(),
            1,
            NOTES_OCCURRENCE
        ),
        NO_IDS
    );
    reader.refresh_window(true, false).unwrap();
    assert_eq!(
        ids(&reader.calc_data(&[query]).unwrap(), 1, NOTES_OCCURRENCE),
        NO_IDS
    );

    use om_data::found::FoundSetChange;
    reader.use_window(7);
    reader
        .change_found_set(1, FoundSetChange::Records(vec![2, 1]))
        .unwrap();
    let sorted = reader
        .change_found_set(
            1,
            FoundSetChange::Sort(vec![om_data::found::SortField::new(
                "Count",
                om_data::found::SortOrder::Descending,
            )]),
        )
        .unwrap();
    assert!(sorted.sorted);
    reader
        .begin_transaction(om_data::runtime::TransactionOptions::default())
        .unwrap();
    reader
        .update_record(1, 2, changes("Name", "Held edit"))
        .unwrap();
    reader.refresh_window(true, true).unwrap();
    let page = reader.records_with_calculations(1, 0, 2, &[]).unwrap();
    assert_eq!(
        page.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        [2, 1]
    );
    let found = reader
        .calc_data(&[CalcNeed::FoundSet {
            occurrence: OCCURRENCE,
            table: TABLE,
        }])
        .unwrap();
    assert_eq!(&*found.found[&OCCURRENCE], &[2, 1]);
    assert_eq!(&*found.sorts[&OCCURRENCE], &[fixture::COUNT]);
    let name = page.fields.iter().position(|f| f.name == "Name").unwrap();
    assert_eq!(page.records[0].values[name].as_deref(), Some("Held edit"));
    assert_eq!(
        writer.records(1, 1, 1).unwrap().records[0].values[name].as_deref(),
        Some("Ship"),
        "refresh did not commit the held edit"
    );
    reader.revert_transaction().unwrap();
    reader.use_window(8);
    assert_eq!(
        reader
            .records_with_calculations(1, 0, 2, &[])
            .unwrap()
            .records
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[test]
fn targeted_refresh_rebuilds_native_join_and_imported_bodies_without_reopening_or_writing() {
    let mut root = matching(NAME);
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Child");
    fixture::add_relationship(&mut root, 10, (OCCURRENCE, NAME), 6, (REMOTE, NAME));
    let host = Host::new(
        "refresh-object",
        vec![(FILE, root), ("Child.fmp12", fixture::tree(&[], true))],
    );
    let mut reader = host.open(FILE);
    let schema = reader.calc_schema().unwrap().unwrap();
    let table = schema.table_of(REMOTE).unwrap().id;
    let row = CalcNeed::Record { table, record: 1 };
    let warm = reader
        .calc_data(&[need(1, NOTES_OCCURRENCE), need(1, REMOTE), row.clone()])
        .unwrap();
    assert_eq!(ids(&warm, 1, NOTES_OCCURRENCE), [1]);
    let mut writer = host.open("Child.fmp12");
    writer
        .update_record(1, 1, changes("Name", "Refreshed peer"))
        .unwrap();
    let mut root_writer = host.open(FILE);
    root_writer
        .update_record(NOTES_LAYOUT as u32, 1, changes("Body", "Off"))
        .unwrap();
    let connections = host.captures.opens_and_logins();
    let generation = reader
        .refresh_object(Some((OCCURRENCE, 1, NOTES_OCCURRENCE)))
        .unwrap();
    assert_ne!(generation, warm.generation);
    let fresh = reader.calc_data(&[need(1, NOTES_OCCURRENCE), row]).unwrap();
    assert_eq!(ids(&fresh, 1, NOTES_OCCURRENCE), NO_IDS);
    assert!(
        matches!(fresh.records[&(table, 1)].as_ref().unwrap().get(NAME, 1), Some(om_data::calc::Value::Text(text)) if text.as_str() == "Refreshed peer")
    );
    assert_eq!(host.captures.opens_and_logins(), connections);
    assert!(std::sync::Arc::ptr_eq(
        &schema,
        &reader.calc_schema().unwrap().unwrap()
    ));
}

/// A relationship of an indexed `=` and a date range (`<=`, `>=`), as a
/// layout's conditional formats follow to a large related table: Tasks to
/// itself through Group = Group, Start <= Due and End >= Due, over 2,000
/// records. The value index on Group narrows the candidates to one group;
/// every predicate then decides on those, as the scan would.
#[test]
fn an_equal_and_a_date_range_read_only_the_equal_candidates() {
    const GROUP: u64 = 40;
    const START: u64 = 41;
    const END: u64 = 42;
    const RECORDS: u64 = 2000;
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=RECORDS);
    fixture::add_field(&mut root, TABLE, GROUP, "Group", 1, 1, 0, 0x8000_0000, 0);
    fixture::add_field(&mut root, TABLE, START, "Start", 1, 3, 0, 0, 0);
    fixture::add_field(&mut root, TABLE, END, "End", 1, 3, 0, 0, 0);
    let due = |id: u64| id % 28 + 1;
    for id in 1..=RECORDS {
        put(&mut root, TABLE, id, GROUP, &format!("G{}", id % 10));
        put(
            &mut root,
            TABLE,
            id,
            fixture::DUE,
            &format!("1/{}/2026", due(id)),
        );
    }
    put(&mut root, TABLE, 1, START, "1/5/2026");
    put(&mut root, TABLE, 1, END, "1/10/2026");
    fixture::add_occurrence(&mut root, REMOTE, "Same Group", TABLE);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, GROUP), 0, (REMOTE, GROUP));
    for (n, op, left) in [(2u8, 3u8, START), (3, 5, END)] {
        let predicate = [vec![op], related::key(left), related::key(fixture::DUE)].concat();
        root.put(
            &[&[3], &[0xfb], &[5], &[9], &[3], &[n]],
            OwnedNode::leaf(Vec::new(), predicate),
        );
    }
    let expected: Vec<u64> = (1..=RECORDS)
        .filter(|id| id % 10 == 1 && (5..=10).contains(&due(*id)))
        .collect();
    let host = Host::new("range-index", vec![(FILE, root)]);
    let mut provider = host.open(FILE);
    // The host builds Group's value index on the first find (FileMaker
    // Server's are on disk already); the find then answers.
    provider
        .calc_data(&[CalcNeed::Lookup {
            table: TABLE,
            field: GROUP,
            text: "G1".into(),
        }])
        .unwrap();
    let started = std::time::Instant::now();
    let data = provider.calc_data(&[need(1, REMOTE)]).unwrap();
    let elapsed = started.elapsed();
    let bodies: usize = host
        .captures
        .row_downloads()
        .iter()
        .filter(|(_, table, _)| *table == TABLE)
        .map(|(_, _, n)| n)
        .sum();
    let finds = host
        .captures
        .transactions()
        .iter()
        .filter(|(_, t)| t[0] == 86)
        .count();
    println!(
        "RANGE related {} (expected {}), Tasks bodies downloaded {bodies}, finds {finds}, {:.1} ms",
        ids(&data, 1, REMOTE).len(),
        expected.len(),
        elapsed.as_secs_f64() * 1000.0
    );
    assert_eq!(ids(&data, 1, REMOTE), expected);
    // Group G1's 200 records, the parent among them; the scan read 2,000.
    assert!(bodies <= 200, "{bodies} Tasks bodies downloaded");
}

/// #982: a footer summary over the found set reads one related set per
/// record, all in one pass, and the window asks for them in one call
/// (`take_wanted`). Each was answered with its own find and its own
/// download, two requests per record; they are now read together. Tasks
/// related to Notes through Name = Body (value-indexed), 3,000 records
/// each, one need per Tasks record.
#[test]
fn a_found_sets_related_sets_are_read_in_few_requests() {
    const RECORDS: u64 = 3000;
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=RECORDS);
    put(&mut root, TABLE, 1, NAME, "Task 1");
    put(&mut root, TABLE, 2, NAME, "Task 2");
    let mut notes = om_hbam::RidList::new();
    for id in 1..=RECORDS {
        put(&mut root, NOTES, id, BODY, &format!("Task {id}"));
        notes.insert(id);
    }
    root.put(&[&uint_segment(NOTES), &[0x0d]], notes.to_node(Vec::new()));
    root.put(
        &[&uint_segment(NOTES), &[0]],
        OwnedNode::leaf(Vec::new(), {
            let last = serial::encode(RECORDS);
            [vec![last.len() as u8], last].concat()
        }),
    );
    fixture::set_field_options(&mut root, NOTES, BODY, 0x8000_0000);
    fixture::add_relationship(
        &mut root,
        9,
        (OCCURRENCE, NAME),
        0,
        (NOTES_OCCURRENCE, BODY),
    );
    let host = Host::new("found-related", vec![(FILE, root)]);
    let mut provider = host.open(FILE);
    // The host builds Body's value index on the first find; the window has
    // the found set's records already (the summary read them first).
    provider
        .calc_data(&[CalcNeed::Lookup {
            table: NOTES,
            field: BODY,
            text: "Task 1".into(),
        }])
        .unwrap();
    let records: Vec<CalcNeed> = (1..=RECORDS)
        .map(|record| CalcNeed::Record {
            table: TABLE,
            record,
        })
        .collect();
    provider.calc_data(&records).unwrap();
    let before = host.captures.transactions().len();
    let needs: Vec<CalcNeed> = (1..=RECORDS)
        .map(|record| need(record, NOTES_OCCURRENCE))
        .collect();
    let started = std::time::Instant::now();
    let data = provider.calc_data(&needs).unwrap();
    let elapsed = started.elapsed();
    let sent = host.captures.transactions();
    let sent = &sent[before..];
    let count = |op: u8| sent.iter().filter(|(_, t)| t[0] == op).count();
    println!(
        "FOUND-RELATED {RECORDS} needs: {} requests ({} op86, {} op92), {:.0} ms",
        sent.len(),
        count(86),
        count(92),
        elapsed.as_secs_f64() * 1000.0
    );
    let mut by: std::collections::BTreeMap<u8, usize> = Default::default();
    for (_, t) in sent {
        *by.entry(t[0]).or_default() += 1;
    }
    println!("FOUND-RELATED by op {by:?}");
    for record in 1..=RECORDS {
        assert_eq!(ids(&data, record, NOTES_OCCURRENCE), [record]);
    }
    // A join and a download per 256 records, and the record list: each
    // record had its own find and download (6,001 requests).
    assert!(sent.len() <= 30, "{by:?}");
}

/// What one call reads for many records' related sets together (#982) is
/// what each record's own read gives: through a text `=` (keys in other
/// case, empty, with a find operator or longer than the index holds, which
/// the index path leaves to the scan) and a number `=` (fractions near the
/// neighborhood's bounds, zero, the largest whole number a find takes),
/// over 300 records, two batches.
#[test]
fn related_sets_read_together_equal_each_records_own() {
    const KEY: u64 = 41;
    const NUM: u64 = 40;
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=300);
    fixture::add_field(&mut root, TABLE, KEY, "Key", 1, 1, 0, 0, 0);
    fixture::add_field(&mut root, NOTES, NUM, "Num", 1, 2, 0, 0x8000_0000, 0);
    let long = "A".repeat(110);
    let keys = ["Alpha", "alpha", "Beta", "", "A.B", "Gamma", long.as_str()];
    let largest = (i64::MAX - 1).to_string();
    for record in 1..=300u64 {
        put(&mut root, TABLE, record, KEY, keys[(record % 7) as usize]);
        let count = if record % 50 == 7 {
            largest.clone()
        } else {
            (record % 10).to_string()
        };
        put(&mut root, TABLE, record, fixture::COUNT, &count);
    }
    let bodies = ["Alpha", "ALPHA", "beta", "A.B", "Delta", long.as_str()];
    let numbers = [
        "1",
        "2",
        "2.00000000000000001",
        "1.99999999999999999",
        "3.5",
        "0",
        "9",
    ];
    let mut notes = om_hbam::RidList::new();
    for record in 1..=200u64 {
        put(
            &mut root,
            NOTES,
            record,
            BODY,
            bodies[(record % 6) as usize],
        );
        let number = if record % 50 == 7 {
            largest.as_str()
        } else {
            numbers[(record % 7) as usize]
        };
        put(&mut root, NOTES, record, NUM, number);
        notes.insert(record);
    }
    root.put(&[&uint_segment(NOTES), &[0x0d]], notes.to_node(Vec::new()));
    root.put(
        &[&uint_segment(NOTES), &[0]],
        OwnedNode::leaf(Vec::new(), {
            let last = serial::encode(200);
            [vec![last.len() as u8], last].concat()
        }),
    );
    fixture::set_field_options(&mut root, NOTES, BODY, 0x8000_0000);
    fixture::add_occurrence(&mut root, REMOTE, "Notes by Num", NOTES);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, KEY), 0, (NOTES_OCCURRENCE, BODY));
    fixture::add_relationship(
        &mut root,
        10,
        (OCCURRENCE, fixture::COUNT),
        0,
        (REMOTE, NUM),
    );
    let warm = |provider: &mut om_provider::NativeProvider| {
        for (field, text) in [(BODY, "Alpha"), (NUM, "1")] {
            provider
                .calc_data(&[CalcNeed::Lookup {
                    table: NOTES,
                    field,
                    text: text.into(),
                }])
                .unwrap();
        }
    };
    let needs: Vec<CalcNeed> = (1..=300u64)
        .flat_map(|record| [need(record, NOTES_OCCURRENCE), need(record, REMOTE)])
        .collect();
    let together = Host::new("related-together", vec![(FILE, root.clone())]);
    let mut provider = together.open(FILE);
    warm(&mut provider);
    let before = together.captures.transactions().len();
    let data = provider.calc_data(&needs).unwrap();
    let requests = together.captures.transactions().len() - before;
    let alone = Host::new("related-alone", vec![(FILE, root)]);
    let mut each = alone.open(FILE);
    warm(&mut each);
    let mut related = 0;
    for need in &needs {
        let CalcNeed::RelatedIds { record, to, .. } = need else {
            unreachable!()
        };
        let own = each.calc_data(std::slice::from_ref(need)).unwrap();
        assert_eq!(
            ids(&data, *record, *to),
            ids(&own, *record, *to),
            "record {record} to {to:#x}"
        );
        related += ids(&own, *record, *to).len();
    }
    println!("TOGETHER 600 needs: {requests} requests, {related} related records in all");
    assert!(related > 0);
    assert!(requests < 600, "{requests} requests");
}

/// A batch of records whose keys do not fit one request is joined in
/// parts (#982): 256 records with 20 keys of about 100 characters each
/// would make a program larger than a request takes. Each record still
/// finds its own note.
#[test]
fn a_join_too_large_for_one_request_is_sent_in_parts() {
    const KEY: u64 = 41;
    const RECORDS: u64 = 256;
    let key =
        |record: u64, repetition: u64| format!("{}{record:03}{repetition:02}", "x".repeat(98));
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=RECORDS);
    fixture::add_field(&mut root, TABLE, KEY, "Key", 1, 1, 0, 0, 0);
    fixture::set_field_repetitions(&mut root, TABLE, KEY, 20);
    let mut notes = om_hbam::RidList::new();
    for record in 1..=RECORDS {
        for repetition in 1..=20u64 {
            let cell = if repetition == 1 {
                KEY
            } else {
                KEY * 256 + repetition
            };
            root.put(
                &[
                    &uint_segment(TABLE),
                    &[5],
                    &serial::encode(record),
                    &uint_segment(cell),
                ],
                om_scripts::text_node(&[], &key(record, repetition)),
            );
        }
        put(&mut root, NOTES, record, BODY, &key(record, 20));
        notes.insert(record);
    }
    root.put(&[&uint_segment(NOTES), &[0x0d]], notes.to_node(Vec::new()));
    root.put(
        &[&uint_segment(NOTES), &[0]],
        OwnedNode::leaf(Vec::new(), {
            let last = serial::encode(RECORDS);
            [vec![last.len() as u8], last].concat()
        }),
    );
    fixture::set_field_options(&mut root, NOTES, BODY, 0x8000_0000);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, KEY), 0, (NOTES_OCCURRENCE, BODY));
    let host = Host::new("join-parts", vec![(FILE, root)]);
    let mut provider = host.open(FILE);
    provider
        .calc_data(&[CalcNeed::Lookup {
            table: NOTES,
            field: BODY,
            text: key(1, 20).into(),
        }])
        .unwrap();
    let needs: Vec<CalcNeed> = (1..=RECORDS)
        .map(|record| need(record, NOTES_OCCURRENCE))
        .collect();
    let before = host.captures.transactions().len();
    let data = provider.calc_data(&needs).unwrap();
    let joins = host.captures.transactions()[before..]
        .iter()
        .filter(|(_, t)| t[0] == 86)
        .count();
    for record in 1..=RECORDS {
        assert_eq!(ids(&data, record, NOTES_OCCURRENCE), [record]);
    }
    // Two or more joins, not one find per record.
    assert!((2..16).contains(&joins), "{joins} op86 requests");
}

/// A window's read stops when the window no longer needs it (#987): a
/// token cancelled before the read sends nothing; one cancelled while the
/// read goes on (once the host has seen 200 requests) stops it at the next
/// request boundary, and the answer is superseded as a whole. Nothing is marked missing or failed by it: the next read gives
/// every set. Dates relate Tasks to Notes, which each record reads on its
/// own (a find and a download each), 3,000 of them.
#[test]
fn a_cancelled_read_stops_between_requests_and_fails_nothing() {
    use om_data::host::CalcCancel;
    const RECORDS: u64 = 3000;
    const WHEN: u64 = 42;
    let date = |record: u64| {
        format!(
            "{}/{}/{}",
            (record / 28) % 12 + 1,
            record % 28 + 1,
            1900 + record / 336
        )
    };
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=RECORDS);
    fixture::add_field(&mut root, NOTES, WHEN, "When", 1, 3, 0, 0x8000_0000, 0);
    let mut notes = om_hbam::RidList::new();
    for record in 1..=RECORDS {
        put(&mut root, TABLE, record, fixture::DUE, &date(record));
        put(&mut root, NOTES, record, WHEN, &date(record));
        notes.insert(record);
    }
    root.put(&[&uint_segment(NOTES), &[0x0d]], notes.to_node(Vec::new()));
    root.put(
        &[&uint_segment(NOTES), &[0]],
        OwnedNode::leaf(Vec::new(), {
            let last = serial::encode(RECORDS);
            [vec![last.len() as u8], last].concat()
        }),
    );
    fixture::add_relationship(
        &mut root,
        9,
        (OCCURRENCE, fixture::DUE),
        0,
        (NOTES_OCCURRENCE, WHEN),
    );
    let host = Host::new("cancelled-read", vec![(FILE, root)]);
    let mut provider = host.open(FILE);
    provider
        .calc_data(&[CalcNeed::Lookup {
            table: NOTES,
            field: WHEN,
            text: date(1).into(),
        }])
        .unwrap();
    let records: Vec<CalcNeed> = (1..=RECORDS)
        .map(|record| CalcNeed::Record {
            table: TABLE,
            record,
        })
        .collect();
    provider.calc_data(&records).unwrap();
    let needs: Vec<CalcNeed> = (1..=RECORDS)
        .map(|record| need(record, NOTES_OCCURRENCE))
        .collect();
    let sent = || host.captures.transactions().len();

    // Cancelled before it starts: nothing is sent.
    let cancel = CalcCancel::default();
    cancel.cancel();
    let before = sent();
    let data = provider.calc_data_for(&needs, &cancel).unwrap();
    assert!(data.superseded);
    assert_eq!(sent(), before);

    // Cancelled once the host has seen 200 of its requests: it stops at
    // the next request boundary, long before its 6,000.
    let cancel = CalcCancel::default();
    let stop = cancel.clone();
    let before = sent();
    // Captures removes its folder when dropped: the host's must stay.
    let watched = std::mem::ManuallyDrop::new(related::Captures(host.captures.0.clone()));
    let stopper = std::thread::spawn(move || loop {
        // A connection's recording may not exist yet while it is read.
        let Ok(seen) = std::panic::catch_unwind(|| watched.transactions().len()) else {
            continue;
        };
        if seen >= before + 200 {
            stop.cancel();
            return seen;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    });
    let data = provider.calc_data_for(&needs, &cancel).unwrap();
    let at_cancel = stopper.join().unwrap();
    let after_cancel = sent() - at_cancel;
    assert!(data.superseded);
    assert!(data.related_ids.is_empty() && data.failures.is_empty());
    // The one in flight, and any sent while the barrier looked.
    assert!(
        after_cancel <= 8,
        "{after_cancel} requests after the cancel"
    );
    let stopped_after = sent() - before;

    // Nothing was marked missing or failed: the full read answers all.
    let data = provider.calc_data(&needs).unwrap();
    assert!(!data.superseded);
    for record in 1..=RECORDS {
        assert_eq!(ids(&data, record, NOTES_OCCURRENCE), [record]);
    }
    println!("CANCELLED read stopped after {stopped_after} requests");
}
