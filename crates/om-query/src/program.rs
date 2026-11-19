//! Parsing a query program (spec `docs/re/query-program.md` 2).
//!
//! A program is one slot tree: `[1]` holds the instructions in execution
//! order, `[2]` the uploaded inputs and scratch spots, `[3]` the result spots
//! the client gets back. Instructions read and write spots by path.

use std::cell::Cell;

use om_hbam::{serial, OwnedNode, SlotReader};

use crate::QueryError;

/// What one program may hold and do. FileMaker Server's own limits are
/// unmeasured; these are OpenMaker's, set far above what clients send so
/// that no program can exhaust the host's stack, memory or time. A program
/// that goes past one fails as a malformed program does (error `ffff`,
/// query-program.md 11.2), with the spots written before it.
pub mod limits {
    use super::Size;

    /// The deepest key path in a program's tree, and the longest spot path:
    /// the slot reader's limit (`om_hbam::slot::MAX_DEPTH`, the 17 levels
    /// of file-format.md 3.2). No program a client uploads is deeper, and
    /// whatever an instruction writes stays as deep at most, so the reply
    /// reads back and the tree's recursive encoding, copying and dropping
    /// stay shallow. A key path into the file is no longer either.
    pub const MAX_DEPTH: usize = om_hbam::slot::MAX_DEPTH;

    /// The deepest path an instruction may write into the file: the file
    /// writer's limit (hbam-writer.md 2.3), so the file can still be saved.
    pub const MAX_FILE_DEPTH: usize = om_hbam::writer::MAX_DEPTH;

    /// Instructions in one program. FileMaker Pro's captured programs hold
    /// at most three (query-program.md 0), OpenMaker's own client sends one
    /// per index check plus the find; this leaves room for finds of
    /// hundreds of requests.
    pub const MAX_INSTRUCTIONS: usize = 1024;

    /// The program's tree at any time: what it uploaded and what its
    /// instructions wrote, with the file changes it holds for the caller.
    /// About 600 MiB of memory at most. The largest results real clients
    /// ask for fit: a sort of a million records with distinct keys takes
    /// about four million nodes (four per record, 10.4); FileMaker Pro's
    /// captured programs and replies are under 3 KB (11.1).
    pub const MAX_TREE: Size = Size {
        nodes: 1 << 22,
        bytes: 1 << 28,
    };

    /// What the instructions read from the tree and write into it, and
    /// read from the file's indexes and record lists, in total, which
    /// bounds the time they take: four times the tree, so a program can
    /// read and rewrite its largest results a few times but cannot copy
    /// them over and over. Reading an index counts each key once when the
    /// program first reads it (its order is checked), the keys a binary
    /// search compares for each key looked up, and the key and RID list of
    /// each entry read; using a table's record list counts a node per 64
    /// records (query-program.md 7.1, 11.4).
    pub const MAX_WORK: Size = Size {
        nodes: 1 << 24,
        bytes: 1 << 30,
    };

    /// Sort fields in one SortList. The sort spec's count is a u16 and
    /// FileMaker's own limit is unmeasured; sorts name a handful of fields,
    /// and each field costs a key per record.
    pub const MAX_SORT_FIELDS: usize = 64;
}

/// The limits one program runs within: [`Budget::DEFAULT`], from
/// [`limits`], for every program a client sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Budget {
    pub tree: Size,
    pub work: Size,
    pub instructions: usize,
    pub sort_fields: usize,
}

impl Budget {
    pub const DEFAULT: Self = Self {
        tree: limits::MAX_TREE,
        work: limits::MAX_WORK,
        instructions: limits::MAX_INSTRUCTIONS,
        sort_fields: limits::MAX_SORT_FIELDS,
    };
}

/// What a program's instructions have read and written so far, kept within
/// its budget's work limit ([`limits::MAX_WORK`]).
#[derive(Clone, Debug)]
pub(crate) struct Work {
    done: Cell<Size>,
    limit: Size,
}

