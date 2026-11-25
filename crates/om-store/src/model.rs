//! The reference model (`docs/server-storage.md` section 12, check 1): the
//! state as plain maps in memory, applying the same entries as
//! [`crate::apply`]. It is the second opinion on what an entry means, so it
//! shares no code with `apply` for that: only the payload's decoding, the
//! hash of an entry and the types of a refusal. Each file is a map of paths
//! to nodes, written from the rules of `docs/server-storage-format.md`
//! section 5.5 (which follow `om_hbam::OwnedNode`), not from the page
//! trees.
//!
//! An entry is checked whole before the model changes, in the order of
//! format section 5.6, so a refused entry leaves it as it was and names the
//! same refusal `apply` names. A change's lock edits are checked against the
//! edits before them kept by session, so a release touches only its own
//! session's, and [`Outcome::steps`] counts the work.

use std::collections::BTreeMap;

use crate::apply::{Position, Refusal, MAX_OPEN_GROUPS};
use crate::entry::{
    entry_hash, Entry, FileEvent, Group as GroupOf, GroupEvent, GroupPath, Kind, LockEdit, Members,
    Payload, ServerId, Session, SessionEvent, Step,
};
use crate::page::FORMAT_VERSION;
use crate::tree::Node;

pub type Path = Vec<Vec<u8>>;

/// A file: its row and its nodes. The root, the empty path, is not in the
/// map: it always exists and is always a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub name: Vec<u8>,
    pub flags: u32,
    pub page0: Vec<u8>,
    pub nodes: BTreeMap<Path, Node>,
}

/// A lock's identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LockId {
    pub session: Session,
    pub file: u64,
    pub key: Vec<u8>,
    pub user: Vec<u8>,
}

/// The state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Model {
    pub applied: Position,
    pub format: u8,
    pub files: BTreeMap<u64, File>,
    /// Each server's highest start count and the highest connection number
    /// opened in it.
    pub servers: BTreeMap<ServerId, (u64, u64)>,
    /// Each open session's last completed sequence number and its reply.
    pub sessions: BTreeMap<Session, (u64, Vec<u8>)>,
    pub locks: BTreeMap<LockId, Vec<u8>>,
    /// The open groups, by id.
    pub groups: BTreeMap<u64, Group>,
}

/// An open group: the paths it declared, and its parts' edits, each with
/// its file, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub paths: Vec<GroupPath>,
    pub staged: Vec<(u64, Vec<crate::tree::Edit>)>,
}

impl Group {
    /// The paths it declared in `file`.
    fn paths_in(&self, file: u64) -> impl Iterator<Item = &Path> {
        self.paths
            .iter()
            .filter(move |p| p.file == file)
            .map(|p| &p.path)
    }
}

/// Whether one path is at, above or below the other.
fn meets(a: &[Vec<u8>], b: &[Vec<u8>]) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// A change's lock edits so far, for one session: whether it released the
/// session's locks on the file, and the locks it set (true) or removed
/// (false), by key and user.
type Pending = (bool, BTreeMap<(Vec<u8>, Vec<u8>), bool>);

/// What an entry did besides changing the model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub opened: Option<Session>,
    pub closed: Vec<Session>,
    pub added: Option<u64>,
    pub removed: Option<u64>,
    /// The map entries a change's lock edits visited, checked and made:
    /// linear in its lock edits and the locks they release.
    pub steps: u64,
}

impl Default for Model {
    fn default() -> Self {
        Model::new()
    }
}

impl Model {
    /// The state before entry 1.
    pub fn new() -> Model {
        Model {
            applied: Position::START,
            format: 1,
            files: BTreeMap::new(),
            servers: BTreeMap::new(),
            sessions: BTreeMap::new(),
            locks: BTreeMap::new(),
            groups: BTreeMap::new(),
        }
    }

