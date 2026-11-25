//! The state in trees, and applying one entry to it
//! (`docs/server-storage.md` sections 6.2, 6.3 and 7.3). The layout and
//! the rules are `docs/server-storage-format.md` section 5.
//!
//! The state is a set of named trees ([`TreeId`]): the store's own tree,
//! and one tree per file holding exactly the file's key tree. The page
//! store holds their roots by name ([`PageStore::trees`]), outside any
//! tree; before entry 1 there is no tree. The store's own tree holds the files'
//! rows, the session table with each server's start count, the lock table,
//! the tree format version and the last entry applied ([`StoreTree`] reads
//! it). No item of any tree holds a page number, so a root hash depends on
//! content alone.
//!
//! [`apply`] is a function of the state and the entry alone: it reads the
//! state, decides everything the entry does or refuses it, and only then
//! writes, every tree's changes through one unit of work of the page store:
//! the pages of each tree it changes, the file trees it makes and drops,
//! and each changed tree's new root, committed once. An entry that cannot
//! be right is refused ([`Error::Refused`]) before anything is written, and
//! every server refuses it the same way; any error leaves the store as it
//! was.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::Bound;

use crate::entry::{
    entry_hash, Entry, FileEvent, Kind, LockEdit, Members, Op, Payload, PayloadError, ServerId,
    Session, SessionEvent,
};
use crate::page::{EntryPages, Hash, PageStore, StoreError, TreeName, FORMAT_VERSION};
use crate::tree::{self, Direction, Edit, Node, Root, Tree};

/// The top-level segments of the store's own tree, and the segments under a
/// file's row (format section 5.3). Each is one byte.
mod keys {
    pub const APPLIED: u8 = 0x01;
    pub const FORMAT: u8 = 0x02;
    pub const FILES: u8 = 0x03;
    pub const NAMES: u8 = 0x04;
    pub const SERVERS: u8 = 0x05;
    pub const SESSIONS: u8 = 0x06;
    pub const LOCKS: u8 = 0x07;

    pub const FILE_NAME: u8 = 0x01;
    pub const FILE_FLAGS: u8 = 0x02;
    pub const FILE_PAGE0: u8 = 0x03;

    /// Under a session's list `[SESSIONS, server, session]`.
    pub const SESSION_SEQ: u8 = 0x01;
    pub const SESSION_REPLY: u8 = 0x02;
}

type Path = Vec<Vec<u8>>;

/// One of the state's trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TreeId {
    /// The store's own tree.
    Store,
    /// A file's tree, by the file's id.
    File(u64),
}

impl TreeId {
    /// The tree's name, as the page store keeps it: `store`, or `file:`
    /// followed by the file's id in decimal.
    pub fn name(&self) -> TreeName {
        let name = match self {
            TreeId::Store => b"store".to_vec(),
            TreeId::File(id) => format!("file:{id}").into_bytes(),
        };
        TreeName::new(name).expect("a name of at most 25 bytes")
    }

    /// The tree a name names: only the names [`TreeId::name`] gives.
    pub fn from_name(name: &[u8]) -> Option<TreeId> {
        if name == b"store" {
            return Some(TreeId::Store);
        }
        let digits = name.strip_prefix(b"file:")?;
        let canonical =
            !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) && digits[0] != b'0';
        let id: u64 = std::str::from_utf8(digits).ok()?.parse().ok()?;
        canonical.then_some(TreeId::File(id))
    }
}

/// An entry's place in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub index: u64,
    pub term: u64,
    pub hash: Hash,
}

impl Position {
    /// Before entry 1.
    pub const START: Position = Position {
        index: 0,
        term: 0,
        hash: Hash([0; 32]),
    };

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(48);
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&self.term.to_le_bytes());
        out.extend_from_slice(&self.hash.0);
        out
    }

    fn decode(bytes: &[u8]) -> Option<Position> {
        (bytes.len() == 48).then(|| Position {
            index: le64(&bytes[..8]),
            term: le64(&bytes[8..16]),
            hash: Hash(bytes[16..48].try_into().unwrap_or([0; 32])),
        })
    }
}

/// A file's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub name: Vec<u8>,
    pub flags: u32,
    pub page0: Vec<u8>,
}

/// A server's row: its highest start count, and the highest connection
/// number opened in that start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerRow {
    pub start: u64,
    pub connection: u64,
}

/// An open session's row: the last sequence number it completed (0 for
/// none) and that operation's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub seq: u64,
    pub reply: Vec<u8>,
}

/// A lock held.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockRow {
    pub session: Session,
    pub file: u64,
    pub key: Vec<u8>,
    pub user: Vec<u8>,
    pub value: Vec<u8>,
}

