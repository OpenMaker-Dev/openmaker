//! Find mode criteria turned into SimpleFind finders (query-program.md 6.4)
//! and the program FileMaker Pro has a host run for one request (6.5).
//! OpenMaker Server's scripts run it with [`crate::execute`] against the
//! file's indexes (6.2, 6.3); the desktop client sends it as op86.
//!
//! [`plan`] maps every criterion [`crate::criteria`] reads onto finders of
//! the kinds whose bytes are established (6.2, 6.3: 0 IndexMatch, 2
//! IndexRange, 7 FindDuplicates): exactly for words of a text field (each
//! word a prefix of a word of the field, as FileMaker Pro finds), `=word`,
//! `==value`, `=` (empty), `*` (not empty), `!` (duplicates), whole numbers,
//! dates, times and timestamps, whole days, months, years and hours, and
//! `<`, `<=`, `>`, `>=` and `a...b` ranges of them (text ranges on the word
//! index, Claris "Finding ranges of information"). Wildcards, quoted
//! phrases, `?`, `~`, `==` alone, fractions and dates with wildcard parts
//! would need Match or DateTimeMatch finders (kinds 8, 9), whose operands
//! are not established: their finders select candidates (a literal prefix,
//! the words of a phrase, the enclosing period) and the caller keeps the
//! candidates [`Criterion::matches`] accepts. [`finders`] answers only the
//! exact ones and fails with error 3 otherwise.

use std::collections::{BTreeSet, HashMap};

use om_collate::DateOrder;
use om_hbam::slot::uint_segment;
use om_hbam::{DataType, KeyTree, OwnedNode, RidList, SlotReader};

use crate::criteria::{
    self, Bound, DatePattern, Kind, Moment, Part, Term, TimePattern, Unit, Whole,
};
pub use crate::criteria::{Criterion, FindContext, Unreadable, Ymd};
use crate::keys::TextKeys;

/// Finder kinds (`DBFinder::FindKind`, 6.2).
mod kind {
    pub const INDEX_MATCH: u8 = 0;
    pub const INDEX_RANGE: u8 = 2;
    pub const FIND_DUPLICATES: u8 = 7;
}

/// Finder scopes: the word index or the value index (6.3).
const WORD_SCOPE: u8 = 0;
const VALUE_SCOPE: u8 = 1;

/// Finder option bits (6.2).
const INCLUDE_BEGIN: u8 = 1;
const INCLUDE_END: u8 = 2;

/// The value-index key of empty text (query-program.md 7.2).
const EMPTY_TEXT_KEY: &[u8] = &[0x01];

/// The end of a prefix range: every key that starts with the prefix sorts
/// below `prefix ff fe` (6.2, cap:912).
const PREFIX_END: [u8; 2] = [0xff, 0xfe];

/// SimpleFind (op 10).
const SIMPLE_FIND: u8 = 10;

/// Why a request cannot be run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindError {
    /// No request has a criterion: FileMaker's 400.
    Empty,
    /// A criterion this engine cannot express: FileMaker's 3.
    Unsupported,
    /// The query failed: its FileMaker error.
    Query(u16),
}

impl FindError {
    pub fn code(self) -> i32 {
        match self {
            FindError::Empty => 400,
            FindError::Unsupported => 3,
            FindError::Query(code) => i32::from(code),
        }
    }
}

/// One find request: the criteria typed into its fields and whether it
/// omits the records it matches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub omit: bool,
    /// Per field of the window's table, the criterion text, in the order
    /// the fields were set; setting a field again replaces its criterion.
    pub criteria: Vec<(u64, String)>,
}

impl Request {
    pub fn set(&mut self, field: u64, text: String) {
        match self.criteria.iter_mut().find(|(f, _)| *f == field) {
            Some(criterion) => criterion.1 = text,
            None => self.criteria.push((field, text)),
        }
    }

    fn is_empty(&self) -> bool {
        self.criteria.iter().all(|(_, text)| text.trim().is_empty())
    }
}

/// A finder and whether a request keeps the records it selects (`false`:
/// the request is every record without them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finder {
    pub bytes: Vec<u8>,
    pub keep: bool,
}

impl Finder {
    /// The index of its field the finder reads (6.3): scope 0 the word
    /// index, scopes 1 and 2 the value index.
    pub fn reads(&self) -> IndexKind {
        if self.bytes.get(1) == Some(&WORD_SCOPE) {
            IndexKind::Word
        } else {
            IndexKind::Value
        }
    }
}

/// One of a field's indexes (7.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IndexKind {
    /// `[T, 01, F, 00]`, with its counts leaf `[T, 01, F, 10]`.
    Value,
    /// `[T, 01, F, 01]`, with its counts leaf `[T, 01, F, 11]`.
    Word,
}

/// An index of a field of a program's table: (field, index).
pub type FieldIndex = (u64, IndexKind);

/// A finder key: `[u8 total][u8 length][bytes]`, `00` for an open bound
/// (6.2).
fn key_operand(key: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return vec![0];
    }
    let mut out = vec![key.len() as u8 + 1, key.len() as u8];
    out.extend_from_slice(key);
    out
}

fn finder(
    kind: u8,
    scope: u8,
    options: u8,
    table: u64,
    field: u64,
    begin: &[u8],
    end: &[u8],
) -> Vec<u8> {
    let mut out = vec![kind, scope, options];
    out.extend(key_operand(&uint_segment(table)));
    out.extend(key_operand(&uint_segment(field)));
    out.extend(1u16.to_be_bytes());
    out.extend(key_operand(begin));
    out.extend(key_operand(end));
    out
}

/// The finders of one criterion and whether they select exactly its
/// records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Finders ANDed with the request's others; no finder is every record.
    pub finders: Vec<Finder>,
    /// The finders select exactly the records the criterion matches;
    /// otherwise they select a superset, which the caller filters with
    /// [`Criterion::matches`].
    pub exact: bool,
}

impl Plan {
    fn exact(finders: Vec<Finder>) -> Self {
        Plan {
            finders,
            exact: true,
        }
    }

    fn candidates(finders: Vec<Finder>) -> Self {
        Plan {
            finders,
            exact: false,
        }
    }
}

fn keep(bytes: Vec<u8>) -> Finder {
    Finder { bytes, keep: true }
}

/// The index key of a date (query-program.md 7.3).
fn date_key((year, month, day): Ymd) -> Option<Vec<u8>> {
    om_collate::date_key(&format!("{month}/{day}/{year}"), DateOrder::MonthDayYear)
}