impl Work {
    fn new(limit: Size) -> Self {
        Self {
            done: Cell::default(),
            limit,
        }
    }

    /// Count `size`, or refuse it when it would pass the limit.
    pub(crate) fn spend(&self, size: Size) -> Result<(), QueryError> {
        let done = self.done.get().plus(size);
        if !done.within(self.limit) {
            return Err(QueryError::Limit("work"));
        }
        self.done.set(done);
        Ok(())
    }
}

/// An amount of key tree: nodes, and the bytes of their keys and values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    pub nodes: usize,
    pub bytes: usize,
}

impl Size {
    /// One node, without its children.
    pub(crate) fn of(node: &OwnedNode) -> Self {
        Self {
            nodes: 1,
            bytes: node.segment.len() + node.value.as_ref().map_or(0, Vec::len),
        }
    }

    pub(crate) fn plus(self, other: Self) -> Self {
        Self {
            nodes: self.nodes.saturating_add(other.nodes),
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }

    fn minus(self, other: Self) -> Self {
        Self {
            nodes: self.nodes.saturating_sub(other.nodes),
            bytes: self.bytes.saturating_sub(other.bytes),
        }
    }

    pub(crate) fn within(self, limit: Self) -> bool {
        self.nodes <= limit.nodes && self.bytes <= limit.bytes
    }
}

/// A subtree's size and its height (1 for a leaf), walked without
/// recursion, so any tree can be measured before it is kept.
pub(crate) fn measure(node: &OwnedNode) -> (Size, usize) {
    let mut size = Size::of(node);
    let mut height = 1;
    let mut stack = vec![node.children.iter()];
    while let Some(children) = stack.last_mut() {
        match children.next() {
            Some(child) => {
                size = size.plus(Size::of(child));
                stack.push(child.children.iter());
                height = height.max(stack.len());
            }
            None => {
                stack.pop();
            }
        }
    }
    (size, height)
}

/// Top-level keys of a program.
pub mod keys {
    pub const INSTRUCTIONS: u8 = 1;
    pub const UPLOAD: u8 = 2;
    pub const DOWNLOAD: u8 = 3;
}

/// Leaves of an instruction (`[1, i, leaf]`), spec 2.2.
pub mod leaf {
    pub const OPCODE: u8 = 0x00;
    pub const SRC: u8 = 0x01;
    pub const DEST: u8 = 0x02;
    pub const DEL_LIST: u8 = 0x03;
    pub const FILE_ID: u8 = 0x04;
    pub const TABLE: u8 = 0x05;
    pub const PROJ_LIST: u8 = 0x06;
    pub const ACCESS_KEY: u8 = 0x07;
    pub const FIELD_KEY: u8 = 0x09;
    pub const OPERAND1: u8 = 0x0a;
    pub const OPERAND2: u8 = 0x0b;
    pub const MASTER_LIST: u8 = 0x0d;
    pub const RESULT1: u8 = 0x0d;
    pub const RESULT2: u8 = 0x0e;
    pub const EXCLUDE_ROWS: u8 = 0x10;
    /// CreateValueListEx: the index-key prefix values must start with, as
    /// `[u8 len][key]`.
    pub const VALUE_PREFIX: u8 = 0x13;
    /// CreateValueListEx: the most values to return (u32).
    pub const MAX_VALUES: u8 = 0x14;
}

/// Opcodes (spec 2.3).
pub mod opcode {
    pub const VERSION: u8 = 1;
    pub const DELETE_NODE: u8 = 3;
    pub const COPY_NODE: u8 = 4;
    pub const AND: u8 = 5;
    pub const OR: u8 = 6;
    pub const AND_NOT: u8 = 7;
    pub const INVERT: u8 = 8;
    pub const SIMPLE_FIND: u8 = 10;
    pub const PROJECTION: u8 = 11;
    pub const JOIN_PROJECTION: u8 = 12;
    pub const SORT_LIST: u8 = 14;
    pub const LOOKUP: u8 = 17;
    pub const CREATE_VALUE_LIST_EX: u8 = 19;
    pub const EXISTS_IN_INDEX: u8 = 18;
    pub const UPLOAD_MULTIPLE: u8 = 40;
    pub const NODE_EXISTS: u8 = 41;
    pub const GET_KEY: u8 = 42;
    pub const GENERATE_MEMBER_KEY: u8 = 43;
    pub const DELETE_TABLE: u8 = 50;
}

/// A path to a spot, e.g. `[2, 1]` (upload spot 1) or `[3, 2]`.
pub type SpotPath = Vec<Vec<u8>>;

/// One instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction {
    /// The decoded instruction number `i` of `[1, i]`.
    pub number: u32,
    pub opcode: u8,
    /// The instruction's leaves by key, raw.
    leaves: Vec<(u8, Vec<u8>)>,
}

