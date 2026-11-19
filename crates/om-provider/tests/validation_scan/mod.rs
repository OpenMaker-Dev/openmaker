//! A partial op92 copy must not turn unknown stored values into absence.
use super::*;

fn omitted_duplicate(omit_cell: bool) {
    for warm in [false, true] {
        let mut root = fixture::tree(&[], true);
        fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
        fixture::set_field_validation(&mut root, TABLE, NAME, 0x410);
        let file = TempFile::from_tree("validation-omitted-duplicate", &root);
        let (port, ca) = host_with(&file.0, None, |server| {
            let mut path = vec![uint_segment(TABLE), vec![5], serial::encode(2)];
            if omit_cell {
                path.push(uint_segment(NAME));
            }
            let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            server
                .with_late_index_builds()
                .omitting_list_download(&refs)
        });
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        if warm {
            build_indexes(&mut provider, &[(TABLE, NAME)]);
        }
        let mut raw = raw_session(port);
        let original = raw.download(&[&uint_segment(TABLE), &[5], &[2]]).unwrap();
        let result = provider.update_record(tasks, 1, values(&[("Name", "SHIP")]));
        assert_eq!(result.unwrap_err().code, Some(504), "warm={warm}");
        let after = raw.download(&[&uint_segment(TABLE), &[5], &[2]]).unwrap();
        assert_eq!(original.slots, after.slots, "the duplicate still exists");
        // Refusal must also release the record and uniqueness locks.
        assert!(matches!(
            raw.lock(&[&uint_segment(TABLE), &[1], &[1]], 900, false)
                .unwrap(),
            om_protocol::write::LockReply::Granted { .. }
        ));
        raw.kill_all_locks(900).unwrap();
        raw.disconnect().unwrap();
        provider.close();
    }
}

#[test]
fn omitted_rows_cannot_hide_duplicates() {
    omitted_duplicate(false);
}

#[test]
fn omitted_source_cells_cannot_hide_duplicates() {
    omitted_duplicate(true);
}

#[test]
fn a_refused_confirmation_cannot_establish_unique_absence() {
    let mut root = fixture::tree(&[], true);
    fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
    fixture::set_field_validation(&mut root, TABLE, NAME, 0x410);
    let file = TempFile::from_tree("validation-refused-confirmation", &root);
    let (port, ca) = host_with(&file.0, None, |server| {
        let path = [uint_segment(TABLE), vec![5], serial::encode(2)];
        let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        server
            .with_late_index_builds()
            .omitting_list_download(&refs)
            .refusing_downloads(&refs)
    });
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    let before = all_records(&mut provider, tasks);
    assert!(provider
        .update_record(tasks, 1, values(&[("Name", "SHIP")]))
        .is_err());
    assert_eq!(all_records(&mut provider, tasks), before);
    let mut raw = raw_session(port);
    assert!(matches!(
        raw.lock(&[&uint_segment(TABLE), &[1], &[1]], 900, false)
            .unwrap(),
        om_protocol::write::LockReply::Granted { .. }
    ));
    raw.kill_all_locks(900).unwrap();
    raw.disconnect().unwrap();
    provider.close();
}

#[test]
fn unreadable_records_are_not_empty_source_values() {
    for shape in ["empty", "terminal", "empty-terminal"] {
        let mut root = fixture::tree(&[], true);
        validation_list(&mut root, fixture::NOTES_OCCURRENCE, fixture::BODY);
        let mut node = OwnedNode::list(Vec::new(), Vec::new());
        if shape != "empty" {
            node.value = Some(if shape == "terminal" {
                vec![0xaa]
            } else {
                vec![]
            });
            node.push = false;
        }
        root.put(&[&uint_segment(fixture::NOTES), &[5], &[1]], node);
        let file = TempFile::from_tree("validation-record-shape", &root);
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        let result = provider.update_record(tasks, 1, values(&[("Code", "\u{301}")]));
        assert_eq!(
            result.as_ref().err().and_then(|e| e.code),
            (shape != "empty").then_some(3),
            "shape={shape}: {result:?}"
        );
        provider.close();
    }
}

#[test]
fn genuinely_deleted_rows_are_skipped_and_incomplete_cells_are_refused() {
    for partial in [false, true] {
        let mut root = fixture::tree(&[], true);
        fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
        fixture::set_field_validation(&mut root, TABLE, NAME, 0x410);
        if partial {
            root.put(
                &[&uint_segment(TABLE), &[5], &[2], &[1]],
                OwnedNode::list(
                    Vec::new(),
                    vec![
                        OwnedNode::leaf(vec![0], 4u32.to_be_bytes().to_vec()),
                        OwnedNode::leaf(vec![1], om_scsu::encode_scsu("Sh")),
                    ],
                ),
            );
        } else {
            // The master list still names record 2; both op92 and op90 lack it.
            root.remove(&[&uint_segment(TABLE), &[5], &[2]]);
        }
        let file = TempFile::from_tree("validation-disappeared-row", &root);
        let (port, ca) = host_with(&file.0, None, Server::with_late_index_builds);
        let mut provider = open(port, &ca);
        let tasks = layout(&mut provider, "Tasks");
        let result = provider.update_record(tasks, 1, values(&[("Name", "Ship")]));
        assert_eq!(
            result.as_ref().err().and_then(|e| e.code),
            partial.then_some(3),
            "partial={partial}: {result:?}"
        );
        provider.close();
    }
}

