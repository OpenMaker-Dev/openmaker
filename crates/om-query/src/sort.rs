//! SortList (op 14, spec `docs/re/query-program.md` 10): sort a row set by
//! one or more fields into the sort tree the client loads as its sorted
//! found set.
//!
//! Each record's sort key comes from the field's value index (10.3), or for
//! an unindexed text field from its stored value keyed by `om-collate`.

use std::collections::{BTreeMap, HashMap};

use om_hbam::{serial, DataType, FieldKind, OwnedNode, RidList};

use crate::exec::single_segment;
use crate::index::{Indexes, VALUE_INDEX};
use crate::program::{keys, leaf, measure, take_key_string, Instruction, Program, Size, Work};
use crate::QueryError;

/// The inline row set under `[01, i, 01]` (spec 10.1).
mod rows {
    /// `KAll`: every record of the table.
    pub const ALL: u8 = 0xff;
    /// `KItems`: the records of the RID list under this node.
    pub const ITEMS: u8 = 0x05;
}

/// `DBSortSpec::Entry::SortOrder` (spec 10.2).
mod order {
    pub const UP: u8 = 0;
    pub const DOWN: u8 = 1;
    pub const BY_VALUE_LIST: u8 = 2;
}

/// The key prefix of a level's `[N 00]` and `[N 01]` maps (spec 10.4; its
/// meaning is open).
const ASCENDING_PREFIX: &[u8] = &[0x80, 0x00, 0x00];
const DESCENDING_PREFIX: &[u8] = &[0x7f, 0xff, 0xff, 0x00];

/// Key of the `ff` leaves of a sort tree (spec 10.4).
const END: u8 = 0xff;

/// The value-index key of an empty text value (spec 7.2).
const EMPTY_TEXT_KEY: &[u8] = &[0x01];

/// One sort field (spec 10.2).
#[derive(Debug, PartialEq, Eq)]
struct SortField {
    field: Vec<u8>,
    descending: bool,
}

/// Parse the sort spec `DBSortSpec::Write` produced (spec 10.2); every entry
/// must sort by a field of `table`, and there may be at most `max_fields`.
fn parse_spec(
    mut bytes: &[u8],
    table: &[u8],
    max_fields: usize,
) -> Result<Vec<SortField>, QueryError> {
    fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Result<&'a [u8], QueryError> {
        if bytes.len() < n {
            return Err(QueryError::Malformed("sort spec truncated"));
        }
        let (head, rest) = bytes.split_at(n);
        *bytes = rest;
        Ok(head)
    }
    fn key(bytes: &mut &[u8]) -> Result<Vec<Vec<u8>>, QueryError> {
        let (segments, rest) = take_key_string(bytes)?;
        *bytes = rest;
        Ok(segments)
    }
    let _flags = take(&mut bytes, 2)?;
    let count = u16::from_be_bytes(take(&mut bytes, 2)?.try_into().expect("two bytes"));
    if usize::from(count) > max_fields {
        return Err(QueryError::Limit("sort fields"));
    }
    let mut fields = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let entry_table = key(&mut bytes)?;
        let field = single_segment(key(&mut bytes)?)?;
        let _repetition = take(&mut bytes, 2)?;
        let summary_table = key(&mut bytes)?;
        let summary_field = key(&mut bytes)?;
        let _summary_repetition = take(&mut bytes, 2)?;
        let value_list = key(&mut bytes)?;
        let sort_order = take(&mut bytes, 1)?[0];
        let locale = take(&mut bytes, 4)?;

        if entry_table != [table] {
            return Err(QueryError::Unimplemented("sort by a related field"));
        }
        if !summary_table.is_empty() || !summary_field.is_empty() {
            return Err(QueryError::Unimplemented("sort by a summary field"));
        }
        if locale != [0, 0, 0, 0] {
            return Err(QueryError::Unimplemented("sort with a language override"));
        }
        let descending = match sort_order {
            order::UP => false,
            order::DOWN => true,
            // With no value list FileMaker Server sorts ascending.
            order::BY_VALUE_LIST if value_list.is_empty() => false,
            order::BY_VALUE_LIST => {
                return Err(QueryError::Unimplemented("sort by a value list"));
            }
            other => return Err(QueryError::Unsupported("sort order", other)),
        };
        fields.push(SortField { field, descending });
    }
    Ok(fields)
}