impl Instruction {
    /// A leaf's raw value.
    pub fn leaf(&self, key: u8) -> Option<&[u8]> {
        self.leaves
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_slice())
    }

    /// A spot operand (spec 2.5): segments as `[u8 len][bytes]`, at most
    /// [`limits::MAX_DEPTH`] of them.
    pub fn spot(&self, key: u8) -> Result<Option<SpotPath>, QueryError> {
        self.leaf(key).map(decode_path).transpose()
    }

    /// A required spot operand.
    pub fn required_spot(&self, key: u8) -> Result<SpotPath, QueryError> {
        self.spot(key)?
            .ok_or(QueryError::Malformed("instruction lacks a spot operand"))
    }

    /// The instruction's file id (`[1, i, 4]`).
    pub fn file_id(&self) -> Result<u32, QueryError> {
        let value = self
            .leaf(leaf::FILE_ID)
            .ok_or(QueryError::Malformed("instruction lacks a file id"))?;
        Ok(u32::from_be_bytes(value.try_into().map_err(|_| {
            QueryError::Malformed("file id is not four bytes")
        })?))
    }

    /// The target table's key segment (`[1, i, 5]` = `[u8 n][n bytes]`).
    pub fn table(&self) -> Result<Vec<u8>, QueryError> {
        let value = self
            .leaf(leaf::TABLE)
            .ok_or(QueryError::Malformed("instruction lacks a table"))?;
        let (&n, rest) = value
            .split_first()
            .ok_or(QueryError::Malformed("empty table key"))?;
        if rest.len() != usize::from(n) {
            return Err(QueryError::Malformed("table key length"));
        }
        Ok(rest.to_vec())
    }
}

/// A parsed program: its instructions and the spot tree they work on.
///
/// Every write into the tree goes through [`Program::set_spot`],
/// [`Program::copy_spot`] or [`Program::remove_spot`], which keep it within
/// [`limits::MAX_DEPTH`] and [`limits::MAX_TREE`]; every read of a spot by
/// [`Program::spot`] and every write counts towards [`limits::MAX_WORK`],
/// as do the instructions' reads of the file's indexes ([`Program::work`]).
/// (Tests may run a program within a smaller budget.)
#[derive(Clone, Debug)]
pub struct Program {
    pub instructions: Vec<Instruction>,
    /// The whole program tree; instructions write their results into it.
    tree: OwnedNode,
    /// The tree's size now, with the file changes held for the caller.
    held: Size,
    /// What the instructions have read and written so far.
    work: Work,
    budget: Budget,
}

impl Program {
    /// Parse the uploaded program bytes (a bracketed slot list). A program
    /// larger than [`limits::MAX_TREE`] or with more than
    /// [`limits::MAX_INSTRUCTIONS`] is refused before it is built.
    pub fn parse(bytes: &[u8]) -> Result<Self, QueryError> {
        Self::parse_within(bytes, Budget::DEFAULT)
    }