/// Why an entry cannot be right. Every server refuses such an entry the same
/// way, with nothing changed, and the caller stops the store: applying
/// never guesses and never skips part of an entry. The checks run in the
/// order of format section 5.6, and the first that fails is the one
/// reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The entry is not the one after the last applied: repeated, skipped or
    /// out of order.
    NotNext { last: u64, index: u64 },
    /// Its term is below the last applied entry's.
    TermBehind { last: u64, term: u64 },
    /// Its hash does not continue the last applied entry's.
    Chain { expected: Hash, found: Hash },
    /// The state's tree format version, or the one a `members` entry
    /// names, is not one this build knows (0 is not a version).
    UnknownFormatVersion(u8),
    /// A `members` entry names a lower tree format version than the
    /// state's: it cannot arise while 1 is the only version.
    FormatVersionLowered { current: u8, version: u8 },
    /// The payload does not decode: a kind or payload version this build
    /// does not know, malformed bytes, something beyond a bound, or
    /// something reserved ([`PayloadError::Unsupported`]).
    Payload(PayloadError),
    /// The entry's file field does not fit its kind.
    FileField { kind: Kind, file: u64 },
    /// The entry names a file the state does not have.
    NoSuchFile(u64),
    /// A file is added or renamed to a name a file has.
    NameTaken(Vec<u8>),
    /// A change names a session that is not open, for its operation or a
    /// lock edit.
    SessionNotOpen(Session),
    /// A change's sequence number is not the one after its session's last.
    Sequence {
        session: Session,
        last: u64,
        seq: u64,
    },
    /// Lock edit number `edit` removes a lock that is not held.
    NoSuchLock { edit: usize },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotNext { last, index } => {
                write!(f, "entry {index} is not the one after entry {last}")
            }
            Refusal::TermBehind { last, term } => {
                write!(f, "term {term} is below the last entry's term {last}")
            }
            Refusal::Chain { expected, found } => {
                write!(f, "hash {found} does not continue the chain ({expected})")
            }
            Refusal::UnknownFormatVersion(v) => {
                write!(f, "tree format version {v} is not one this build knows")
            }
            Refusal::FormatVersionLowered { current, version } => {
                write!(
                    f,
                    "tree format version {version} is below the state's {current}"
                )
            }
            Refusal::Payload(e) => e.fmt(f),
            Refusal::FileField { kind, file } => {
                write!(f, "a {kind:?} entry cannot name file {file}")
            }
            Refusal::NoSuchFile(id) => write!(f, "file {id} does not exist"),
            Refusal::NameTaken(name) => write!(f, "a file is named {}", name.escape_ascii()),
            Refusal::SessionNotOpen(s) => write!(f, "session {s:?} is not open"),
            Refusal::Sequence { session, last, seq } => write!(
                f,
                "sequence number {seq} does not follow {last} in session {session:?}"
            ),
            Refusal::NoSuchLock { edit } => write!(f, "lock edit {edit} removes a lock not held"),
        }
    }
}

/// Why an entry was not applied. Whatever the error, nothing changed: the
/// page store is as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The entry cannot be right ([`Refusal`]).
    Refused(Refusal),
    /// A page could not be read or the page store failed: this server's own
    /// trouble, not the entry's.
    Tree(tree::Error),
    /// The page store holds no tree for a file the state has.
    MissingRoot(TreeId),
    /// The store's tree holds a row this build cannot read.
    Malformed(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused(r) => write!(f, "entry refused: {r}"),
            Error::Tree(e) => e.fmt(f),
            Error::MissingRoot(t) => write!(f, "the page store holds no tree {t:?}"),
            Error::Malformed(why) => write!(f, "the store's tree is malformed: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<tree::Error> for Error {
    fn from(e: tree::Error) -> Error {
        Error::Tree(e)
    }
}

impl From<StoreError> for Error {
    fn from(e: StoreError) -> Error {
        Error::Tree(tree::Error::Store(e))
    }
}

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Error {
        Error::Refused(r)
    }
}

/// What the checks of an entry listed from the store's tree: the rows the
/// listings visited, and the most listed rows held at once. A listing holds
/// one row at a time, so this is the depth of listings within listings
/// (format section 5.8), whatever the number of rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Listed {
    pub rows: u64,
    pub held: u64,
}

/// What applying an entry did, for the caller to keep and publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub position: Position,
    /// The entry's payload. A change's notices and reply, with its
    /// operation's session and sequence number, are delivered when the
    /// entry is published.
    pub payload: Payload,
    /// The session the entry opened. An opening that changed nothing is
    /// `None`.
    pub opened: Option<Session>,
    /// The sessions the entry closed, with their locks.
    pub closed: Vec<Session>,
    pub listed: Listed,
}