#[test]
fn omitted_nonempty_cells_cannot_satisfy_empty_value_list_membership() {
    let mut root = fixture::tree(&[], true);
    validation_list(&mut root, fixture::NOTES_OCCURRENCE, fixture::BODY);
    let file = TempFile::from_tree("validation-omitted-empty-key", &root);
    let (port, ca) = host_with(&file.0, None, |server| {
        server.with_late_index_builds().omitting_list_download(&[
            &uint_segment(fixture::NOTES),
            &[5],
            &[1],
            &uint_segment(fixture::BODY),
        ])
    });
    let mut provider = open(port, &ca);
    let tasks = layout(&mut provider, "Tasks");
    let before = all_records(&mut provider, tasks);
    assert!(provider
        .update_record(tasks, 1, values(&[("Code", "\u{301}")]))
        .is_err());
    assert_eq!(all_records(&mut provider, tasks), before);
    provider.close();
}

fn literal_source_index(root: &mut OwnedNode, table: u64, field: u64, keys: &[&[u8]]) {
    for key in keys {
        root.put(
            &[&uint_segment(table), &[1], &uint_segment(field), &[0], key],
            RidList::from_iter([1]).to_node(Vec::new()),
        );
    }
    root.put(
        &[&uint_segment(table), &[1], &uint_segment(field), &[0x10]],
        OwnedNode::leaf(
            Vec::new(),
            [1u64.to_be_bytes(), (keys.len() as u64).to_be_bytes()].concat(),
        ),
    );
}