fn clock(seconds: u32) -> String {
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn time_key(seconds: u32) -> Option<Vec<u8>> {
    om_collate::time_key(&clock(seconds))
}

fn stamp_key((year, month, day): Ymd, seconds: u32) -> Option<Vec<u8>> {
    om_collate::timestamp_key(
        &format!("{month}/{day}/{year} {}", clock(seconds)),
        DateOrder::MonthDayYear,
    )
}

/// The key of the last microsecond of second `seconds` (`h:mm:ss.999999`):
/// a time period ends there, so a value with a fraction of that second is
/// inside it. FileMaker's own finder for `12:30 PM` reads up to
/// 45,059.999999 s (spec 7.3).
fn time_end_key(seconds: u32) -> Option<Vec<u8>> {
    om_collate::time_key(&format!("{}.999999", clock(seconds)))
}

/// [`time_end_key`] of a timestamp (the same fraction, inferred).
fn stamp_end_key((year, month, day): Ymd, seconds: u32) -> Option<Vec<u8>> {
    om_collate::timestamp_key(
        &format!("{month}/{day}/{year} {}.999999", clock(seconds)),
        DateOrder::MonthDayYear,
    )
}

/// The key of a bound that stands for the last second of what it names
/// (the end of an inclusive range, the start of `>x`): through that
/// second's last microsecond for times and timestamps ([`time_end_key`]).
fn bound_end_key(bound: &Bound) -> Option<Vec<u8>> {
    match bound {
        Bound::Time(seconds) => time_end_key(*seconds),
        Bound::Stamp(date, seconds) => stamp_end_key(*date, *seconds),
        other => bound_key(other),
    }
}

/// A bound's index key; `None` when its form is not established.
fn bound_key(bound: &Bound) -> Option<Vec<u8>> {
    match bound {
        Bound::Number(n) => n.index_key(),
        Bound::Word(key) => Some(key.clone()),
        Bound::Date(date) => date_key(*date),
        Bound::Time(seconds) => time_key(*seconds),
        Bound::Stamp(date, seconds) => stamp_key(*date, *seconds),
    }
}

/// The key of a pattern's literal start (`Jo` of `Jo*n`): `01` and the
/// characters' weights, as a text key.
fn literal_prefix(pattern: &[Unit]) -> Option<Vec<u8>> {
    let mut key = vec![0x01];
    for unit in pattern {
        match unit {
            Unit::Char(weights) => key.extend(weights),
            _ => break,
        }
    }
    (key.len() > 1).then_some(key)
}

/// The smallest key above every key that starts with `key`: `key` with its
/// last byte below `ff` raised by one and what follows dropped; empty (an
/// open end) when every byte is `ff`.
fn successor(key: &[u8]) -> Vec<u8> {
    let mut end = key.to_vec();
    while end.last() == Some(&0xff) {
        end.pop();
    }
    if let Some(last) = end.last_mut() {
        *last += 1;
    }
    end
}

/// The finders of a criterion of `field` in `table` (6.4).
///
/// An index holds the first [`om_collate::INDEX_KEY_LIMIT`] bytes of a key
/// (7.1), so a finder's keys are cut the same way. A key that reaches the
/// limit also stands for every longer value with the same start: its
/// finder selects a superset (its bounds included), and the plan is not
/// exact, so the caller keeps the records [`Criterion::matches`] accepts.
pub fn plan(table: u64, field: u64, criterion: &Criterion) -> Plan {
    let approximate = std::cell::Cell::new(false);
    let mut plan = keyed_plan(table, field, criterion, &|key: &[u8]| {
        let mut key = key.to_vec();
        om_collate::cut_index_key(&mut key);
        if key.len() >= om_collate::INDEX_KEY_LIMIT {
            approximate.set(true);
        }
        key
    });
    plan.exact &= !approximate.get();
    plan
}

/// [`plan`], each index key of a finder passed through `held` first.
fn keyed_plan(
    table: u64,
    field: u64,
    criterion: &Criterion,
    held: &dyn Fn(&[u8]) -> Vec<u8>,
) -> Plan {
    let text_field = matches!(criterion.data_type, DataType::Text | DataType::Unknown(_));
    let make = |kind, scope, options, begin: &[u8], end: &[u8]| {
        let (begin, end) = (held(begin), held(end));
        let at_limit = |key: &[u8]| key.len() >= om_collate::INDEX_KEY_LIMIT;
        // A range bound at the limit also stands for longer keys: include
        // it.
        let options = match kind {
            kind::INDEX_RANGE => {
                options
                    | if at_limit(&begin) { INCLUDE_BEGIN } else { 0 }
                    | if at_limit(&end) { INCLUDE_END } else { 0 }
            }
            _ => options,
        };
        finder(kind, scope, options, table, field, &begin, &end)
    };
    let prefix_range = |scope, key: &[u8]| {
        let key = held(key);
        // `end` is past every key that starts with `key`, not a key: `ff fe`
        // after weights (no weight is that high), but a Unicode raw key may
        // hold the code unit `ff ff`, so there the key's successor.
        let end = if criterion.text == TextKeys::Raw {
            successor(&key)
        } else {
            [key.as_slice(), &PREFIX_END].concat()
        };
        keep(finder(
            kind::INDEX_RANGE,
            scope,
            INCLUDE_BEGIN,
            table,
            field,
            &key,
            &end,
        ))
    };
    let range = |scope, begin: &[u8], end: &[u8]| {
        keep(make(
            kind::INDEX_RANGE,
            scope,
            INCLUDE_BEGIN | INCLUDE_END,
            begin,
            end,
        ))
    };
    // An empty value of any type is indexed under `01` (7.2, 7.3).
    let empty = || keep(make(kind::INDEX_MATCH, VALUE_SCOPE, 0, EMPTY_TEXT_KEY, &[]));
    match &criterion.kind {
        Kind::Empty => Plan::exact(vec![empty()]),
        // The value index cannot tell an empty line from nothing.
        Kind::EntirelyEmpty if text_field => Plan::candidates(vec![empty()]),
        Kind::EntirelyEmpty => Plan::exact(vec![empty()]),
        Kind::NotEmpty => Plan::exact(vec![keep(make(
            kind::INDEX_RANGE,
            VALUE_SCOPE,
            0,
            EMPTY_TEXT_KEY,
            &[],
        ))]),
        Kind::Invalid => Plan::candidates(Vec::new()),
        Kind::Duplicates(rest) => {
            let mut finders = vec![keep(make(kind::FIND_DUPLICATES, VALUE_SCOPE, 0, &[], &[]))];
            let mut exact = true;
            if let Some(rest) = rest {
                let rest = keyed_plan(table, field, rest, held);
                finders.extend(rest.finders);
                exact = rest.exact;
            }
            Plan { finders, exact }
        }
        Kind::Terms(terms) => {
            let mut finders = Vec::new();
            let mut exact = !terms.is_empty();
            for term in terms {
                match term {
                    Term::Prefix(key) => finders.push(prefix_range(WORD_SCOPE, key)),
                    Term::Word(key) => {
                        finders.push(keep(make(kind::INDEX_MATCH, WORD_SCOPE, 0, key, &[])))
                    }
                    Term::Pattern(pattern) => {
                        exact = false;
                        finders
                            .extend(literal_prefix(pattern).map(|p| prefix_range(WORD_SCOPE, &p)));
                    }
                    Term::Phrase {
                        text,
                        anywhere,
                        open_end,
                    } => {
                        exact = false;
                        let phrase: String = text.iter().collect();
                        let words = om_collate::words(&phrase);
                        let last = words.len().saturating_sub(1);
                        for (i, word) in words.iter().enumerate() {
                            if i == 0 && *anywhere {
                                continue;
                            }
                            // Whole words inside the phrase; its first word
                            // starts a word, its last may end inside one.
                            let whole = i < last || !open_end;
                            let form = criterion.text;
                            finders.push(if i == 0 || (i == last && !whole) {
                                prefix_range(WORD_SCOPE, &criteria::prefix_key(form, word))
                            } else {
                                let key = criteria::whole_key(form, word);
                                keep(make(kind::INDEX_MATCH, WORD_SCOPE, 0, &key, &[]))
                            });
                        }
                    }
                }
            }
            Plan { finders, exact }
        }
        Kind::Whole(Whole::Key(key)) => Plan::exact(vec![keep(make(
            kind::INDEX_MATCH,
            VALUE_SCOPE,
            0,
            key,
            &[],
        ))]),
        Kind::Whole(Whole::Pattern(pattern)) => Plan::candidates(if text_field {
            literal_prefix(pattern)
                .map(|p| prefix_range(VALUE_SCOPE, &p))
                .into_iter()
                .collect()
        } else {
            Vec::new()
        }),
        Kind::Number(n) => match n.index_key() {
            Some(key) => Plan::exact(vec![keep(make(
                kind::INDEX_MATCH,
                VALUE_SCOPE,
                0,
                &key,
                &[],
            ))]),
            None => Plan::candidates(Vec::new()),
        },
        Kind::Range { low, high } => {
            let scope = if text_field { WORD_SCOPE } else { VALUE_SCOPE };
            // An open end of a typed range stops short of the empty value
            // (`01`) and of values FileMaker cannot read (`fe ff`), as
            // FileMaker's own open bounds do (6.4: `(01, K)`, `[K, fe ff)`).
            let open = |typed: &[u8]| {
                if text_field {
                    Vec::new()
                } else {
                    typed.to_vec()
                }
            };
            // `<=x`, `a...b` and `>x` take x's last second (criteria.rs),
            // which ends at its last microsecond.
            let key = |b: &Option<(Bound, bool)>, open: Vec<u8>, high: bool| match b {
                None => Some((open, false)),
                Some((bound, inclusive)) if *inclusive == high => {
                    Some((bound_end_key(bound)?, *inclusive))
                }
                Some((bound, inclusive)) => Some((bound_key(bound)?, *inclusive)),
            };
            match (
                key(low, open(EMPTY_TEXT_KEY), false),
                key(high, open(crate::keys::INVALID_KEY), true),
            ) {
                (Some((begin, low_in)), Some((end, high_in))) => {
                    let options = if low_in { INCLUDE_BEGIN } else { 0 }
                        | if high_in { INCLUDE_END } else { 0 };
                    Plan::exact(vec![keep(make(
                        kind::INDEX_RANGE,
                        scope,
                        options,
                        &begin,
                        &end,
                    ))])
                }
                _ => Plan::candidates(Vec::new()),
            }
        }
        Kind::Moment(moment) => {
            let period = |begin: Option<Vec<u8>>, end: Option<Vec<u8>>, exact| match (begin, end) {
                (Some(b), Some(e)) => Plan {
                    finders: vec![range(VALUE_SCOPE, &b, &e)],
                    exact,
                },
                _ => Plan::candidates(Vec::new()),
            };
            let days = |d: &DatePattern| d.bounds();
            match moment {
                Moment::Date(d) => match days(d) {
                    Some((first, last)) => period(date_key(first), date_key(last), d.is_period()),
                    None => Plan::candidates(Vec::new()),
                },
                Moment::Time(t) => period(
                    time_key(t.earliest()),
                    time_end_key(t.latest()),
                    t.is_period(),
                ),
                Moment::Stamp(d, t) => match days(d) {
                    Some((first, last)) => {
                        let whole_days = *t == any_time();
                        let one_day = first == last && d.is_period() && t.is_period();
                        if one_day {
                            period(
                                stamp_key(first, t.earliest()),
                                stamp_end_key(last, t.latest()),
                                true,
                            )
                        } else {
                            period(
                                stamp_key(first, 0),
                                stamp_end_key(last, 86_399),
                                whole_days && d.is_period(),
                            )
                        }
                    }
                    None => Plan::candidates(Vec::new()),
                },
            }
        }
        // The index keys text by primary weights, which already ignore
        // kana form, voicing and size (7.2): the relaxed criterion's
        // finders select a superset.
        // `~` reads kana forms, voicing and size as one: the English keys
        // ignore them, so its finders narrow to a superset; Unicode raw and
        // the padded form's secondary weights tell them apart, so every
        // record is a candidate there.
        Kind::Relaxed(inner) if inner.text == TextKeys::Collated => {
            Plan::candidates(keyed_plan(table, field, inner, held).finders)
        }
        Kind::Relaxed(_) => Plan::candidates(Vec::new()),
    }
}

fn any_time() -> TimePattern {
    TimePattern {
        hour: Part::Any,
        minute: Part::Any,
        second: Part::Any,
    }
}

/// The finders of one criterion (6.4), ANDed within the request, when they
/// select exactly its records; criteria that need the client's evaluation
/// ([`plan`]) and text a field cannot read are [`FindError::Unsupported`].
pub fn finders(
    table: u64,
    field: u64,
    data_type: DataType,
    criterion: &str,
    order: DateOrder,
) -> Result<Vec<Finder>, FindError> {
    let context = FindContext::new(order, None);
    match Criterion::parse(criterion, data_type, &context) {
        Ok(None) => Ok(Vec::new()),
        Ok(Some(criterion)) => {
            let plan = plan(table, field, &criterion);
            if plan.exact {
                Ok(plan.finders)
            } else {
                Err(FindError::Unsupported)
            }
        }
        Err(Unreadable) => Err(FindError::Unsupported),
    }
}

/// The finder [`finders`] gives for `==` and a literal text whose index key
/// is `key`: a whole-value match on `field`'s value index (6.4), built from
/// the key. No find syntax is read, so a value holding characters a find
/// treats as operators (`#`, `.`, `=`, quotes) is looked up as itself; the
/// value index holds such a value under its own key (7.1). `None` when the
/// index holds only the key's start, where the finder would also select
/// longer values (7.1).
pub fn whole_value_finder(table: u64, field: u64, key: &[u8]) -> Option<Vec<u8>> {
    let mut key = key.to_vec();
    om_collate::cut_index_key(&mut key);
    (key.len() < om_collate::INDEX_KEY_LIMIT)
        .then(|| finder(kind::INDEX_MATCH, VALUE_SCOPE, 0, table, field, &key, &[]))
}

/// The SimpleFind program for `finders` of `table` (6.5): the finders in
/// upload spot `[2, 1]`, the found set to download spot `[3, 2]`, and
/// `master` (Constrain Found Set) in upload spot `[2, 3]` as KMasterList.
/// The instructions carry no access key, which FileMaker Server allows
/// (2.4).
pub fn program(file_id: u32, table: u64, finders: &[Vec<u8>], master: Option<&RidList>) -> Vec<u8> {
    keyed_program(file_id, table, finders, master, None)
}

/// [`program`] with the instruction's access key `[1, 1, 7]` (2.4), as
/// FileMaker Pro sends it.
pub fn keyed_program(
    file_id: u32,
    table: u64,
    finders: &[Vec<u8>],
    master: Option<&RidList>,
    access_key: Option<[u8; 16]>,
) -> Vec<u8> {
    find_program(file_id, table, finders, master, access_key, &[])
}

/// [`keyed_program`] after a check that each index of `checks` (fields of
/// `table`) exists: a GetKey (8.5) of its counts leaf, `[table, 01, field,
/// 10]` for the value index and `11` for the word index (7.1), into
/// download spot `[3, 3 + i]`, as instruction `1 + i`. The find is
/// instruction `checks.len() + 1`, so `access_key` must be that
/// instruction's. The host runs instructions in order (0), so the GetKeys
/// see the file before the find does: FileMaker Server answers a finder on
/// a missing index with no records and only then builds the index (7.1).
/// [`indexes_existed`] reads the answers.
pub fn checked_find_program(
    file_id: u32,
    table: u64,
    checks: &[FieldIndex],
    finders: &[Vec<u8>],
    access_key: Option<[u8; 16]>,
) -> Vec<u8> {
    find_program(file_id, table, finders, None, access_key, checks)
}

/// The GetKeys of a [`checked_find_program`] alone, into the same spots,
/// which [`indexes_existed`] reads. A client sends it after a host refused
/// a checked program, to learn whether an index the program read is
/// missing: OpenMaker Server refuses a query on an index it cannot build
/// in its field's language, where FileMaker Server would answer the
/// finder with no records and then build the index (7.1).
pub fn index_check_program(file_id: u32, table: u64, checks: &[FieldIndex]) -> Vec<u8> {
    assemble(index_checks(file_id, table, checks), Vec::new())
        .to_wire()
        .expect("index checks fit their slots")
}

/// [`checked_find_program`] of `field`'s value index alone: the find is
/// instruction 2.
pub fn checked_program(
    file_id: u32,
    table: u64,
    field: u64,
    finders: &[Vec<u8>],
    access_key: Option<[u8; 16]>,
) -> Vec<u8> {
    checked_find_program(
        file_id,
        table,
        &[(field, IndexKind::Value)],
        finders,
        access_key,
    )
}

/// For each of the first `count` checks of a [`checked_find_program`] or a
/// [`checked_join_program`], whether its GetKey found the index's counts:
/// a missing path leaves its spot empty (8.5). `None` for a malformed
/// reply.
pub fn indexes_existed(slots: &[u8], count: usize) -> Option<Vec<bool>> {
    let tree = KeyTree::from_slots(SlotReader::new(slots)).ok()?;
    Some(
        (0..count)
            .map(|i| {
                tree.node(&[&check_spot(i)])
                    .and_then(|spot| spot.child(&[0x00]))
                    .and_then(|counts| counts.value)
                    .is_some_and(|counts| !counts.is_empty())
            })
            .collect(),
    )
}

/// Whether the GetKey of [`checked_program`] found the value index's
/// counts.
pub fn value_index_existed(slots: &[u8]) -> Option<bool> {
    Some(indexes_existed(slots, 1)?[0])
}

/// GetKey (op 42).
const GET_KEY: u8 = 42;
/// Under a table: its field indexes (7.1).
const INDEXES: u8 = 1;
/// Under a field's index node: the value and word indexes' counts leaves
/// (7.1).
const VALUE_COUNTS: u8 = 0x10;
const WORD_COUNTS: u8 = 0x11;

/// The download spot (under `[3]`) of a program's check `i`: `3 + i`, as a
/// serial key (2.6).
fn check_spot(i: usize) -> Vec<u8> {
    om_hbam::serial::encode(3 + i as u64)
}

/// A key's operand form: its raw segment, `[u8 len][bytes]` (2.5, 8).
fn segment(value: u64) -> Vec<u8> {
    let bytes = uint_segment(value);
    [vec![bytes.len() as u8], bytes].concat()
}

/// The GetKey instructions of `checks` (fields of `table`), each into its
/// [`check_spot`].
fn index_checks(file_id: u32, table: u64, checks: &[FieldIndex]) -> Vec<Vec<OwnedNode>> {
    let leaf = |k: u8, v: Vec<u8>| OwnedNode::leaf(vec![k], v);
    checks
        .iter()
        .enumerate()
        .map(|(i, &(field, index))| {
            let counts = match index {
                IndexKind::Value => VALUE_COUNTS,
                IndexKind::Word => WORD_COUNTS,
            };
            // A key path into the hosted file, written as its raw segments
            // (2.5). FileMaker Pro sends its own GetKey checks without an
            // access key (2.4).
            let path = [
                segment(table),
                vec![1, INDEXES],
                segment(field),
                vec![1, counts],
            ]
            .concat();
            let spot = check_spot(i);
            vec![
                leaf(0x00, vec![GET_KEY]),
                leaf(0x01, path),
                leaf(0x02, [vec![1, 3, spot.len() as u8], spot].concat()),
                leaf(0x04, file_id.to_be_bytes().to_vec()),
            ]
        })
        .collect()
}

/// A program of `instructions`, numbered `1`, `2`, ... (2.6), and the
/// `upload` spots.
fn assemble(instructions: Vec<Vec<OwnedNode>>, upload: Vec<OwnedNode>) -> OwnedNode {
    let instructions = instructions
        .into_iter()
        .enumerate()
        .map(|(i, leaves)| OwnedNode::list(om_hbam::serial::encode(i as u64 + 1), leaves))
        .collect();
    OwnedNode::list(
        Vec::new(),
        vec![
            OwnedNode::list(vec![1], instructions),
            OwnedNode::list(vec![2], upload),
        ],
    )
}

fn find_program(
    file_id: u32,
    table: u64,
    finders: &[Vec<u8>],
    master: Option<&RidList>,
    access_key: Option<[u8; 16]>,
    checks: &[FieldIndex],
) -> Vec<u8> {
    let leaf = |k: u8, v: Vec<u8>| OwnedNode::leaf(vec![k], v);
    let mut instructions = index_checks(file_id, table, checks);
    let mut instruction = vec![
        leaf(0x00, vec![SIMPLE_FIND]),
        leaf(0x01, vec![1, 2, 1, 1]),
        leaf(0x02, vec![1, 3, 1, 2]),
        leaf(0x04, file_id.to_be_bytes().to_vec()),
        leaf(0x05, segment(table)),
    ];
    if let Some(key) = access_key {
        instruction.push(leaf(0x07, key.to_vec()));
    }
    let mut upload = vec![OwnedNode::list(
        vec![1],
        finders
            .iter()
            .enumerate()
            .map(|(i, f)| leaf(i as u8 + 1, f.clone()))
            .collect(),
    )];
    if let Some(master) = master {
        instruction.push(leaf(0x0d, vec![1, 2, 1, 3]));
        upload.push(master.to_node(vec![3]));
    }
    instructions.push(instruction);
    assemble(instructions, upload)
        .to_wire()
        .expect("a find program fits its slots")
}

/// CreateValueListEx (op 19).
const CREATE_VALUE_LIST: u8 = 19;

/// The CreateValueListEx program (query-program.md 8.7) that lists the
/// distinct values of `field`'s value index in `table`, in index order, at
/// most `max` of them: the values of a value list from a field and the
/// Insert from Index dialog. Laid out as FileMaker Pro's auto-complete
/// request (`0a` = `01`, the answer in spot `[3, 2]`), with the
/// instruction's access key.
pub fn value_list_program(
    file_id: u32,
    table: u64,
    field: u64,
    max: u32,
    access_key: Option<[u8; 16]>,
) -> Vec<u8> {
    list_program(file_id, table, field, max, access_key, false)
}

/// A value list preceded by GetKey of the value-index counts in the same
/// program (7.1, 8.5, 8.7). The list is instruction **2**; its access key
/// must use that instruction number. The check's answer is in spot 3.
pub fn checked_value_list_program(
    file_id: u32,
    table: u64,
    field: u64,
    max: u32,
    access_key: Option<[u8; 16]>,
) -> Vec<u8> {
    list_program(file_id, table, field, max, access_key, true)
}

fn list_program(
    file_id: u32,
    table: u64,
    field: u64,
    max: u32,
    access_key: Option<[u8; 16]>,
    checked: bool,
) -> Vec<u8> {
    let leaf = |k: u8, v: Vec<u8>| OwnedNode::leaf(vec![k], v);
    let segment = |value: u64| {
        let bytes = uint_segment(value);
        [vec![bytes.len() as u8], bytes].concat()
    };
    let mut instruction = vec![
        leaf(0x00, vec![CREATE_VALUE_LIST]),
        leaf(0x02, vec![1, 3, 1, 2]),
        leaf(0x04, file_id.to_be_bytes().to_vec()),
        leaf(0x05, segment(table)),
    ];
    if let Some(key) = access_key {
        instruction.push(leaf(0x07, key.to_vec()));
    }
    instruction.extend([
        leaf(0x09, segment(field)),
        leaf(0x0a, vec![1]),
        leaf(0x14, max.to_be_bytes().to_vec()),
    ]);
    let mut instructions = if checked {
        index_checks(file_id, table, &[(field, IndexKind::Value)])
    } else {
        Vec::new()
    };
    instructions.push(instruction);
    assemble(instructions, Vec::new())
        .to_wire()
        .expect("a value-list program fits its slots")
}

/// Query replies must finish their root list. KeyTree also accepts open
/// page continuations, so the value-list wire decoder checks completion here.
fn value_list_tree(slots: &[u8]) -> Option<KeyTree<'_>> {
    let mut reader = SlotReader::new(slots);
    let tree = KeyTree::from_slots(&mut reader).ok()?;
    (reader.root_closed() && reader.path().is_empty()).then_some(tree)
}