impl Applied {
    /// The notices to deliver.
    pub fn notices(&self) -> &[Vec<u8>] {
        match &self.payload {
            Payload::Change(c) => &c.notices,
            _ => &[],
        }
    }

    /// The operation the entry completed, and its reply.
    pub fn reply(&self) -> Option<(&Op, &[u8])> {
        match &self.payload {
            Payload::Change(c) => c.op.as_ref().map(|op| (op, c.reply.as_slice())),
            _ => None,
        }
    }
}

fn le64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_le_bytes(a)
}

fn be64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_be_bytes(a)
}

fn seg(b: u8) -> Vec<u8> {
    vec![b]
}

/// A session within its server: start count and connection, big-endian, so
/// that a server's sessions sort by start, then connection.
fn session_segment(s: &Session) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&s.start.to_be_bytes());
    out.extend_from_slice(&s.connection.to_be_bytes());
    out
}

/// A lock within a session's locks on a file: the key's length (2 bytes,
/// big-endian), the key, then the user.
fn lock_segment(key: &[u8], user: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + key.len() + user.len());
    // `entry` holds a lock key to 16 KiB.
    out.extend_from_slice(&(key.len() as u16).to_be_bytes());
    out.extend_from_slice(key);
    out.extend_from_slice(user);
    out
}

fn file_path(id: u64) -> Path {
    vec![seg(keys::FILES), id.to_be_bytes().to_vec()]
}

fn file_field(id: u64, field: u8) -> Path {
    vec![seg(keys::FILES), id.to_be_bytes().to_vec(), seg(field)]
}

fn name_path(name: &[u8]) -> Path {
    vec![seg(keys::NAMES), name.to_vec()]
}

fn server_path(server: &ServerId) -> Path {
    vec![seg(keys::SERVERS), server.to_vec()]
}

fn server_sessions(server: &ServerId) -> Path {
    vec![seg(keys::SESSIONS), server.to_vec()]
}

/// A session's list: present exactly while the session is open.
fn session_path(s: &Session) -> Path {
    vec![seg(keys::SESSIONS), s.server.to_vec(), session_segment(s)]
}

fn session_field(s: &Session, field: u8) -> Path {
    let mut p = session_path(s);
    p.push(seg(field));
    p
}

fn server_locks(server: &ServerId) -> Path {
    vec![seg(keys::LOCKS), server.to_vec()]
}

fn session_locks(s: &Session) -> Path {
    vec![seg(keys::LOCKS), s.server.to_vec(), session_segment(s)]
}

fn file_locks(s: &Session, file: u64) -> Path {
    let mut p = session_locks(s);
    p.push(file.to_be_bytes().to_vec());
    p
}

fn lock_path(s: &Session, file: u64, key: &[u8], user: &[u8]) -> Path {
    let mut p = file_locks(s, file);
    p.push(lock_segment(key, user));
    p
}

fn server_value(row: ServerRow) -> Vec<u8> {
    let mut out = row.start.to_le_bytes().to_vec();
    out.extend_from_slice(&row.connection.to_le_bytes());
    out
}

/// The edits that record a session's last completed operation: two leaves
/// under the session's list, so that listing a server's sessions reads
/// lists and no reply.
fn session_row(s: &Session, seq: u64, reply: &[u8]) -> [Edit; 2] {
    [
        Edit::leaf(
            session_field(s, keys::SESSION_SEQ),
            seq.to_le_bytes().to_vec(),
        ),
        Edit::leaf(session_field(s, keys::SESSION_REPLY), reply.to_vec()),
    ]
}

/// Whether a tree format version is one this build applies entries with.
fn known_version(version: u8) -> bool {
    (1..=FORMAT_VERSION).contains(&version)
}

/// The store's own tree, read as the tables it holds. Every row is checked
/// as it is read; one this build cannot read is [`Error::Malformed`].
pub struct StoreTree<'s, S: ?Sized> {
    /// None before entry 1: the store has no tree yet.
    tree: Option<Tree<'s, S>>,
    listed: Cell<Listed>,
    /// Listed rows held now.
    holding: Cell<u64>,
}

impl<'s, S: PageStore + ?Sized> StoreTree<'s, S> {
    /// The store's tree in the newest version `store` holds.
    pub fn of(store: &'s S) -> Result<Self, Error> {
        match store.root(&TreeId::Store.name()) {
            Some(root) => Self::open(store, root),
            None => Ok(StoreTree {
                tree: None,
                listed: Cell::new(Listed::default()),
                holding: Cell::new(0),
            }),
        }
    }

    pub fn open(store: &'s S, root: Root) -> Result<Self, Error> {
        Ok(StoreTree {
            tree: Some(Tree::open(store, root)?),
            listed: Cell::new(Listed::default()),
            holding: Cell::new(0),
        })
    }

