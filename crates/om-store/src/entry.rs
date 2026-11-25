//! What a log entry holds (`docs/server-storage.md` section 6.3): the
//! payload of each entry kind, its bytes, and the hash that chains entries.
//! The byte formats are `docs/server-storage-format.md` section 5.
//!
//! The log stores an entry's index, term, kind, file and payload without
//! interpreting them. This module gives the kind and the payload their
//! meaning as data; what applying an entry does to the state is
//! [`crate::apply`].
//!
//! Decoding is the first check an entry passes, and its input is hostile: a
//! payload comes from a file on disk or from another server. Every length is
//! checked against the bytes that remain and against its bound before
//! anything is taken, counts are never used to reserve memory, nothing
//! recurses, and malformed bytes are an error, never a panic. A payload
//! decodes to at most a fixed multiple of its own size (format section 5.2).
//! Encoding checks the same bounds, so what encodes decodes again to the
//! same value.

use std::fmt;

use ring::digest;

use crate::page::{Hash, MAX_VALUE};
use crate::tree::{self, Edit, Node, Refusal, MAX_DEPTH};

/// The payload format version this build writes and reads: byte 0 of every
/// payload except an `empty` entry's.
pub const VERSION: u8 = 1;

/// The largest payload, as the log's format bounds it
/// (`docs/server-storage-format.md` section 3.4).
pub const MAX_PAYLOAD: usize = 64 << 20;
/// The longest name of a file or a mark, in bytes.
pub const MAX_NAME: usize = 1024;
/// The longest page 0 a file is added with: an `.fmp12` page.
pub const MAX_PAGE0: usize = 4096;
/// The longest reply: the session's row keeps it as one value.
pub const MAX_REPLY: usize = MAX_VALUE;
/// The longest notice.
pub const MAX_NOTICE: usize = MAX_VALUE;
/// The most notices one change carries.
pub const MAX_NOTICES: usize = 1 << 16;
/// The longest lock key and lock user. With them a lock's row in the store's
/// tree stays within the tree's 64 KiB path bound (format section 5.3).
pub const MAX_LOCK_KEY: usize = 16 * 1024;
pub const MAX_LOCK_USER: usize = 1024;
/// The longest lock value.
pub const MAX_LOCK_VALUE: usize = MAX_VALUE;
/// The most lock edits and tree edits one change carries.
pub const MAX_LOCK_EDITS: usize = 1 << 20;
pub const MAX_EDITS: usize = 1 << 20;

/// A file's flag: it is hosted read-only.
pub const READ_ONLY: u32 = 1;
/// Every flag this build knows.
pub const KNOWN_FLAGS: u32 = READ_ONLY;

/// The kind of an entry, as the log's kind field holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Change = 1,
    Session = 2,
    File = 3,
    Members = 4,
    Mark = 5,
    Empty = 6,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Change,
        Kind::Session,
        Kind::File,
        Kind::Members,
        Kind::Mark,
        Kind::Empty,
    ];

    pub fn code(self) -> u16 {
        self as u16
    }

    pub fn from_code(code: u16) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.code() == code)
    }
}

/// A server's id: the 16 random bytes its `state` file holds
/// (`docs/server-storage.md` section 6.1).
pub type ServerId = [u8; 16];

/// A session (`docs/server-storage.md` section 7.3): a connection of one
/// start of one server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Session {
    pub server: ServerId,
    /// The server's start count when the connection was made.
    pub start: u64,
    /// The connection's number, counted upward within the start.
    pub connection: u64,
}

/// An entry as the log holds it, borrowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub index: u64,
    pub term: u64,
    /// The kind's code ([`Kind::code`]); a code this build does not know is
    /// refused when the entry is applied.
    pub kind: u16,
    /// The file the entry belongs to: the id of a file, which is the index
    /// of the `file` entry that added it, or 0 for none (format section
    /// 5.1).
    pub file: u64,
    pub payload: &'a [u8],
    /// The entry's hash, as the log holds it: [`entry_hash`] over the
    /// previous entry's hash and this entry.
    pub hash: Hash,
}