/// The counts checked by [`checked_value_list_program`]. A missing path
/// leaves an empty spot (which can be omitted); a nonempty count leaf must
/// contain both u64s (7.1). The list reply must be validated separately.
pub fn value_list_index_existed(slots: &[u8]) -> Option<bool> {
    value_list_index_counts(slots).map(|counts| counts.is_some())
}

/// The value index's counts that [`checked_value_list_program`] read
/// (7.1): records indexed and distinct keys, or `None` when the index does
/// not exist. `None` outside for a malformed reply.
pub fn value_list_index_counts(slots: &[u8]) -> Option<Option<(u64, u64)>> {
    let tree = value_list_tree(slots)?;
    let Some(spot) = tree.node(&[&[3]]) else {
        return Some(None);
    };
    if spot.value.is_some() {
        return None;
    }
    match spot.children.as_slice() {
        [] => Some(None),
        [counts] if counts.segment.0 == [0] && counts.children.is_empty() => {
            match counts.value.unwrap_or(&[]) {
                [] => Some(None),
                value => {
                    let value: [u8; 16] = value.try_into().ok()?;
                    let records = u64::from_be_bytes(value[..8].try_into().ok()?);
                    let keys = u64::from_be_bytes(value[8..].try_into().ok()?);
                    Some(Some((records, keys)))
                }
            }
        }
        _ => None,
    }
}

