//! Running a program's instructions against a file (spec
//! `docs/re/query-program.md` 3, 6).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Bound, Range, RangeBounds};

use om_hbam::{serial, slot::be_uint, DataType, OwnedNode, RidList};

use crate::index::{Index, Indexes, VALUE_INDEX, WORD_INDEX};
use crate::program::{
    decode_path, leaf, opcode, take_key_string, Budget, Instruction, Program, Size, Work,
};
use crate::{QueryError, Source};

/// Who is running the program, for the per-instruction checks (spec 2.4).
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// The server's id for the open file; every instruction's `[1, i, 4]`
    /// must equal it.
    pub file_id: u32,
    /// The session's connection key. `Some` verifies each instruction's
    /// access key as FileMaker Server does; `None` skips that check (local
    /// files, recorded test vectors).
    pub connection_key: Option<[u8; 16]>,
}

/// The result of running a program.
#[derive(Debug)]
pub struct Outcome {
    /// The reply's slot stream: the result spots `[3]` (spec 4.1), headed by
    /// the error slot when the program failed (spec 11.2).
    pub slots: Vec<u8>,
    /// Why the program failed, if it did.
    pub error: Option<QueryError>,
    /// The changes the program's instructions make to the file, in order,
    /// for the caller to apply (the schema instructions, schema-path.md
    /// 3.4). A program that fails keeps the changes of the instructions
    /// before the failure, as FileMaker Server applies each as it runs.
    pub writes: Vec<Write>,
}

/// A change a program makes to the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    /// Replace the subtree at `path` with `node` (renamed to the path's
    /// last segment), creating missing parents.
    Put { path: Vec<Vec<u8>>, node: OwnedNode },
    /// Remove the subtree at `path`, if present.
    Remove { path: Vec<Vec<u8>> },
}

/// Run `program` against `file`. Instructions run in order; on the first
/// failure the reply carries the error slot `[00] = 02 <u16 code>` followed
/// by the spots written so far, as FileMaker Server answers. The program runs
/// within OpenMaker's limits ([`crate::program::limits`]): one that goes past
/// them fails the same way.
pub fn execute(source: &dyn Source, program: &[u8], context: &Context) -> Outcome {
    execute_within(source, program, context, Budget::DEFAULT)
}

/// [`execute`] within `budget`.
pub(crate) fn execute_within(
    source: &dyn Source,
    program: &[u8],
    context: &Context,
    budget: Budget,
) -> Outcome {
    execute_parsed(source, Program::parse_within(program, budget), context)
}

/// [`execute`] for a program already parsed with [`Program::parse`], or the
/// error parsing it gave, answered as [`execute`] answers it. A host parses
/// a client's program once this way, before it holds the file for reading,
/// and can look at its instructions first.
pub fn execute_parsed(
    source: &dyn Source,
    program: Result<Program, QueryError>,
    context: &Context,
) -> Outcome {
    let mut program = match program {
        Ok(program) => program,
        Err(error) => {
            let mut empty = Program::parse(&[0x80, 0x40]).expect("empty program");
            return failed(&mut empty, error);
        }
    };
    let mut indexes = Indexes::new(source);
    let mut writes = Vec::new();
    let instructions = program.instructions.clone();
    for instruction in &instructions {
        if let Err(error) = run(
            &mut program,
            &mut indexes,
            instruction,
            context,
            &mut writes,
        ) {
            let mut outcome = failed(&mut program, error);
            outcome.writes = writes;
            return outcome;
        }
    }
    match program.reply_slots() {
        Some(slots) => Outcome {
            slots,
            error: None,
            writes,
        },
        // A result no reply can carry fails the program (spec 8.7, 11.4),
        // rather than answering the empty list `80 40` as a success.
        None => {
            let error = QueryError::Limit("reply value size");
            Outcome {
                slots: error_alone(error.code()),
                error: Some(error),
                writes,
            }
        }
    }
}

fn failed(program: &mut Program, error: QueryError) -> Outcome {
    let code = error.code().to_be_bytes();
    program.set_error(vec![0x02, code[0], code[1]]);
    Outcome {
        slots: program
            .reply_slots()
            .unwrap_or_else(|| error_alone(error.code())),
        error: Some(error),
        writes: Vec::new(),
    }
}

/// The reply of a failed program whose spots cannot be sent: the error
/// slot `[00] = 02 <u16 code>` alone (spec 11.2).
fn error_alone(code: u16) -> Vec<u8> {
    let [high, low] = code.to_be_bytes();
    vec![0x80, 0x06, 0x00, 0x03, 0x02, high, low, 0x40]
}

fn run(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
    context: &Context,
    writes: &mut Vec<Write>,
) -> Result<(), QueryError> {
    check_access(instruction, context)?;
    match instruction.opcode {
        opcode::VERSION => Err(QueryError::InvalidCommand),
        opcode::DELETE_NODE => {
            program.remove_spot(&instruction.required_spot(leaf::DEST)?);
            Ok(())
        }
        opcode::COPY_NODE => {
            let src = instruction.required_spot(leaf::SRC)?;
            let dest = instruction.required_spot(leaf::DEST)?;
            program.copy_spot(&src, &dest)
        }
        opcode::AND | opcode::OR | opcode::AND_NOT => combine(program, instruction),
        opcode::INVERT => invert(program, indexes, instruction),
        opcode::SIMPLE_FIND => simple_find(program, indexes, instruction),
        opcode::PROJECTION => projection(program, indexes, instruction),
        opcode::JOIN_PROJECTION => join_projection(program, indexes, instruction),
        opcode::LOOKUP => lookup_value(program, indexes, instruction),
        opcode::SORT_LIST => crate::sort::sort_list(program, indexes, instruction),
        opcode::CREATE_VALUE_LIST_EX => create_value_list(program, indexes, instruction),
        opcode::EXISTS_IN_INDEX => exists_in_index(program, indexes, instruction),
        opcode::NODE_EXISTS | opcode::GET_KEY => file_node(program, indexes, instruction),
        opcode::UPLOAD_MULTIPLE => upload_multiple(program, indexes.source(), instruction, writes),
        opcode::GENERATE_MEMBER_KEY => generate_member_key(program, indexes, instruction),
        opcode::DELETE_TABLE => delete_table(instruction, writes),
        other => Err(QueryError::Unsupported("opcode", other)),
    }
}

/// UploadMultiple (op 40, schema-path.md 3.4): copy each child of the source
/// spot into the file under the destination path, then remove each key the
/// delete-list spot names there. FileMaker Pro saves catalog members with
/// it. An empty source or destination operand skips the instruction.
fn upload_multiple(
    program: &mut Program,
    source: &dyn Source,
    instruction: &Instruction,
    writes: &mut Vec<Write>,
) -> Result<(), QueryError> {
    let (Some(src), Some(dest)) = (instruction.leaf(leaf::SRC), instruction.leaf(leaf::DEST))
    else {
        return Ok(());
    };
    if src.is_empty() || dest.is_empty() {
        return Ok(());
    }
    let src = decode_path(src)?;
    let dest = decode_path(dest)?;
    // The members go beneath the destination: a node on its path in the
    // file that holds a value would become a list with a value, which no
    // file holds (hbam-writer.md 2.2), so the instruction fails instead.
    // Each node read counts as work; below a missing one there is nothing
    // to read. The host checks each write again as it applies it.
    for depth in 1..=dest.len() {
        let path: Vec<&[u8]> = dest[..depth].iter().map(Vec::as_slice).collect();
        let Some(node) = source.node(&path)? else {
            break;
        };
        program.spend(Size::of(&node))?;
        if om_hbam::writer::holds_value(&node) {
            return Err(QueryError::Malformed("a write beneath a value"));
        }
    }
    for (path, node) in program.copy_out(&src, &dest)? {
        writes.push(Write::Put { path, node });
    }
    if let Some(list) = instruction.spot(leaf::DEL_LIST)? {
        let keys: Vec<Vec<u8>> = program
            .spot(&list)?
            .map(|n| n.children.iter().map(|m| m.segment.clone()).collect())
            .unwrap_or_default();
        // Each removal holds its path: the destination and the key.
        let dest_bytes: usize = dest.iter().map(Vec::len).sum();
        program.hold(Size {
            nodes: keys.len() * (dest.len() + 1),
            bytes: keys.iter().map(|key| dest_bytes + key.len()).sum(),
        })?;
        for key in keys {
            let path = dest.iter().cloned().chain([key]).collect();
            writes.push(Write::Remove { path });
        }
    }
    Ok(())
}

/// GenerateMemberKey (op 43, schema-path.md 3.4): reserve the next member
/// id and modification count of a catalog, for a client about to add a
/// member (a layout) without the catalog's locks. Results: `{00 = id}` at
/// result spot 1, `{00 = mod count}` at result spot 2.
fn generate_member_key(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    let catalog = decode_path(
        instruction
            .leaf(leaf::OPERAND1)
            .ok_or(QueryError::Malformed("GenerateMemberKey without a catalog"))?,
    )?;
    if catalog.is_empty() {
        return Err(QueryError::Malformed("GenerateMemberKey of the root"));
    }
    let (id, mod_count) =
        indexes
            .source()
            .generate_member_key(&catalog)
            .ok_or(QueryError::Unsupported(
                "opcode",
                opcode::GENERATE_MEMBER_KEY,
            ))?;
    for (spot, value) in [(leaf::RESULT1, id), (leaf::RESULT2, mod_count)] {
        if let Some(spot) = instruction.spot(spot)? {
            program.set_spot(&spot, vec![OwnedNode::leaf(vec![0x00], value)])?;
        }
    }
    Ok(())
}

/// DeleteTable (op 50, schema-path.md 3.4): remove base table `[T]` with
/// everything under it (records, indexes, field catalog). The table catalog
/// and its occurrences are the client's to update.
fn delete_table(instruction: &Instruction, writes: &mut Vec<Write>) -> Result<(), QueryError> {
    let table = decode_path(
        instruction
            .leaf(leaf::SRC)
            .ok_or(QueryError::Malformed("DeleteTable without a table"))?,
    )?;
    match table.as_slice() {
        [t] if t.first().is_some_and(|b| (0x80..=0xcf).contains(b)) => {
            writes.push(Write::Remove { path: table });
            Ok(())
        }
        _ => Err(QueryError::Malformed(
            "DeleteTable of a key that is not a table",
        )),
    }
}

/// The instruction must name this file, and an access key it carries must be
/// the session's. FileMaker Server runs an instruction that has no access key
/// leaf at all (FileMaker Pro sends its schema checks, GetKey and NodeExists
/// on `[3, ...]`, without one) and refuses one whose key is present but wrong,
/// even empty (live:out12).
fn check_access(instruction: &Instruction, context: &Context) -> Result<(), QueryError> {
    if instruction.file_id()? != context.file_id {
        return Err(QueryError::WrongFile);
    }
    if let (Some(key), Some(given)) = (&context.connection_key, instruction.leaf(leaf::ACCESS_KEY))
    {
        let expected = om_crypto::query_access_key(instruction.number, key, context.file_id);
        if given != expected {
            return Err(QueryError::BadAccessKey);
        }
    }
    Ok(())
}

/// The RID list at a spot; a missing spot is empty.
fn rid_list(program: &Program, path: &[Vec<u8>]) -> Result<RidList, QueryError> {
    Ok(program
        .spot(path)?
        .map(RidList::from_owned)
        .transpose()?
        .unwrap_or_default())
}

/// The records a spot lists by key, as `KExcludeRows` comes from FileMaker
/// Pro: one leaf per record of the client's own transaction, `R = 01` (the
/// commit list's "new" state; capture s06-omserver-n), rather than a RID
/// list. A key with a value of up to one byte is one record; a longer value
/// is a bitmap run (spec 5.1), so a RID list reads the same.
fn row_set(program: &Program, path: &[Vec<u8>]) -> Result<RidList, QueryError> {
    let entries = program
        .spot(path)?
        .map(|n| n.children.as_slice())
        .unwrap_or_default();
    let is_run = |entry: &OwnedNode| entry.value.as_ref().is_some_and(|v| v.len() > 1);
    // The bitmap runs, decoded together.
    let runs = entries.iter().filter(|e| is_run(e)).cloned().collect();
    let mut rows = RidList::from_owned(&OwnedNode::list(Vec::new(), runs))?;
    for entry in entries.iter().filter(|e| !is_run(e)) {
        let id = serial::decode(&entry.segment)
            .ok_or(QueryError::Malformed("row key is not a record id"))?;
        rows.insert(id);
    }
    Ok(rows)
}