/// The hash of an entry after the entry whose hash is `prev`: SHA-256 over
/// `prev` and the entry's bytes as the log writes them before the hash
/// (`docs/server-storage-format.md` section 3.4): index, term and file as
/// 8 bytes each, the kind as 2, two zero bytes, the payload's length as 4,
/// then the payload, integers little-endian. Before entry 1, `prev` is 32
/// zero bytes. A payload longer than [`MAX_PAYLOAD`] has no hash the log
/// could hold; its length is saturated here, and such an entry is refused.
pub fn entry_hash(
    prev: &Hash,
    index: u64,
    term: u64,
    kind: u16,
    file: u64,
    payload: &[u8],
) -> Hash {
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(&prev.0);
    ctx.update(&index.to_le_bytes());
    ctx.update(&term.to_le_bytes());
    ctx.update(&file.to_le_bytes());
    ctx.update(&kind.to_le_bytes());
    ctx.update(&[0, 0]);
    ctx.update(&len.to_le_bytes());
    ctx.update(payload);
    let mut out = [0u8; 32];
    out.copy_from_slice(ctx.finish().as_ref());
    Hash(out)
}

/// What an entry holds, by kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    Change(Change),
    Session(SessionEvent),
    File(FileEvent),
    Members(Members),
    Mark(Mark),
    Empty,
}

/// One write operation's effects on one file (the entry's file).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// The operation's id: its session and sequence number. None for work
    /// no session asked for.
    pub op: Option<Op>,
    /// The operation's reply, which the session's row keeps. Empty when
    /// there is no `op`.
    pub reply: Vec<u8>,
    /// The notices for other connections, delivered in order once the entry
    /// is published. This crate does not interpret them.
    pub notices: Vec<Vec<u8>>,
    /// Changes to the lock table, in order, all of the entry's file.
    pub locks: Vec<LockEdit>,
    /// Edits of the file's tree, in order.
    pub edits: Vec<Edit>,
}

/// An operation's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Op {
    pub session: Session,
    /// The session's sequence number for the operation: one more than the
    /// last it completed (the first is 1).
    pub seq: u64,
}

/// A change to the lock table, for the entry's file. The key, the user
/// within the session and the value (mode, count, the holder's name) are
/// bytes this crate does not interpret; the layer above judges conflicts
/// before it appends the entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockEdit {
    /// The lock is held with this value: added, or its value replaced.
    Set {
        session: Session,
        key: Vec<u8>,
        user: Vec<u8>,
        value: Vec<u8>,
    },
    /// The lock, which is held, is released.
    Remove {
        session: Session,
        key: Vec<u8>,
        user: Vec<u8>,
    },
    /// Every lock the session holds on the file is released (a logout from
    /// the file, or its close, by a session that stays open).
    ReleaseFile { session: Session },
}

impl LockEdit {
    pub fn session(&self) -> &Session {
        match self {
            LockEdit::Set { session, .. }
            | LockEdit::Remove { session, .. }
            | LockEdit::ReleaseFile { session } => session,
        }
    }
}

/// A `session` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    Opened(Session),
    Closed(Session),
    /// A server started with this start count.
    Started {
        server: ServerId,
        count: u64,
    },
}

/// A `file` entry. The file is the entry's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileEvent {
    /// A file is added, empty, with the entry's index as its id.
    Added {
        name: Vec<u8>,
        flags: u32,
        /// The file's page 0, as imported (`docs/server-storage.md`
        /// section 4).
        page0: Vec<u8>,
    },
    Removed,
    Renamed {
        name: Vec<u8>,
    },
}

/// A `members` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Members {
    /// The tree format version every server applies entries with from this
    /// entry on.
    FormatVersion(u8),
}

/// A `mark` entry: a named point in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub purpose: Purpose,
    pub name: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    RestorePoint = 1,
    Release = 2,
    Comparison = 3,
}