    /// [`Program::parse`], the program's tree read within `memory` bytes of
    /// memory as `om_hbam::tree::read_owned` counts it, beside the
    /// program's own limits. Past it the program is refused with
    /// `QueryError::Limit("program memory")`. A host passes the memory it
    /// reserved for the program before reading it.
    pub fn parse_with_memory_limit(bytes: &[u8], memory: usize) -> Result<Self, QueryError> {
        Self::read(bytes, Budget::DEFAULT, memory)
    }

    /// [`Program::parse`] for a program to run within `budget`.
    pub(crate) fn parse_within(bytes: &[u8], budget: Budget) -> Result<Self, QueryError> {
        Self::read(bytes, budget, usize::MAX)
    }

    /// Read a program within `budget` and `memory` bytes of tree memory.
    fn read(bytes: &[u8], budget: Budget, memory: usize) -> Result<Self, QueryError> {
        let mut held = Size::default();
        let mut too_large = false;
        let slots = SlotReader::new(bytes).map(|slot| {
            let slot = slot?;
            held = held.plus(Size {
                nodes: 1,
                bytes: slot.path.last().map_or(0, |s| s.0.len())
                    + slot.value.map_or(0, <[u8]>::len),
            });
            if !held.within(budget.tree) {
                too_large = true;
                return Err(om_hbam::Error::Write("program over its size limit"));
            }
            Ok(slot)
        });
        // Read in one pass, without a borrowed tree beside the owned one:
        // the counting above keeps the tree within the budget.
        let tree = om_hbam::tree::read_owned(slots, memory);
        if too_large {
            return Err(QueryError::Limit("program size"));
        }
        let tree = tree.map_err(|e| match e {
            om_hbam::Error::TooLarge { .. } => QueryError::Limit("program memory"),
            _ => QueryError::Malformed("program is not a slot tree"),
        })?;
        let mut instructions = Vec::new();
        if let Some(list) = tree.child(&[keys::INSTRUCTIONS]) {
            if list.children.len() > budget.instructions {
                return Err(QueryError::Limit("instructions"));
            }
            for node in &list.children {
                let number = serial::decode(&node.segment)
                    .and_then(|n| u32::try_from(n).ok())
                    .ok_or(QueryError::Malformed("instruction number"))?;
                let leaves: Vec<(u8, Vec<u8>)> = node
                    .children
                    .iter()
                    .filter(|c| c.segment.len() == 1)
                    .map(|c| (c.segment[0], c.value.clone().unwrap_or_default()))
                    .collect();
                let opcode = leaves
                    .iter()
                    .find(|(k, _)| *k == leaf::OPCODE)
                    .and_then(|(_, v)| v.first().copied())
                    .ok_or(QueryError::Malformed("instruction lacks an opcode"))?;
                instructions.push(Instruction {
                    number,
                    opcode,
                    leaves,
                });
            }
        }
        Ok(Self {
            instructions,
            tree,
            held,
            work: Work::new(budget.work),
            budget,
        })
    }

    /// The limits the program runs within.
    pub(crate) fn budget(&self) -> Budget {
        self.budget
    }

    /// The program's work so far, which instructions also count their reads
    /// of the file towards.
    pub(crate) fn work(&self) -> &Work {
        &self.work
    }

    /// The spot at `path`, if present. An instruction reads it whole, so it
    /// counts towards the program's work.
    pub fn spot(&self, path: &[Vec<u8>]) -> Result<Option<&OwnedNode>, QueryError> {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        let node = self.tree.at(&segments);
        if let Some(node) = node {
            self.spend(measure(node).0)?;
        }
        Ok(node)
    }

    /// Write `node`'s children as the spot at `path`, creating its parents.
    pub fn set_spot(
        &mut self,
        path: &[Vec<u8>],
        children: Vec<OwnedNode>,
    ) -> Result<(), QueryError> {
        let Some(last) = path.last() else {
            return Ok(());
        };
        let node = OwnedNode::list(last.clone(), children);
        let (size, height) = measure(&node);
        self.admit(path, size, height)?;
        self.insert(path, node);
        Ok(())
    }