/// A CreateValueListEx reply's list (8.7).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListedValues {
    /// Each listed value's index key and text, in key order.
    pub values: Vec<(Vec<u8>, String)>,
    /// Whether the host listed every value whose key matched: its count is
    /// the number listed. `false` when the count is minus the number listed,
    /// the host having cut the list at the request's maximum `14` (8.7), or
    /// all ones, an older OpenMaker Server's cut at one slot value.
    pub complete: bool,
}

/// The values a CreateValueListEx reply lists in spot `2` (8.7): each
/// entry's record id and index key strings, then the record's text for
/// the value (SCSU, unmasked), and whether the list is complete. `None`
/// for a malformed reply.
///
/// The entries `00` are one value up to 1,000 bytes. Past that, FileMaker
/// Server sends a node: child `00` holds the total length (u32 BE), and
/// children `01`, `02`, ... hold the pieces in order, the form of a long
/// value in a file (8.7; observed on a live reply of 3,772 bytes in four
/// pieces, #464). `ff` counts the values listed, not the pieces.
pub fn value_list_values(slots: &[u8]) -> Option<ListedValues> {
    let tree = value_list_tree(slots)?;
    let spot = tree.node(&[&[2]])?;
    let entries_node = spot.child(&[0x00])?;
    let count_leaf = spot.child(&[0xff])?;
    if spot.value.is_some() || spot.children.len() != 2 || !count_leaf.children.is_empty() {
        return None;
    }
    let count = u64::from_be_bytes(count_leaf.value?.try_into().ok()?);
    let entries = match (entries_node.value, entries_node.children.is_empty()) {
        (Some(value), true) => std::borrow::Cow::Borrowed(value),
        (None, true) => std::borrow::Cow::Borrowed(&[][..]),
        // The total is a u32 (8.7), not a text header; the pieces must add
        // up to it, in order (`assembled`).
        (None, false) => {
            let total = entries_node.child(&[0x00])?;
            if total.value.map(<[u8]>::len) != Some(4) {
                return None;
            }
            entries_node.assembled()?
        }
        (Some(_), false) => return None,
    };
    let mut entries = &entries[..];
    let mut values = Vec::new();
    let mut previous: Option<&[u8]> = None;
    while !entries.is_empty() {
        let mut take_key = || {
            let (&total, rest) = entries.split_first()?;
            let (&len, key) = rest.get(..usize::from(total))?.split_first()?;
            if len == 0 || usize::from(len) != key.len() {
                return None;
            }
            entries = rest.get(usize::from(total)..)?;
            Some(key)
        };
        om_hbam::serial::decode(take_key()?).filter(|id| *id != 0)?;
        let key = take_key()?;
        if key == crate::keys::EMPTY_TEXT_KEY || previous.is_some_and(|prev| prev >= key) {
            return None;
        }
        previous = Some(key);
        let length = u32::from_be_bytes(entries.get(..4)?.try_into().ok()?) as usize;
        let end = 4usize.checked_add(length)?;
        let text = entries.get(4..end)?;
        entries = &entries[end..];
        let masked: Vec<u8> = text.iter().map(|b| b ^ 0x5a).collect();
        values.push((key.to_vec(), om_scsu::decode_scsu(&masked).ok()?));
    }
    // `ff` is the number listed, or minus it when the most values `14`
    // stopped the list before more keys with the prefix (8.7). All ones is
    // also an older OpenMaker Server's cut marker, whatever the number.
    let listed = values.len() as u64;
    let complete = count == listed;
    let cut = count == u64::MAX || (listed != 0 && count == listed.wrapping_neg());
    (complete || cut).then_some(ListedValues { values, complete })
}