    /// What the listings made through this reader visited and held.
    pub fn listed(&self) -> Listed {
        self.listed.get()
    }

    /// The node at `path`, if there is one.
    fn get(&self, path: &[Vec<u8>]) -> Result<Option<Node>, Error> {
        match &self.tree {
            Some(tree) => Ok(tree.get(path)?),
            None => Ok(None),
        }
    }

    /// The value at `path`, if a value is there.
    fn value(&self, path: &[Vec<u8>]) -> Result<Option<Vec<u8>>, Error> {
        match self.get(path)? {
            None => Ok(None),
            Some(Node::Value(v)) => Ok(Some(v)),
            Some(_) => Err(Error::Malformed("a list where a row's value belongs")),
        }
    }

    /// Calls `f` with each child of the node at `path`, in order, one at a
    /// time: the listing holds one child at once, however many there are.
    fn each_child(
        &self,
        path: &[Vec<u8>],
        mut f: impl FnMut(Vec<u8>, Node) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let Some(tree) = &self.tree else {
            return Ok(());
        };
        for child in tree.children(path, Bound::Unbounded, Direction::Forward) {
            let (segment, node) = child?;
            let holding = self.holding.get() + 1;
            self.holding.set(holding);
            let mut listed = self.listed.get();
            listed.rows += 1;
            listed.held = listed.held.max(holding);
            self.listed.set(listed);
            let done = f(segment, node);
            self.holding.set(holding - 1);
            done?;
        }
        Ok(())
    }

    /// The last entry applied: [`Position::START`] before entry 1.
    pub fn applied(&self) -> Result<Position, Error> {
        match self.value(&[seg(keys::APPLIED)])? {
            None => Ok(Position::START),
            Some(v) => Position::decode(&v).ok_or(Error::Malformed("the last entry applied")),
        }
    }

    /// The tree format version entries are applied with: 1 until a
    /// `members` entry sets it.
    pub fn format_version(&self) -> Result<u8, Error> {
        match self.value(&[seg(keys::FORMAT)])? {
            None => Ok(1),
            Some(v) if v.len() == 1 => Ok(v[0]),
            Some(_) => Err(Error::Malformed("the tree format version")),
        }
    }

    pub fn file(&self, id: u64) -> Result<Option<FileRow>, Error> {
        let Some(name) = self.value(&file_field(id, keys::FILE_NAME))? else {
            return Ok(None);
        };
        let flags = self
            .value(&file_field(id, keys::FILE_FLAGS))?
            .filter(|f| f.len() == 4)
            .ok_or(Error::Malformed("a file's flags"))?;
        let page0 = self
            .value(&file_field(id, keys::FILE_PAGE0))?
            .ok_or(Error::Malformed("a file's page 0"))?;
        Ok(Some(FileRow {
            name,
            flags: u32::from_le_bytes([flags[0], flags[1], flags[2], flags[3]]),
            page0,
        }))
    }

    /// Every file, by id.
    pub fn files(&self) -> Result<Vec<(u64, FileRow)>, Error> {
        let mut out = Vec::new();
        self.each_child(&[seg(keys::FILES)], |segment, _| {
            if segment.len() != 8 {
                return Err(Error::Malformed("a file's id"));
            }
            let id = be64(&segment);
            let row = self
                .file(id)?
                .ok_or(Error::Malformed("a file without a name"))?;
            out.push((id, row));
            Ok(())
        })?;
        Ok(out)
    }

    /// The id of the file named `name`.
    pub fn file_named(&self, name: &[u8]) -> Result<Option<u64>, Error> {
        match self.value(&name_path(name))? {
            None => Ok(None),
            Some(v) if v.len() == 8 => Ok(Some(le64(&v))),
            Some(_) => Err(Error::Malformed("a name's file")),
        }
    }

    /// The whole name index: every name with the id of its file.
    pub fn names(&self) -> Result<Vec<(Vec<u8>, u64)>, Error> {
        let mut out = Vec::new();
        self.each_child(&[seg(keys::NAMES)], |name, node| match node {
            Node::Value(v) if v.len() == 8 => {
                out.push((name, le64(&v)));
                Ok(())
            }
            _ => Err(Error::Malformed("a name's file")),
        })?;
        Ok(out)
    }

    pub fn server(&self, server: &ServerId) -> Result<Option<ServerRow>, Error> {
        match self.value(&server_path(server))? {
            None => Ok(None),
            Some(v) if v.len() == 16 => Ok(Some(ServerRow {
                start: le64(&v[..8]),
                connection: le64(&v[8..]),
            })),
            Some(_) => Err(Error::Malformed("a server's row")),
        }
    }