/// What the formats leave room for and this build refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// A change that is part of a group (`docs/server-storage.md`
    /// section 6.9).
    Group,
    /// A file made as a clone of another (section 6.8).
    Clone,
    /// The cluster's members (section 7.6).
    MembersList,
}

/// Why a payload cannot be decoded or encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayloadError {
    /// The entry's kind is not one this build knows.
    UnknownKind(u16),
    /// The payload's format version is not one this build reads.
    Version(u8),
    /// The payload ends before its last field.
    Short,
    /// Bytes follow the last field.
    Trailing(usize),
    /// A tag (an event, an edit, a purpose) this build does not know.
    Tag { field: &'static str, tag: u8 },
    /// A length beyond its bound.
    TooLong {
        field: &'static str,
        len: usize,
        max: usize,
    },
    /// A count beyond its bound.
    TooMany {
        field: &'static str,
        count: usize,
        max: usize,
    },
    /// A name that is empty.
    EmptyName,
    /// Flags this build does not know.
    Flags(u32),
    /// A reply without an operation to answer.
    ReplyWithoutOp,
    /// Tree edit number `edit` is beyond the tree's bounds
    /// (`docs/server-storage-format.md` section 2.1).
    Edit { edit: usize, why: Refusal },
    /// Something the format leaves room for and this build refuses.
    Unsupported(Feature),
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PayloadError::UnknownKind(k) => write!(f, "entry kind {k} is not one this build knows"),
            PayloadError::Version(v) => {
                write!(f, "payload format version {v} is not one this build reads")
            }
            PayloadError::Short => f.write_str("the payload ends early"),
            PayloadError::Trailing(n) => write!(f, "{n} bytes follow the payload's last field"),
            PayloadError::Tag { field, tag } => {
                write!(f, "{field} {tag} is not one this build knows")
            }
            PayloadError::TooLong { field, len, max } => {
                write!(f, "{field} of {len} bytes is longer than {max}")
            }
            PayloadError::TooMany { field, count, max } => {
                write!(f, "{count} {field} are more than {max}")
            }
            PayloadError::EmptyName => f.write_str("an empty name"),
            PayloadError::Flags(flags) => write!(f, "file flags {flags:#x} are not all known"),
            PayloadError::ReplyWithoutOp => f.write_str("a reply without an operation"),
            PayloadError::Edit { edit, why } => write!(f, "edit {edit} refused: {why:?}"),
            PayloadError::Unsupported(what) => write!(f, "{what:?} is not supported yet"),
        }
    }
}

impl std::error::Error for PayloadError {}

impl Payload {
    pub fn kind(&self) -> Kind {
        match self {
            Payload::Change(_) => Kind::Change,
            Payload::Session(_) => Kind::Session,
            Payload::File(_) => Kind::File,
            Payload::Members(_) => Kind::Members,
            Payload::Mark(_) => Kind::Mark,
            Payload::Empty => Kind::Empty,
        }
    }

    /// The payload of an entry of kind `kind`.
    pub fn decode(kind: u16, bytes: &[u8]) -> Result<Payload, PayloadError> {
        let kind = Kind::from_code(kind).ok_or(PayloadError::UnknownKind(kind))?;
        if bytes.len() > MAX_PAYLOAD {
            return Err(PayloadError::TooLong {
                field: "payload",
                len: bytes.len(),
                max: MAX_PAYLOAD,
            });
        }
        let mut r = Reader { bytes, at: 0 };
        if kind == Kind::Empty {
            r.done()?;
            return Ok(Payload::Empty);
        }
        let version = r.u8()?;
        if version != VERSION {
            return Err(PayloadError::Version(version));
        }
        let payload = match kind {
            Kind::Change => Payload::Change(decode_change(&mut r)?),
            Kind::Session => Payload::Session(decode_session(&mut r)?),
            Kind::File => Payload::File(decode_file(&mut r)?),
            Kind::Members => Payload::Members(decode_members(&mut r)?),
            Kind::Mark => Payload::Mark(decode_mark(&mut r)?),
            Kind::Empty => Payload::Empty,
        };
        r.done()?;
        Ok(payload)
    }