    /// Replace the spot at `dest` with a copy of the spot at `src` (renamed
    /// to `dest`'s last segment), creating its parents, or remove it when
    /// `src` is missing (CopyNode, spec 8.3). The copy is measured against
    /// the limits before it is made.
    pub fn copy_spot(&mut self, src: &[Vec<u8>], dest: &[Vec<u8>]) -> Result<(), QueryError> {
        let Some(last) = dest.last() else {
            return Ok(());
        };
        let Some(source) = self.spot(src)? else {
            self.remove_spot(dest);
            return Ok(());
        };
        let (mut size, height) = measure(source);
        size.bytes = size.bytes - source.segment.len() + last.len();
        self.admit(dest, size, height)?;
        let segments: Vec<&[u8]> = src.iter().map(Vec::as_slice).collect();
        let mut node = self.tree.at(&segments).expect("measured above").clone();
        node.segment = last.clone();
        self.insert(dest, node);
        Ok(())
    }

    /// Remove the spot at `path`, if present.
    pub fn remove_spot(&mut self, path: &[Vec<u8>]) {
        let segments: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        if let Some(removed) = self.tree.remove(&segments) {
            self.held = self.held.minus(measure(&removed).0);
        }
    }

    /// Copies of the children of the spot at `src`, each keyed under `dest`:
    /// the changes to the file UploadMultiple asks for. Each must fit the
    /// file writer's depth and everything else the writer can hold (key
    /// segments of 1 to 219 bytes, a page's room; hbam-writer.md 2), so the
    /// host never keeps a change it cannot save; the copies, held for the
    /// caller, count as part of the program, and checking them as work.
    pub(crate) fn copy_out(
        &mut self,
        src: &[Vec<u8>],
        dest: &[Vec<u8>],
    ) -> Result<Vec<(SpotPath, OwnedNode)>, QueryError> {
        let Some(spot) = self.spot(src)? else {
            return Ok(Vec::new());
        };
        let parents: Vec<&[u8]> = dest.iter().map(Vec::as_slice).collect();
        let mut size = Size::default();
        for member in &spot.children {
            let (member_size, height) = measure(member);
            if dest.len() + height > limits::MAX_FILE_DEPTH {
                return Err(QueryError::Malformed("a write deeper than a file holds"));
            }
            if !om_hbam::writer::unwritable(&parents, member, 1).is_empty() {
                return Err(QueryError::Malformed("a write the file cannot hold"));
            }
            size = size.plus(member_size);
        }
        // The writer's check read every member once, as the copy will.
        self.spend(size)?;
        self.hold(size)?;
        let segments: Vec<&[u8]> = src.iter().map(Vec::as_slice).collect();
        let spot = self.tree.at(&segments).expect("read above");
        Ok(spot
            .children
            .iter()
            .map(|member| {
                let path = dest
                    .iter()
                    .cloned()
                    .chain([member.segment.clone()])
                    .collect();
                (path, member.clone())
            })
            .collect())
    }

    /// Count `size`, which the program's outcome holds besides the tree
    /// (its changes to the file), as part of the program and as written.
    pub(crate) fn hold(&mut self, size: Size) -> Result<(), QueryError> {
        let held = self.held.plus(size);
        if !held.within(self.budget.tree) {
            return Err(QueryError::Limit("program size"));
        }
        self.spend(size)?;
        self.held = held;
        Ok(())
    }

    /// Count `size` towards the program's work.
    pub(crate) fn spend(&self, size: Size) -> Result<(), QueryError> {
        self.work.spend(size)
    }