    pub fn servers(&self) -> Result<Vec<(ServerId, ServerRow)>, Error> {
        let mut out = Vec::new();
        self.each_child(&[seg(keys::SERVERS)], |segment, _| {
            let server: ServerId = segment
                .as_slice()
                .try_into()
                .map_err(|_| Error::Malformed("a server's id"))?;
            let row = self
                .server(&server)?
                .ok_or(Error::Malformed("a server's row"))?;
            out.push((server, row));
            Ok(())
        })?;
        Ok(out)
    }

    /// Whether a session is open: its list is there. Reads no value.
    pub fn is_open(&self, s: &Session) -> Result<bool, Error> {
        match self.get(&session_path(s))? {
            None => Ok(false),
            Some(Node::List) => Ok(true),
            Some(_) => Err(Error::Malformed("a value where a session's list belongs")),
        }
    }

    /// The last sequence number an open session completed (0 for none),
    /// without its reply.
    fn seq(&self, s: &Session) -> Result<Option<u64>, Error> {
        if !self.is_open(s)? {
            return Ok(None);
        }
        match self.value(&session_field(s, keys::SESSION_SEQ))? {
            Some(v) if v.len() == 8 => Ok(Some(le64(&v))),
            _ => Err(Error::Malformed("a session's sequence number")),
        }
    }

    pub fn session(&self, s: &Session) -> Result<Option<SessionRow>, Error> {
        let Some(seq) = self.seq(s)? else {
            return Ok(None);
        };
        let reply = self
            .value(&session_field(s, keys::SESSION_REPLY))?
            .ok_or(Error::Malformed("a session's reply"))?;
        Ok(Some(SessionRow { seq, reply }))
    }

    /// Calls `f` with each open session of `server`, from their lists
    /// alone: no sequence number or reply is read.
    fn each_session(
        &self,
        server: &ServerId,
        mut f: impl FnMut(Session) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.each_child(&server_sessions(server), |segment, node| {
            let (Node::List, 16) = (node, segment.len()) else {
                return Err(Error::Malformed("a session's list"));
            };
            f(Session {
                server: *server,
                start: be64(&segment[..8]),
                connection: be64(&segment[8..]),
            })
        })
    }

    /// Every open session, with its row.
    pub fn sessions(&self) -> Result<Vec<(Session, SessionRow)>, Error> {
        let mut out = Vec::new();
        self.each_child(&[seg(keys::SESSIONS)], |segment, _| {
            let server: ServerId = segment
                .as_slice()
                .try_into()
                .map_err(|_| Error::Malformed("a server's id"))?;
            self.each_session(&server, |session| {
                let row = self
                    .session(&session)?
                    .ok_or(Error::Malformed("a session's row"))?;
                out.push((session, row));
                Ok(())
            })
        })?;
        Ok(out)
    }

    /// The value of a lock, if it is held.
    pub fn lock(
        &self,
        session: &Session,
        file: u64,
        key: &[u8],
        user: &[u8],
    ) -> Result<Option<Vec<u8>>, Error> {
        self.value(&lock_path(session, file, key, user))
    }

    /// Every lock held, in the order of the tree: by session, then file,
    /// then key and user. The layer above builds its lookup by key from
    /// this at startup.
    pub fn locks(&self) -> Result<Vec<LockRow>, Error> {
        let mut out = Vec::new();
        let Some(tree) = &self.tree else {
            return Ok(out);
        };
        for item in tree.walk(&[seg(keys::LOCKS)]) {
            let item = item?;
            let path = item.key.path();
            if path.len() < 5 {
                // The lists of servers, sessions and files.
                if item.node != Node::List {
                    return Err(Error::Malformed("a value above a lock"));
                }
                continue;
            }
            let (Node::Value(value), 5, 16, 16, 8) = (
                item.node,
                path.len(),
                path[1].len(),
                path[2].len(),
                path[3].len(),
            ) else {
                return Err(Error::Malformed("a lock's row"));
            };
            let ku = &path[4];
            let key_len = ku
                .get(..2)
                .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
                .filter(|n| 2 + n <= ku.len())
                .ok_or(Error::Malformed("a lock's key"))?;
            out.push(LockRow {
                session: Session {
                    server: path[1].as_slice().try_into().unwrap_or_default(),
                    start: be64(&path[2][..8]),
                    connection: be64(&path[2][8..]),
                },
                file: be64(&path[3]),
                key: ku[2..2 + key_len].to_vec(),
                user: ku[2 + key_len..].to_vec(),
                value,
            });
        }
        Ok(out)
    }