    /// The payload's bytes, refused when anything is beyond the bounds the
    /// decoder holds it to. Every bound of every kind, and the payload's
    /// whole size, are checked before anything is written ([`Payload::size`]),
    /// so a payload past a bound is refused without building any of it.
    pub fn encode(&self) -> Result<Vec<u8>, PayloadError> {
        let len = self.size()?;
        if len > MAX_PAYLOAD {
            return Err(PayloadError::TooLong {
                field: "payload",
                len,
                max: MAX_PAYLOAD,
            });
        }
        let mut w = Vec::with_capacity(len);
        match self {
            Payload::Empty => {}
            Payload::Change(c) => {
                w.push(VERSION);
                encode_change(&mut w, c)?;
            }
            Payload::Session(event) => {
                w.push(VERSION);
                match event {
                    SessionEvent::Opened(session) => {
                        w.push(1);
                        put_session(&mut w, session);
                    }
                    SessionEvent::Closed(session) => {
                        w.push(2);
                        put_session(&mut w, session);
                    }
                    SessionEvent::Started { server, count } => {
                        w.push(3);
                        w.extend_from_slice(server);
                        w.extend_from_slice(&count.to_le_bytes());
                    }
                }
            }
            Payload::File(event) => {
                w.push(VERSION);
                match event {
                    FileEvent::Added { name, flags, page0 } => {
                        w.push(1);
                        put_name(&mut w, name)?;
                        w.extend_from_slice(&flags.to_le_bytes());
                        put16(&mut w, "page 0", page0, MAX_PAGE0)?;
                    }
                    FileEvent::Removed => w.push(2),
                    FileEvent::Renamed { name } => {
                        w.push(3);
                        put_name(&mut w, name)?;
                    }
                }
            }
            Payload::Members(Members::FormatVersion(v)) => {
                w.extend_from_slice(&[VERSION, 1, *v]);
            }
            Payload::Mark(mark) => {
                w.extend_from_slice(&[VERSION, mark.purpose as u8]);
                put_name(&mut w, &mark.name)?;
            }
        }
        debug_assert_eq!(w.len(), len, "size and encode agree");
        Ok(w)
    }

    /// The bytes [`Payload::encode`] writes, with every field and count
    /// checked against its bound, in the order the fields are written.
    pub fn size(&self) -> Result<usize, PayloadError> {
        fn name(name: &[u8]) -> Result<usize, PayloadError> {
            if name.is_empty() {
                return Err(PayloadError::EmptyName);
            }
            if name.len() > MAX_NAME {
                return Err(PayloadError::TooLong {
                    field: "name",
                    len: name.len(),
                    max: MAX_NAME,
                });
            }
            Ok(2 + name.len())
        }
        Ok(match self {
            Payload::Empty => 0,
            Payload::Change(c) => change_size(c)?.saturating_add(1),
            Payload::Session(SessionEvent::Opened(_) | SessionEvent::Closed(_)) => 2 + 32,
            Payload::Session(SessionEvent::Started { .. }) => 2 + 16 + 8,
            Payload::File(FileEvent::Added {
                name: n,
                flags,
                page0,
            }) => {
                let n = name(n)?;
                if flags & !KNOWN_FLAGS != 0 {
                    return Err(PayloadError::Flags(*flags));
                }
                if page0.len() > MAX_PAGE0 {
                    return Err(PayloadError::TooLong {
                        field: "page 0",
                        len: page0.len(),
                        max: MAX_PAGE0,
                    });
                }
                2 + n + 4 + 2 + page0.len()
            }
            Payload::File(FileEvent::Removed) => 2,
            Payload::File(FileEvent::Renamed { name: n }) => 2 + name(n)?,
            Payload::Members(_) => 3,
            Payload::Mark(mark) => 2 + name(&mark.name)?,
        })
    }
}