    /// A change's group checks (format section 5.6), in `apply`'s order.
    fn check_group(&self, entry: &Entry<'_>, change: &crate::entry::Change) -> Result<(), Refusal> {
        let file = entry.file;
        let own: Option<(u64, Vec<Path>)> = match &change.group {
            None => None,
            Some(GroupOf {
                id,
                step: Step::Open(paths),
            }) => {
                if *id != entry.index {
                    return Err(Refusal::GroupId {
                        id: *id,
                        index: entry.index,
                    });
                }
                if self.groups.len() as u64 >= MAX_OPEN_GROUPS {
                    return Err(Refusal::TooManyGroups);
                }
                if let Some(p) = paths.iter().find(|p| !self.files.contains_key(&p.file)) {
                    return Err(Refusal::NoSuchFile(p.file));
                }
                for p in paths {
                    for (other, group) in &self.groups {
                        if group.paths_in(p.file).any(|theirs| meets(&p.path, theirs)) {
                            return Err(Refusal::GroupOverlap { group: *other });
                        }
                    }
                }
                let mine = paths
                    .iter()
                    .filter(|p| p.file == file)
                    .map(|p| p.path.clone())
                    .collect();
                Some((*id, mine))
            }
            Some(GroupOf { id, .. }) => {
                let group = self.groups.get(id).ok_or(Refusal::NoSuchGroup(*id))?;
                Some((*id, group.paths_in(file).cloned().collect()))
            }
        };
        if let Some((id, mine)) = &own {
            if mine.is_empty() {
                return Err(Refusal::GroupFile { group: *id, file });
            }
        }
        for (i, edit) in change.edits.iter().enumerate() {
            let path = match edit {
                crate::tree::Edit::Put { path, .. } | crate::tree::Edit::Delete { path } => path,
            };
            if let Some((_, mine)) = &own {
                if !mine.iter().any(|d| path.starts_with(d)) {
                    return Err(Refusal::OutsideGroup { edit: i });
                }
            }
            for (other, group) in &self.groups {
                if own.as_ref().map(|(id, _)| id) == Some(other) {
                    continue;
                }
                if group.paths_in(file).any(|theirs| meets(path, theirs)) {
                    return Err(Refusal::InGroup {
                        group: *other,
                        edit: i,
                    });
                }
            }
        }
        Ok(())
    }

    /// Applies `entry`, or says why it cannot be right and changes nothing.
    pub fn apply(&mut self, entry: &Entry<'_>) -> Result<Outcome, Refusal> {
        let next = self.applied.index.checked_add(1);
        if next != Some(entry.index) {
            return Err(Refusal::NotNext {
                last: self.applied.index,
                index: entry.index,
            });
        }
        if entry.term < self.applied.term {
            return Err(Refusal::TermBehind {
                last: self.applied.term,
                term: entry.term,
            });
        }
        let hash = entry_hash(
            &self.applied.hash,
            entry.index,
            entry.term,
            entry.kind,
            entry.file,
            entry.payload,
        );
        if hash != entry.hash {
            return Err(Refusal::Chain {
                expected: hash,
                found: entry.hash,
            });
        }
        if !(1..=FORMAT_VERSION).contains(&self.format) {
            return Err(Refusal::UnknownFormatVersion(self.format));
        }
        let payload = Payload::decode(entry.kind, entry.payload).map_err(Refusal::Payload)?;
        let outcome = self.effect(entry, &payload)?;
        self.applied = Position {
            index: entry.index,
            term: entry.term,
            hash: entry.hash,
        };
        Ok(outcome)
    }

    fn no_file_field(entry: &Entry<'_>, kind: Kind) -> Result<(), Refusal> {
        if entry.file == 0 {
            Ok(())
        } else {
            Err(Refusal::FileField {
                kind,
                file: entry.file,
            })
        }
    }

    fn the_file(&self, entry: &Entry<'_>, kind: Kind) -> Result<u64, Refusal> {
        if entry.file == 0 {
            return Err(Refusal::FileField { kind, file: 0 });
        }
        if !self.files.contains_key(&entry.file) {
            return Err(Refusal::NoSuchFile(entry.file));
        }
        Ok(entry.file)
    }

    fn name_free(&self, name: &[u8]) -> Result<(), Refusal> {
        if self.files.values().any(|f| f.name == name) {
            return Err(Refusal::NameTaken(name.to_vec()));
        }
        Ok(())
    }