/// CombineRidLists (ops 5-7, spec 8.1): `dest := dest OP src`.
fn combine(program: &mut Program, instruction: &Instruction) -> Result<(), QueryError> {
    let src = rid_list(program, &instruction.required_spot(leaf::SRC)?)?;
    let dest_path = instruction.required_spot(leaf::DEST)?;
    let dest = rid_list(program, &dest_path)?;
    let result = match instruction.opcode {
        opcode::AND => dest.and(&src),
        opcode::OR => dest.or(&src),
        _ => dest.and_not(&src),
    };
    program.set_spot(&dest_path, result.entries())
}

/// InvertRidList (op 8, spec 8.2): every record of the table not in `dest`.
fn invert(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    let table = instruction.table()?;
    let dest_path = instruction.required_spot(leaf::DEST)?;
    let dest = rid_list(program, &dest_path)?;
    let all = indexes.all_records(&table)?;
    read_list(program.work(), all)?;
    let result = all.and_not(&dest);
    program.set_spot(&dest_path, result.entries())
}

/// Count a set operation over `list`, a RID list the program did not upload
/// (the table's records), towards the program's work: a block of 64 records
/// as a node of eight bytes.
fn read_list(work: &Work, list: &RidList) -> Result<(), QueryError> {
    work.spend(Size {
        nodes: list.blocks(),
        bytes: 8 * list.blocks(),
    })
}

/// A boolean result spot: `[dest, 00] = 01 01` or `01 00` (spec 8.4).
fn set_bool(program: &mut Program, dest: &[Vec<u8>], value: bool) -> Result<(), QueryError> {
    let flag = if value { [0x01, 0x01] } else { [0x01, 0x00] };
    program.set_spot(dest, vec![OwnedNode::leaf(vec![0x00], flag.to_vec())])
}