/// Reads fields, each checked against what is left before it is taken.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], PayloadError> {
        let rest = &self.bytes[self.at..];
        if rest.len() < n {
            return Err(PayloadError::Short);
        }
        self.at += n;
        Ok(&rest[..n])
    }

    fn u8(&mut self) -> Result<u8, PayloadError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PayloadError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, PayloadError> {
        let mut b = [0u8; 4];
        b.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Result<u64, PayloadError> {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(b))
    }

    /// Bytes with a 2-byte length, at most `max`.
    fn bytes16(&mut self, field: &'static str, max: usize) -> Result<Vec<u8>, PayloadError> {
        let len = usize::from(self.u16()?);
        if len > max {
            return Err(PayloadError::TooLong { field, len, max });
        }
        Ok(self.take(len)?.to_vec())
    }

    /// Bytes with a 4-byte length, at most `max`.
    fn bytes32(&mut self, field: &'static str, max: usize) -> Result<Vec<u8>, PayloadError> {
        let len = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if len > max {
            return Err(PayloadError::TooLong { field, len, max });
        }
        Ok(self.take(len)?.to_vec())
    }

    /// A 4-byte count, at most `max`. It is never used to reserve memory:
    /// each element is checked against what is left as it is read.
    fn count(&mut self, field: &'static str, max: usize) -> Result<usize, PayloadError> {
        let count = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        if count > max {
            return Err(PayloadError::TooMany { field, count, max });
        }
        Ok(count)
    }

    fn session(&mut self) -> Result<Session, PayloadError> {
        let mut server = [0u8; 16];
        server.copy_from_slice(self.take(16)?);
        Ok(Session {
            server,
            start: self.u64()?,
            connection: self.u64()?,
        })
    }

    fn name(&mut self) -> Result<Vec<u8>, PayloadError> {
        let name = self.bytes16("name", MAX_NAME)?;
        if name.is_empty() {
            return Err(PayloadError::EmptyName);
        }
        Ok(name)
    }

    fn done(&self) -> Result<(), PayloadError> {
        match self.bytes.len() - self.at {
            0 => Ok(()),
            n => Err(PayloadError::Trailing(n)),
        }
    }
}

fn decode_change(r: &mut Reader<'_>) -> Result<Change, PayloadError> {
    if r.u64()? != 0 {
        return Err(PayloadError::Unsupported(Feature::Group));
    }
    let op = match r.u8()? {
        0 => None,
        1 => Some(Op {
            session: r.session()?,
            seq: r.u64()?,
        }),
        tag => return Err(PayloadError::Tag { field: "op", tag }),
    };
    let reply = r.bytes32("reply", MAX_REPLY)?;
    if op.is_none() && !reply.is_empty() {
        return Err(PayloadError::ReplyWithoutOp);
    }
    let mut notices = Vec::new();
    for _ in 0..r.count("notices", MAX_NOTICES)? {
        notices.push(r.bytes32("notice", MAX_NOTICE)?);
    }
    let mut locks = Vec::new();
    for _ in 0..r.count("lock edits", MAX_LOCK_EDITS)? {
        let tag = r.u8()?;
        let session = r.session()?;
        locks.push(match tag {
            1 => LockEdit::Set {
                session,
                key: r.bytes16("lock key", MAX_LOCK_KEY)?,
                user: r.bytes16("lock user", MAX_LOCK_USER)?,
                value: r.bytes32("lock value", MAX_LOCK_VALUE)?,
            },
            2 => LockEdit::Remove {
                session,
                key: r.bytes16("lock key", MAX_LOCK_KEY)?,
                user: r.bytes16("lock user", MAX_LOCK_USER)?,
            },
            3 => LockEdit::ReleaseFile { session },
            tag => {
                return Err(PayloadError::Tag {
                    field: "lock edit",
                    tag,
                })
            }
        });
    }
    let mut edits = Vec::new();
    for i in 0..r.count("edits", MAX_EDITS)? {
        let tag = r.u8()?;
        if !(1..=4).contains(&tag) {
            return Err(PayloadError::Tag { field: "edit", tag });
        }
        let depth = usize::from(r.u8()?);
        if depth > MAX_DEPTH {
            return Err(PayloadError::Edit {
                edit: i,
                why: Refusal::TooDeep(depth),
            });
        }
        let mut path = Vec::new();
        for _ in 0..depth {
            path.push(r.bytes16("segment", usize::from(u16::MAX))?);
        }
        let value = |r: &mut Reader<'_>| -> Result<Vec<u8>, PayloadError> {
            let len = usize::try_from(r.u32()?).unwrap_or(usize::MAX);
            if len > MAX_VALUE {
                return Err(PayloadError::Edit {
                    edit: i,
                    why: Refusal::ValueTooLong(len),
                });
            }
            Ok(r.take(len)?.to_vec())
        };
        edits.push(match tag {
            1 => Edit::list(path),
            2 => Edit::leaf(path, value(r)?),
            3 => Edit::Put {
                path,
                node: Node::Both(value(r)?),
            },
            _ => Edit::delete(path),
        });
    }
    check_edits(&edits)?;
    Ok(Change {
        op,
        reply,
        notices,
        locks,
        edits,
    })
}

