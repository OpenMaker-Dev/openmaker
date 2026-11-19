//! Literal native reply/key forms from query-program.md 7.1, 7.2, 8.5, 8.7.
//! These do not use OpenMaker Server's key or value-list builders as an oracle.
use om_hbam::{KeyTree, OwnedNode, SlotReader};
use om_query::find::{
    checked_value_list_program, value_list_index_counts, value_list_index_existed,
    value_list_values, ListedValues,
};

fn reply(entries: Vec<u8>, count: u64, check: Vec<OwnedNode>) -> Vec<u8> {
    reply_with(OwnedNode::leaf(vec![0], entries), count, check)
}

fn reply_with(entries: OwnedNode, count: u64, check: Vec<OwnedNode>) -> Vec<u8> {
    OwnedNode::list(
        Vec::new(),
        vec![
            OwnedNode::list(
                vec![2],
                vec![
                    entries,
                    OwnedNode::leaf(vec![0xff], count.to_be_bytes().to_vec()),
                ],
            ),
            OwnedNode::list(vec![3], check),
        ],
    )
    .to_wire()
    .unwrap()
}

/// Entries past 1,000 bytes as FileMaker Server sends them (8.7, #464): a
/// node whose child `00` holds the total length and whose children `01`,
/// `02`, ... hold the pieces, 1,000 bytes each but the last.
fn pieces(entries: &[u8], total: u32) -> OwnedNode {
    let mut children = vec![OwnedNode::leaf(vec![0], total.to_be_bytes().to_vec())];
    for (n, piece) in (1u8..).zip(entries.chunks(1_000)) {
        children.push(OwnedNode::leaf(vec![n], piece.to_vec()));
    }
    OwnedNode::list(vec![0], children)
}

/// An entry of record `record` whose value is `text` (ASCII, so its SCSU
/// is itself), under index key `key`.
fn entry(record: u8, key: &[u8], text: &[u8]) -> Vec<u8> {
    let mut bytes = vec![2, 1, record];
    bytes.push(u8::try_from(key.len() + 1).unwrap());
    bytes.push(u8::try_from(key.len()).unwrap());
    bytes.extend(key);
    bytes.extend(u32::try_from(text.len()).unwrap().to_be_bytes());
    bytes.extend(text);
    bytes
}

/// The shape of the live reply on #464: two values, of 3,507 and 9 text
/// bytes, whose entries take 3,772 bytes. Its bytes are not public; these
/// are synthetic, of the same lengths and form.
#[test]
fn entries_past_1000_bytes_arrive_as_pieces_and_ff_counts_the_values() {
    let long = vec![b'a'; 3_507];
    let short = b"Short one";
    // Index keys in ascending order, as the host lists them.
    let mut entries = entry(1, &[1, 0x10], &long);
    entries.extend(entry(2, &[1, 0x20], short));
    let total = u32::try_from(entries.len()).unwrap();
    let list = value_list_values(&reply_with(pieces(&entries, total), 2, vec![])).unwrap();
    assert!(list.complete);
    let texts: Vec<usize> = list.values.iter().map(|(_, text)| text.len()).collect();
    assert_eq!(texts, [3_507, 9]);
    // The pieces must add up to the total, and come in order.
    for wrong in [total - 1, total + 1] {
        assert_eq!(
            value_list_values(&reply_with(pieces(&entries, wrong), 2, vec![])),
            None
        );
    }
    let mut swapped = pieces(&entries, total);
    swapped.children.swap(1, 2);
    assert_eq!(value_list_values(&reply_with(swapped, 2, vec![])), None);
    // The total is a u32, not a long text value's 8- or 12-byte header.
    for extra in [4, 8] {
        let mut header = pieces(&entries, total);
        header.children[0]
            .value
            .as_mut()
            .unwrap()
            .extend(vec![0; extra]);
        assert_eq!(value_list_values(&reply_with(header, 2, vec![])), None);
    }
    // A count of the pieces is not the count of values.
    assert_eq!(
        value_list_values(&reply_with(pieces(&entries, total), 4, vec![])),
        None
    );
}

/// A list the most values `14` stopped before more keys with the prefix
/// counts minus the number listed (8.7, live1:vl_all_max2): it is read, as
/// a cut list. Minus another number is malformed.
#[test]
fn a_negative_count_is_a_cut_list_of_that_many_values() {
    let mut entries = entry(1, &[1, 0x10], b"Alpha");
    entries.extend(entry(2, &[1, 0x20], b"Beta"));
    let list = value_list_values(&reply(entries.clone(), 2u64.wrapping_neg(), vec![])).unwrap();
    assert!(!list.complete);
    assert_eq!(list.values.len(), 2);
    assert_eq!(
        value_list_values(&reply(entries, 3u64.wrapping_neg(), vec![])),
        None
    );
}

// The vl_first_t example: RID 1, English "Test" key, unmasked ASCII text.
fn test_entry() -> Vec<u8> {
    let mut bytes = vec![
        2, 1, 1, 10, 9, 1, 0x14, 0x33, 0x12, 0x6b, 0x14, 0x10, 0x14, 0x33,
    ];
    bytes.extend(4u32.to_be_bytes());
    bytes.extend(b"Test");
    bytes
}