    /// Calls `f` with the path of each session's lock list for `file`,
    /// looking through the sessions that hold lock lists one at a time:
    /// what it holds does not grow with the sessions that have none for the
    /// file.
    fn each_file_lock_list(
        &self,
        file: u64,
        mut f: impl FnMut(Path) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.each_child(&[seg(keys::LOCKS)], |server, _| {
            let above = vec![seg(keys::LOCKS), server];
            self.each_child(&above, |session, _| {
                let path = vec![
                    above[0].clone(),
                    above[1].clone(),
                    session,
                    file.to_be_bytes().to_vec(),
                ];
                if self.get(&path)?.is_some() {
                    f(path)?;
                }
                Ok(())
            })
        })
    }
}

/// What an entry does, decided before anything is written.
struct Plan {
    /// Edits of the store's tree, in order.
    store: Vec<Edit>,
    /// The file whose tree the change's edits go to, and its root.
    file: Option<(u64, Root)>,
    make: Option<u64>,
    drop: Option<u64>,
    opened: Option<Session>,
    closed: Vec<Session>,
}

/// The lock edits a change has made so far, by session, so that a later
/// edit of the change sees them. A release clears only its own session's
/// edits, each of which one earlier edit made, so planning a change is
/// linear in its lock edits.
#[derive(Default)]
struct Pending {
    sessions: BTreeMap<Session, PendingSession>,
    /// The map entries visited, which a test holds to linear.
    visited: u64,
}

struct PendingSession {
    open: bool,
    /// The change released the session's locks on the file.
    released: bool,
    /// Locks the change set (true) or removed (false), by lock segment.
    locks: BTreeMap<Vec<u8>, bool>,
}

impl Pending {
    /// Whether a session is open, read from the state once per change.
    fn is_open<S: PageStore + ?Sized>(
        &mut self,
        state: &StoreTree<'_, S>,
        s: &Session,
    ) -> Result<bool, Error> {
        self.visited += 1;
        if let Some(p) = self.sessions.get(s) {
            return Ok(p.open);
        }
        let open = state.is_open(s)?;
        self.sessions.insert(
            *s,
            PendingSession {
                open,
                released: false,
                locks: BTreeMap::new(),
            },
        );
        Ok(open)
    }

    fn session(&mut self, s: &Session) -> &mut PendingSession {
        // `is_open` has entered every session an edit names.
        self.sessions.entry(*s).or_insert_with(|| PendingSession {
            open: true,
            released: false,
            locks: BTreeMap::new(),
        })
    }

    fn set(&mut self, s: &Session, lock: Vec<u8>, held: bool) {
        self.visited += 1;
        self.session(s).locks.insert(lock, held);
    }

    /// Whether the change has decided that the lock is held: `None` when
    /// the state before the change decides.
    fn held(&mut self, s: &Session, lock: &[u8]) -> Option<bool> {
        self.visited += 1;
        let p = self.sessions.get(s)?;
        match p.locks.get(lock) {
            Some(&held) => Some(held),
            None => p.released.then_some(false),
        }
    }

    fn release(&mut self, s: &Session) {
        let p = self.session(s);
        let cleared = p.locks.len() as u64;
        p.locks.clear();
        p.released = true;
        self.visited += 1 + cleared;
    }
}

/// Applies `entry` to the state `store` holds, and says what it did.
///
/// The entry must be the next one: its index one more than the last
/// applied, its term not below that entry's, and its hash
/// [`entry_hash`] over that entry's hash and its own bytes. Every check
/// (format section 5.6) is made from reads of the state before anything is
/// written; then everything the entry changes goes through one unit of work
/// of `store`, which names the trees made and dropped and every changed
/// tree's new root, committed once. An error at any point leaves the store
/// as it was.
pub fn apply<S: PageStore + ?Sized>(store: &S, entry: &Entry<'_>) -> Result<Applied, Error> {
    let store_root = store.root(&TreeId::Store.name());
    let state = StoreTree::of(store)?;
    let last = state.applied()?;
    if last.index.checked_add(1) != Some(entry.index) {
        return Err(Refusal::NotNext {
            last: last.index,
            index: entry.index,
        }
        .into());
    }
    if entry.term < last.term {
        return Err(Refusal::TermBehind {
            last: last.term,
            term: entry.term,
        }
        .into());
    }
    let expected = entry_hash(
        &last.hash,
        entry.index,
        entry.term,
        entry.kind,
        entry.file,
        entry.payload,
    );
    if expected != entry.hash {
        return Err(Refusal::Chain {
            expected,
            found: entry.hash,
        }
        .into());
    }
    let version = state.format_version()?;
    if !known_version(version) {
        return Err(Refusal::UnknownFormatVersion(version).into());
    }
    let payload = Payload::decode(entry.kind, entry.payload).map_err(Refusal::Payload)?;
    let mut plan = plan(store, &state, entry, &payload, version)?;
    let position = Position {
        index: entry.index,
        term: entry.term,
        hash: entry.hash,
    };
    plan.store
        .push(Edit::leaf(vec![seg(keys::APPLIED)], position.encode()));
    let listed = state.listed();
    drop(state);

    // Everything is decided; now the writes, all in one unit.
    let mut unit = store.begin()?;
    if let Some(id) = plan.make {
        let root = tree::apply(store, &mut unit, None, entry.index, &[])?;
        unit.add_tree(&TreeId::File(id).name(), root);
    }
    if let (Some((id, root)), Payload::Change(change)) = (plan.file, &payload) {
        if !change.edits.is_empty() {
            let new = tree::apply(store, &mut unit, Some(root), entry.index, &change.edits)?;
            if new != root {
                unit.set_root(&TreeId::File(id).name(), new);
            }
        }
    }
    if let Some(id) = plan.drop {
        unit.drop_tree(&TreeId::File(id).name());
    }
    let new = tree::apply(store, &mut unit, store_root, entry.index, &plan.store)?;
    match store_root {
        None => unit.add_tree(&TreeId::Store.name(), new),
        Some(old) if old != new => unit.set_root(&TreeId::Store.name(), new),
        Some(_) => {}
    }
    unit.commit()?;
    Ok(Applied {
        position,
        payload,
        opened: plan.opened,
        closed: plan.closed,
        listed,
    })
}