/// The tree's bounds, as a payload error.
fn check_edits(edits: &[Edit]) -> Result<(), PayloadError> {
    match tree::check_edits(edits) {
        Ok(()) => Ok(()),
        Err(tree::Error::Refused { edit, why }) => Err(PayloadError::Edit { edit, why }),
        // `check_edits` refuses and does nothing else.
        Err(_) => Err(PayloadError::Short),
    }
}

fn decode_session(r: &mut Reader<'_>) -> Result<SessionEvent, PayloadError> {
    Ok(match r.u8()? {
        1 => SessionEvent::Opened(r.session()?),
        2 => SessionEvent::Closed(r.session()?),
        3 => {
            let mut server = [0u8; 16];
            server.copy_from_slice(r.take(16)?);
            SessionEvent::Started {
                server,
                count: r.u64()?,
            }
        }
        tag => {
            return Err(PayloadError::Tag {
                field: "session event",
                tag,
            })
        }
    })
}

fn decode_file(r: &mut Reader<'_>) -> Result<FileEvent, PayloadError> {
    Ok(match r.u8()? {
        1 => {
            let name = r.name()?;
            let flags = r.u32()?;
            if flags & !KNOWN_FLAGS != 0 {
                return Err(PayloadError::Flags(flags));
            }
            FileEvent::Added {
                name,
                flags,
                page0: r.bytes16("page 0", MAX_PAGE0)?,
            }
        }
        2 => FileEvent::Removed,
        3 => FileEvent::Renamed { name: r.name()? },
        4 => return Err(PayloadError::Unsupported(Feature::Clone)),
        tag => {
            return Err(PayloadError::Tag {
                field: "file event",
                tag,
            })
        }
    })
}

fn decode_members(r: &mut Reader<'_>) -> Result<Members, PayloadError> {
    Ok(match r.u8()? {
        1 => Members::FormatVersion(r.u8()?),
        2 => return Err(PayloadError::Unsupported(Feature::MembersList)),
        tag => {
            return Err(PayloadError::Tag {
                field: "members event",
                tag,
            })
        }
    })
}

fn decode_mark(r: &mut Reader<'_>) -> Result<Mark, PayloadError> {
    let purpose = match r.u8()? {
        1 => Purpose::RestorePoint,
        2 => Purpose::Release,
        3 => Purpose::Comparison,
        tag => {
            return Err(PayloadError::Tag {
                field: "mark purpose",
                tag,
            })
        }
    };
    Ok(Mark {
        purpose,
        name: r.name()?,
    })
}