/// ExistsInIndex (op 18, spec 8.4): whether any key under `KSrc` belongs to
/// a record in the field's value index, other than the excluded records.
fn exists_in_index(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    let table = instruction.table()?;
    let field = single_segment(crate::program::decode_segments(
        instruction
            .leaf(leaf::FIELD_KEY)
            .ok_or(QueryError::Malformed("ExistsInIndex lacks a field"))?,
    )?)
    .map_err(|_| QueryError::Missing("field"))?;
    let dest = instruction.required_spot(leaf::DEST)?;
    if !indexes.field_exists(&table, &field)? {
        return Err(QueryError::Missing("field"));
    }
    let mut keys: Vec<&[u8]> = match instruction.spot(leaf::SRC)? {
        Some(src) => program
            .spot(&src)?
            .map(|spot| spot.children.iter().map(|k| k.segment.as_slice()).collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let excluded = match instruction.spot(leaf::EXCLUDE_ROWS)? {
        Some(path) => row_set(program, &path)?,
        None => RidList::new(),
    };
    let work = program.work();
    let index = indexes.finder_index(work, &table, &field, VALUE_INDEX)?;
    // The client's keys in index order, each looked up from where the last
    // one was: the index and the keys are walked together.
    keys.sort_unstable();
    keys.dedup();
    let mut exists = false;
    let mut from = 0;
    for key in keys {
        match index.find_from(from, key, work)? {
            Ok(at) => {
                if !index.records(at, work)?.and_not(&excluded).is_empty() {
                    exists = true;
                    break;
                }
                from = at + 1;
            }
            Err(at) => from = at,
        }
    }
    set_bool(program, &dest, exists)
}

/// NodeExists (41) and GetKey (42), spec 8.5: `KSrc` is a key path into the
/// hosted file. NodeExists answers whether it exists; GetKey returns the
/// value stored there (nothing for a missing path; a node, for which
/// FileMaker Server returns an opaque position, also yields nothing here).
fn file_node(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    let path = instruction.required_spot(leaf::SRC)?;
    let dest = instruction.required_spot(leaf::DEST)?;
    let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let node = indexes.source().node(&segments)?;
    if instruction.opcode == opcode::NODE_EXISTS {
        set_bool(program, &dest, node.is_some())
    } else {
        let value = node
            .filter(|n| !n.push && n.children.is_empty())
            .and_then(|n| n.value.clone());
        let children = value
            .map(|v| vec![OwnedNode::leaf(vec![0x00], v)])
            .unwrap_or_default();
        program.set_spot(&dest, children)
    }
}

/// Finder kinds (spec 6.2, `DBFinder::FindKind`).
mod kind {
    pub const INDEX_MATCH: u8 = 0;
    pub const NOT_INDEX_MATCH: u8 = 1;
    pub const INDEX_RANGE: u8 = 2;
    pub const ROW_ID_MATCH: u8 = 4;
    pub const NOT_ROW_ID_MATCH: u8 = 5;
    pub const ROW_ID_RANGE: u8 = 6;
    pub const INDEX_SCAN: u8 = 3;
    pub const FIND_DUPLICATES: u8 = 7;
    pub const MATCH: u8 = 8;
}

/// Finder option bit 7, IsOptional (spec 6.2): the finder only narrows the
/// records an exact finder after it tests, so a host may leave it out.
const IS_OPTIONAL: u8 = 1 << 7;

/// Finder scopes (spec 6.2): words, the whole value, each repetition.
const WORD_SCOPE: u8 = 0;

/// Finder option bits (spec 6.2).
const INCLUDE_BEGIN: u8 = 1 << 0;
const INCLUDE_END: u8 = 1 << 1;

/// The value index's key for an empty text value (spec 6.3, 7.2).
const EMPTY_TEXT_KEY: &[u8] = &[0x01];

/// One parsed finder (spec 6.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Finder {
    kind: u8,
    scope: u8,
    options: u8,
    field: Vec<u8>,
    begin: Vec<u8>,
    end: Vec<u8>,
    /// A Match finder's pattern (kind 8).
    pattern: Option<crate::matching::Pattern>,
}

impl Finder {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, QueryError> {
        let truncated = QueryError::Malformed("finder truncated");
        let [kind, scope, options, rest @ ..] = bytes else {
            return Err(truncated);
        };
        let (_table, rest) = take_key_string(rest)?;
        let (field, rest) = take_key_string(rest)?;
        let field = single_segment(field)?;
        let rest = rest.get(2..).ok_or(truncated)?; // u16 repetition
        if *kind == kind::MATCH {
            let (pattern, _) = crate::matching::Pattern::parse(rest)?;
            return Ok(Self {
                kind: *kind,
                scope: *scope,
                options: *options,
                field,
                begin: Vec::new(),
                end: Vec::new(),
                pattern: Some(pattern),
            });
        }
        if !matches!(kind, 0..=7 | 10..=12) {
            return Err(QueryError::Unsupported("finder kind", *kind));
        }
        let (begin, rest) = take_key_string(rest)?;
        let (end, _) = take_key_string(rest)?;
        Ok(Self {
            kind: *kind,
            scope: *scope,
            options: *options,
            field,
            begin: begin.concat(),
            end: end.concat(),
            pattern: None,
        })
    }

    /// The finder's bounds as a range over byte keys; an empty bound is open.
    fn range(&self) -> (Bound<&[u8]>, Bound<&[u8]>) {
        (
            bound(&self.begin, self.options & INCLUDE_BEGIN != 0),
            bound(&self.end, self.options & INCLUDE_END != 0),
        )
    }

    /// The field the finder reads and whether it reads the word index
    /// (scope 0) rather than the value index (spec 6.3).
    pub(crate) fn reads(&self) -> (&[u8], bool) {
        (&self.field, self.scope == 0)
    }

    /// Whether the finder reads the index to compare records: FindDuplicates
    /// (spec 6.3), which [`Finder::selects`] cannot decide alone.
    pub(crate) fn finds_duplicates(&self) -> bool {
        self.kind == kind::FIND_DUPLICATES
    }

    /// Whether [`Finder::evaluate`] would select `record`, which an index
    /// holds under exactly `keys`: what the finder's kind decides from one
    /// record (every kind but FindDuplicates).
    pub(crate) fn selects(
        &self,
        record: u64,
        keys: &BTreeSet<Vec<u8>>,
    ) -> Result<bool, QueryError> {
        let row_id = |key: &[u8]| {
            serial::decode(key).ok_or(QueryError::Malformed("row id is not a serial key"))
        };
        let bound_id = |bound: Bound<&[u8]>| -> Result<Bound<u64>, QueryError> {
            Ok(match bound {
                Bound::Included(k) => Bound::Included(row_id(k)?),
                Bound::Excluded(k) => Bound::Excluded(row_id(k)?),
                Bound::Unbounded => Bound::Unbounded,
            })
        };
        Ok(match self.kind {
            kind::INDEX_MATCH => keys.contains(&self.begin),
            kind::NOT_INDEX_MATCH => !keys.contains(&self.begin),
            kind::INDEX_RANGE => {
                let range = self.range();
                keys.iter().any(|k| in_range(k, range))
            }
            kind::ROW_ID_MATCH => record == row_id(&self.begin)?,
            kind::NOT_ROW_ID_MATCH => record != row_id(&self.begin)?,
            kind::ROW_ID_RANGE => {
                let (low, high) = self.range();
                (bound_id(low)?, bound_id(high)?).contains(&record)
            }
            other => return Err(QueryError::Unsupported("finder kind", other)),
        })
    }

    /// The records of `candidates` whose text in the field matches a Match
    /// finder's pattern (spec 6.3): each repetition's whole value (value and
    /// repetition scope) or each of its words (word scope), read from the
    /// stored record whether the field is indexed or not.
    fn matching(
        &self,
        pattern: &crate::matching::Pattern,
        indexes: &mut Indexes<'_>,
        work: &Work,
        table: &[u8],
        candidates: &RidList,
    ) -> Result<RidList, QueryError> {
        let repetitions = indexes
            .field(table, &self.field)?
            .and_then(|f| f.decoded_options())
            .map_or(1, |o| o.max_repetitions.clamp(1, 255));
        let cells: Vec<Vec<u8>> = std::iter::once(self.field.clone())
            .chain(be_uint(&self.field).into_iter().flat_map(|id| {
                (2..=u64::from(repetitions))
                    .filter_map(move |n| id.checked_mul(256)?.checked_add(n))
                    .map(om_hbam::slot::uint_segment)
            }))
            .collect();
        let mut found = RidList::new();
        for id in candidates.iter() {
            let mut matched = false;
            for cell in &cells {
                // Each look counts, a repetition the record lacks included.
                work.spend(Size { nodes: 1, bytes: 0 })?;
                let Some(value) = indexes.stored_text(work, table, cell, id)? else {
                    continue;
                };
                // Matching costs up to the pattern's length per character.
                let steps = value.len().saturating_mul(pattern.len().max(1));
                work.spend(Size {
                    nodes: 0,
                    bytes: steps,
                })?;
                matched = if self.scope == WORD_SCOPE {
                    om_collate::words(&value)
                        .into_iter()
                        .any(|word| pattern.matches(word))
                } else {
                    pattern.matches(&value)
                };
                if matched {
                    break;
                }
            }
            if matched {
                found.insert(id);
            }
        }
        Ok(found)
    }

    /// The records of `table` this finder selects. An index finder finds
    /// its keys by binary search and reads only the entries it selects
    /// (spec 7.1); a row-id finder reads the table's record list.
    fn evaluate(
        &self,
        indexes: &mut Indexes<'_>,
        work: &Work,
        table: &[u8],
    ) -> Result<RidList, QueryError> {
        let index_kind = if self.scope == 0 {
            WORD_INDEX
        } else {
            VALUE_INDEX
        };
        Ok(match self.kind {
            kind::INDEX_MATCH => {
                let index = indexes.finder_index(work, table, &self.field, index_kind)?;
                key_records(index, &self.begin, work)?
            }
            kind::NOT_INDEX_MATCH => {
                let index = indexes.finder_index(work, table, &self.field, index_kind)?;
                let matched = key_records(index, &self.begin, work)?;
                let all = indexes.all_records(table)?;
                read_list(work, all)?;
                all.and_not(&matched)
            }
            kind::INDEX_RANGE => {
                let index = indexes.finder_index(work, table, &self.field, index_kind)?;
                let mut found = RidList::new();
                index.add_records(index.range(self.range(), work)?, &mut found, work)?;
                found
            }
            kind::ROW_ID_MATCH | kind::NOT_ROW_ID_MATCH => {
                let id = serial::decode(&self.begin)
                    .ok_or(QueryError::Malformed("row id is not a serial key"))?;
                let all = indexes.all_records(table)?;
                if self.kind == kind::ROW_ID_MATCH {
                    work.spend(Size { nodes: 1, bytes: 8 })?;
                    all.within(Bound::Included(id), Bound::Included(id))
                } else {
                    read_list(work, all)?;
                    all.and_not(&[id].into_iter().collect())
                }
            }
            kind::ROW_ID_RANGE => {
                let decode = |key: &[u8]| {
                    serial::decode(key).ok_or(QueryError::Malformed("row id is not a serial key"))
                };
                let low = match self.range().0 {
                    Bound::Included(k) => Bound::Included(decode(k)?),
                    Bound::Excluded(k) => Bound::Excluded(decode(k)?),
                    Bound::Unbounded => Bound::Unbounded,
                };
                let high = match self.range().1 {
                    Bound::Included(k) => Bound::Included(decode(k)?),
                    Bound::Excluded(k) => Bound::Excluded(decode(k)?),
                    Bound::Unbounded => Bound::Unbounded,
                };
                let found = indexes.all_records(table)?.within(low, high);
                read_list(work, &found)?;
                found
            }
            // Records sharing a key with another record; the empty value is
            // not reported (spec 6.3, Med).
            kind::FIND_DUPLICATES => {
                let index = indexes.finder_index(work, table, &self.field, index_kind)?;
                let mut found = RidList::new();
                for at in 0..index.len() {
                    if index.key(at) == EMPTY_TEXT_KEY {
                        continue;
                    }
                    let records = index.records(at, work)?;
                    if records.len() > 1 {
                        found |= &records;
                    }
                }
                found
            }
            other => return Err(QueryError::Unsupported("finder kind", other)),
        })
    }
}

/// The records under `key` in `index`; none when it does not hold the key.
fn key_records(index: &Index<'_>, key: &[u8], work: &Work) -> Result<RidList, QueryError> {
    match index.find(key, work)? {
        Some(at) => index.records(at, work),
        None => Ok(RidList::new()),
    }
}

/// A finder bound; an empty key is open.
fn bound(key: &[u8], inclusive: bool) -> Bound<&[u8]> {
    match (key.is_empty(), inclusive) {
        (true, _) => Bound::Unbounded,
        (false, true) => Bound::Included(key),
        (false, false) => Bound::Excluded(key),
    }
}

fn in_range(key: &[u8], (low, high): (Bound<&[u8]>, Bound<&[u8]>)) -> bool {
    let above = match low {
        Bound::Included(b) => key >= b,
        Bound::Excluded(b) => key > b,
        Bound::Unbounded => true,
    };
    let below = match high {
        Bound::Included(b) => key <= b,
        Bound::Excluded(b) => key < b,
        Bound::Unbounded => true,
    };
    above && below
}

/// A key string that must name one segment (a field or table).
pub(crate) fn single_segment(mut segments: Vec<Vec<u8>>) -> Result<Vec<u8>, QueryError> {
    match segments.len() {
        1 => Ok(segments.remove(0)),
        _ => Err(QueryError::Malformed("expected a one-segment key")),
    }
}

/// SimpleFind (op 10, spec 6): the records of the table satisfying every
/// finder under `KSrc`, optionally restricted to `KMasterList`.
fn simple_find(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    let table = instruction.table()?;
    let src = instruction.required_spot(leaf::SRC)?;
    let dest = instruction.required_spot(leaf::DEST)?;
    if !indexes.table_exists(&table)? {
        program.set_spot(&dest, Vec::new())?;
        return Err(QueryError::Missing("table"));
    }
    let finders: Vec<Finder> = program
        .spot(&src)?
        .map(|spot| {
            spot.children
                .iter()
                .map(|f| Finder::parse(f.value.as_deref().unwrap_or_default()))
                .collect::<Result<_, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let work = program.work();
    let all = indexes.all_records(&table)?;
    read_list(work, all)?;
    let mut found = all.clone();
    if let Some(master) = instruction.spot(leaf::MASTER_LIST)? {
        let master = program
            .spot(&master)?
            .map(RidList::from_owned)
            .transpose()?
            .unwrap_or_default();
        found = found.and(&master);
    }
    // Every finder runs, even once nothing is left: a finder may have its
    // index built on demand for later programs (spec 7.1), or be one this
    // engine refuses.
    for finder in &finders {
        // IndexScan (3) is not built; FileMaker Pro sends it only as an
        // optional pre-filter of a Match finder (6.4: `*x*`), which then
        // tests every record the other finders leave.
        if finder.kind == kind::INDEX_SCAN && finder.options & IS_OPTIONAL != 0 {
            continue;
        }
        found = match &finder.pattern {
            // A Match finder tests the records the others leave.
            Some(pattern) => finder.matching(pattern, indexes, work, &table, &found)?,
            None => found.and(&finder.evaluate(indexes, work, &table)?),
        };
    }
    program.set_spot(&dest, found.entries())
}

/// `DBDataType`, the requested key type of a projected field (spec 9.1).
fn data_type_code(data_type: DataType) -> Option<u8> {
    match data_type {
        DataType::Text => Some(1),
        DataType::Number => Some(2),
        DataType::Date => Some(3),
        DataType::Time => Some(4),
        DataType::Timestamp => Some(5),
        DataType::Container => Some(6),
        DataType::Unknown(_) => None,
    }
}

/// Text index languages (`Locale::Type`, spec 7.2) by the key form they give.
mod language {
    /// No key at all.
    pub const NONE: u8 = 0;
    /// The English form plus `00 00`: System, Unicode standard.
    pub const SYSTEM: u8 = 1;
    pub const UNICODE: u8 = 3;
    /// The English form: English, English international.
    pub const ENGLISH: u8 = 21;
    pub const ENGLISH_INTERNATIONAL: u8 = 77;
}

/// The keys `field` gives each record of `records`, as Projection requests
/// them (`[u8 data type][u8 index language]`, spec 9.1). An indexed field is
/// answered from its value index; an unindexed text field from its stored
/// values, each line keyed as the value index would key it. Conversion to
/// another type, a language whose form the index does not hold, and
/// unindexed fields of other types are not supported yet. Empty values give
/// no key.
fn projected_keys(
    indexes: &mut Indexes<'_>,
    work: &Work,
    table: &[u8],
    field: &[u8],
    request: &[u8],
    records: &RidList,
) -> Result<Vec<(u64, Vec<u8>)>, QueryError> {
    let [requested_type, requested_language] = request else {
        return Err(QueryError::Malformed("projection field type"));
    };
    let definition = indexes
        .field(table, field)?
        .ok_or(QueryError::Missing("field"))?;
    let text = *requested_type == 1;
    // Type 0 (no type) keys nothing (live:proj_text_00), nor does text in
    // language 0.
    if *requested_type == 0 || text && *requested_language == language::NONE {
        return Ok(Vec::new());
    }
    let suffix: &[u8] = match *requested_language {
        _ if !text => &[],
        language::ENGLISH | language::ENGLISH_INTERNATIONAL => &[],
        language::SYSTEM | language::UNICODE => &[0x00, 0x00],
        _ => {
            return Err(QueryError::Unimplemented(
                "projection in this index language",
            ))
        }
    };
    let same_type = data_type_code(definition.data_type) == Some(*requested_type);

    if !indexes.is_indexed(work, table, field, VALUE_INDEX)? {
        let mut out = Vec::new();
        for id in records.iter() {
            let value = indexes
                .stored_text(work, table, field, id)?
                .unwrap_or_default();
            let mut lines: Vec<&str> = value
                .split(['\r', '\n'])
                .filter(|line| !line.is_empty())
                .collect();
            if lines.is_empty() {
                continue;
            }
            if !same_type {
                return Err(QueryError::Unimplemented("projection to another data type"));
            }
            if definition.data_type != DataType::Text {
                return Err(QueryError::Unimplemented(
                    "projection of an unindexed non-text field",
                ));
            }
            lines.sort_unstable();
            lines.dedup();
            out.extend(
                lines
                    .into_iter()
                    .map(|line| (id, projected_key(&om_collate::text_key(line), suffix))),
            );
        }
        return Ok(out);
    }

    // Every key of the index but the empty value's: inverting the index
    // reads all of it.
    let index = indexes.index(work, table, field, VALUE_INDEX)?;
    let keys: Vec<usize> = (0..index.len())
        .filter(|&at| index.key(at) != EMPTY_TEXT_KEY)
        .collect();
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    if !same_type {
        return Err(QueryError::Unimplemented("projection to another data type"));
    }
    // The index must hold the English form (no weight is `0000`).
    if text
        && keys
            .iter()
            .any(|&at| index.key(at).ends_with(&[0x00, 0x00]))
    {
        return Err(QueryError::Unimplemented(
            "projection from this index language",
        ));
    }
    let mut out = Vec::new();
    for at in keys {
        let key = projected_key(index.key(at), suffix);
        let holders = index.records(at, work)?;
        out.extend(holders.and(records).iter().map(|id| (id, key.clone())));
    }
    Ok(out)
}

/// A projected key: an English-form key with the requested language's
/// suffix, cut as an index cuts keys (query-program.md 7.1), so that it
/// meets the keys of the index a join reads.
fn projected_key(key: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut key = [key, suffix].concat();
    om_collate::cut_index_key(&mut key);
    key
}

/// Projection (op 11, spec 9.1): each record of the `KSrc` RID list keyed by
/// the fields of `KProjList`, as `dest / rid / field / key`. A record with no
/// key for any field gives no node. The optional Bloom filter (`0a`, `0b`)
/// only trims keys the next join would not match, so it is not applied.
fn projection(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    let table = instruction.table()?;
    // Only the table's records have keys; the list may name others.
    let src = rid_list(program, &instruction.required_spot(leaf::SRC)?)?;
    let src = src.and(indexes.all_records(&table)?);
    let fields: Vec<(Vec<u8>, Vec<u8>)> = program
        .spot(&instruction.required_spot(leaf::PROJ_LIST)?)?
        .map(|spot| {
            spot.children
                .iter()
                .map(|f| (f.segment.clone(), f.value.clone().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    let dest = instruction.required_spot(leaf::DEST)?;
    if !indexes.table_exists(&table)? {
        program.set_spot(&dest, Vec::new())?;
        return Err(QueryError::Missing("table"));
    }
    let mut rows: BTreeMap<u64, Vec<OwnedNode>> = BTreeMap::new();
    for (field, request) in &fields {
        let mut keys: BTreeMap<u64, Vec<OwnedNode>> = BTreeMap::new();
        for (id, key) in projected_keys(indexes, program.work(), &table, field, request, &src)? {
            keys.entry(id)
                .or_default()
                .push(OwnedNode::leaf(key, Vec::new()));
        }
        for (id, mut keys) in keys {
            keys.sort_by(|a, b| a.segment.cmp(&b.segment));
            rows.entry(id)
                .or_default()
                .push(OwnedNode::list(field.clone(), keys));
        }
    }
    let children = rows
        .into_iter()
        .map(|(id, mut fields)| {
            fields.sort_by(|a, b| a.segment.cmp(&b.segment));
            OwnedNode::list(serial::encode(id), fields)
        })
        .collect();
    program.set_spot(&dest, children)
}

/// A key string `[u8 total][u8 len][bytes]` of one segment.
fn one_segment_key(bytes: &[u8]) -> Vec<u8> {
    let mut out = vec![bytes.len() as u8 + 1, bytes.len() as u8];
    out.extend_from_slice(bytes);
    out
}

/// CreateValueListEx (op 19, spec 8.7): the distinct values of a field's
/// value index whose keys start with a prefix (every value without one), in
/// key order. FileMaker Pro asks for these for a value list of a field's
/// values and as the user types in a field with auto-complete (#464). The
/// result spot holds `00` = one entry per value, `[record id key
/// string][index key string][u32 len][text]` with the text unmasked, and
/// `ff` = u64 count of values listed. The empty value is left out.
///
/// As FileMaker Server answers:
/// - the most values `14` applies only with a prefix `13`; without one,
///   every value is listed (live1:vl_noprefix_max1);
/// - a list stopped at `14` while the next key also starts with the prefix
///   has `ff` = minus the count (live1:vl_all_max2), else the count;
/// - there is no size cut: past 1,000 bytes `00` is a node, child `00` the
///   u32 total and `01`, `02`, ... pieces of 1,000 bytes, the form of a long
///   value in a file (observed live, #464).
fn create_value_list(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    let table = indexes
        .base_table(&instruction.table()?)?
        .ok_or(QueryError::Missing("table"))?;
    let field = single_segment(crate::program::decode_segments(
        instruction
            .leaf(leaf::FIELD_KEY)
            .ok_or(QueryError::Malformed("CreateValueListEx lacks a field"))?,
    )?)?;
    let dest = instruction.required_spot(leaf::DEST)?;
    let prefix = match instruction.leaf(leaf::VALUE_PREFIX) {
        None => Vec::new(),
        Some([len, key @ ..]) if usize::from(*len) == key.len() => key.to_vec(),
        Some(_) => return Err(QueryError::Malformed("value-list prefix")),
    };
    let max = match (
        instruction.leaf(leaf::VALUE_PREFIX),
        instruction.leaf(leaf::MAX_VALUES),
    ) {
        (Some(_), Some(value)) => u32::from_be_bytes(
            value
                .try_into()
                .map_err(|_| QueryError::Malformed("value-list limit"))?,
        ) as usize,
        _ => usize::MAX,
    };
    if !indexes.field_exists(&table, &field)? {
        return Err(QueryError::Missing("field"));
    }
    // The keys with the prefix follow one another in the index; each that
    // holds a record is a value, listed with its lowest record.
    let work = program.work();
    let index = indexes.index(work, &table, &field, VALUE_INDEX)?;
    let mut matches: Vec<(Vec<u8>, u64)> = Vec::new();
    for at in index.prefixed(&prefix, work)? {
        if index.key(at) == EMPTY_TEXT_KEY {
            continue;
        }
        if let Some(id) = index.records(at, work)?.iter().next() {
            matches.push((index.key(at).to_vec(), id));
        }
    }
    let mut entries = Vec::new();
    let mut listed = 0;
    for (key, id) in matches.iter().take(max) {
        // The record's text for this key: its line of the stored value.
        let value = indexes
            .stored_text(work, &table, &field, *id)?
            .unwrap_or_default();
        let text = value
            .split(['\r', '\n'])
            .find(|line| om_collate::text_index_key(line) == *key)
            .unwrap_or(&value);
        let text: Vec<u8> = om_scsu::encode_scsu(text)
            .iter()
            .map(|b| b ^ 0x5a)
            .collect();
        entries.extend(one_segment_key(&serial::encode(*id)));
        entries.extend(one_segment_key(key));
        entries.extend((text.len() as u32).to_be_bytes());
        entries.extend(text);
        listed += 1;
    }
    // Minus the number listed when the limit stopped the list before more
    // values with the prefix.
    let count = listed as u64;
    let count = match listed < matches.len() {
        true => count.wrapping_neg(),
        false => count,
    };
    program.set_spot(
        &dest,
        vec![
            long_value(vec![0x00], entries)?,
            OwnedNode::leaf(vec![0xff], count.to_be_bytes().to_vec()),
        ],
    )
}

/// The bytes a long value's pieces hold (spec 8.7; file-format.md 4.3).
const PIECE: usize = 1_000;

/// `value` under `segment` as FileMaker writes a long value: one leaf up to
/// [`PIECE`] bytes, else a node whose child `00` holds the u32 total and
/// whose children `01`, `02`, ... (serial keys) hold the pieces.
fn long_value(segment: Vec<u8>, value: Vec<u8>) -> Result<OwnedNode, QueryError> {
    if value.len() <= PIECE {
        return Ok(OwnedNode::leaf(segment, value));
    }
    let total = u32::try_from(value.len())
        .map_err(|_| QueryError::Limit("value list larger than 4 GiB"))?;
    let mut children = vec![OwnedNode::leaf(vec![0x00], total.to_be_bytes().to_vec())];
    for (n, piece) in (1u64..).zip(value.chunks(PIECE)) {
        children.push(OwnedNode::leaf(serial::encode(n), piece.to_vec()));
    }
    Ok(OwnedNode::list(segment, children))
}

/// Join predicate types (spec 3.1, `DBJoinPred::Type`).
mod pred {
    pub const EQ: u8 = 0;
    pub const NE: u8 = 1;
    pub const LT: u8 = 2;
    pub const LE: u8 = 3;
    pub const GT: u8 = 4;
    pub const GE: u8 = 5;
    pub const CARTESIAN: u8 = 6;
}

/// One join predicate: compare the target's field `a` with the source row's
/// field `b`.
struct Predicate {
    kind: u8,
    a: Vec<u8>,
    b: Vec<u8>,
}

impl Predicate {
    fn parse(bytes: &[u8]) -> Result<Self, QueryError> {
        let (&kind, rest) = bytes
            .split_first()
            .ok_or(QueryError::Malformed("predicate truncated"))?;
        let (a, rest) = take_key_string(rest)?;
        let (b, _) = take_key_string(rest)?;
        Ok(Self {
            kind,
            a: single_segment(a)?,
            b: single_segment(b)?,
        })
    }

    /// The positions of the index keys that relate by this predicate to at
    /// least one of a source row's `keys`, found by binary search: one
    /// position per matching key for `=`, one range otherwise, since a key
    /// below some source key is below the greatest, and so on (spec 9.2).
    /// No keys on either side relate nothing; a predicate type this engine
    /// does not know fails only then, as when it compared keys one by one.
    fn related(
        &self,
        index: &Index<'_>,
        keys: &[&[u8]],
        work: &Work,
    ) -> Result<Vec<Range<usize>>, QueryError> {
        let (Some(least), Some(greatest)) = (keys.iter().min(), keys.iter().max()) else {
            return Ok(Vec::new());
        };
        if index.is_empty() {
            return Ok(Vec::new());
        }
        let all = 0..index.len();
        let unbounded = Bound::Unbounded;
        Ok(match self.kind {
            pred::EQ => {
                let mut found = Vec::new();
                for key in keys {
                    if let Some(at) = index.find(key, work)? {
                        found.push(at..at + 1);
                    }
                }
                found
            }
            // Every key differs from one of two different source keys.
            pred::NE if least != greatest => vec![all],
            pred::NE => match index.find(least, work)? {
                Some(at) => vec![0..at, at + 1..index.len()],
                None => vec![all],
            },
            pred::LT => vec![index.range((unbounded, Bound::Excluded(greatest)), work)?],
            pred::LE => vec![index.range((unbounded, Bound::Included(greatest)), work)?],
            pred::GT => vec![index.range((Bound::Excluded(least), unbounded), work)?],
            pred::GE => vec![index.range((Bound::Included(least), unbounded), work)?],
            other => return Err(QueryError::Unsupported("join predicate", other)),
        })
    }
}

/// LookupValue (op 17, spec 8.6): the record a lookup copies from when no
/// related record matches exactly, its "next lower" or "next higher" value.
/// The predicates are taken in order, each as JoinProjection takes it, the
/// records of each ANDed with those of the earlier ones. While some records
/// satisfy every predicate so far, the next one is taken; if records satisfy
/// them all, an exact match names no record (`[dest, 00] = 01 00`). At the
/// first predicate that leaves none, the source row's key for that
/// predicate (its last) is looked for in the value index of its target
/// field: from the largest key below it (lower) or the smallest above it
/// (higher), the first entry holding a record that satisfied every earlier
/// predicate gives its lowest record. None, or a row without a key there,
/// is error 401.
fn lookup_value(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    let table = instruction.table()?;
    let src = instruction.required_spot(leaf::SRC)?;
    let dest = instruction.required_spot(leaf::DEST)?;
    let preds = instruction.required_spot(leaf::DEL_LIST)?;
    let higher = instruction.leaf(leaf::LOOKUP_HIGHER) == Some(&[0x01, 0x01][..]);
    // The inputs are read before the destination is cleared: a program may
    // name one spot for both.
    let predicates: Vec<Vec<u8>> = program
        .spot(&preds)?
        .map(|spot| {
            spot.children
                .iter()
                .map(|p| p.value.clone().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    // FileMaker Pro sends the current record's one row.
    let row = program
        .spot(&src)?
        .and_then(|spot| spot.children.first().cloned())
        .unwrap_or_default();
    program.set_spot(&dest, Vec::new())?;
    if !indexes.table_exists(&table)? {
        return Err(QueryError::Missing("table"));
    }
    let work = program.work();
    let mut so_far: Option<RidList> = None;
    // Each predicate is read when it is reached: once the walk begins the
    // later ones are not consulted (8.6).
    for bytes in &predicates {
        let predicate = &Predicate::parse(bytes)?;
        let keys = source_keys(&row, &predicate.b);
        // The row's keys are read again for each predicate.
        work.spend(Size {
            nodes: keys.len(),
            bytes: keys.iter().map(|k| k.len()).sum(),
        })?;
        let matches = if predicate.kind == pred::CARTESIAN {
            let all = indexes.all_records(&table)?;
            read_list(work, all)?;
            all.clone()
        } else {
            let index = indexes.index(work, &table, &predicate.a, VALUE_INDEX)?;
            let ranges = predicate.related(index, &keys, work)?;
            records_at(index, ranges, work)?
        };
        let both = match &so_far {
            Some(earlier) => earlier.and(&matches),
            None => matches,
        };
        if !both.is_empty() {
            so_far = Some(both);
            continue;
        }
        let Some(&key) = keys.last() else {
            return Err(QueryError::NoRecords);
        };
        if predicate.kind == pred::CARTESIAN {
            return Err(QueryError::NoRecords);
        }
        let index = indexes.index(work, &table, &predicate.a, VALUE_INDEX)?;
        let positions: Box<dyn Iterator<Item = usize>> = if higher {
            let above = index.range((Bound::Excluded(key), Bound::Unbounded), work)?;
            Box::new(above)
        } else {
            let below = index.range((Bound::Unbounded, Bound::Excluded(key)), work)?;
            Box::new(below.rev())
        };
        for at in positions {
            let records = index.records(at, work)?;
            let candidates = match &so_far {
                Some(earlier) => records.and(earlier),
                None => records,
            };
            let lowest = candidates.iter().next();
            if let Some(id) = lowest {
                return program.set_spot(&dest, vec![lookup_reply(id)]);
            }
        }
        return Err(QueryError::NoRecords);
    }
    // Every predicate is satisfied: an exact match, which names no record.
    program.set_spot(&dest, vec![lookup_reply(0)])
}

/// LookupValue's reply leaf `00`: the record id as a one-segment key,
/// `[u8 len][serial key]` (`01 02` for record 2).
fn lookup_reply(id: u64) -> OwnedNode {
    let serial = serial::encode(id);
    let mut value = vec![serial.len() as u8];
    value.extend(serial);
    OwnedNode::leaf(vec![0x00], value)
}

/// The keys a source row gives for its field `field`.
fn source_keys<'r>(row: &'r OwnedNode, field: &[u8]) -> Vec<&'r [u8]> {
    row.child(field)
        .map(|keys| keys.children.iter().map(|k| k.segment.as_slice()).collect())
        .unwrap_or_default()
}

/// Positions as ascending, disjoint ranges, so each entry is read once.
fn merged(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.retain(|r| !r.is_empty());
    ranges.sort_unstable_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match out.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => out.push(range),
        }
    }
    out
}

/// The records under the index entries at `ranges`, each entry read once.
fn records_at(
    index: &Index<'_>,
    ranges: Vec<Range<usize>>,
    work: &Work,
) -> Result<RidList, QueryError> {
    let mut found = RidList::new();
    for range in merged(ranges) {
        index.add_records(range, &mut found, work)?;
    }
    Ok(found)
}

/// JoinProjection (op 12, spec 3.1): the target-table records related to any
/// source row, a row matching when every predicate relates some value index
/// key of the target's field to some key the row gives for its field.
///
/// Each predicate finds a row's related keys by binary search (7.1). With
/// one predicate the rows' related keys are gathered first, so each index
/// entry is read once however many rows relate to it; with several, each
/// row's records are the intersection of what its predicates relate. A
/// cartesian predicate relates every record of the table, so it restricts
/// the union once rather than every row.
fn join_projection(
    program: &mut Program,
    indexes: &mut Indexes<'_>,
    instruction: &Instruction,
) -> Result<(), QueryError> {
    let table = instruction.table()?;
    let src = instruction.required_spot(leaf::SRC)?;
    let dest = instruction.required_spot(leaf::DEST)?;
    let preds = instruction.required_spot(leaf::DEL_LIST)?;
    if !indexes.table_exists(&table)? {
        program.set_spot(&dest, Vec::new())?;
        return Err(QueryError::Missing("table"));
    }
    let predicates: Vec<Predicate> = program
        .spot(&preds)?
        .map(|spot| {
            spot.children
                .iter()
                .map(|p| Predicate::parse(p.value.as_deref().unwrap_or_default()))
                .collect::<Result<_, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let rows = program
        .spot(&src)?
        .map(|spot| spot.children.as_slice())
        .unwrap_or_default();
    let work = program.work();
    let (cartesian, keyed): (Vec<&Predicate>, Vec<&Predicate>) =
        predicates.iter().partition(|p| p.kind == pred::CARTESIAN);

    let found = if rows.is_empty() || predicates.is_empty() {
        RidList::new()
    } else {
        for predicate in &keyed {
            indexes.index(work, &table, &predicate.a, VALUE_INDEX)?;
        }
        let targets: Vec<&Index<'_>> = keyed
            .iter()
            .map(|p| indexes.cached(&table, &p.a, VALUE_INDEX))
            .collect();
        let related = match (keyed.as_slice(), targets.as_slice()) {
            ([], _) => None,
            ([predicate], [index]) => {
                let mut related = Vec::new();
                for row in rows {
                    let keys = source_keys(row, &predicate.b);
                    related.extend(predicate.related(index, &keys, work)?);
                }
                Some(records_at(index, related, work)?)
            }
            _ => {
                let mut found = RidList::new();
                for row in rows {
                    let mut row_matches: Option<RidList> = None;
                    for (predicate, index) in keyed.iter().zip(&targets) {
                        let keys = source_keys(row, &predicate.b);
                        let related = predicate.related(index, &keys, work)?;
                        let matches = records_at(index, related, work)?;
                        row_matches = Some(match row_matches {
                            Some(so_far) => so_far.and(&matches),
                            None => matches,
                        });
                    }
                    found |= &row_matches.unwrap_or_default();
                }
                Some(found)
            }
        };
        if cartesian.is_empty() {
            related.unwrap_or_default()
        } else {
            let all = indexes.all_records(&table)?;
            read_list(work, all)?;
            match related {
                Some(related) => related.and(all),
                None => all.clone(),
            }
        }
    };
    program.set_spot(&dest, found.entries())
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::HbamFile;

    /// A one-instruction program: `[1, 1]` with the given leaves.
    fn program(leaves: &[(u8, &[u8])]) -> Vec<u8> {
        let instruction = OwnedNode::list(
            vec![1],
            leaves
                .iter()
                .map(|(k, v)| OwnedNode::leaf(vec![*k], v.to_vec()))
                .collect(),
        );
        let root = OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(vec![1], vec![instruction])],
        );
        root.to_wire().unwrap()
    }

    /// The analysis host's git-ignored copy of test project.
    fn test_project() -> HbamFile {
        HbamFile::open("../../re/work/omserver/testproject-live-1234.fmp12").unwrap()
    }

    #[test]
    #[ignore = "requires a git-ignored copy of test project"]
    fn rejects_a_wrong_access_key_with_unknown_error() {
        let key = [7u8; 16];
        let good = om_crypto::query_access_key(1, &key, 5);
        let mut bad = good;
        bad[0] ^= 1;
        let file = test_project();
        let context = Context {
            file_id: 5,
            connection_key: Some(key),
        };
        let leaves = |access: &[u8; 16]| {
            program(&[
                (leaf::OPCODE, &[opcode::INVERT]),
                (leaf::DEST, &[1, 3, 1, 2]),
                (leaf::FILE_ID, &5u32.to_be_bytes()),
                (leaf::TABLE, &[2, 0x80, 0x02]),
                (leaf::ACCESS_KEY, access),
            ])
        };
        let ok = execute(&file, &leaves(&good), &context);
        assert!(ok.error.is_none(), "{:?}", ok.error);
        let refused = execute(&file, &leaves(&bad), &context);
        assert!(matches!(refused.error, Some(QueryError::BadAccessKey)));
        assert_eq!(
            refused.slots,
            [0x80, 0x06, 0x00, 0x03, 0x02, 0xff, 0xff, 0x40]
        );
    }

    /// The reply of a program that failed before writing any spot: the
    /// error slot `[00] = 02 ffff` alone (spec 11.2).
    const UNKNOWN_ERROR: [u8; 8] = [0x80, 0x06, 0x00, 0x03, 0x02, 0xff, 0xff, 0x40];

    const FILE_ID: u32 = 5;

    /// Table `80 01` with records 1 to 20 and a text field 7 that no record
    /// fills, so every record ties on it.
    struct Tasks;

    impl Source for Tasks {
        fn catalog(&self) -> om_hbam::Result<om_hbam::Catalog> {
            let field = om_hbam::Field {
                id: 7,
                name: "Code".into(),
                kind: om_hbam::FieldKind::Normal,
                data_type: DataType::Text,
                options: Vec::new(),
            };
            Ok(om_hbam::Catalog {
                tables: vec![om_hbam::Table {
                    id: 0x8001,
                    name: "Tasks".into(),
                    fields: vec![field],
                }],
                ..om_hbam::Catalog::default()
            })
        }

        fn node(&self, path: &[&[u8]]) -> om_hbam::Result<Option<std::borrow::Cow<'_, OwnedNode>>> {
            Ok((path == [&[0x80, 0x01][..], &[0x0d]]).then(|| {
                std::borrow::Cow::Owned((1..=20).collect::<RidList>().to_node(vec![0x0d]))
            }))
        }
    }

    /// Instruction `number` with `opcode`, this file's id and `leaves`.
    fn instruction(number: u64, opcode: u8, leaves: Vec<OwnedNode>) -> OwnedNode {
        let mut children = vec![
            OwnedNode::leaf(vec![leaf::OPCODE], vec![opcode]),
            OwnedNode::leaf(vec![leaf::FILE_ID], FILE_ID.to_be_bytes().to_vec()),
        ];
        children.extend(leaves);
        children.sort_by(|a, b| a.segment.cmp(&b.segment));
        OwnedNode::list(serial::encode(number), children)
    }

    /// CopyNode (op 4) of the spot at `src` to `dest`, paths as operands.
    fn copy(number: u64, src: &[u8], dest: &[u8]) -> OwnedNode {
        instruction(
            number,
            opcode::COPY_NODE,
            vec![
                OwnedNode::leaf(vec![leaf::SRC], src.to_vec()),
                OwnedNode::leaf(vec![leaf::DEST], dest.to_vec()),
            ],
        )
    }

    fn instructions(instructions: Vec<OwnedNode>) -> Vec<u8> {
        OwnedNode::list(Vec::new(), vec![OwnedNode::list(vec![1], instructions)])
            .to_wire()
            .unwrap()
    }

    /// Run `program` within `budget` on a thread with the 2 MiB stack each
    /// OpenMaker Server connection has.
    fn on_a_connection_thread(program: Vec<u8>, budget: Budget) -> Outcome {
        let context = Context {
            file_id: FILE_ID,
            connection_key: None,
        };
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(2 << 20)
                .spawn_scoped(scope, || execute_within(&Tasks, &program, &context, budget))
                .unwrap()
                .join()
                .unwrap()
        })
    }

    /// #301: one CopyNode into a spot path of 10,000 empty segments made the
    /// tree that deep, and encoding the reply overflowed the stack and
    /// aborted the server. No spot may be deeper than a program can be.
    #[test]
    fn a_spot_deeper_than_a_program_holds_is_refused() {
        let deep = |segments: usize| {
            let mut dest = vec![1, 3];
            dest.extend(std::iter::repeat(0).take(segments));
            // The instruction list `[01]`: instructions, then their leaves.
            instructions(vec![copy(1, &[1, 1], &dest)])
        };
        let outcome = on_a_connection_thread(deep(10_000), Budget::DEFAULT);
        assert!(matches!(outcome.error, Some(QueryError::Malformed(_))));
        assert_eq!(outcome.slots, UNKNOWN_ERROR);
        // Paths that fit, whose copy would not: `[03]` and 15 segments, with
        // the copy's two levels below, make 18.
        let outcome = on_a_connection_thread(deep(15), Budget::DEFAULT);
        assert!(matches!(outcome.error, Some(QueryError::Malformed(_))));
        assert_eq!(outcome.slots, UNKNOWN_ERROR);
        // 17 levels are what a program may hold.
        let outcome = on_a_connection_thread(deep(14), Budget::DEFAULT);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
    }

    /// #301: each CopyNode of the whole program (an empty source) into
    /// `[2, i]` doubled the tree; 18 of them took 3 GB. Each such copy nests
    /// the ones before it, so the depth limit stops it after a few. Copies
    /// that nest one level each are stopped by the program's size, counted
    /// before the copy is made, and copying and deleting over and over by
    /// its work. (Small budgets keep the test light; [`Budget::DEFAULT`]'s
    /// limits work the same way.)
    #[test]
    fn copies_of_the_program_stop_at_its_limits() {
        let doubling = (1..=22)
            .map(|i| copy(i, &[], &[1, 2, 1, i as u8]))
            .collect();
        let outcome = on_a_connection_thread(instructions(doubling), Budget::DEFAULT);
        assert!(matches!(outcome.error, Some(QueryError::Malformed(_))));
        assert_eq!(outcome.slots, UNKNOWN_ERROR);

        let budget = Budget {
            tree: Size {
                nodes: 100_000,
                bytes: 1 << 20,
            },
            work: Size {
                nodes: 1_000_000,
                bytes: 1 << 24,
            },
            ..Budget::DEFAULT
        };
        // `[2]` holds 100 leaves; each copy of `[2]` into `[2, i]` doubles it.
        let upload = OwnedNode::list(
            vec![2],
            vec![OwnedNode::list(
                vec![0],
                (0..100u8)
                    .map(|k| OwnedNode::leaf(vec![k], vec![k]))
                    .collect(),
            )],
        );
        let doubling = (1..=14)
            .map(|i| copy(i, &[1, 2], &[1, 2, 1, i as u8]))
            .collect();
        let program = OwnedNode::list(Vec::new(), vec![OwnedNode::list(vec![1], doubling), upload]);
        let outcome = on_a_connection_thread(program.to_wire().unwrap(), budget);
        assert!(matches!(
            outcome.error,
            Some(QueryError::Limit("program size"))
        ));
        assert_eq!(outcome.slots, UNKNOWN_ERROR);

        let delete = |number| {
            instruction(
                number,
                opcode::DELETE_NODE,
                vec![OwnedNode::leaf(vec![leaf::DEST], vec![1, 2, 1, 1])],
            )
        };
        let churn = (1..=1000)
            .map(|i| match i % 2 {
                1 => copy(i, &[], &[1, 2, 1, 1]),
                _ => delete(i),
            })
            .collect();
        let outcome = on_a_connection_thread(instructions(churn), budget);
        assert!(matches!(outcome.error, Some(QueryError::Limit("work"))));
        assert_eq!(outcome.slots, UNKNOWN_ERROR);
    }

    /// #301: a SortList by one field named 3,400 times recursed once per
    /// level for records that tie and overflowed the stack. A sort names at
    /// most 64 fields, and at that limit the sort answers on a connection's
    /// stack.
    #[test]
    fn a_sort_names_at_most_64_fields() {
        let sort = |fields: u16| {
            let mut spec = vec![0, 0];
            spec.extend(fields.to_be_bytes());
            for _ in 0..fields {
                // Table 80 01, field 7, repetition 1, no summary or value
                // list, ascending, the field's own language.
                spec.extend([
                    3, 2, 0x80, 0x01, 2, 1, 7, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                ]);
            }
            instructions(vec![instruction(
                1,
                opcode::SORT_LIST,
                vec![
                    OwnedNode::list(vec![leaf::SRC], vec![OwnedNode::leaf(vec![0xff], vec![])]),
                    OwnedNode::leaf(vec![leaf::DEST], vec![1, 3, 1, 1]),
                    OwnedNode::leaf(vec![leaf::TABLE], vec![2, 0x80, 0x01]),
                    OwnedNode::leaf(vec![leaf::OPERAND1], spec),
                    OwnedNode::leaf(vec![leaf::OPERAND2], vec![2, 0x80, 0x01]),
                ],
            )])
        };
        let outcome = on_a_connection_thread(sort(3_400), Budget::DEFAULT);
        assert!(matches!(
            outcome.error,
            Some(QueryError::Limit("sort fields"))
        ));
        assert_eq!(outcome.slots, UNKNOWN_ERROR);

        let outcome = on_a_connection_thread(sort(64), Budget::DEFAULT);
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        let reply = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&outcome.slots)).unwrap();
        let sorted = reply.node(&[&[1], &[0]]).unwrap().value.unwrap();
        assert_eq!(sorted, 20u64.to_be_bytes());
        // Every record ties on every level: one group per level, three
        // lists each, and the records' RID list under the last.
        assert_eq!(reply.node(&[&[1]]).unwrap().children.len(), 1 + 3 * 64);
    }

    /// Table `80 01` with records 1 to `records` and text fields 1, 2 and
    /// 3: field 1's value index holds the given entries as given (in any
    /// order, keys possibly repeated), field 2's index has no keys and
    /// field 3 has none.
    struct Indexed {
        root: OwnedNode,
    }

    impl Indexed {
        fn new(records: u64, entries: Vec<OwnedNode>) -> Self {
            let mut root = OwnedNode::default();
            let table: &[u8] = &[0x80, 0x01];
            let all: RidList = (1..=records).collect();
            root.put(&[table, &[0x0d]], all.to_node(Vec::new()));
            root.put(
                &[table, &[1], &[1], &[0]],
                OwnedNode::list(Vec::new(), entries),
            );
            root.put(
                &[table, &[1], &[2], &[0]],
                OwnedNode::list(Vec::new(), Vec::new()),
            );
            Self { root }
        }
    }

    impl Source for Indexed {
        fn catalog(&self) -> om_hbam::Result<om_hbam::Catalog> {
            let field = |id| om_hbam::Field {
                id,
                name: format!("Field {id}"),
                kind: om_hbam::FieldKind::Normal,
                data_type: DataType::Text,
                options: Vec::new(),
            };
            Ok(om_hbam::Catalog {
                tables: vec![om_hbam::Table {
                    id: 0x8001,
                    name: "Table".into(),
                    fields: vec![field(1), field(2), field(3)],
                }],
                ..om_hbam::Catalog::default()
            })
        }

        fn node(&self, path: &[&[u8]]) -> om_hbam::Result<Option<std::borrow::Cow<'_, OwnedNode>>> {
            Ok(self.root.at(path).map(std::borrow::Cow::Borrowed))
        }
    }

    /// An index entry: `key` holding `records`.
    fn entry(key: &[u8], records: &[u64]) -> OwnedNode {
        records
            .iter()
            .copied()
            .collect::<RidList>()
            .to_node(key.to_vec())
    }

    /// Instruction 1 with `opcode`, `leaves`, table `80 01` and the result
    /// spot `[3, 1]`, and the uploads `[2, 1]` and `[2, 2]`.
    fn lookup(opcode: u8, mut leaves: Vec<OwnedNode>, uploads: [Vec<OwnedNode>; 2]) -> Vec<u8> {
        leaves.push(OwnedNode::leaf(vec![leaf::TABLE], vec![2, 0x80, 0x01]));
        leaves.push(OwnedNode::leaf(vec![leaf::DEST], vec![1, 3, 1, 1]));
        let [one, two] = uploads;
        let uploads = vec![OwnedNode::list(vec![1], one), OwnedNode::list(vec![2], two)];
        OwnedNode::list(
            Vec::new(),
            vec![
                OwnedNode::list(vec![1], vec![instruction(1, opcode, leaves)]),
                OwnedNode::list(vec![2], uploads),
            ],
        )
        .to_wire()
        .unwrap()
    }

    /// The result spot `[3, 1]` of `program`.
    fn result(
        source: &dyn Source,
        program: &[u8],
        budget: Budget,
    ) -> Result<OwnedNode, QueryError> {
        let context = Context {
            file_id: FILE_ID,
            connection_key: None,
        };
        let outcome = execute_within(source, program, &context, budget);
        if let Some(error) = outcome.error {
            return Err(error);
        }
        let reply = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&outcome.slots)).unwrap();
        Ok(reply
            .node(&[&[1]])
            .map(|n| n.to_owned_node())
            .unwrap_or_default())
    }

    fn uploaded(keys: &[&[u8]]) -> Vec<OwnedNode> {
        let mut keys: Vec<OwnedNode> = keys
            .iter()
            .map(|k| OwnedNode::leaf(k.to_vec(), Vec::new()))
            .collect();
        keys.sort_by(|a, b| a.segment.cmp(&b.segment));
        keys
    }

    fn numbered(values: Vec<Vec<u8>>) -> Vec<OwnedNode> {
        values
            .into_iter()
            .enumerate()
            .map(|(i, value)| OwnedNode::leaf(serial::encode(i as u64 + 1), value))
            .collect()
    }

    /// A value-scope finder on field `field`; an empty bound is open.
    fn finder(kind: u8, options: u8, field: u8, begin: &[u8], end: &[u8]) -> Vec<u8> {
        let mut bytes = vec![kind, 1, options, 3, 2, 0x80, 0x01, 2, 1, field, 0, 1];
        for key in [begin, end] {
            match key {
                [] => bytes.push(0),
                key => bytes.extend(one_segment_key(key)),
            }
        }
        bytes
    }

    fn find_program(finders: &[Vec<u8>]) -> Vec<u8> {
        let src = OwnedNode::leaf(vec![leaf::SRC], vec![1, 2, 1, 1]);
        lookup(
            opcode::SIMPLE_FIND,
            vec![src],
            [numbered(finders.to_vec()), Vec::new()],
        )
    }

    fn exists_program(field: u8, keys: &[&[u8]], excluded: &[u64]) -> Vec<u8> {
        let leaves = vec![
            OwnedNode::leaf(vec![leaf::SRC], vec![1, 2, 1, 1]),
            OwnedNode::leaf(vec![leaf::FIELD_KEY], vec![1, field]),
            OwnedNode::leaf(vec![leaf::EXCLUDE_ROWS], vec![1, 2, 1, 2]),
        ];
        let excluded = excluded.iter().copied().collect::<RidList>().entries();
        lookup(opcode::EXISTS_IN_INDEX, leaves, [uploaded(keys), excluded])
    }

    /// JoinProjection of `rows`, each the keys a source row gives for its
    /// field 9, by `predicates` (type, target field).
    fn join_program(rows: &[&[&[u8]]], predicates: &[(u8, u8)]) -> Vec<u8> {
        let rows = rows
            .iter()
            .enumerate()
            .map(|(r, keys)| {
                let keys = OwnedNode::list(vec![9], uploaded(keys));
                OwnedNode::list(serial::encode(r as u64 + 1), vec![keys])
            })
            .collect();
        let predicates = predicates
            .iter()
            .map(|&(kind, field)| vec![kind, 2, 1, field, 2, 1, 9])
            .collect();
        let leaves = vec![
            OwnedNode::leaf(vec![leaf::SRC], vec![1, 2, 1, 1]),
            OwnedNode::leaf(vec![leaf::DEL_LIST], vec![1, 2, 1, 2]),
        ];
        lookup(
            opcode::JOIN_PROJECTION,
            leaves,
            [rows, numbered(predicates)],
        )
    }

    fn ids(node: &OwnedNode) -> Vec<u64> {
        RidList::from_owned(node).unwrap().iter().collect()
    }

    /// Field 1's index of [`lookup_index`] read through each instruction
    /// that finds keys in it: first, last, missing, shared and prefix keys,
    /// and the empty and missing indexes of fields 2 and 3.
    fn check_lookups(source: &dyn Source) {
        let run = |program: Vec<u8>| result(source, &program, Budget::DEFAULT).unwrap();
        let found = |finders: &[Vec<u8>]| ids(&run(find_program(finders)));
        let both = INCLUDE_BEGIN | INCLUDE_END;
        // SimpleFind (6.3).
        for (finders, records) in [
            (vec![finder(0, 0, 1, &[0x01], &[])], &[6][..]),
            (vec![finder(0, 0, 1, &[0x01, 0x20], &[])], &[5]),
            (vec![finder(0, 0, 1, &[0x00], &[])], &[]),
            (vec![finder(0, 0, 1, &[0x01, 0x10, 0x25], &[])], &[]),
            (vec![finder(0, 0, 1, &[0x02], &[])], &[]),
            (vec![finder(0, 0, 1, &[0x01, 0x10, 0x20], &[])], &[2, 3]),
            (vec![finder(1, 0, 1, &[0x01, 0x10], &[])], &[2, 3, 4, 5, 6]),
            (vec![finder(1, 0, 1, &[0x03], &[])], &[1, 2, 3, 4, 5, 6]),
            (
                vec![finder(
                    2,
                    INCLUDE_BEGIN,
                    1,
                    &[0x01, 0x10],
                    &[0x01, 0x10, 0xff, 0xfe],
                )],
                &[1, 2, 3, 4],
            ),
            (
                vec![finder(
                    2,
                    INCLUDE_END,
                    1,
                    &[0x01, 0x10],
                    &[0x01, 0x10, 0x30],
                )],
                &[2, 3, 4],
            ),
            (
                vec![finder(2, both, 1, &[0x01, 0x05], &[0x01, 0x15])],
                &[1, 2, 3, 4],
            ),
            (vec![finder(2, both, 1, &[0x01, 0x20], &[0x01, 0x10])], &[]),
            (vec![finder(2, 0, 1, &[], &[])], &[1, 2, 3, 4, 5, 6]),
            (vec![finder(2, 0, 1, &[0x01, 0x20], &[])], &[]),
            (vec![finder(2, 0, 1, &[], &[0x01])], &[]),
            (vec![finder(7, 0, 1, &[], &[])], &[2, 3]),
            (
                vec![
                    finder(0, 0, 1, &[0x01, 0x20], &[]),
                    finder(0, 0, 1, &[0x01, 0x20], &[]),
                ],
                &[5],
            ),
            (
                vec![
                    finder(0, 0, 1, &[0x01, 0x10, 0x20], &[]),
                    finder(2, both, 1, &[0x01, 0x10, 0x20], &[0x01, 0x20]),
                ],
                &[2, 3],
            ),
            (vec![finder(0, 0, 2, &[0x01], &[])], &[]),
            (vec![finder(2, 0, 2, &[], &[])], &[]),
            (vec![finder(7, 0, 2, &[], &[])], &[]),
            (vec![finder(0, 0, 3, &[0x01], &[])], &[]),
        ] {
            assert_eq!(found(&finders), records, "{finders:02x?}");
        }

        // ExistsInIndex (8.4).
        let exists = |field, keys: &[&[u8]], excluded: &[u64]| {
            let node = run(exists_program(field, keys, excluded));
            node.child(&[0]).and_then(|n| n.value.clone()) == Some(vec![1, 1])
        };
        for (field, keys, excluded, answer) in [
            (1, &[&[0x01][..]][..], &[][..], true),
            (1, &[&[0x01, 0x20][..]], &[], true),
            (1, &[&[0x00][..], &[0x01, 0x10, 0x25], &[0x02]], &[], false),
            (1, &[&[0x02][..], &[0x00], &[0x01, 0x10, 0x30]], &[], true),
            (1, &[&[0x01, 0x10, 0x20][..]], &[2, 3], false),
            (1, &[&[0x01, 0x10, 0x20][..]], &[2], true),
            (
                1,
                &[&[0x01, 0x10, 0x20][..], &[0x01, 0x20]],
                &[2, 3, 5],
                false,
            ),
            (1, &[], &[], false),
            (2, &[&[0x01][..]], &[], false),
            (3, &[&[0x01][..]], &[], false),
        ] {
            assert_eq!(
                exists(field, keys, excluded),
                answer,
                "{keys:02x?} - {excluded:?}"
            );
        }

        // JoinProjection (9.2), source keys against field 1's keys.
        let joined =
            |rows: &[&[&[u8]]], predicates: &[(u8, u8)]| ids(&run(join_program(rows, predicates)));
        let shared: &[u8] = &[0x01, 0x10, 0x20];
        for (rows, predicates, records) in [
            (&[&[shared][..]][..], &[(pred::EQ, 1)][..], &[2, 3][..]),
            (
                &[&[&[0x01][..], &[0x01, 0x20]][..]],
                &[(pred::EQ, 1)],
                &[5, 6],
            ),
            (&[&[&[0x01, 0x10, 0x25][..]][..]], &[(pred::EQ, 1)], &[]),
            (
                &[&[&[0x01, 0x10][..]][..], &[&[0x01, 0x10, 0x30][..]]],
                &[(pred::EQ, 1)],
                &[1, 4],
            ),
            (&[&[shared][..]], &[(pred::LT, 1)], &[1, 6]),
            (&[&[shared][..]], &[(pred::LE, 1)], &[1, 2, 3, 6]),
            (&[&[shared][..]], &[(pred::GT, 1)], &[4, 5]),
            (&[&[shared][..]], &[(pred::GE, 1)], &[2, 3, 4, 5]),
            (
                &[&[&[0x01, 0x10][..], &[0x01, 0x10, 0x30]][..]],
                &[(pred::LT, 1)],
                &[1, 2, 3, 6],
            ),
            (
                &[&[&[0x01, 0x10][..], &[0x01, 0x10, 0x30]][..]],
                &[(pred::GT, 1)],
                &[2, 3, 4, 5],
            ),
            (&[&[&[0x00][..]][..]], &[(pred::LT, 1)], &[]),
            (&[&[&[0x00][..]][..]], &[(pred::GT, 1)], &[1, 2, 3, 4, 5, 6]),
            (&[&[&[0x02][..]][..]], &[(pred::GE, 1)], &[]),
            (
                &[&[&[0x01, 0x10][..]][..]],
                &[(pred::NE, 1)],
                &[2, 3, 4, 5, 6],
            ),
            (
                &[&[&[0x01, 0x10][..], &[0x01, 0x20]][..]],
                &[(pred::NE, 1)],
                &[1, 2, 3, 4, 5, 6],
            ),
            (
                &[&[&[0x01, 0x05][..]][..]],
                &[(pred::NE, 1)],
                &[1, 2, 3, 4, 5, 6],
            ),
            (&[&[shared][..]], &[(pred::EQ, 1), (pred::GE, 1)], &[2, 3]),
            (&[&[shared][..]], &[(pred::EQ, 1), (pred::GT, 1)], &[]),
            (
                &[&[&[0x01, 0x10][..]][..]],
                &[(pred::EQ, 1), (pred::CARTESIAN, 1)],
                &[1],
            ),
            (&[&[][..]], &[(pred::CARTESIAN, 1)], &[1, 2, 3, 4, 5, 6]),
            (&[&[shared][..]], &[(pred::EQ, 1), (pred::EQ, 2)], &[]),
            (&[&[&[0x01][..]][..]], &[(pred::EQ, 2)], &[]),
            (&[], &[(pred::EQ, 1)], &[]),
        ] {
            assert_eq!(
                joined(rows, predicates),
                records,
                "{rows:02x?} {predicates:?}"
            );
        }

        // CreateValueListEx (8.7): the values whose keys start with the
        // prefix, in key order, with their lowest records, and the count.
        let values = |field: u8, prefix: Option<&[u8]>, max: Option<u32>| {
            let mut leaves = vec![OwnedNode::leaf(vec![leaf::FIELD_KEY], vec![1, field])];
            if let Some(prefix) = prefix {
                let value = [&[prefix.len() as u8][..], prefix].concat();
                leaves.push(OwnedNode::leaf(vec![leaf::VALUE_PREFIX], value));
            }
            if let Some(max) = max {
                leaves.push(OwnedNode::leaf(
                    vec![leaf::MAX_VALUES],
                    max.to_be_bytes().to_vec(),
                ));
            }
            let node = run(lookup(
                opcode::CREATE_VALUE_LIST_EX,
                leaves,
                [vec![], vec![]],
            ));
            let bytes = node
                .child(&[0])
                .and_then(|n| n.assembled())
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_default();
            let mut rest = bytes.as_slice();
            let mut listed = Vec::new();
            while !rest.is_empty() {
                let (record, after) = take_key_string(rest).unwrap();
                let (key, after) = take_key_string(after).unwrap();
                let len = u32::from_be_bytes(after[..4].try_into().unwrap()) as usize;
                rest = &after[4 + len..];
                listed.push((serial::decode(&record[0]).unwrap(), key.concat()));
            }
            let count = node.child(&[0xff]).and_then(|n| n.value.clone()).unwrap();
            let count = u64::from_be_bytes(count.try_into().unwrap());
            // OpenMaker's client reads the reply as written: the values, and
            // whether the list is whole (a negative count is a cut list).
            let reply = OwnedNode::list(
                Vec::new(),
                vec![OwnedNode::list(vec![2], node.children.clone())],
            );
            let read = crate::find::value_list_values(&reply.to_wire().unwrap()).unwrap();
            assert_eq!(read.values.len(), listed.len());
            assert_eq!(read.complete, count == listed.len() as u64);
            (listed, count)
        };
        let value = |record: u64, key: &[u8]| (record, key.to_vec());
        let prefixed = vec![
            value(1, &[0x01, 0x10]),
            value(2, shared),
            value(4, &[0x01, 0x10, 0x30]),
        ];
        for (field, prefix, max, listed, count) in [
            (1, Some(&[0x01, 0x10][..]), None, prefixed.clone(), 3),
            (
                1,
                None,
                None,
                [prefixed.clone(), vec![value(5, &[0x01, 0x20])]].concat(),
                4,
            ),
            (
                1,
                Some(&[0x01][..]),
                Some(100),
                [prefixed.clone(), vec![value(5, &[0x01, 0x20])]].concat(),
                4,
            ),
            (1, Some(shared), None, vec![value(2, shared)], 1),
            // Without a prefix the most values does not apply
            // (live1:vl_noprefix_max1).
            (
                1,
                None,
                Some(1),
                [prefixed.clone(), vec![value(5, &[0x01, 0x20])]].concat(),
                4,
            ),
            // Stopped at the most while more values have the prefix: minus
            // the count (live1:vl_all_max2); reaching it with no more: the
            // count.
            (
                1,
                Some(&[0x01][..]),
                Some(2),
                prefixed[..2].to_vec(),
                2u64.wrapping_neg(),
            ),
            (1, Some(&[0x01, 0x10][..]), Some(3), prefixed.clone(), 3),
            (
                1,
                Some(&[0x01, 0x10][..]),
                Some(1),
                vec![value(1, &[0x01, 0x10])],
                u64::MAX,
            ),
            (1, Some(&[0x01, 0x30][..]), None, vec![], 0),
            (1, Some(&[0x00][..]), None, vec![], 0),
            (2, None, None, vec![], 0),
        ] {
            assert_eq!(
                values(field, prefix, max),
                (listed, count),
                "{prefix:02x?} {max:?}"
            );
        }
    }

    /// The index [`check_lookups`] reads, in key order: `01` the empty
    /// value, then keys of which some are prefixes of others, and a key
    /// two records share.
    fn lookup_index() -> Vec<OwnedNode> {
        vec![
            entry(&[0x01], &[6]),
            entry(&[0x01, 0x10], &[1]),
            entry(&[0x01, 0x10, 0x20], &[2, 3]),
            entry(&[0x01, 0x10, 0x30], &[4]),
            entry(&[0x01, 0x20], &[5]),
        ]
    }

    /// #306: instructions find keys in an index by binary search; each
    /// answers at the edges of the index as the scans it replaced did.
    /// LookupValue against live1's tables (spec 8.6): PK (field 1 here)
    /// keyed 10 < 3 < 1 < 2 < 12 < 13, all below "A"; Company (field 2)
    /// empty but in record 1, whose key is above "A".
    #[test]
    fn lookups_find_the_next_lower_or_higher_record_as_filemaker_server_does() {
        let pk = |n: u8| vec![0x01, 0x12, 0x05 + n];
        let order = [10u64, 3, 1, 2, 12, 13];
        let entries = order
            .iter()
            .enumerate()
            .map(|(i, &id)| entry(&pk(i as u8), &[id]))
            .collect();
        let mut source = Indexed::new(13, entries);
        let company_one = om_collate::text_key("Test Co");
        source.root.put(
            &[&[0x80, 0x01], &[1], &[2], &[0]],
            OwnedNode::list(
                Vec::new(),
                vec![
                    entry(&[0x01], &[2, 3, 10, 12, 13]),
                    entry(&company_one, &[1]),
                ],
            ),
        );
        let a = om_collate::text_key("A");
        let record_one = pk(2);
        // Source rows: field 9 the PK key, field 5 the Company key.
        let lookup_of = |pk_key: Option<&[u8]>, company: Option<&[u8]>, higher: bool| {
            let mut fields = Vec::new();
            let mut predicates = vec![vec![pred::EQ, 2, 1, 1, 2, 1, 9]];
            for (field, key) in [(9u8, pk_key), (5, company)] {
                if let Some(key) = key {
                    fields.push(OwnedNode::list(vec![field], uploaded(&[key])));
                }
            }
            if company.is_some() {
                predicates.push(vec![pred::EQ, 2, 1, 2, 2, 1, 5]);
            }
            if pk_key.is_none() && company.is_some() {
                predicates.remove(0);
            }
            fields.sort_by(|x, y| x.segment.cmp(&y.segment));
            let rows = vec![OwnedNode::list(serial::encode(1), fields)];
            let mut leaves = vec![
                OwnedNode::leaf(vec![leaf::SRC], vec![1, 2, 1, 1]),
                OwnedNode::leaf(vec![leaf::DEL_LIST], vec![1, 2, 1, 2]),
            ];
            if higher {
                leaves.push(OwnedNode::leaf(vec![leaf::LOOKUP_HIGHER], vec![0x01, 0x01]));
            }
            let program = lookup(opcode::LOOKUP, leaves, [rows, numbered(predicates)]);
            result(&source, &program, Budget::DEFAULT).map(|dest| {
                dest.child(&[0x00])
                    .and_then(|leaf| leaf.value.clone())
                    .unwrap_or_default()
            })
        };
        let record = |id: u8| Some(vec![0x01, id]);
        // lk_pk_lower_A, lk_pk_higher_A, lk_pk_higher_low, lk_pk_lower_low,
        // lk_pk_exact_higher.
        assert_eq!(lookup_of(Some(&a), None, false).ok(), record(13));
        assert!(matches!(
            lookup_of(Some(&a), None, true),
            Err(QueryError::NoRecords)
        ));
        assert_eq!(lookup_of(Some(&[0x01, 0x11]), None, true).ok(), record(10));
        assert!(matches!(
            lookup_of(Some(&[0x01, 0x11]), None, false),
            Err(QueryError::NoRecords)
        ));
        assert_eq!(lookup_of(Some(&record_one), None, true).ok(), record(0));
        // lk_company_lower_A: the empty key's lowest record.
        assert_eq!(lookup_of(None, Some(&a), false).ok(), record(2));
        // lk_two_lower, lk_two_higher: PK = record 1's key and Company = "A".
        assert!(matches!(
            lookup_of(Some(&record_one), Some(&a), false),
            Err(QueryError::NoRecords)
        ));
        assert_eq!(lookup_of(Some(&record_one), Some(&a), true).ok(), record(1));
        // lk_nokey: a source row without a key.
        let nokey = {
            let rows = vec![OwnedNode::list(serial::encode(1), Vec::new())];
            let leaves = vec![
                OwnedNode::leaf(vec![leaf::SRC], vec![1, 2, 1, 1]),
                OwnedNode::leaf(vec![leaf::DEL_LIST], vec![1, 2, 1, 2]),
                OwnedNode::leaf(vec![leaf::LOOKUP_HIGHER], vec![0x01, 0x01]),
            ];
            let predicates = numbered(vec![vec![pred::EQ, 2, 1, 1, 2, 1, 9]]);
            result(
                &source,
                &lookup(opcode::LOOKUP, leaves, [rows, predicates]),
                Budget::DEFAULT,
            )
        };
        assert!(matches!(nokey, Err(QueryError::NoRecords)));
        // The walk starts at the first predicate that leaves no record; a
        // later one is not read, even malformed.
        let later_malformed = {
            let fields = vec![OwnedNode::list(vec![9], uploaded(&[&a]))];
            let rows = vec![OwnedNode::list(serial::encode(1), fields)];
            let leaves = vec![
                OwnedNode::leaf(vec![leaf::SRC], vec![1, 2, 1, 1]),
                OwnedNode::leaf(vec![leaf::DEL_LIST], vec![1, 2, 1, 2]),
            ];
            let predicates = numbered(vec![vec![pred::EQ, 2, 1, 1, 2, 1, 9], vec![pred::EQ, 9]]);
            result(
                &source,
                &lookup(opcode::LOOKUP, leaves, [rows, predicates]),
                Budget::DEFAULT,
            )
            .map(|dest| dest.child(&[0x00]).and_then(|l| l.value.clone()))
            .ok()
            .flatten()
        };
        assert_eq!(later_malformed, record(13));
        assert_eq!(QueryError::NoRecords.code(), 401);
    }

    /// SimpleFind with Match finders (spec 6.3): each record's stored text
    /// against the pattern, the whole value or each word (live1's
    /// m_text_test, m_text_test_w), after the other finders' records.
    #[test]
    fn match_finders_test_stored_values() {
        let values = ["Test", "Hello", "Ima", "A long text, Test", "One Test here"];
        let mut source = Indexed::new(5, Vec::new());
        for (i, value) in values.iter().enumerate() {
            source.root.put(
                &[&[0x80, 0x01], &[5], &serial::encode(i as u64 + 1), &[1]],
                OwnedNode::leaf(Vec::new(), om_scsu::encode_scsu(value)),
            );
        }
        let test_operand = [0x00, 0x04, 0x0e, 0x3f, 0x29, 0x2e, 0x00];
        let matcher = |scope: u8| {
            let mut bytes = vec![8, scope, 0, 3, 2, 0x80, 0x01, 2, 1, 1, 0, 1];
            bytes.extend([0, 1, 0]);
            bytes.extend(test_operand);
            bytes
        };
        let find = |finders: &[Vec<u8>]| {
            ids(&result(&source, &find_program(finders), Budget::DEFAULT).unwrap())
        };
        assert_eq!(find(&[matcher(1)]), [1]);
        assert_eq!(find(&[matcher(0)]), [1, 4, 5]);
        // After a row-id range of records 2 to 5, only 4 and 5 are tested.
        let rows = finder(
            6,
            INCLUDE_BEGIN | INCLUDE_END,
            1,
            &serial::encode(2),
            &serial::encode(5),
        );
        assert_eq!(find(&[rows, matcher(0)]), [4, 5]);
        // An optional IndexScan before a Match finder is left out (`*x*`,
        // 6.4); a required one is still refused.
        let scan = |options: u8| {
            let mut bytes = vec![3, 0, options, 3, 2, 0x80, 0x01, 2, 1, 1, 0, 1];
            bytes.extend([5, 4, 0x14, 0x33, 0x12, 0x6b, 2, 1, 0xff]);
            bytes
        };
        assert_eq!(find(&[scan(0x88), matcher(0)]), [1, 4, 5]);
        assert!(result(&source, &find_program(&[scan(0x08)]), Budget::DEFAULT).is_err());
        // DateTimeMatch (9) is not built: refused, as before.
        let mut date_time = matcher(1);
        date_time[0] = 9;
        assert!(result(&source, &find_program(&[date_time]), Budget::DEFAULT).is_err());
    }

    #[test]
    fn index_lookups_answer_at_the_edges() {
        check_lookups(&Indexed::new(6, lookup_index()));
    }

    /// #306: an index whose keys are out of order or repeated (no file or
    /// OpenMaker Server index is; a source is trusted for nothing) is read
    /// as if sorted, the records of a repeated key merged, so every lookup
    /// answers as on the sorted index.
    #[test]
    fn an_unsorted_index_reads_as_sorted() {
        let unsorted = vec![
            entry(&[0x01, 0x20], &[5]),
            entry(&[0x01, 0x10, 0x20], &[3]),
            entry(&[0x01], &[6]),
            entry(&[0x01, 0x10, 0x30], &[4]),
            entry(&[0x01, 0x10, 0x20], &[2]),
            entry(&[0x01, 0x10], &[1]),
        ];
        check_lookups(&Indexed::new(6, unsorted));
    }

    /// #306: a JoinProjection compared every key of the index with each
    /// source row, a SimpleFind scanned it for every finder, ExistsInIndex
    /// searched the client's keys for every index key, and a range find
    /// merged the growing result with each key's records: rows, finders or
    /// keys times the index (here 2,000 x 20,000 = 40,000,000 comparisons,
    /// and for the range 20,000 merges of up to 313 blocks). Each now reads
    /// the index's keys once to check their order, a binary search per
    /// key it looks up and the entries it answers with, within a budget of
    /// the index plus 32 per row, finder or key. Reading the index counts:
    /// one key less and the work limit stops the program.
    #[test]
    fn lookups_cost_the_index_once_and_the_program() {
        const KEYS: u64 = 20_000;
        const PROGRAM: u64 = 2_000;
        // Record i holds key `01` + u32 2i; the odd keys are missing.
        let key = |n: u64| [&[0x01][..], &(n as u32).to_be_bytes()].concat();
        let index = (1..=KEYS).map(|i| entry(&key(2 * i), &[i])).collect();
        let source = Indexed::new(KEYS, index);
        let missing: Vec<Vec<u8>> = (0..PROGRAM).map(|i| key(2 * i * 7 + 1)).collect();
        let missing: Vec<&[u8]> = missing.iter().map(Vec::as_slice).collect();

        let rows: Vec<&[&[u8]]> = missing.chunks(1).collect();
        let finders: Vec<Vec<u8>> = missing.iter().map(|k| finder(0, 0, 1, k, &[])).collect();
        let shapes = [
            ("join", join_program(&rows, &[(pred::EQ, 1)])),
            ("finders", find_program(&finders)),
            ("exists", exists_program(1, &missing, &[])),
            ("range", find_program(&[finder(2, 0, 1, &[], &[])])),
        ];
        let within = |nodes: u64| Budget {
            work: Size {
                nodes: nodes as usize,
                bytes: 1 << 30,
            },
            ..Budget::DEFAULT
        };
        for (shape, program) in &shapes {
            let answer = result(&source, program, within(KEYS + 32 * PROGRAM));
            let answer = answer.unwrap_or_else(|e| panic!("{shape}: {e}"));
            match *shape {
                "exists" => assert_eq!(answer.child(&[0]).unwrap().value, Some(vec![1, 0])),
                "range" => assert_eq!(ids(&answer), (1..=KEYS).collect::<Vec<_>>()),
                _ => assert_eq!(ids(&answer), [], "{shape}"),
            }
            let refused = result(&source, program, within(KEYS - 1));
            assert!(
                matches!(refused, Err(QueryError::Limit("work"))),
                "{shape}: {refused:?}"
            );
        }
    }

    /// #465: a value list has no size cut. Past 1,000 bytes its entries
    /// are a node of 1,000-byte pieces under their u32 total, as FileMaker
    /// Server writes them (spec 8.7, #464), and `ff` counts every value; a
    /// reply that still cannot be encoded fails with the error slot, never
    /// the empty success `80 40`.
    #[test]
    fn a_long_value_list_comes_whole_in_pieces_and_an_unencodable_reply_fails() {
        const VALUES: u64 = 6_000;
        let key = |n: u64| [&[0x01][..], &(n as u32).to_be_bytes()].concat();
        let index = (1..=VALUES).map(|i| entry(&key(i), &[i])).collect();
        let mut source = Indexed::new(VALUES, index);
        let leaves = vec![OwnedNode::leaf(vec![leaf::FIELD_KEY], vec![1, 1])];
        let list = lookup(opcode::CREATE_VALUE_LIST_EX, leaves, [vec![], vec![]]);
        let node = result(&source, &list, Budget::DEFAULT).unwrap();
        let pieces = node.child(&[0]).unwrap();
        assert_eq!(pieces.value, None);
        assert!(pieces.children[1..].iter().rev().skip(1).all(|piece| piece
            .value
            .as_ref()
            .map(Vec::len)
            == Some(1_000)));
        let entries = pieces.assembled().unwrap();
        let count = node.child(&[0xff]).unwrap().value.clone().unwrap();
        assert_eq!(count, VALUES.to_be_bytes());
        let mut rest = &entries[..];
        let mut keys = Vec::new();
        while !rest.is_empty() {
            let (_, after) = take_key_string(rest).unwrap();
            let (key, after) = take_key_string(after).unwrap();
            let len = u32::from_be_bytes(after[..4].try_into().unwrap()) as usize;
            rest = &after[4 + len..];
            keys.push(key.concat());
        }
        assert_eq!(keys, (1..=VALUES).map(key).collect::<Vec<_>>());

        // GetKey of a stored value no slot value can hold.
        source.root.put(
            &[&[0x80, 0x01], &[9]],
            OwnedNode::leaf(Vec::new(), vec![0; 70_000]),
        );
        let src = OwnedNode::leaf(vec![leaf::SRC], vec![2, 0x80, 0x01, 1, 9]);
        let get = lookup(opcode::GET_KEY, vec![src], [vec![], vec![]]);
        let context = Context {
            file_id: FILE_ID,
            connection_key: None,
        };
        let outcome = execute_within(&source, &get, &context, Budget::DEFAULT);
        assert!(
            matches!(outcome.error, Some(QueryError::Limit(_))),
            "{:?}",
            outcome.error
        );
        assert_eq!(outcome.slots, UNKNOWN_ERROR);
    }

    #[test]
    fn an_unparseable_program_is_answered_with_unknown_error() {
        let file_bytes = std::fs::read("../../re/work/login/testproject.fmp12");
        let Ok(bytes) = file_bytes else {
            return; // the analysis-host copy is git-ignored
        };
        let file = HbamFile::from_bytes(bytes).unwrap();
        let outcome = execute(
            &file,
            &[0x80, 0x20],
            &Context {
                file_id: 1,
                connection_key: None,
            },
        );
        assert!(matches!(outcome.error, Some(QueryError::Malformed(_))));
        assert_eq!(
            outcome.slots,
            [0x80, 0x06, 0x00, 0x03, 0x02, 0xff, 0xff, 0x40]
        );
    }
}