    fn effect(&mut self, entry: &Entry<'_>, payload: &Payload) -> Result<Outcome, Refusal> {
        let mut out = Outcome::default();
        match payload {
            Payload::Change(change) => {
                let file = self.the_file(entry, Kind::Change)?;
                if let Some(op) = &change.op {
                    let (last, _) = self
                        .sessions
                        .get(&op.session)
                        .ok_or(Refusal::SessionNotOpen(op.session))?;
                    if op.seq != last.wrapping_add(1) || *last == u64::MAX {
                        return Err(Refusal::Sequence {
                            session: op.session,
                            last: *last,
                            seq: op.seq,
                        });
                    }
                }
                // Every lock edit is checked first, against the lock table
                // and the edits before it, which are kept by session: a
                // release forgets only its own session's.
                let mut pending: BTreeMap<Session, Pending> = BTreeMap::new();
                for (i, edit) in change.locks.iter().enumerate() {
                    out.steps += 1;
                    if !self.sessions.contains_key(edit.session()) {
                        return Err(Refusal::SessionNotOpen(*edit.session()));
                    }
                    let (released, made) = pending.entry(*edit.session()).or_default();
                    match edit {
                        LockEdit::Set { key, user, .. } => {
                            made.insert((key.clone(), user.clone()), true);
                        }
                        LockEdit::Remove { session, key, user } => {
                            let mine = (key.clone(), user.clone());
                            let held = match made.get(&mine) {
                                Some(&held) => held,
                                None => {
                                    !*released
                                        && self.locks.contains_key(&LockId {
                                            session: *session,
                                            file,
                                            key: key.clone(),
                                            user: user.clone(),
                                        })
                                }
                            };
                            if !held {
                                return Err(Refusal::NoSuchLock { edit: i });
                            }
                            made.insert(mine, false);
                        }
                        LockEdit::ReleaseFile { .. } => {
                            out.steps += made.len() as u64;
                            made.clear();
                            *released = true;
                        }
                    }
                }
                self.check_group(entry, change)?;
                // Then they are made, in order.
                for edit in &change.locks {
                    out.steps += 1;
                    match edit {
                        LockEdit::Set {
                            session,
                            key,
                            user,
                            value,
                        } => {
                            let id = LockId {
                                session: *session,
                                file,
                                key: key.clone(),
                                user: user.clone(),
                            };
                            self.locks.insert(id, value.clone());
                        }
                        LockEdit::Remove { session, key, user } => {
                            self.locks.remove(&LockId {
                                session: *session,
                                file,
                                key: key.clone(),
                                user: user.clone(),
                            });
                        }
                        LockEdit::ReleaseFile { session } => {
                            // The session's locks on the file sort together:
                            // the first of them is removed until none is
                            // left, one id held at a time.
                            let from = LockId {
                                session: *session,
                                file,
                                key: Vec::new(),
                                user: Vec::new(),
                            };
                            while let Some(id) = self
                                .locks
                                .range(&from..)
                                .next()
                                .map(|(id, _)| id)
                                .filter(|id| id.session == *session && id.file == file)
                                .cloned()
                            {
                                self.locks.remove(&id);
                                out.steps += 1;
                            }
                        }
                    }
                }
                if let Some(op) = &change.op {
                    self.sessions
                        .insert(op.session, (op.seq, change.reply.clone()));
                }
                // A part's edits are staged; a close applies the staged
                // edits, in order; other edits go to the file at once.
                let mut apply: Vec<(u64, Vec<crate::tree::Edit>)> = Vec::new();
                match &change.group {
                    None => apply.push((file, change.edits.clone())),
                    Some(GroupOf {
                        id,
                        step: Step::Open(paths),
                    }) => {
                        self.groups.insert(
                            *id,
                            Group {
                                paths: paths.clone(),
                                staged: vec![(file, change.edits.clone())],
                            },
                        );
                    }
                    Some(GroupOf {
                        id,
                        step: Step::Part,
                    }) => {
                        if let Some(group) = self.groups.get_mut(id) {
                            group.staged.push((file, change.edits.clone()));
                        }
                    }
                    Some(GroupOf {
                        id,
                        step: Step::Close,
                    }) => {
                        if let Some(group) = self.groups.remove(id) {
                            apply = group.staged;
                        }
                    }
                }
                for (file, edits) in apply {
                    let nodes = &mut self
                        .files
                        .get_mut(&file)
                        .ok_or(Refusal::NoSuchFile(file))?
                        .nodes;
                    for edit in &edits {
                        match edit {
                            crate::tree::Edit::Put { path, node } => put(nodes, path, node.clone()),
                            crate::tree::Edit::Delete { path } => delete(nodes, path),
                        }
                    }
                }
            }
            Payload::Session(event) => {
                Self::no_file_field(entry, Kind::Session)?;
                match event {
                    SessionEvent::Opened(s) => {
                        if let Some((start, highest)) = self.servers.get_mut(&s.server) {
                            if *start == s.start && s.connection > *highest {
                                *highest = s.connection;
                                self.sessions.insert(*s, (0, Vec::new()));
                                out.opened = Some(*s);
                            }
                        }
                    }
                    SessionEvent::Closed(s) => {
                        if self.sessions.remove(s).is_some() {
                            self.locks.retain(|id, _| id.session != *s);
                            out.closed.push(*s);
                        }
                    }
                    SessionEvent::Started { server, count } => {
                        // Format 5.6: applied when the count is above the
                        // server's recorded count, or the server has no row.
                        let applies = match self.servers.get(server) {
                            None => true,
                            Some((recorded, _)) => count > recorded,
                        };
                        if applies {
                            out.closed = self
                                .sessions
                                .keys()
                                .filter(|s| s.server == *server)
                                .copied()
                                .collect();
                            for s in &out.closed {
                                self.sessions.remove(s);
                            }
                            self.locks.retain(|id, _| id.session.server != *server);
                            self.servers.insert(*server, (*count, 0));
                        }
                    }
                }
            }
            Payload::File(event) => match event {
                FileEvent::Added { name, flags, page0 } => {
                    if entry.file != entry.index {
                        return Err(Refusal::FileField {
                            kind: Kind::File,
                            file: entry.file,
                        });
                    }
                    self.name_free(name)?;
                    self.files.insert(
                        entry.index,
                        File {
                            name: name.clone(),
                            flags: *flags,
                            page0: page0.clone(),
                            nodes: BTreeMap::new(),
                        },
                    );
                    out.added = Some(entry.index);
                }
                FileEvent::Removed => {
                    let id = self.the_file(entry, Kind::File)?;
                    if let Some((group, _)) = self
                        .groups
                        .iter()
                        .find(|(_, g)| g.paths.iter().any(|p| p.file == id))
                    {
                        return Err(Refusal::FileInGroup { group: *group });
                    }
                    self.files.remove(&id);
                    self.locks.retain(|lock, _| lock.file != id);
                    out.removed = Some(id);
                }
                FileEvent::Renamed { name } => {
                    let id = self.the_file(entry, Kind::File)?;
                    self.name_free(name)?;
                    if let Some(file) = self.files.get_mut(&id) {
                        file.name = name.clone();
                    }
                }
            },
            Payload::Members(Members::FormatVersion(v)) => {
                Self::no_file_field(entry, Kind::Members)?;
                if !(1..=FORMAT_VERSION).contains(v) {
                    return Err(Refusal::UnknownFormatVersion(*v));
                }
                if *v < self.format {
                    return Err(Refusal::FormatVersionLowered {
                        current: self.format,
                        version: *v,
                    });
                }
                self.format = *v;
            }
            Payload::Mark(_) => {
                if entry.file != 0 {
                    self.the_file(entry, Kind::Mark)?;
                }
            }
            Payload::Empty => {
                Self::no_file_field(entry, Kind::Empty)?;
                self.groups.clear();
            }
            Payload::Group(GroupEvent::Discard { id }) => {
                Self::no_file_field(entry, Kind::Group)?;
                if self.groups.remove(id).is_none() {
                    return Err(Refusal::NoSuchGroup(*id));
                }
            }
        }
        Ok(out)
    }
}