fn put_session(w: &mut Vec<u8>, s: &Session) {
    w.extend_from_slice(&s.server);
    w.extend_from_slice(&s.start.to_le_bytes());
    w.extend_from_slice(&s.connection.to_le_bytes());
}

fn put16(
    w: &mut Vec<u8>,
    field: &'static str,
    bytes: &[u8],
    max: usize,
) -> Result<(), PayloadError> {
    let len = bytes.len();
    if len > max || len > usize::from(u16::MAX) {
        return Err(PayloadError::TooLong { field, len, max });
    }
    w.extend_from_slice(&(len as u16).to_le_bytes());
    w.extend_from_slice(bytes);
    Ok(())
}

fn put32(
    w: &mut Vec<u8>,
    field: &'static str,
    bytes: &[u8],
    max: usize,
) -> Result<(), PayloadError> {
    let len = bytes.len();
    let Some(n) = u32::try_from(len).ok().filter(|_| len <= max) else {
        return Err(PayloadError::TooLong { field, len, max });
    };
    w.extend_from_slice(&n.to_le_bytes());
    w.extend_from_slice(bytes);
    Ok(())
}

fn put_count(
    w: &mut Vec<u8>,
    field: &'static str,
    count: usize,
    max: usize,
) -> Result<(), PayloadError> {
    if count > max {
        return Err(PayloadError::TooMany { field, count, max });
    }
    // `max` fits in 4 bytes.
    w.extend_from_slice(&(count as u32).to_le_bytes());
    Ok(())
}

fn put_name(w: &mut Vec<u8>, name: &[u8]) -> Result<(), PayloadError> {
    if name.is_empty() {
        return Err(PayloadError::EmptyName);
    }
    put16(w, "name", name, MAX_NAME)
}

/// The bytes a change encodes to after the version byte, with every field
/// and count checked against its bound: what [`encode_change`] writes.
fn change_size(c: &Change) -> Result<usize, PayloadError> {
    fn bounded(field: &'static str, len: usize, max: usize) -> Result<usize, PayloadError> {
        if len > max {
            return Err(PayloadError::TooLong { field, len, max });
        }
        Ok(len)
    }
    fn counted(field: &'static str, count: usize, max: usize) -> Result<(), PayloadError> {
        if count > max {
            return Err(PayloadError::TooMany { field, count, max });
        }
        Ok(())
    }
    if c.op.is_none() && !c.reply.is_empty() {
        return Err(PayloadError::ReplyWithoutOp);
    }
    // Group, op tag and op, reply, and the three counts.
    let mut n: usize = 8 + 1 + if c.op.is_some() { 40 } else { 0 } + 4 + 12;
    let add = |n: usize, more: usize| n.saturating_add(more);
    n = add(n, bounded("reply", c.reply.len(), MAX_REPLY)?);
    counted("notices", c.notices.len(), MAX_NOTICES)?;
    for notice in &c.notices {
        n = add(n, 4 + bounded("notice", notice.len(), MAX_NOTICE)?);
    }
    counted("lock edits", c.locks.len(), MAX_LOCK_EDITS)?;
    for edit in &c.locks {
        n = add(n, 1 + 32);
        if let LockEdit::Set { key, user, .. } | LockEdit::Remove { key, user, .. } = edit {
            n = add(n, 2 + bounded("lock key", key.len(), MAX_LOCK_KEY)?);
            n = add(n, 2 + bounded("lock user", user.len(), MAX_LOCK_USER)?);
        }
        if let LockEdit::Set { value, .. } = edit {
            n = add(n, 4 + bounded("lock value", value.len(), MAX_LOCK_VALUE)?);
        }
    }
    check_edits(&c.edits)?;
    counted("edits", c.edits.len(), MAX_EDITS)?;
    for edit in &c.edits {
        let (path, value) = match edit {
            Edit::Put { path, node } => (path, (!matches!(node, Node::List)).then(|| node.value())),
            Edit::Delete { path } => (path, None),
        };
        n = add(n, 2);
        for segment in path {
            n = add(n, 2 + segment.len());
        }
        if let Some(v) = value {
            n = add(n, 4 + v.len());
        }
    }
    Ok(n)
}