/// Each record's sort key for one field (spec 10.3): its value-index key,
/// text keys followed by `00 00`, and the bitwise NOT for a descending sort
/// (text then followed by `ff ff`). A record the index does not hold sorts
/// as an empty value; a record under several keys sorts by the lowest. An
/// unindexed text field is keyed from its stored values.
fn sort_keys(
    indexes: &mut Indexes<'_>,
    work: &Work,
    table: &[u8],
    sort: &SortField,
    records: &RidList,
) -> Result<HashMap<u64, Vec<u8>>, QueryError> {
    let (keys, text) = if indexes.is_indexed(work, table, &sort.field, VALUE_INDEX)? {
        // Text by the field's type: a typed index's first key may be the
        // empty value's `01` as well (7.3).
        let text = indexes
            .field(table, &sort.field)?
            .is_none_or(|field| matches!(field.data_type, DataType::Text | DataType::Unknown(_)));
        let index = indexes.index(work, table, &sort.field, VALUE_INDEX)?;
        // The index in key order, so a record's first key is its lowest,
        // until every record has one.
        let mut keys: HashMap<u64, Vec<u8>> = HashMap::new();
        let wanted = records.len();
        let mut entries = index.entries(work);
        while keys.len() < wanted {
            let Some(entry) = entries.next() else {
                break;
            };
            let entry = entry?;
            for id in entry.records(work)?.and(records).iter() {
                keys.entry(id).or_insert_with(|| entry.key().to_vec());
            }
        }
        (keys, text)
    } else {
        (
            stored_value_keys(indexes, work, table, &sort.field, records)?,
            true,
        )
    };
    Ok(records
        .iter()
        .map(|id| {
            let mut key = keys
                .get(&id)
                .cloned()
                .unwrap_or_else(|| EMPTY_TEXT_KEY.to_vec());
            if text && key != EMPTY_TEXT_KEY {
                key.extend([0x00, 0x00]);
            }
            if sort.descending {
                key.iter_mut().for_each(|b| *b = !*b);
                if text {
                    key.extend([0xff, 0xff]);
                }
            }
            (id, key)
        })
        .collect())
}

/// The keys of an unindexed text field, computed from each record's stored
/// value as FileMaker Server does (live:sortA_init_up), cut as an index
/// cuts them (7.1), so long values sort as they would from the index. An
/// unstored calculation stores nothing; its results need the calculation
/// engine.
fn stored_value_keys(
    indexes: &mut Indexes<'_>,
    work: &Work,
    table: &[u8],
    field: &[u8],
    records: &RidList,
) -> Result<HashMap<u64, Vec<u8>>, QueryError> {
    let definition = indexes
        .field(table, field)?
        .ok_or(QueryError::Missing("field"))?;
    if definition.data_type != DataType::Text {
        return Err(QueryError::Unimplemented(
            "sort by an unindexed non-text field",
        ));
    }
    let mut keys = HashMap::new();
    for id in records.iter() {
        if let Some(value) = indexes.stored_text(work, table, field, id)? {
            keys.insert(id, om_collate::text_index_key(&value));
        }
    }
    if keys.is_empty() && definition.kind == FieldKind::Calculation && !records.is_empty() {
        return Err(QueryError::Unimplemented("sort by an unstored calculation"));
    }
    Ok(keys)
}

/// `[u8 len][bytes]`, the form the sort tree's references take.
fn short_bytes(bytes: &[u8]) -> Result<Vec<u8>, QueryError> {
    let len = u8::try_from(bytes.len())
        .map_err(|_| QueryError::Unimplemented("sort key over 255 bytes"))?;
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(len);
    out.extend_from_slice(bytes);
    Ok(out)
}

/// Builds the nodes of a sort tree, numbering them from 1 as they are made.
///
/// One level of recursion per sort field, at most
/// [`crate::program::limits::MAX_SORT_FIELDS`], well within a connection
/// thread's stack. Each level takes its group's records over from the level
/// above, so the records are held once however deep the sort goes.
struct TreeBuilder<'k> {
    /// Per sort level: each record's key, and whether the level descends.
    levels: &'k [(&'k HashMap<u64, Vec<u8>>, bool)],
    next: u64,
    nodes: Vec<OwnedNode>,
    /// The tree built so far, kept within `limit`, what a program may hold.
    size: Size,
    limit: Size,
}

