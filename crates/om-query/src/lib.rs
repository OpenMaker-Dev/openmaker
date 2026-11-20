//! FileMaker query programs: the finds, relationship joins and found-set
//! operations a FileMaker client asks a host to run (op86 QueryProgram), run
//! against an HBAM file's stored indexes.
//!
//! The client converts every find value and match value into index keys, so
//! the engine mostly compares keys with the file's value and word indexes;
//! only sorts and projections of unindexed text fields key stored values
//! themselves, with `om-collate`. Shared by OpenMaker Server (hosted files)
//! and the OpenMaker client (local files).
//!
//! Spec: `docs/re/query-program.md`. Implemented: SimpleFind (op 10, finder
//! kinds 0-2 and 4-7), Projection (11, keys as the fields' value indexes hold
//! them), JoinProjection (12, predicate types 0-6), SortList
//! (14, by fields of the sorted table), CreateValueListEx (19, auto-complete
//! values), the RID-list operations (5-8), node
//! copy and delete (3, 4), ExistsInIndex (18), NodeExists (41) and GetKey
//! (42), and the schema instructions UploadMultiple (40) and DeleteTable
//! (50), whose changes to the file the caller applies, and
//! GenerateMemberKey (43), which the source answers
//! (`docs/re/schema-path.md` 3.4). Others fail with the error FileMaker
//! Server uses for an unknown opcode.
//!
//! Programs come from clients and are hostile until proven otherwise. Every
//! program runs within OpenMaker's limits ([`program::limits`], spec 11.4;
//! FileMaker Server's are unmeasured): the depth of every key path and of
//! the program's tree, the number of instructions, the tree's size, what
//! the instructions read and write in total, the file's indexes included,
//! and the fields of a sort. One that goes past them fails as a malformed
//! program does. Instructions find index keys by binary search (spec 7.1):
//! a program reads each index's keys once, to check their order, and
//! otherwise what its own size and its answer need.

pub mod criteria;
mod exec;
pub mod find;
mod index;
pub mod keys;
pub mod program;
mod sort;
mod source;

pub use exec::{execute, execute_parsed, Context, Outcome, Write};
pub use program::{Instruction, Program};
pub use source::Source;

/// Why a program could not run. [`QueryError::code`] is the FileMaker error
/// the reply's error slot carries (spec 11.2).
#[derive(Debug)]
pub enum QueryError {
    /// The program or an operand is not well-formed.
    Malformed(&'static str),
    /// An instruction names a file this connection has not opened.
    WrongFile,
    /// An instruction's access key does not match the session.
    BadAccessKey,
    /// Opcode 1, which a host refuses as an invalid command.
    InvalidCommand,
    /// An opcode, finder kind or predicate this engine does not implement.
    Unsupported(&'static str, u8),
    /// A table or field the program names does not exist.
    Missing(&'static str),
    /// An instruction that needs a table names none.
    NoTable,
    /// A feature of an implemented instruction this engine does not support.
    Unimplemented(&'static str),
    /// The program goes past one of OpenMaker's limits on what a program
    /// may hold or do ([`program::limits`]).
    Limit(&'static str),
    /// No record answers an instruction that needs one (LookupValue, spec
    /// 8.6): FileMaker's 401, "no records match".
    NoRecords,
    /// Reading the file failed.
    File(om_hbam::Error),
}

impl QueryError {
    /// The FileMaker error code for the reply's error slot.
    pub fn code(&self) -> u16 {
        match self {
            QueryError::WrongFile => 18,
            QueryError::InvalidCommand => 5,
            QueryError::Missing(_) => 102,
            QueryError::NoTable => 106,
            QueryError::NoRecords => 401,
            QueryError::Malformed(_)
            | QueryError::BadAccessKey
            | QueryError::Unsupported(..)
            | QueryError::Unimplemented(_)
            | QueryError::Limit(_)
            | QueryError::File(_) => 0xffff,
        }
    }
}

impl From<om_hbam::Error> for QueryError {
    fn from(e: om_hbam::Error) -> Self {
        QueryError::File(e)
    }
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::Malformed(what) => write!(f, "malformed program: {what}"),
            QueryError::WrongFile => f.write_str("instruction names another file"),
            QueryError::BadAccessKey => f.write_str("instruction access key does not match"),
            QueryError::InvalidCommand => f.write_str("opcode 1 is not allowed on a host"),
            QueryError::Unsupported(what, n) => write!(f, "unsupported {what} {n}"),
            QueryError::Missing(what) => write!(f, "{what} does not exist"),
            QueryError::NoTable => f.write_str("instruction names no table"),
            QueryError::Unimplemented(what) => write!(f, "unsupported: {what}"),
            QueryError::Limit(what) => write!(f, "program goes past OpenMaker's limit on {what}"),
            QueryError::NoRecords => f.write_str("no records match"),
            QueryError::File(e) => write!(f, "reading the file: {e}"),
        }
    }
}

impl std::error::Error for QueryError {}