    /// Check that a subtree of `size` and `height` may go at the non-empty
    /// `path`, replacing what is there, and count it as written.
    fn admit(&mut self, path: &[Vec<u8>], size: Size, height: usize) -> Result<(), QueryError> {
        if path.len() - 1 + height > limits::MAX_DEPTH {
            return Err(QueryError::Malformed("a spot deeper than a program holds"));
        }
        // The parents the write creates, and the subtree it replaces.
        let mut created = Size::default();
        let mut replaced = Size::default();
        let mut node = Some(&self.tree);
        for (i, segment) in path.iter().enumerate() {
            node = node.and_then(|n| n.child(segment));
            match node {
                Some(existing) if i + 1 == path.len() => replaced = measure(existing).0,
                Some(_) => {}
                None if i + 1 < path.len() => {
                    created = created.plus(Size {
                        nodes: 1,
                        bytes: segment.len(),
                    });
                }
                None => {}
            }
        }
        let held = self.held.minus(replaced).plus(created).plus(size);
        if !held.within(self.budget.tree) {
            return Err(QueryError::Limit("program size"));
        }
        self.spend(created.plus(size))?;
        self.held = held;
        Ok(())
    }

    /// Put `node` at the non-empty `path`, replacing what is there and
    /// creating missing parents.
    fn insert(&mut self, path: &[Vec<u8>], node: OwnedNode) {
        let parents: Vec<&[u8]> = path[..path.len() - 1].iter().map(Vec::as_slice).collect();
        self.tree.ensure(&parents).set_child(node);
    }

    /// Head the result spots with the error slot `[00] = value` (spec 11.2).
    pub fn set_error(&mut self, value: Vec<u8>) {
        self.tree
            .ensure(&[&[keys::DOWNLOAD]])
            .set_child(OwnedNode::leaf(vec![0x00], value));
    }

    /// The result spots `[3]` in reply form: `80`, one pushed list per spot,
    /// `40` (spec 4.1); `80 40` when no spot was written. `None` when a spot
    /// holds what no reply can carry, such as a value over 65,535 bytes
    /// (file-format.md 4.1): the caller answers an error instead.
    pub fn reply_slots(&self) -> Option<Vec<u8>> {
        match self.tree.child(&[keys::DOWNLOAD]) {
            Some(spots) => spots.to_wire(),
            None => Some(vec![om_hbam::slot::BEGIN, om_hbam::slot::POP]),
        }
    }
}

/// A key path operand: segments as `[u8 len][bytes]`, at most
/// [`limits::MAX_DEPTH`] of them, as no key in a program or a file is
/// deeper.
pub fn decode_path(bytes: &[u8]) -> Result<SpotPath, QueryError> {
    segments(bytes, limits::MAX_DEPTH).ok_or(QueryError::Malformed(
        "key path deeper than a program or file holds",
    ))?
}

/// Split `[u8 len][bytes]...` into segments.
pub fn decode_segments(bytes: &[u8]) -> Result<Vec<Vec<u8>>, QueryError> {
    segments(bytes, usize::MAX).expect("no limit")
}

/// Split `[u8 len][bytes]...` into at most `max` segments; `None` when
/// there are more.
fn segments(mut bytes: &[u8], max: usize) -> Option<Result<Vec<Vec<u8>>, QueryError>> {
    let mut out = Vec::new();
    while let Some((&len, rest)) = bytes.split_first() {
        if out.len() == max {
            return None;
        }
        let len = usize::from(len);
        if rest.len() < len {
            return Some(Err(QueryError::Malformed("segment length")));
        }
        out.push(rest[..len].to_vec());
        bytes = &rest[len..];
    }
    Some(Ok(out))
}

/// A key string as finders and predicates carry it: `[u8 total][segments]`,
/// each segment `[u8 len][bytes]`. Returns the segments and the bytes after.
pub fn take_key_string(bytes: &[u8]) -> Result<(Vec<Vec<u8>>, &[u8]), QueryError> {
    let (&total, rest) = bytes
        .split_first()
        .ok_or(QueryError::Malformed("key string truncated"))?;
    let total = usize::from(total);
    if rest.len() < total {
        return Err(QueryError::Malformed("key string truncated"));
    }
    Ok((decode_segments(&rest[..total])?, &rest[total..]))
}