/// JoinProjection (op 12).
const JOIN_PROJECTION: u8 = 12;

/// One source row of a JoinProjection: a row id and, per field, the index
/// keys the row gives for it.
pub type JoinRow = (u64, Vec<(u64, Vec<Vec<u8>>)>);

/// The JoinProjection program (query-program.md 9.2) that finds the records
/// of `table` related to any of `rows`: a record matches a row when, for
/// every predicate `(type, a, b)`, some value-index key of its field `a`
/// and some key the row gives for field `b` satisfy `a type b`
/// (`DBJoinPred::Type`: 0 `=`, 1 `≠`, 2 `<`, 3 `≤`, 4 `>`, 5 `≥`, 6 every
/// record). The rows go in upload spot `[2, 1]`, the predicates in
/// `[2, 2]`, the related records to `[3, 2]` ([`found`] reads them). A find
/// on a related field joins its matches back to the layout's table this
/// way, one relationship at a time (6.5).
pub fn join_program(
    file_id: u32,
    table: u64,
    rows: &[JoinRow],
    predicates: &[(u8, u64, u64)],
    access_key: Option<[u8; 16]>,
) -> Option<Vec<u8>> {
    checked_join_program(file_id, table, &[], rows, predicates, access_key)
}

/// [`join_program`] after a check that each index of `checks` (fields of
/// `table`) exists, as in [`checked_find_program`]: the JoinProjection is
/// instruction `checks.len() + 1`, and [`indexes_existed`] reads the
/// checks. A JoinProjection on a field without its value index relates
/// nothing (9.2, live:jp_unindexed).
pub fn checked_join_program(
    file_id: u32,
    table: u64,
    checks: &[FieldIndex],
    rows: &[JoinRow],
    predicates: &[(u8, u64, u64)],
    access_key: Option<[u8; 16]>,
) -> Option<Vec<u8>> {
    let leaf = |k: u8, v: Vec<u8>| OwnedNode::leaf(vec![k], v);
    let key_string = |value: u64| {
        let segment = uint_segment(value);
        [vec![segment.len() as u8 + 1, segment.len() as u8], segment].concat()
    };
    let mut instructions = index_checks(file_id, table, checks);
    let mut instruction = vec![
        leaf(0x00, vec![JOIN_PROJECTION]),
        leaf(0x01, vec![1, 2, 1, 1]),
        leaf(0x02, vec![1, 3, 1, 2]),
        leaf(0x03, vec![1, 2, 1, 2]),
        leaf(0x04, file_id.to_be_bytes().to_vec()),
        leaf(0x05, segment(table)),
    ];
    if let Some(key) = access_key {
        instruction.push(leaf(0x07, key.to_vec()));
    }
    let mut sorted: Vec<&JoinRow> = rows.iter().collect();
    sorted.sort_by_key(|(id, _)| *id);
    let source = sorted
        .into_iter()
        .map(|(id, fields)| {
            let mut fields: Vec<&(u64, Vec<Vec<u8>>)> = fields.iter().collect();
            fields.sort_by_key(|(field, _)| *field);
            OwnedNode::list(
                om_hbam::serial::encode(*id),
                fields
                    .into_iter()
                    .map(|(field, keys)| {
                        let mut keys = keys.clone();
                        keys.sort();
                        keys.dedup();
                        OwnedNode::list(
                            uint_segment(*field),
                            keys.into_iter()
                                .map(|k| OwnedNode::leaf(k, Vec::new()))
                                .collect(),
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    let predicates = predicates
        .iter()
        .enumerate()
        .map(|(i, (kind, a, b))| {
            leaf(
                i as u8 + 1,
                [vec![*kind], key_string(*a), key_string(*b)].concat(),
            )
        })
        .collect();
    instructions.push(instruction);
    assemble(
        instructions,
        vec![
            OwnedNode::list(vec![1], source),
            OwnedNode::list(vec![2], predicates),
        ],
    )
    .to_wire()
}

/// The records of `targets` that a JoinProjection relates to any of `rows`
/// (9.2), from the keys each target record gives for the predicates'
/// fields `a` (the keys its value index would hold) rather than from the
/// host's index: a target matches a row when every predicate `(type, a,
/// b)` relates some key of its `a` to some key of the row's `b`, keys
/// compared as bytes, `×` relating every record. No rows or no predicates
/// relate nothing. As the host leaves an empty value's key `01` out of
/// `≠` and the inequalities ([`crate::exec`], file-metadata.md 4), a
/// target's `01` relates only through `=`, `≥` and `×`.
pub fn join_on_keys(
    targets: &[JoinRow],
    rows: &[JoinRow],
    predicates: &[(u8, u64, u64)],
) -> RidList {
    fn keys(fields: &[(u64, Vec<Vec<u8>>)], field: u64) -> impl Iterator<Item = &[u8]> {
        fields
            .iter()
            .filter(move |(f, _)| *f == field)
            .flat_map(|(_, keys)| keys.iter().map(Vec::as_slice))
    }
    let relates = |kind: u8, a: &[u8], b: &[u8]| match kind {
        0 => a == b,
        1..=3 if a == EMPTY_TEXT_KEY => false,
        1 => a != b,
        2 => a < b,
        3 => a <= b,
        4 => a > b,
        5 => a >= b,
        6 => true,
        _ => false,
    };
    let mut related = RidList::new();
    if predicates.is_empty() {
        return related;
    }
    for (id, fields) in targets {
        let matches = rows.iter().any(|(_, row)| {
            predicates.iter().all(|&(kind, a, b)| {
                kind == 6 || keys(fields, a).any(|ka| keys(row, b).any(|kb| relates(kind, ka, kb)))
            })
        });
        if matches {
            related.insert(*id);
        }
    }
    related
}

/// What the host's SimpleFinds of groups of finders would answer from
/// indexes holding each record's keys (6.1, 6.3), decided as the records
/// stream past rather than from an index: for the finds whose index the
/// host did not have yet (7.1). An index holds a record under exactly the
/// keys the record gives ([`crate::keys`]), so a finder is decided record
/// by record and only the matching record ids are kept. FindDuplicates
/// compares records: it keeps a count per key, and the records the
/// group's other finders select that hold the key, within a budget.
pub struct ClientFind {
    groups: Vec<ClientGroup>,
    reads: Vec<FieldIndex>,
    budget: usize,
    resident: usize,
}

/// One SimpleFind's finders: those decided record by record and those
/// that find duplicates, each with the position of its index in
/// [`ClientFind::reads`].
struct ClientGroup {
    finders: Vec<(usize, crate::exec::Finder)>,
    duplicates: Vec<(usize, Duplicates)>,
    /// The records every finder selects, for a group without duplicates.
    selected: RidList,
}

/// For FindDuplicates: every non-empty key (the empty value is not
/// reported, 6.3), how many records hold it, and the records holding it
/// that the group's other finders select.
#[derive(Default)]
struct Duplicates {
    keys: HashMap<Vec<u8>, (usize, Vec<u64>)>,
}

/// Conservative charges for the map entry, spare capacity and transient
/// map growth, besides the key's bytes; and each holder including spare
/// vector capacity. This is an accounting budget, not an allocator limit.
/// Input keys live for one record only; result ids are output-sized.
const KEY_COST: usize = 256;
const HOLDER_COST: usize = 32;

/// Why a [`ClientFind`] cannot answer.
#[derive(Debug)]
pub enum ClientFindError {
    /// A finder this engine cannot evaluate.
    Query(crate::QueryError),
    /// Finding duplicates exceeds the state accounting budget.
    OverBudget { budget: usize },
}

impl std::fmt::Display for ClientFindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientFindError::Query(e) => e.fmt(f),
            ClientFindError::OverBudget { budget } => {
                write!(
                    f,
                    "finding duplicates exceeds the {budget}-byte state budget"
                )
            }
        }
    }
}

impl std::error::Error for ClientFindError {}

impl From<crate::QueryError> for ClientFindError {
    fn from(e: crate::QueryError) -> Self {
        ClientFindError::Query(e)
    }
}

impl ClientFind {
    /// Finds for `groups`, each the finders of one SimpleFind (ANDed),
    /// charging duplicate state against `budget` before retaining it.
    pub fn new(groups: &[Vec<Vec<u8>>], budget: usize) -> Result<Self, ClientFindError> {
        let mut reads: Vec<FieldIndex> = Vec::new();
        let mut position = |finder: &crate::exec::Finder| -> Result<usize, ClientFindError> {
            let (field, word) = finder.reads();
            let field = om_hbam::slot::be_uint(field)
                .ok_or(crate::QueryError::Malformed("finder field"))?;
            let read = (
                field,
                if word {
                    IndexKind::Word
                } else {
                    IndexKind::Value
                },
            );
            Ok(match reads.iter().position(|r| *r == read) {
                Some(i) => i,
                None => {
                    reads.push(read);
                    reads.len() - 1
                }
            })
        };
        let mut built = Vec::with_capacity(groups.len());
        for finders in groups {
            let mut group = ClientGroup {
                finders: Vec::new(),
                duplicates: Vec::new(),
                selected: RidList::new(),
            };
            for bytes in finders {
                let finder = crate::exec::Finder::parse(bytes)?;
                let read = position(&finder)?;
                if finder.finds_duplicates() {
                    group.duplicates.push((read, Duplicates::default()));
                } else {
                    group.finders.push((read, finder));
                }
            }
            built.push(group);
        }
        Ok(Self {
            groups: built,
            reads,
            budget,
            resident: 0,
        })
    }

    /// The indexes whose keys [`ClientFind::add`] takes, in its order.
    pub fn reads(&self) -> &[FieldIndex] {
        &self.reads
    }

    /// One record of the table: `keys[i]` are the keys the index
    /// `reads()[i]` holds it under.
    pub fn add(&mut self, record: u64, keys: &[BTreeSet<Vec<u8>>]) -> Result<(), ClientFindError> {
        if keys.len() != self.reads.len() {
            return Err(crate::QueryError::Malformed("client find keys").into());
        }
        let charge = |resident: &mut usize, bytes: usize| {
            if bytes > self.budget.saturating_sub(*resident) {
                return Err(ClientFindError::OverBudget {
                    budget: self.budget,
                });
            }
            *resident += bytes;
            Ok(())
        };
        for group in &mut self.groups {
            let mut selected = true;
            for (read, finder) in &group.finders {
                if !finder.selects(record, &keys[*read])? {
                    selected = false;
                    break;
                }
            }
            if group.duplicates.is_empty() {
                if selected {
                    group.selected.insert(record);
                }
                continue;
            }
            for (read, duplicates) in &mut group.duplicates {
                for key in &keys[*read] {
                    if key.as_slice() == crate::keys::EMPTY_TEXT_KEY {
                        continue;
                    }
                    let entry = match duplicates.keys.get_mut(key) {
                        Some(entry) => entry,
                        None => {
                            charge(&mut self.resident, KEY_COST)?;
                            charge(&mut self.resident, key.len())?;
                            duplicates.keys.entry(key.clone()).or_default()
                        }
                    };
                    entry.0 = (entry.0 + 1).min(2);
                    if selected {
                        charge(&mut self.resident, HOLDER_COST)?;
                        entry.1.push(record);
                    }
                }
            }
        }
        Ok(())
    }

    /// Each group's records, in the order of the groups.
    pub fn finish(self) -> Vec<RidList> {
        self.groups
            .into_iter()
            .map(|group| {
                if group.duplicates.is_empty() {
                    return group.selected;
                }
                group
                    .duplicates
                    .into_iter()
                    .map(|(_, duplicates)| {
                        duplicates
                            .keys
                            .into_values()
                            .filter(|(count, _)| *count > 1)
                            .flat_map(|(_, holders)| holders)
                            .collect::<RidList>()
                    })
                    .reduce(|all, next| all.and(&next))
                    .unwrap_or_default()
            })
            .collect()
    }
}

/// The found set in a SimpleFind reply's result spots: spot `2`.
pub fn found(slots: &[u8]) -> Option<RidList> {
    let tree = KeyTree::from_slots(SlotReader::new(slots)).ok()?;
    match tree.node(&[&[2]]) {
        Some(node) => RidList::from_node(node).ok(),
        None => Some(RidList::new()),
    }
}

/// Whether every request is empty (FileMaker's 400 "Find criteria are
/// empty").
pub fn all_empty(requests: &[Request]) -> bool {
    requests.iter().all(Request::is_empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: u64 = 0x8001;

    #[test]
    fn text_words_become_prefix_ranges_of_the_word_index() {
        // cap:912: "x" in field 81 45 of table 80 01.
        let found = finders(TABLE, 0x8145, DataType::Text, "x", DateOrder::MonthDayYear).unwrap();
        let key = om_collate::text_key("x");
        let mut expected = vec![2, 0, 1, 3, 2, 0x80, 0x01, 3, 2, 0x81, 0x45, 0, 1];
        expected.extend(key_operand(&key));
        let mut end = key.clone();
        end.extend([0xff, 0xfe]);
        expected.extend(key_operand(&end));
        assert_eq!(
            found,
            [Finder {
                bytes: expected,
                keep: true
            }]
        );
        // Two words are two finders.
        let two = finders(
            TABLE,
            2,
            DataType::Text,
            "big  apple",
            DateOrder::MonthDayYear,
        )
        .unwrap();
        assert_eq!(two.len(), 2);
    }

    #[test]
    fn operators_become_value_index_finders() {
        let order = DateOrder::MonthDayYear;
        let number = |text| finders(TABLE, 3, DataType::Number, text, order).unwrap();
        let seven = om_collate::number_key("7").unwrap();
        let exact = number("7");
        let [Finder { bytes, keep: true }] = exact.as_slice() else {
            panic!("one finder");
        };
        assert_eq!(&bytes[..3], [kind::INDEX_MATCH, VALUE_SCOPE, 0]);
        assert!(bytes.ends_with(&[key_operand(&seven), vec![0]].concat()));
        let at_least = number(">=7");
        assert_eq!(
            &at_least[0].bytes[..3],
            [kind::INDEX_RANGE, VALUE_SCOPE, INCLUDE_BEGIN]
        );
        // An open end stops short of the empty value and of invalid ones,
        // as FileMaker's do (6.4).
        assert!(at_least[0]
            .bytes
            .ends_with(&[key_operand(&seven), key_operand(&[0xfe, 0xff])].concat()));
        let below = number("<7");
        assert_eq!(below[0].bytes[2], 0);
        assert!(below[0]
            .bytes
            .ends_with(&[key_operand(&[0x01]), key_operand(&seven)].concat()));
        // U+2212 is a minus sign, as in a stored number (7.3).
        let minus_two = om_collate::number_key("-2").unwrap();
        assert!(number("\u{2212}2")[0]
            .bytes
            .ends_with(&[key_operand(&minus_two), vec![0]].concat()));
        let between = number("1...7");
        assert_eq!(between[0].bytes[2], INCLUDE_BEGIN | INCLUDE_END);
        // The finder FileMaker Server's engine built for `12:30 PM` on a
        // time field (7.3): the whole minute, to 45,059.999999 s.
        let minute = finders(TABLE, 3, DataType::Time, "12:30 PM", order).unwrap();
        assert_eq!(
            &minute[0].bytes[..3],
            [kind::INDEX_RANGE, VALUE_SCOPE, INCLUDE_BEGIN | INCLUDE_END]
        );
        let begin = [0x80, 0, 0, 0x80, 0x05, 0x56, 0x11, 0x10];
        let end = [0x80, 0, 0, 0x80, 0x05, 0x56, 0x16, 0xaa, 0xaa, 0xaa, 0xa0];
        assert!(minute[0]
            .bytes
            .ends_with(&[key_operand(&begin), key_operand(&end)].concat()));
        // Empty: the empty value's key `01`, for every type (7.3).
        let empty = finders(TABLE, 1, DataType::Text, "=", order).unwrap();
        assert!(empty[0].bytes.ends_with(&[2, 1, 1, 0]));
        let empty = number("=");
        assert!(empty[0].keep && empty[0].bytes.ends_with(&[2, 1, 1, 0]));
        assert_eq!(number("!")[0].bytes[0], kind::FIND_DUPLICATES);
        // Unknown syntax and values a field's type cannot key.
        for criterion in ["a*", "\"x\"", "//", "=="] {
            assert_eq!(
                finders(TABLE, 1, DataType::Text, criterion, order),
                Err(FindError::Unsupported)
            );
        }
        assert_eq!(
            finders(TABLE, 3, DataType::Date, "someday", order),
            Err(FindError::Unsupported)
        );
        assert_eq!(
            finders(TABLE, 3, DataType::Text, "  ", order),
            Ok(Vec::new())
        );
    }

    #[test]
    fn plans_are_exact_where_the_finders_express_the_operator() {
        let context = FindContext::new(DateOrder::MonthDayYear, Some((2019, 4, 4)));
        let plan_of = |data_type, text: &str| {
            let criterion = Criterion::parse(text, data_type, &context)
                .unwrap()
                .unwrap();
            plan(TABLE, 1, &criterion)
        };
        for (data_type, text) in [
            (DataType::Text, "chris smith"),
            (DataType::Text, "=Market"),
            (DataType::Text, "==John Smith"),
            (DataType::Text, "*"),
            (DataType::Text, "<M"),
            (DataType::Number, "12"),
            (DataType::Number, "1...7"),
            // FileMaker's own finders (6.4): IndexMatch `7f ff 9f`, IndexRange.
            (DataType::Number, "-0.5"),
            (DataType::Number, "-1...1"),
            (DataType::Number, "0"),
            (DataType::Number, "0...5"),
            (DataType::Date, "//"),
            (DataType::Date, "6/2019"),
            (DataType::Date, "2019"),
            (DataType::Date, "7/2019...10/2020"),
            (DataType::Time, "3 PM"),
            (DataType::Timestamp, "3/3/2019"),
            (DataType::Timestamp, "// 3 PM"),
        ] {
            let plan = plan_of(data_type, text);
            assert!(plan.exact, "{text}");
            assert!(!plan.finders.is_empty(), "{text}");
        }
        for (data_type, text, finders) in [
            (DataType::Text, "Jo*n", 1),
            (DataType::Text, "*phan*", 0),
            (DataType::Text, "\"Marten and Jones\"", 3),
            (DataType::Text, "*\"son & Phil\"*", 1),
            (DataType::Text, "==", 1),
            (DataType::Text, "?", 0),
            (DataType::Date, "Tuesday", 0),
            (DataType::Date, "*/{10..15}/2019", 1),
            (DataType::Time, "*:15", 1),
        ] {
            let plan = plan_of(data_type, text);
            assert!(!plan.exact, "{text}");
            assert_eq!(plan.finders.len(), finders, "{text}");
        }
        // A Unicode raw prefix ends at its successor: a key may hold the
        // code unit `ff ff`, which `ff fe` would leave out.
        assert_eq!(successor(&[0x01, 0x00, 0x41]), [0x01, 0x00, 0x42]);
        assert_eq!(successor(&[0x01, 0x00, 0xff, 0xff]), [0x01, 0x01]);
        assert!(successor(&[0xff]).is_empty());
        let raw_prefix =
            Criterion::parse("A", DataType::Text, &context.with_text_keys(TextKeys::Raw))
                .unwrap()
                .unwrap();
        assert!(plan(TABLE, 1, &raw_prefix).finders[0].bytes.ends_with(
            &[
                key_operand(&[0x01, 0x00, 0x41]),
                key_operand(&[0x01, 0x00, 0x42])
            ]
            .concat()
        ));
        // `~` on Unicode raw: no index narrows it, every record is
        // evaluated, and `ガッコウ` matches `~かつこう`.
        let raw = context.with_text_keys(TextKeys::Raw);
        let relaxed = Criterion::parse("~かつこう", DataType::Text, &raw)
            .unwrap()
            .unwrap();
        let plan = plan(TABLE, 1, &relaxed);
        assert!(!plan.exact && plan.finders.is_empty());
        assert!(relaxed.matches("ガッコウ", DateOrder::MonthDayYear, &|_| false));
        // A literal prefix narrows a pattern to the words that start with it.
        let pattern = plan_of(DataType::Text, "Jo*n");
        let jo = om_collate::text_key("jo");
        assert!(pattern.finders[0].bytes.ends_with(
            &[
                key_operand(&jo),
                key_operand(&[jo.clone(), PREFIX_END.to_vec()].concat())
            ]
            .concat()
        ));
        // Text ranges use the word index.
        assert_eq!(
            plan_of(DataType::Text, "<M").finders[0].bytes[1],
            WORD_SCOPE
        );
    }

    #[test]
    fn requests_replace_criteria_per_field() {
        let mut request = Request::default();
        assert!(all_empty(std::slice::from_ref(&request)));
        request.set(1, "a".into());
        request.set(2, "b".into());
        request.set(1, "c".into());
        assert_eq!(request.criteria, [(1, "c".into()), (2, "b".into())]);
        assert!(!all_empty(&[request]));
    }

    #[test]
    fn finders_read_the_index_of_their_scope() {
        let reads = |criterion: &str, data_type| -> Vec<IndexKind> {
            finders(TABLE, 1, data_type, criterion, DateOrder::MonthDayYear)
                .unwrap()
                .iter()
                .map(Finder::reads)
                .collect()
        };
        assert_eq!(reads("ship", DataType::Text), [IndexKind::Word]);
        assert_eq!(reads("==Ship", DataType::Text), [IndexKind::Value]);
        assert_eq!(reads("5", DataType::Number), [IndexKind::Value]);
        assert_eq!(reads("10/1/2026", DataType::Date), [IndexKind::Value]);
    }

    /// A whole-value finder built from a text key is the one `==` and that
    /// text give, and also stands for a text whose characters a find reads
    /// as operators.
    #[test]
    fn whole_value_finders_come_from_the_key() {
        let order = DateOrder::MonthDayYear;
        let parsed = finders(TABLE, 1, DataType::Text, "==Ship to dock 4", order).unwrap();
        assert_eq!(
            whole_value_finder(TABLE, 1, &om_collate::text_key("Ship to dock 4")),
            Some(parsed[0].bytes.clone())
        );
        // `#` is a find's digit wildcard: as a criterion, this text is no
        // whole-value find.
        assert!(finders(TABLE, 1, DataType::Text, "==50#Offset 25.5", order).is_err());
        assert!(whole_value_finder(TABLE, 1, &om_collate::text_key("50#Offset 25.5")).is_some());
        assert!(whole_value_finder(TABLE, 1, &om_collate::text_key(&"A".repeat(200))).is_none());
    }

    /// The live JoinProjection results of query-program.md 9.2 (target
    /// primary keys of records 1, 2, 3 ordered 3 < 1 < 2, Company empty in
    /// all three, source key K = record 1's), joined on the client.
    #[test]
    fn client_joins_relate_as_the_hosts_join_projection() {
        const PK: u64 = 9;
        const COMPANY: u64 = 5;
        let pk = |record: u8| vec![0x01, 0x12, record];
        let empty = vec![0x01];
        let targets: Vec<JoinRow> = [(1, 2), (2, 3), (3, 1)]
            .into_iter()
            .map(|(id, key)| {
                (
                    id,
                    vec![(PK, vec![pk(key)]), (COMPANY, vec![empty.clone()])],
                )
            })
            .collect();
        let k = pk(2);
        let row = |fields: Vec<(u64, Vec<Vec<u8>>)>| -> JoinRow { (0x51, fields) };
        let join = |rows: &[JoinRow], predicates: &[(u8, u64, u64)]| -> Vec<u64> {
            join_on_keys(&targets, rows, predicates).iter().collect()
        };
        let by_pk = [row(vec![(1, vec![k.clone()])])];
        for (kind, related) in [
            (0, &[1][..]),
            (1, &[2, 3]),
            (2, &[3]),
            (3, &[1, 3]),
            (4, &[2]),
            (5, &[1, 2]),
            (6, &[1, 2, 3]),
        ] {
            assert_eq!(join(&by_pk, &[(kind, PK, 1)]), related, "type {kind}");
        }
        // jp_two_keys, jp_two_rows.
        let two_keys = [row(vec![(1, vec![pk(9), k.clone()])])];
        assert_eq!(join(&two_keys, &[(0, PK, 1)]), [1]);
        let two_rows = [
            row(vec![(1, vec![k.clone()])]),
            (0x52, vec![(1, vec![pk(9)])]),
        ];
        assert_eq!(join(&two_rows, &[(0, PK, 1)]), [1]);
        // jp_two_preds_hit, jp_two_preds_miss, jp_company_emptykey.
        let both = |company: Vec<u8>| [row(vec![(1, vec![k.clone()]), (2, vec![company])])];
        let predicates = [(0, PK, 1), (0, COMPANY, 2)];
        assert_eq!(join(&both(empty.clone()), &predicates), [1]);
        assert!(join(&both(vec![0x01, 0x12, 0x0f]), &predicates).is_empty());
        let company = [row(vec![(2, vec![empty.clone()])])];
        assert_eq!(join(&company, &[(0, COMPANY, 2)]), [1, 2, 3]);
        // jp_pk_miss, jp_empty_src; and no predicate relates nothing.
        assert!(join(&[row(vec![(1, vec![pk(9)])])], &[(0, PK, 1)]).is_empty());
        assert!(join(&[], &[(0, PK, 1)]).is_empty());
        assert!(join(&by_pk, &[]).is_empty());
    }

    /// Finders decided from each record's keys as a host's index holds
    /// them: keys per line and per repetition, white space keyed, empty
    /// text `01`, `!` only for shared non-empty keys.
    #[test]
    fn client_finds_answer_from_the_keys_a_host_holds() {
        const NAME: u64 = 1;
        let texts = [
            (1, ["red\nblue", ""]),
            (2, ["red", ""]),
            (3, [" Ship ", ""]),
            (4, ["Write tests", "Ship"]),
            (5, ["", ""]),
        ];
        let find = |criterion: &str| -> Vec<u64> {
            let context = FindContext::new(DateOrder::MonthDayYear, None);
            let criterion = Criterion::parse(criterion, DataType::Text, &context)
                .unwrap()
                .unwrap();
            let finders: Vec<Vec<u8>> = plan(TABLE, NAME, &criterion)
                .finders
                .into_iter()
                .map(|f| f.bytes)
                .collect();
            let mut client = ClientFind::new(&[finders], usize::MAX).unwrap();
            for (record, texts) in &texts {
                let texts: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
                let keys: Vec<BTreeSet<Vec<u8>>> = client
                    .reads()
                    .iter()
                    .map(|(_, index)| {
                        let english = crate::keys::TextKeys::Collated;
                        match index {
                            IndexKind::Value => crate::keys::record_value_keys(
                                DataType::Text,
                                &texts,
                                DateOrder::MonthDayYear,
                                english,
                            ),
                            IndexKind::Word => crate::keys::word_keys(&texts, english),
                        }
                    })
                    .collect();
                client.add(*record, &keys).unwrap();
            }
            client.finish().remove(0).iter().collect()
        };
        assert_eq!(find("!"), [1, 2]);
        assert_eq!(find("==Ship"), [4]);
        assert_eq!(find("==red"), [1, 2]);
        assert_eq!(find("ship"), [3, 4]);
        assert_eq!(find("="), [5]);
    }

    /// Duplicates beyond the budget fail rather than grow.
    #[test]
    fn client_duplicates_stay_within_their_budget() {
        let finders = finders(TABLE, 1, DataType::Text, "!", DateOrder::MonthDayYear).unwrap();
        let finders: Vec<Vec<u8>> = finders.into_iter().map(|f| f.bytes).collect();
        let mut client = ClientFind::new(&[finders], 1024).unwrap();
        let mut failed = None;
        for record in 1..=100u64 {
            let keys = [BTreeSet::from([crate::keys::TextKeys::Collated
                .key(&format!("value {record}"))
                .unwrap()])];
            if let Err(e) = client.add(record, &keys) {
                failed = Some((record, e));
                break;
            }
        }
        let (record, error) = failed.expect("over budget");
        assert!(record < 20, "{record}");
        assert!(matches!(
            error,
            ClientFindError::OverBudget { budget: 1024 }
        ));
    }
}