impl TreeBuilder<'_> {
    /// Group `records` by the keys of `level` into a new node `[N]` with its
    /// maps `[N 00]` and `[N 01]` (spec 10.4). A group a later level must
    /// still order points to that level's node (`key = [u8 len] N`), which
    /// points back with `ff = [u8 len] N [u8 len] key`. Returns `N`.
    fn level(
        &mut self,
        level: usize,
        records: Vec<u64>,
        parent: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, QueryError> {
        let segment = serial::encode(self.next);
        self.next += 1;
        let (keys, descending) = self.levels[level];
        let prefix = if descending {
            DESCENDING_PREFIX
        } else {
            ASCENDING_PREFIX
        };
        let mut groups: BTreeMap<&[u8], Vec<u64>> = BTreeMap::new();
        for id in records {
            groups.entry(&keys[&id]).or_default().push(id);
        }

        let mut node = Vec::with_capacity(groups.len() + 1);
        let mut by_prefixed = Vec::with_capacity(groups.len() + 1);
        let mut by_key = Vec::with_capacity(groups.len() + 1);
        for (key, ids) in groups {
            let entry = if ids.len() > 1 && level + 1 < self.levels.len() {
                let mut back = short_bytes(&segment)?;
                back.extend(short_bytes(key)?);
                let child = self.level(level + 1, ids, Some(back))?;
                OwnedNode::leaf(key, short_bytes(&child)?)
            } else {
                ids.into_iter().collect::<RidList>().to_node(key)
            };
            let prefixed = [prefix, key].concat();
            let to_key = OwnedNode::leaf(prefixed.clone(), short_bytes(key)?);
            let to_prefixed = OwnedNode::leaf(key, short_bytes(&prefixed)?);
            self.grow(
                measure(&entry)
                    .0
                    .plus(Size::of(&to_key))
                    .plus(Size::of(&to_prefixed)),
            )?;
            node.push(entry);
            by_prefixed.push(to_key);
            by_key.push(to_prefixed);
        }
        if let Some(back) = parent {
            node.push(OwnedNode::leaf(vec![END], back));
            by_prefixed.push(OwnedNode::leaf(vec![END], short_bytes(&[END])?));
        }
        by_key.push(OwnedNode::leaf(vec![END], short_bytes(&[END])?));
        for list in [&mut node, &mut by_prefixed, &mut by_key] {
            list.sort_by(|a, b| a.segment.cmp(&b.segment));
        }

        let lists = [
            OwnedNode::list(segment.clone(), node),
            OwnedNode::list([&segment[..], &[0x00]].concat(), by_prefixed),
            OwnedNode::list([&segment[..], &[0x01]].concat(), by_key),
        ];
        // The three lists and their `ff` leaves.
        let ends = lists.iter().filter_map(|list| list.child(&[END]));
        self.grow(
            lists
                .iter()
                .chain(ends)
                .fold(Size::default(), |size, n| size.plus(Size::of(n))),
        )?;
        self.nodes.extend(lists);
        Ok(segment)
    }

    /// Count `size` more of the tree, which must stay within what a program
    /// may hold.
    fn grow(&mut self, size: Size) -> Result<(), QueryError> {
        self.size = self.size.plus(size);
        if !self.size.within(self.limit) {
            return Err(QueryError::Limit("program size"));
        }
        Ok(())
    }
}