/// Decides what the entry does, from reads of the state alone.
fn plan<S: PageStore + ?Sized>(
    store: &S,
    state: &StoreTree<'_, S>,
    entry: &Entry<'_>,
    payload: &Payload,
    version: u8,
) -> Result<Plan, Error> {
    let mut plan = Plan {
        store: Vec::new(),
        file: None,
        make: None,
        drop: None,
        opened: None,
        closed: Vec::new(),
    };
    let kind = payload.kind();
    let field = || {
        Error::Refused(Refusal::FileField {
            kind,
            file: entry.file,
        })
    };
    // The file the entry names, which must exist.
    let existing = || -> Result<FileRow, Error> {
        if entry.file == 0 {
            return Err(field());
        }
        state
            .file(entry.file)?
            .ok_or(Error::Refused(Refusal::NoSuchFile(entry.file)))
    };
    let root_of = |id: u64| {
        store
            .root(&TreeId::File(id).name())
            .ok_or(Error::MissingRoot(TreeId::File(id)))
    };
    match payload {
        Payload::Change(change) => {
            existing()?;
            let file = entry.file;
            if let Some(op) = &change.op {
                let Some(last) = state.seq(&op.session)? else {
                    return Err(Refusal::SessionNotOpen(op.session).into());
                };
                if last.checked_add(1) != Some(op.seq) {
                    return Err(Refusal::Sequence {
                        session: op.session,
                        last,
                        seq: op.seq,
                    }
                    .into());
                }
            }
            let mut pending = Pending::default();
            for (i, edit) in change.locks.iter().enumerate() {
                let session = edit.session();
                if !pending.is_open(state, session)? {
                    return Err(Refusal::SessionNotOpen(*session).into());
                }
                match edit {
                    LockEdit::Set {
                        session,
                        key,
                        user,
                        value,
                    } => {
                        pending.set(session, lock_segment(key, user), true);
                        plan.store.push(Edit::leaf(
                            lock_path(session, file, key, user),
                            value.clone(),
                        ));
                    }
                    LockEdit::Remove { session, key, user } => {
                        let lock = lock_segment(key, user);
                        let is_held = match pending.held(session, &lock) {
                            Some(h) => h,
                            None => state.lock(session, file, key, user)?.is_some(),
                        };
                        if !is_held {
                            return Err(Refusal::NoSuchLock { edit: i }.into());
                        }
                        pending.set(session, lock, false);
                        plan.store
                            .push(Edit::delete(lock_path(session, file, key, user)));
                    }
                    LockEdit::ReleaseFile { session } => {
                        pending.release(session);
                        plan.store.push(Edit::delete(file_locks(session, file)));
                    }
                }
            }
            if let Some(op) = &change.op {
                plan.store
                    .extend(session_row(&op.session, op.seq, &change.reply));
            }
            plan.file = Some((file, root_of(file)?));
        }
        Payload::Session(event) => {
            if entry.file != 0 {
                return Err(field());
            }
            match event {
                SessionEvent::Opened(s) => {
                    // Only in the start the server recorded, and above every
                    // connection opened in it: a delayed or repeated opening
                    // changes nothing.
                    let row = state.server(&s.server)?;
                    if let Some(row) =
                        row.filter(|r| r.start == s.start && s.connection > r.connection)
                    {
                        plan.store.push(Edit::leaf(
                            server_path(&s.server),
                            server_value(ServerRow {
                                start: row.start,
                                connection: s.connection,
                            }),
                        ));
                        plan.store.extend(session_row(s, 0, &[]));
                        plan.opened = Some(*s);
                    }
                }
                SessionEvent::Closed(s) => {
                    if state.is_open(s)? {
                        plan.store.push(Edit::delete(session_path(s)));
                        plan.store.push(Edit::delete(session_locks(s)));
                        plan.closed.push(*s);
                    }
                }
                SessionEvent::Started { server, count } => {
                    // A server without a row has recorded no start: its
                    // first start is applied, whatever its count.
                    let applies = match state.server(server)? {
                        None => true,
                        Some(row) => *count > row.start,
                    };
                    if applies {
                        state.each_session(server, |s| {
                            plan.closed.push(s);
                            Ok(())
                        })?;
                        plan.store.push(Edit::delete(server_sessions(server)));
                        plan.store.push(Edit::delete(server_locks(server)));
                        plan.store.push(Edit::leaf(
                            server_path(server),
                            server_value(ServerRow {
                                start: *count,
                                connection: 0,
                            }),
                        ));
                    }
                }
            }
        }
        Payload::File(event) => match event {
            FileEvent::Added { name, flags, page0 } => {
                if entry.file != entry.index {
                    return Err(field());
                }
                if state.file_named(name)?.is_some() {
                    return Err(Refusal::NameTaken(name.clone()).into());
                }
                let id = entry.index;
                plan.store
                    .push(Edit::leaf(file_field(id, keys::FILE_NAME), name.clone()));
                plan.store.push(Edit::leaf(
                    file_field(id, keys::FILE_FLAGS),
                    flags.to_le_bytes().to_vec(),
                ));
                plan.store
                    .push(Edit::leaf(file_field(id, keys::FILE_PAGE0), page0.clone()));
                plan.store
                    .push(Edit::leaf(name_path(name), id.to_le_bytes().to_vec()));
                plan.make = Some(id);
            }
            FileEvent::Removed => {
                let row = existing()?;
                let id = entry.file;
                plan.store.push(Edit::delete(file_path(id)));
                plan.store.push(Edit::delete(name_path(&row.name)));
                state.each_file_lock_list(id, |list| {
                    plan.store.push(Edit::delete(list));
                    Ok(())
                })?;
                root_of(id)?;
                plan.drop = Some(id);
            }
            FileEvent::Renamed { name } => {
                let row = existing()?;
                if state.file_named(name)?.is_some() {
                    return Err(Refusal::NameTaken(name.clone()).into());
                }
                let id = entry.file;
                plan.store
                    .push(Edit::leaf(file_field(id, keys::FILE_NAME), name.clone()));
                plan.store.push(Edit::delete(name_path(&row.name)));
                plan.store
                    .push(Edit::leaf(name_path(name), id.to_le_bytes().to_vec()));
            }
        },
        Payload::Members(Members::FormatVersion(v)) => {
            if entry.file != 0 {
                return Err(field());
            }
            if !known_version(*v) {
                return Err(Refusal::UnknownFormatVersion(*v).into());
            }
            if *v < version {
                return Err(Refusal::FormatVersionLowered {
                    current: version,
                    version: *v,
                }
                .into());
            }
            plan.store
                .push(Edit::leaf(vec![seg(keys::FORMAT)], vec![*v]));
        }
        Payload::Mark(_) => {
            if entry.file != 0 {
                existing()?;
            }
        }
        Payload::Empty => {
            if entry.file != 0 {
                return Err(field());
            }
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review of #732: a change that sets many locks for one session and
    /// releases another's many times does linear work in its lock edits.
    #[test]
    fn planning_lock_edits_is_linear() {
        let a = Session {
            server: [1; 16],
            start: 1,
            connection: 1,
        };
        let b = Session { connection: 2, ..a };
        let mut pending = Pending::default();
        pending.session(&a);
        pending.session(&b);
        let n = 20_000u64;
        for i in 0..n {
            pending.set(&a, i.to_be_bytes().to_vec(), true);
            pending.release(&b);
        }
        assert_eq!(pending.held(&a, &5u64.to_be_bytes()), Some(true));
        assert_eq!(pending.held(&b, b"x"), Some(false));
        assert!(
            pending.visited <= 3 * n,
            "{} map entries visited",
            pending.visited
        );
    }

    #[test]
    fn tree_names_round_trip() {
        for tree in [TreeId::Store, TreeId::File(1), TreeId::File(u64::MAX)] {
            assert_eq!(TreeId::from_name(tree.name().as_bytes()), Some(tree));
        }
        for name in [
            &b"file:"[..],
            b"file:01",
            b"file:0",
            b"file:+1",
            b"file:18446744073709551616",
            b"stor",
            b"file:1 ",
        ] {
            assert_eq!(TreeId::from_name(name), None, "{}", name.escape_ascii());
        }
    }
}