fn encode_change(w: &mut Vec<u8>, c: &Change) -> Result<(), PayloadError> {
    // The group the change is part of: none. Section 5.4 reserves it.
    w.extend_from_slice(&0u64.to_le_bytes());
    match &c.op {
        None => w.push(0),
        Some(op) => {
            w.push(1);
            put_session(w, &op.session);
            w.extend_from_slice(&op.seq.to_le_bytes());
        }
    }
    if c.op.is_none() && !c.reply.is_empty() {
        return Err(PayloadError::ReplyWithoutOp);
    }
    put32(w, "reply", &c.reply, MAX_REPLY)?;
    put_count(w, "notices", c.notices.len(), MAX_NOTICES)?;
    for notice in &c.notices {
        put32(w, "notice", notice, MAX_NOTICE)?;
    }
    put_count(w, "lock edits", c.locks.len(), MAX_LOCK_EDITS)?;
    for edit in &c.locks {
        match edit {
            LockEdit::Set {
                session,
                key,
                user,
                value,
            } => {
                w.push(1);
                put_session(w, session);
                put16(w, "lock key", key, MAX_LOCK_KEY)?;
                put16(w, "lock user", user, MAX_LOCK_USER)?;
                put32(w, "lock value", value, MAX_LOCK_VALUE)?;
            }
            LockEdit::Remove { session, key, user } => {
                w.push(2);
                put_session(w, session);
                put16(w, "lock key", key, MAX_LOCK_KEY)?;
                put16(w, "lock user", user, MAX_LOCK_USER)?;
            }
            LockEdit::ReleaseFile { session } => {
                w.push(3);
                put_session(w, session);
            }
        }
    }
    check_edits(&c.edits)?;
    put_count(w, "edits", c.edits.len(), MAX_EDITS)?;
    for edit in &c.edits {
        let (tag, path, value) = match edit {
            Edit::Put {
                path,
                node: Node::List,
            } => (1, path, None),
            Edit::Put {
                path,
                node: Node::Value(v),
            } => (2, path, Some(v)),
            Edit::Put {
                path,
                node: Node::Both(v),
            } => (3, path, Some(v)),
            Edit::Delete { path } => (4, path, None),
        };
        w.push(tag);
        // `check_edits` holds the depth to 64 and each segment, inside a
        // path of at most 64 KiB, to 2 bytes of length.
        w.push(path.len() as u8);
        for segment in path {
            put16(w, "segment", segment, usize::from(u16::MAX))?;
        }
        if let Some(v) = value {
            put32(w, "value", v, MAX_VALUE)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash covers every field the log writes, in its order.
    #[test]
    fn the_hash_covers_every_field() {
        let prev = Hash([7; 32]);
        let base = entry_hash(&prev, 5, 2, 1, 9, b"abc");
        assert_ne!(base, entry_hash(&Hash([8; 32]), 5, 2, 1, 9, b"abc"));
        assert_ne!(base, entry_hash(&prev, 6, 2, 1, 9, b"abc"));
        assert_ne!(base, entry_hash(&prev, 5, 3, 1, 9, b"abc"));
        assert_ne!(base, entry_hash(&prev, 5, 2, 2, 9, b"abc"));
        assert_ne!(base, entry_hash(&prev, 5, 2, 1, 10, b"abc"));
        assert_ne!(base, entry_hash(&prev, 5, 2, 1, 9, b"abd"));
        // The bytes, spelled out.
        let mut bytes = prev.0.to_vec();
        bytes.extend_from_slice(&5u64.to_le_bytes());
        bytes.extend_from_slice(&2u64.to_le_bytes());
        bytes.extend_from_slice(&9u64.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"abc");
        assert_eq!(base, Hash::of(&bytes));
    }
}