/// SortList (op 14, spec 10): sort the inline row set `[01, i, 01]` by the
/// sort spec in `KOperand1` and write the sort tree to `KDest`.
pub(crate) fn sort_list(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    if instruction
        .leaf(leaf::OPERAND2)
        .map_or(true, <[u8]>::is_empty)
    {
        return Err(QueryError::NoTable);
    }
    let table = instruction.table()?;
    let dest = instruction.required_spot(leaf::DEST)?;
    if !indexes.table_exists(&table)? {
        return Err(QueryError::Missing("table"));
    }
    let fields = parse_spec(
        instruction
            .leaf(leaf::OPERAND1)
            .ok_or(QueryError::Malformed("SortList lacks a sort spec"))?,
        &table,
        program.budget().sort_fields,
    )?;
    for sort in &fields {
        if !indexes.field_exists(&table, &sort.field)? {
            return Err(QueryError::Missing("field"));
        }
    }

    // The row set is the instruction's own `01` node, not a spot path; any
    // other form sorts nothing.
    let src = program.spot(&[
        vec![keys::INSTRUCTIONS],
        serial::encode(instruction.number.into()),
        vec![leaf::SRC],
    ])?;
    let all = indexes.all_records(&table)?;
    let records = match src {
        Some(node) if node.child(&[rows::ALL]).is_some() => all.clone(),
        Some(node) => match node.child(&[rows::ITEMS]) {
            Some(items) => RidList::from_owned(items)?.and(all),
            None => RidList::new(),
        },
        None => RidList::new(),
    };

    // Each field and direction is keyed once, however many levels name it.
    let mut keyed: Vec<(&SortField, HashMap<u64, Vec<u8>>)> = Vec::new();
    for sort in &fields {
        if !keyed.iter().any(|(done, _)| *done == sort) {
            keyed.push((
                sort,
                sort_keys(indexes, program.work(), &table, sort, &records)?,
            ));
        }
    }
    let levels: Vec<(&HashMap<u64, Vec<u8>>, bool)> = fields
        .iter()
        .map(|sort| {
            let (_, keys) = keyed
                .iter()
                .find(|(done, _)| *done == sort)
                .expect("keyed above");
            (keys, sort.descending)
        })
        .collect();
    let ids: Vec<u64> = records.iter().collect();
    let mut children = vec![OwnedNode::leaf(
        vec![0x00],
        (ids.len() as u64).to_be_bytes().to_vec(),
    )];
    if !levels.is_empty() {
        let mut builder = TreeBuilder {
            levels: &levels,
            next: 1,
            nodes: Vec::new(),
            size: Size::default(),
            limit: program.budget().tree,
        };
        builder.level(0, ids, None)?;
        children.extend(builder.nodes);
    }
    children.sort_by(|a, b| a.segment.cmp(&b.segment));
    program.set_spot(&dest, children)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::limits;

    #[test]
    fn parses_the_spec_example() {
        // live:sortA_last_up, Last Name ascending.
        let spec = [
            0x00, 0x00, 0x00, 0x01, 0x03, 0x02, 0x80, 0x02, 0x02, 0x01, 0x02, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(
            parse_spec(&spec, &[0x80, 0x02], limits::MAX_SORT_FIELDS).unwrap(),
            [SortField {
                field: vec![0x02],
                descending: false,
            }]
        );
        assert!(matches!(
            parse_spec(&spec, &[0x80, 0x03], limits::MAX_SORT_FIELDS),
            Err(QueryError::Unimplemented(_))
        ));
        assert!(parse_spec(&spec[..20], &[0x80, 0x02], limits::MAX_SORT_FIELDS).is_err());
    }

    fn keyed(levels: &[&[(u64, &[u8])]]) -> Vec<HashMap<u64, Vec<u8>>> {
        levels
            .iter()
            .map(|keys| keys.iter().map(|(id, key)| (*id, key.to_vec())).collect())
            .collect()
    }

    #[test]
    fn a_shared_first_key_points_to_a_second_level() {
        // Records 1 and 2 share key `01` and need level 2; record 3 does not.
        let levels = keyed(&[
            &[(1, &[0x01]), (2, &[0x01]), (3, &[0x02])],
            &[(1, &[0x05]), (2, &[0x04]), (3, &[0x06])],
        ]);
        let levels: Vec<_> = levels.iter().map(|keys| (keys, false)).collect();
        let mut builder = TreeBuilder {
            levels: &levels,
            next: 1,
            nodes: Vec::new(),
            size: Size::default(),
            limit: limits::MAX_TREE,
        };
        assert_eq!(builder.level(0, vec![1, 2, 3], None).unwrap(), [0x01]);
        let root = builder.nodes.iter().find(|n| n.segment == [0x01]).unwrap();
        assert_eq!(
            root.child(&[0x01]).unwrap().value.as_deref(),
            Some(&[1, 2][..])
        );
        assert_eq!(
            root.child(&[0x02]).unwrap().child(&[0x03]).unwrap().value,
            Some(vec![])
        );
        let second = builder.nodes.iter().find(|n| n.segment == [0x02]).unwrap();
        assert_eq!(
            second.child(&[END]).unwrap().value.as_deref(),
            Some(&[1, 1, 1, 1][..])
        );
        assert_eq!(second.children.len(), 3);
    }
}