#[test]
fn checked_list_reads_the_right_count_leaf_before_instruction_two() {
    let key = [0xab; 16];
    let wire = checked_value_list_program(7, 0x8001, 0x8123, 100_000, Some(key));
    let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
    let leaf = |instruction, field| tree.at(&[1, instruction, field]).unwrap().value.unwrap();
    assert_eq!(leaf(1, 0), [42]);
    assert_eq!(leaf(1, 1), [2, 0x80, 1, 1, 1, 2, 0x81, 0x23, 1, 0x10]);
    assert_eq!(leaf(1, 2), [1, 3, 1, 3]);
    assert!(tree.at(&[1, 1, 7]).is_none());
    assert_eq!(leaf(2, 0), [19]);
    assert_eq!(leaf(2, 7), key);
    assert_eq!(leaf(2, 9), [2, 0x81, 0x23]);
    assert_eq!(leaf(2, 0x14), 100_000u32.to_be_bytes());
}

#[test]
fn missing_counts_are_distinct_from_an_existing_empty_index() {
    assert_eq!(
        value_list_index_existed(&reply(vec![], 0, vec![])),
        Some(false)
    );
    let counts = |bytes| vec![OwnedNode::leaf(vec![0], bytes)];
    assert_eq!(
        value_list_index_existed(&reply(vec![], 0, counts(vec![0; 16]))),
        Some(true)
    );
    assert_eq!(
        value_list_index_existed(&reply(vec![], 0, counts(vec![]))),
        Some(false)
    );
    for length in [1, 8, 15, 17] {
        assert_eq!(
            value_list_index_existed(&reply(vec![], 0, counts(vec![0; length]))),
            None
        );
    }
    assert_eq!(value_list_index_existed(&[0x80, 0x40]), Some(false));
    // A host can omit an empty GetKey spot. A complete list reply is still
    // required by value_list_values, so a missing whole reply is an error.
    assert_eq!(value_list_values(&[0x80, 0x40]), None);
}

/// The listed values' texts, and whether the list is complete.
fn texts(list: Option<ListedValues>) -> Option<(Vec<String>, bool)> {
    list.map(|list| {
        let texts = list.values.into_iter().map(|(_, text)| text).collect();
        (texts, list.complete)
    })
}

#[test]
fn literal_reply_is_decoded_including_unmasked_non_ascii_and_limit_sentinel() {
    assert_eq!(
        texts(value_list_values(&reply(test_entry(), 1, vec![]))),
        Some((vec!["Test".into()], true))
    );
    // All ones: the host cut the list, so it is not the whole list.
    assert_eq!(
        texts(value_list_values(&reply(test_entry(), u64::MAX, vec![]))),
        Some((vec!["Test".into()], false))
    );
    assert_eq!(
        texts(value_list_values(&reply(vec![], 0, vec![]))),
        Some((vec![], true))
    );
    // Unicode raw key for é, SCSU SD0 Latin window: e9 unmasked.
    let mut raw = vec![2, 1, 1, 4, 3, 1, 0, 0xe9];
    raw.extend(1u32.to_be_bytes());
    raw.push(0xe9);
    let list = value_list_values(&reply(raw, 1, vec![])).unwrap();
    assert_eq!(list.values, [(vec![1, 0, 0xe9], "é".to_string())]);
}

#[test]
fn malformed_reply_never_becomes_an_empty_value_list() {
    let entry = test_entry();
    for length in 1..entry.len() {
        assert_eq!(
            value_list_values(&reply(entry[..length].to_vec(), 1, vec![])),
            None,
            "truncation {length}"
        );
    }
    assert_eq!(value_list_values(&reply(entry.clone(), 2, vec![])), None);
    let mut malformed = entry.clone();
    malformed[1] = 0; // not a one-segment key string
    assert_eq!(value_list_values(&reply(malformed, 1, vec![])), None);
    let mut duplicate = entry.clone();
    duplicate.extend(entry);
    assert_eq!(value_list_values(&reply(duplicate, 2, vec![])), None);
    assert_eq!(value_list_values(&[0x80, 0x40]), None);
    let empty_key = vec![2, 1, 1, 2, 1, 1, 0, 0, 0, 0];
    assert_eq!(value_list_values(&reply(empty_key, 1, vec![])), None);
}

#[test]
fn unterminated_value_list_streams_are_not_partial_successes() {
    let full = reply(test_entry(), 1, vec![OwnedNode::leaf(vec![0], vec![0; 16])]);
    for length in 0..full.len() {
        let prefix = &full[..length];
        assert_eq!(
            value_list_values(prefix),
            None,
            "list stream prefix {length}"
        );
        assert_eq!(
            value_list_index_existed(prefix),
            None,
            "counts stream prefix {length}"
        );
    }
    assert_eq!(
        texts(value_list_values(&full)),
        Some((vec!["Test".into()], true))
    );
    assert_eq!(value_list_index_existed(&full), Some(true));
    assert_eq!(value_list_index_counts(&full), Some(Some((0, 0))));
    for tail in [vec![0x40], vec![0x80, 0x40]] {
        let mut trailing = full.clone();
        trailing.extend(tail);
        assert_eq!(value_list_values(&trailing), None);
        assert_eq!(value_list_index_existed(&trailing), None);
    }
}