fn confirmation_requests(dir: &Path, table: &[u8]) -> usize {
    let mut count = 0;
    for connection in std::fs::read_dir(dir).unwrap() {
        let bytes = std::fs::read(connection.unwrap().path().join("c2s.bin")).unwrap();
        let mut rest = bytes.as_slice();
        while !rest.is_empty() {
            let (_, length) = match om_giop::parse_header(rest) {
                Ok(header) => header,
                Err(om_giop::ParseError::Incomplete) => break,
                Err(e) => panic!("{e}"),
            };
            let (message, after) = rest.split_at(length);
            rest = after;
            let Ok(request) = om_giop::Request::parse(message) else {
                continue;
            };
            if request.operation != b"Perform" {
                continue;
            }
            let transaction = om_protocol::parse_perform_request(request.body)
                .unwrap()
                .transaction;
            if transaction.first() != Some(&(om_protocol::Opcode::DownloadWithLock as u8)) {
                continue;
            }
            let request = om_protocol::parse_download_request(transaction).unwrap();
            if request.key_path == [table.to_vec(), vec![5], vec![1]] {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn paged_op90_confirmation_restores_omitted_source_member() {
    let mut root = fixture::tree(&[], true);
    validation_list(&mut root, fixture::NOTES_OCCURRENCE, fixture::BODY);
    fixture::set_field_options(&mut root, fixture::NOTES, fixture::BODY, 0x4000_0000);
    let table = uint_segment(fixture::NOTES);
    let long = "x".repeat(1_200_000) + "\nA";
    root.put(&[&table, &[5], &[1], &[1]], text_node(&[], &long));
    let file = TempFile::from_tree("review-paged-confirm", &root);
    let dir = record_dir("review-paged-confirm");
    let (port, ca) = host_with(&file.0, Some(dir.clone()), |s| {
        s.with_late_index_builds()
            .omitting_list_download(&[&table, &[5], &[1]])
    });
    let mut p = open(port, &ca);
    let tasks = layout(&mut p, "Tasks");
    assert_eq!(confirmation_requests(&dir, &table), 0);
    p.update_record(tasks, 1, values(&[("Code", "a")])).unwrap();
    let requests = confirmation_requests(&dir, &table);
    assert!(requests >= 2, "confirmation must cross a page: {requests}");
    println!(
        "op90 confirmation requests: {requests}; source text bytes: {}",
        long.len()
    );
    p.close();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn op90_confirmation_must_stay_inside_requested_record() {
    let mut unexpected = Vec::new();
    for warm in [false, true] {
        for fault in [0, 1, 2] {
            let mut root = fixture::tree(&[], true);
            validation_list(&mut root, fixture::NOTES_OCCURRENCE, fixture::BODY);
            fixture::set_field_options(&mut root, fixture::NOTES, fixture::BODY, 0x4000_0000);
            let table = uint_segment(fixture::NOTES);
            root.put(&[&table, &[5], &[1], &[1]], text_node(&[], "A"));

            if warm {
                literal_source_index(
                    &mut root,
                    fixture::NOTES,
                    fixture::BODY,
                    &[&[1, 0x12, 0x0f]],
                );
            }
            let file = TempFile::from_tree("review-confirm-bounds", &root);
            let (port, ca) = host_with(&file.0, None, |s| {
                let path = [&table[..], &[5][..], &[1][..]];
                let s = s.with_late_index_builds().omitting_list_download(&path);
                match fault {
                    0 => s,
                    1 => s.overriding_download_slots(&path, vec![0x80]),
                    2 => s.overriding_download_slots(&path, vec![0x80, 0x40, 0x00, 0x7f]),
                    _ => unreachable!(),
                }
            });
            let mut p = open(port, &ca);
            let tasks = layout(&mut p, "Tasks");
            let mut raw = raw_session(port);
            let body = raw.download(&[&table, &[5], &[1], &[1]]).unwrap();
            assert_eq!(om_scsu::decode_scsu(&body.value).unwrap(), "A");
            let confirmation = raw.download(&[&table, &[5], &[1]]).unwrap();
            println!(
                "warm={warm}, fault={fault}, op90 slots={:02x?}",
                confirmation.slots
            );
            let result = p.update_record(tasks, 1, values(&[("Code", "\u{301}")]));
            println!("warm={warm}, fault={fault}, result={result:?}");
            let page = all_records(&mut p, tasks);
            if result.is_ok() {
                assert_eq!(
                    text(&page, 1, "Code"),
                    Some("\u{301}"),
                    "accepted value must actually be stored"
                );
                unexpected.push((warm, fault));
            }
            assert_eq!(raw.download(&[&table, &[5], &[1], &[1]]).unwrap(), body);
            raw.disconnect().unwrap();
            p.close();
        }
    }
    assert!(
        unexpected.is_empty(),
        "malformed confirmations committed nonmembers: {unexpected:?}"
    );
}

#[test]
fn op90_escaped_confirmation_must_not_hide_unique_duplicate() {
    let mut unexpected = Vec::new();
    for warm in [false, true] {
        for fault in [0, 1, 2] {
            let mut root = fixture::tree(&[], true);
            fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
            fixture::set_field_validation(&mut root, TABLE, NAME, 0x410);
            let table = uint_segment(TABLE);
            root.put(&[&table, &[5], &[2], &[1]], text_node(&[], "A"));

            if warm {
                root.put(
                    &[&table, &[1], &[1], &[0], &[1, 0x12, 0x0f]],
                    RidList::from_iter([2]).to_node(Vec::new()),
                );
                root.put(
                    &[&table, &[1], &[1], &[0x10]],
                    OwnedNode::leaf(
                        Vec::new(),
                        [2u64.to_be_bytes(), 1u64.to_be_bytes()].concat(),
                    ),
                );
            }
            let file = TempFile::from_tree("review-confirm-unique", &root);
            let (port, ca) = host_with(&file.0, None, |s| {
                let path = [&table[..], &[5][..], &[2][..]];
                let s = s.with_late_index_builds().omitting_list_download(&path);
                match fault {
                    0 => s,
                    1 => s.overriding_download_slots(&path, vec![0x80]),
                    2 => s.overriding_download_slots(&path, vec![0x80, 0x40, 0x00, 0x7f]),
                    _ => unreachable!(),
                }
            });
            let mut p = open(port, &ca);
            let tasks = layout(&mut p, "Tasks");
            let mut raw = raw_session(port);
            let before = raw.download(&[&table, &[5], &[2], &[1]]).unwrap();
            assert_eq!(om_scsu::decode_scsu(&before.value).unwrap(), "A");
            let result = p.update_record(tasks, 1, values(&[("Name", "a")]));
            println!("unique warm={warm}, fault={fault}, result={result:?}");
            if result.is_ok() {
                assert_eq!(text(&all_records(&mut p, tasks), 1, "Name"), Some("a"));
                unexpected.push((warm, fault));
            }
            assert_eq!(raw.download(&[&table, &[5], &[2], &[1]]).unwrap(), before);
            assert!(matches!(
                raw.lock(&[&table, &[1], &[1]], 900, false).unwrap(),
                om_protocol::write::LockReply::Granted { .. }
            ));
            raw.kill_all_locks(900).unwrap();
            raw.disconnect().unwrap();
            p.close();
        }
    }
    assert!(
        unexpected.is_empty(),
        "malformed confirmations committed duplicates: {unexpected:?}"
    );
}