/// The node at `path` becomes `node`, alone: what was under it goes. The
/// ancestors are made as `om_hbam::OwnedNode::ensure` makes them: a missing
/// one is an empty list, one that holds a value becomes a list that keeps
/// its value, and one that holds an empty value an empty list.
pub fn put(nodes: &mut BTreeMap<Path, Node>, path: &[Vec<u8>], node: Node) {
    for depth in 1..path.len() {
        let above = &path[..depth];
        match nodes.get(above) {
            None => {
                nodes.insert(above.to_vec(), Node::List);
            }
            Some(Node::Value(v)) if v.is_empty() => {
                nodes.insert(above.to_vec(), Node::List);
            }
            Some(Node::Value(v)) => {
                let v = v.clone();
                nodes.insert(above.to_vec(), Node::Both(v));
            }
            Some(_) => {}
        }
    }
    delete(nodes, path);
    nodes.insert(path.to_vec(), node);
}

/// The node at `path` and everything under it go; the empty path takes
/// every node.
pub fn delete(nodes: &mut BTreeMap<Path, Node>, path: &[Vec<u8>]) {
    // A list of byte strings sorts as the key tree: a node right before
    // everything under it.
    let mut below = nodes.split_off(path);
    let rest = match below.iter().find(|(p, _)| !p.starts_with(path)) {
        Some((p, _)) => {
            let p = p.clone();
            below.split_off(&p)
        }
        None => BTreeMap::new(),
    };
    nodes.extend(rest);
}
