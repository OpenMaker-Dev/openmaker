# OpenMaker Server storage

How OpenMaker Server keeps hosted files: its two storage engines, the
interface between them and the rest of the server, and what is built on the
store engine (replication, design history and deploys, SQL). All of it is
OpenMaker's own design. FileMaker Server works differently inside
([hbam-writer.md](re/hbam-writer.md) 6.1), and nothing here is compared with
it except the contract in section 2.

**Status: design.** Only the file engine exists
(`crates/om-server/src/store.rs`). The engine interface, the store engine
and everything in sections 5 to 12 are specified here and not built.
[#673](https://github.com/OpenMaker-Dev/openmaker/issues/673) lists the
work. A section is rewritten in place when its part is built.

## 1. Why a second engine

The file engine loads each hosted `.fmp12` whole into memory as its key
tree and writes every change back as a complete new file, renamed over the
old one, while it holds the file's write lock (`Store::save`,
`Hosted::save_held`). That is simple and never leaves a half-written file.
Its costs grow with the file:

- a commit costs a rebuild and a flush of the whole file, and every other
  client of the file waits for it;
- a hosted file takes about three times its size in memory;
- the only thing to replicate, back up or keep as history is a whole file.

The measurements are in
[#673](https://github.com/OpenMaker-Dev/openmaker/issues/673). They are
of synthetic files (one table, eight 64-byte text cells per record, no
indexes), with the save timed outside the server; the factor of three is
the tree held in memory against the file's size, not the peak during a
save, which is higher. The store engine removes these costs. The file
engine stays: it serves local files in the desktop client
(`om_server::LocalHost`, [architecture.md](architecture.md), "Local files
use the hosted path"), it is the default until the maintainer switches it,
and the store engine is tested against it (section 12).

## 2. The contract

Both engines keep these, and a difference between them is a bug:

1. **The wire.** For the same sequence of requests, a client gets the same
   bytes: downloads, find results, record ids, serial numbers, modification
   counts, lock grants and refusals with their error codes, and
   notifications. FileMaker Pro cannot tell which engine it talks to. This
   covers requests that succeed and refusals the protocol defines. What a
   client sees when storage itself fails is specified in section 5.
2. **Always an `.fmp12`.** A hosted file can be exported at any time as an
   `.fmp12` that FileMaker opens. It holds exactly the file's key tree:
   every node with its kind and value, empty lists included. Its page 0 is
   the imported one except for the fields a writer maintains, the
   clean-close flag and the change number
   ([hbam-writer.md](re/hbam-writer.md) 5.1). Container data FileMaker
   keeps in files beside the `.fmp12` is exported beside it (section 8).
3. **Nothing dropped.** Keys and values the server does not understand are
   kept and exported as they came.
4. **Stored means durable.** A reply that says a change is stored is sent
   only after the change is on disk (on a cluster: on a majority of
   servers). An operation has one of three outcomes: stored, and the reply
   says so; not stored, with nothing of it left, and the reply says so; or
   unknown to the client, when the server stopped or lost its place in the
   cluster before it could answer. In the third case the client gets no
   success reply, and after recovery the change is there whole or not at
   all.
5. **Explicit refusals.** What an engine cannot do fails with an error,
   never as a silent success (AGENTS.md rule 4).

Rule 4 is about what the engine does before it answers. A single server
with one disk cannot keep a change that the disk damages after confirming
it; backups and a cluster cover that (section 6.12).

What is OpenMaker's own and may differ from FileMaker Server: everything
on disk, how many servers hold a file, what the Admin Console and
`om-author` offer on top.

## 3. Overview

```text
 FileMaker Pro, OpenMaker clients       SQL clients        Admin Console, om-author
        | RCFMP12                           | Postgres protocol     | commands
 +------v-----------------------------------v-----------------------v---------+
 | Sessions: sign-in, privileges, staged uploads, paging        (om-server)   |
 | Reads from a view            Write operations, one at a time per file      |
 +----------------------------------------------------------------------------+
 | Engine interface (section 5)                                               |
 +-----------------------------+----------------------------------------------+
 | File engine                 | Store engine (om-store, section 6)           |
 | one .fmp12 per file,        | change log, copy-on-write page trees,        |
 | whole in memory,            | views, checkpoints, clones;                  |
 | whole-file save             | replicated by consensus (section 7)          |
 +-----------------------------+----------------------------------------------+
```

The engine is chosen for the whole server when it starts (section 11).

## 4. The data model

A hosted file is its **key tree** ([file-format.md](re/file-format.md) 3):
nodes addressed by a path of byte-string segments, each node a leaf with a
value or a list of children. No file FileMaker wrote has a node that is
both ([hbam-writer.md](re/hbam-writer.md) 2.2), and the writer refuses one.
Siblings sort by the plain byte order of their segments, a shorter segment
before a longer one it begins (file-format.md 3.6). What a file can hold is
defined once, by `om_hbam::writer` (`MAX_SEGMENT`, `MAX_DEPTH`, items a
4,096-byte page can hold; architecture.md, "What the file can hold").
Large values are already split by the format into pieces of 1,000 bytes
that are children of the value's node (file-format.md 3.6).

Both engines keep this model. The store engine does not convert a file
into tables of its own:

- the protocol works on it: FileMaker Pro downloads subtrees by path in the
  file's own slot encoding ([network-protocol.md](re/network-protocol.md)
  5.6.6), and uploads record subtrees ([write-path.md](re/write-path.md));
- rule 3 of the contract holds without a codec for every catalog;
- export is a walk of the tree.

Besides its tree, a file has its **page 0** and its name. Tables, columns
and SQL types are views computed from the tree (section 10), never the
stored form.

**Path groups.** For history and deploys (section 9) the paths of a file
are sorted into three groups. The grouping is OpenMaker's and changes
nothing about how a file is stored.

| Group | Holds | Changed by |
|---|---|---|
| Design | tables, table occurrences, relationships, field definitions, layouts, themes, scripts, custom functions, custom menus, value lists, privilege sets, extended privileges | developers |
| Data | each table's records, indexes, record list and counters; container data | users |
| Access | accounts | administrators |

The paths of each group are listed in one place in code, from
file-format.md 3.4 and [schema-path.md](re/schema-path.md) 5. The list is
by path, not by catalog member, because a member can hold both: a serial
field's next value is a key inside its field definition
([write-path.md](re/write-path.md) 3.2) and is Data. A path in no group
belongs to the file as a whole: it is exported, and never deployed.

## 5. The engine interface

The rest of the server reaches a file's contents only through this
interface. It has three parts.

**Views.** A view is one fixed version of one file's tree. It answers a
node's kind and value, its children in key order from a given segment in
either direction, and a depth-first walk of a subtree. Nothing a view
returned changes later. `om_query::Source`, the download encoder
(`om_hbam::Node::to_wire`), the calculation and script hosts and the
`.fmp12` writer read through views. A view is cheap to take. On the store
engine it never waits for a writer; on the file engine it holds the file's
read lock, as readers do today. Downloads (op90) are plain reads of a view
in both engines. They take no lock in OpenMaker, where FileMaker Server
takes a read lock that waits (schema-path.md 4.5); this design does not
change that.

**Write operations.** Every change the server makes is a value of one
type, a write operation: commit records (op70), reserve record ids (op71),
advance a serial (op72), upload to a catalog (op13 and op86's
UploadMultiple and DeleteTable), update a table (op73), store an index
built for a find, store or release container data (op88, op87), an
authoring edit (op240), a deploy (section 9), and taking, upgrading and
releasing locks (op60, op61, op62, op65). One function runs an operation
against a transaction (a view plus put, delete and replace-subtree) and the
file's lock table. It returns three things: the **reply**, the **effects**
(edits to the tree and to the lock table) and the **notices** for other
clients. Operations on one file run one at a time, in one order.

Reply and effects are separate, because the protocol separates them. A
refusal can keep effects: op71 refused for a lock another user holds on a
reserved id has still used up the ids, as on FileMaker Server
(write-path.md 3.1). What an engine guarantees is about effects:

- the effects an operation returned are stored whole or not at all;
- when they cannot be stored, none is, and the reply is the operation's
  storage error in place of the one it returned;
- the reply and the notices go out only after the effects are stored.

Writes are values, and not closures over the store, so that they can be
sent to another server (section 7.2), recorded with their result
(section 7.3) and replayed in tests (section 12).

**The engine.** An engine gives views, submits write operations, exports
and imports, and says what else it can do:

```text
view(file)              -> View                    never blocks on the store engine
submit(file, operation) -> reply, notices          returns once the effects are durable
export(view, writer)                                an .fmp12 of exactly that view
import(reader, name)    -> file
capabilities()          -> clones, history, replication, point-in-time restore
```

The names are settled in code. A feature that needs a capability the
running engine lacks answers a typed "unavailable".

**Derived state.** The decoded catalog, the accounts, the basis of stored
calculations and the verified-index marks are computed from a view and
cached by the version they were computed from. They are not part of an
engine.

**The file engine behind it.** `submit` takes the write lock, runs the
operation with today's journal (`Store::save_commit`) and saves the file
before it returns. This changes what the file engine does when a save
fails, on purpose. Today op72, op86's stored indexes, op87, op88 and
server scripts call `Hosted::save`, which returns nothing, and op13 and
op73 drop the result of `save_held`, so a client can be told a serial,
catalog or container is stored when it is not. Behind the interface:

- a save that fails before the rename takes the operation's effects back,
  and the reply is the storage error;
- a save whose folder flush fails after the rename (`Store::save` logs a
  warning and reports success today) has an unknown outcome: the reply is
  the storage error, the file is hosted read-only from then on, and an
  alert is raised;
- a leftover `.omsave` is removed when the file is opened for hosting,
  only while the server holds the file's lock, so that no other OpenMaker
  process can be in the middle of a save, and only when it is a regular
  file in the file's own folder reached without following links.

## 6. The store engine

Crate `om-store`. It knows paths, values, files, entries and pages. It
knows nothing of records, catalogs or the protocol.

### 6.1 On disk

One store folder per server, used by one process at a time (a lock file):

```text
<store>/log/       the change log, in segments
<store>/pages      the page file: superblock slots, then pages
<store>/blobs/     container data by content hash (section 8)
<store>/state      this server's id, its start count, and its term and vote
```

### 6.2 State and positions

The **state** of a store at an entry is everything it answers from:

- for each file: its tree, its page 0, its name and id, whether it is
  read-only and the image kept for it (section 6.10), the list of its
  container files stored outside the `.fmp12` (section 8), and its design
  history (section 9);
- the lock table and the session table (section 7.3);
- the number of references to each blob (section 8);
- the cluster's members and the tree format version (section 7.6);
- the groups that are open (section 6.9);
- the index, term and hash of the last entry applied.

All of it is kept in trees in the page file, one per file and one for the
rest. A checkpoint names every root in one superblock (section 6.7), so a
checkpoint is the whole state at one entry, and nothing the state needs
lives only in memory or only in the log.

Each server has four positions in its log, each at or before the one
above it:

| Position | Meaning |
|---|---|
| appended | the last entry written |
| durable | the last entry a successful flush covers |
| committed | the last entry that can never be replaced (section 7.1). On one server it equals durable |
| published | the last entry applied to the state that views read |

### 6.3 The log

Every change is first an **entry** in the log. An entry has an index (its
position, counted from 1), a term (section 7.1), a kind, the file it
belongs to, a payload, and a SHA-256 hash over the previous entry's hash
and its own content. The chain of hashes shows a missing, reordered or
altered entry.

| Kind | Payload |
|---|---|
| change | one write operation's effects on one file: tree edits (put a leaf, put a list, delete a subtree), lock table edits, notices, the operation's id and its reply. It may be part of a group (section 6.9) |
| session | a session opened or closed, or a server started (section 7.3) |
| file | a file added, removed, renamed or cloned |
| members | the cluster's servers, or the tree format version (section 7.6) |
| mark | a named point: a restore point, a release, a comparison of servers (section 7.5) |
| empty | a new leader's first entry |

An entry holds **effects**, not the request: the edits the operation made,
as computed by the server that ran it (section 7.2). Applying an entry is a
function of the state before it and the entry alone. Each entry is applied
once: lock counts and share counts are not safe to apply twice.

**Segments.** The log is a sequence of files of one fixed size. A
segment's space is allocated when it is created, so an append never fails
for lack of space. A segment counts only after its header and its folder
are flushed. The header holds the store's id, the segment's first index
and the hash of the entry before it.

**Batches.** The entries appended between two flushes are a batch. Appends
wait for no timer: one flush covers every entry appended while the
previous flush ran. Each batch begins with a header that records the
**confirmed position**, the entry through which the previous flush
succeeded, and the committed position known then. An entry is durable when
the flush that covers it has returned success. Segments are opened for
direct, synchronous writes where the system offers them, so that a failed
flush cannot be hidden by the system's cache.

**What a damaged log means** is decided by the confirmed position
(section 6.12): entries at or before it were flushed and may have been
answered, so damage there is never treated as an unfinished write.

Entries of different files share the log, so they share flushes, and a
cluster has one election and one replication stream. Each entry names its
file, so one file can be restored or replayed without the others.
FileMaker Pro commits a transaction across files as one op70 per file with
a reply to each, and FileMaker Server answers the op103 that lists the
files "not supported" (write-path.md 1.5 and 3.5). The capture does not
show whether FileMaker Server undoes the first commit when a later one is
refused. OpenMaker Server sees separate requests and stores them as
separate entries; the shared log would let an extension of OpenMaker's own
commit them together.

### 6.4 Trees and pages

Each tree is a B+tree of fixed-size pages (16 KiB) in the page file. A
file's keys are paths in an encoding that sorts as the key tree does: each
segment's bytes with `00` written as `00 ff`, followed by `00 01`. A
subtree is then one contiguous key range, in depth-first order. An item's
value is a kind byte (list, value, or both) and the node's value. An item
too large for a quarter of a page keeps its start in the page and the rest
in overflow pages, hashed like any page.

Pages are **copy-on-write**: a change writes new pages for the leaf and
for every page above it, and the old pages stay as they were. An index
page holds, for each child, its first key, its page number, its hash and
the index of the entry that last wrote it. A page's hash is SHA-256 over
its items and, for an index page, over its children's hashes, without
their page numbers. So:

- reading a page recomputes its hash from its bytes and checks it against
  the hash its parent holds, which finds damaged pages, pages written to
  the wrong place and pages never written. The `.fmp12` format has no such
  check (file-format.md 5);
- the store can tell from a few pages whether anything under a path changed
  since a given entry;
- a tree's root hash depends on its content and its shape, and not on
  where a server put its pages.

**One shape per entry.** How pages split, merge and pack is fixed by the
**tree format version**, which is part of the state. Every server applies
entries with the same version, and the version changes only by a `members`
entry once every member supports the new one (section 7.6). Applying an
entry is deterministic, so every server has the same root hashes at the
same entry. A tree is never rebuilt in another shape except by an entry.

The page cache is keyed by page number and hash, so a page number that is
reused never returns the page that was there before.

The **superblock** names, for a checkpoint (section 6.7), the entry it
covers and the roots of the state's trees, and carries its own hash. Each
checkpoint's superblock is written twice, to slots of their own.

### 6.5 Writing

1. A connection's handler checks what it can against a view and builds a
   write operation.
2. The file's executor runs operations one at a time against the
   **working state**: the published state plus the effects of every entry
   appended and not yet published. It appends each operation's entry.
3. The log is flushed. On a cluster the entry is also replicated until it
   is committed (section 7).
4. The entry is **published**: views taken from now on include it, its
   notices go out and its reply is sent.

A view never shows an entry that is not committed, so no client acts on
data a crash or a change of leader can take away. Operations of different
files run in parallel. An operation that must be all-or-nothing is one
entry, or one group (section 6.9).

New pages stay in memory until a checkpoint writes them, so a commit costs
one share of a log flush and the pages on its path.

### 6.6 Reading

A view is a reference to a file's root at a published entry. Published
pages never change, so the page cache never invalidates anything: it only
evicts, and its size is fixed by configuration. A hosted file larger than
memory is served from disk.

Encoded download replies are cached by file, path and the entry that last
changed anything under the path. A layout or script that many clients
download is encoded once per version.

Long reads hold their view: an export, a backup, a large find or a SQL
query sees one version while writes go on.

### 6.7 Checkpoints, space and recovery

A **checkpoint** writes the pages of the published state that are not on
disk yet, the state's own tree and the space map included, flushes them,
then writes and flushes its superblock copies. Only published entries are
ever checkpointed.

**What is kept.** The two newest checkpoints are kept whole, and so is
every checkpoint held as a restore point (section 11). A page is reused
only when no kept checkpoint, no clone, and no view open in the process
refers to it. The log is kept from the oldest kept checkpoint, and further
back while a group is open (section 6.9) or another server needs it
(section 7.5). So when the newest checkpoint cannot be read, the one
before it still has every page it names and every entry after it.

The **space map** records which pages are free and how many trees share
each page (section 6.8). It is part of each checkpoint. Changes to it
since the checkpoint are rebuilt by applying the log.

**Starting up.** Take the newest checkpoint with a valid superblock copy,
then apply the log's entries after it, each once, in order, as far as the
committed position. On one server that is every durable entry. A member of
a cluster applies as far as the committed position its log recorded and
learns the rest from the leader (section 7.1). No step needs an
administrator.

### 6.8 Clones

A clone is a new file that starts as another file's tree and shares its
pages. A `file` entry makes it, and the clone is the source as it is at
that entry, so applying the log again makes the same clone. Shared pages
are counted in the space map, and counts are passed down lazily when a
shared index page is copied (the method of Rodeh, "B-trees, Shadowing, and
Clones", 2008). Environments (section 9) are clones.

### 6.9 Groups

Some work must appear all at once and is too large for one entry: op73's
deferred field work on a table, an index built for a find, a deploy's
changes to records, an import. Such work is a **group**: change entries
that name the group as its parts, and a last entry that closes it.

- Parts are written, replicated and committed like any entry, and are not
  applied. The last entry applies all parts, in order, in one step. Every
  view shows the file as it was before the group or as it is after it.
- A group declares the paths it changes. While it is open, the leader
  produces no other entry that changes those paths. The protocol's locks
  already keep clients out (op73 runs under the table's lock,
  schema-path.md 7.2); the declared paths make it a rule of the engine.
- A group is opened and closed by one leader in one term. A new leader's
  `empty` entry discards every group left open. So does a restart of a
  single server, which begins a new term. The work then starts again from
  what the file records as pending: for op73, the deferred list at
  `[T, 3, 1, fd]`, which the group's last entry removes.
- The log is kept back to the first part of every open group.

FileMaker Server commits such a walk every 512 records under an exclusive
lock (schema-path.md 7.2), so a crash can leave a table partly converted
with its deferred list still present. OpenMaker shows the change at once.
The file engine already does, since `Store::update_table` runs under the
write lock, and an export or backup taken from a view then never holds a
half-converted table.

Work that FileMaker itself commits record by record is not a group: a
script's loop, Replace Field Contents and an import of records are
separate operations, and other operations run between them.

### 6.10 Import and export

**Import** reads an `.fmp12` through `om-hbam`, walking the slot stream of
its leaf pages in order without loading the file, and writes every node
with its kind and value, empty lists included, as the parts of a group
whose last entry makes the file visible. It refuses what the server
refuses to host today (encrypted files, `crates/om-hbam/src/container.rs`)
and a file with an item beyond the store's bounds (section 6.11).

A file holding content the writer cannot write (section 4) is hosted
read-only, as today. The store keeps such a file's original image whole,
checked by its hash, and its export is that image. A file that cannot be
read to its end is refused.

**Export** walks a view in key order into the bulk writer of
[hbam-writer.md](re/hbam-writer.md) 7.2, which writes leaves, then index
levels, then the root, to a file it can seek in, with memory bounded by
the tree's depth and not its size. It writes page 0 as imported, with the
clean-close flag set and the change number advanced as the writer does
(hbam-writer.md 5.1). The result is checked by reading it back and
comparing it with the view node by node, kinds included. Both the reader
and the writer of `om-hbam` work on a whole file in memory today
(`HbamFile::open`, `writer::rebuild`); the streaming forms are part of
this work.

Export is offered:

- at any time from a running server, from a view, without holding up
  writes;
- by backup schedules: a backup keeps FileMaker Server's layout
  ([admin-console.md](admin-console.md), "Backups"), and each file's copy
  in it is an export from one view;
- when the server stops cleanly, into a configured folder;
- by `om-server export`, which reads a store folder with no server
  running.

### 6.11 Bounds

Input is hostile (AGENTS.md rule 6), and no client or file may make the
store take unbounded memory or disk:

- an item's encoded path and its value are each at most 64 KiB, the
  largest value the format's length fields allow; import refuses a file
  beyond that;
- an entry is at most as large as the request limits the server already
  enforces allow (`ConnectionLimits::upload_memory`); larger work is a
  group, whose parts have a fixed largest size;
- pages waiting for a checkpoint have a budget. Past it a checkpoint is
  forced and operations wait. Pages a group's last entry creates may be
  written before the checkpoint that names them;
- a view held longer than a configured time by a SQL query is ended, so
  that old pages can be reused;
- a server too far behind for the log to be worth sending gets the state
  (section 7.5).

### 6.12 Storage faults

| Fault | Found by | Response |
|---|---|---|
| An entry after the confirmed position is damaged or incomplete, with nothing valid after it | its hash | cut it off, and log the cut. On one server this cannot be told from a write the crash interrupted: an answered change is lost only if the disk damaged the last batch after confirming it. A cluster member cuts nothing at or before its committed position; it fetches |
| An entry at or before the confirmed position is damaged, or valid entries follow a damaged one | the hash chain | never cut. On a cluster, fetch it from another server. On one server, stop the store and report; the administrator restores (section 11) or explicitly accepts the loss |
| A page is damaged, misplaced or missing | its parent's hash | on a cluster, fetch it (section 7.5). On one server, reads of that range fail, the file is marked damaged and the event is logged |
| The newest checkpoint cannot be read | its superblock copies or its pages | use the checkpoint before it, which is kept whole (section 6.7) |
| A flush fails | the system's error | stop the server at once and recover on restart. Never retry the flush: the system may already have dropped the data (PostgreSQL's 2018 finding). An entry the failed flush covered may be found whole on restart; its outcome was unknown to its client (section 2) |
| The disk is full | a segment or page that cannot be allocated | refuse writes before appending anything, keep serving reads |
| Slow damage | a background job that reads every page and entry and recomputes its hash | as for the fault found |

## 7. Replication

A cluster is a set of servers with the same log. One server is a cluster
of one, and everything below holds for it trivially.

### 7.1 Consensus

The log is replicated with Raft (Ongaro and Ousterhout, 2014), implemented
in `om-store`, with these rules as the paper gives them:

- A term has at most one leader. A server's term and vote are flushed
  before it answers a vote or an append.
- A server votes only for a candidate whose log is at least as up to date
  as its own, and asks the others whether they would vote before it starts
  an election.
- A follower whose entry at an index differs from the leader's cuts its
  log from there, never at or before its committed position.
- The leader counts servers only for entries of its own term. An entry is
  **committed** when a majority has it durable and it is of the leader's
  term, or when a later entry is committed.
- A new leader's first entry is `empty`. It runs no operation and answers
  no request until that entry is committed and published, so that its
  state, with the session and lock tables, holds everything committed
  before its term.
- A leader that cannot reach a majority steps down.

A server that restarts publishes only what it knows is committed
(section 6.7). Entries after that may be replaced by a later leader.

Servers talk to each other over TLS with certificates both ends check, on
a port of its own.

### 7.2 Who executes

Only the leader runs write operations. It runs them against its working
state (section 6.5), appends each one's effects, and the other servers
apply effects. A server that received a client's request sends the
operation to the leader, which checks it against its own working state:
the sending server's checks only save a round trip. A leader that loses
its place drops its working state back to its published state.

So the servers need not compute the same result from the same request,
which they could not promise across a rolling upgrade: calculation,
collation and index-key code may differ between versions, and clocks
differ. They only need to apply edits the same way (section 6.4).

### 7.3 Sessions, locks and notices

- **Sessions.** A session is named by its server, that server's start
  count and its connection. A `session` entry opens it and one closes it.
  A server writes a `session` entry when it starts, which closes every
  session of its earlier starts. When a server is lost, the leader closes
  its sessions after the time a silent session is kept (FileMaker Server
  drops a silent session after 125 s; that a lost transport counts as
  silence is inferred, network-protocol.md 6.3). Closing a session
  releases its locks. An operation that names a session the state does not
  hold is refused, and a server that learns its sessions were closed drops
  those connections. A client that reconnects is a new session.
- **Locks.** A lock belongs to a lock user as today (connection, session
  id and transaction, write-path.md 4.1) within a session as above, and
  lock counts are kept per lock. Locks change only by entries, so every
  server holds the same lock table and a new leader starts with it. A
  single server need not wait for the flush before it grants a lock: if it
  stops, every session ends, and the entry written at its next start
  releases their locks.
- **Notices.** A change entry carries the notices its operation raised:
  the path, the attention code, the session that caused it and whether
  that session is left out (write-path.md 6, schema-path.md 9). Each
  server delivers them, in entry order, to the connections registered with
  it (op63), applying each registration's mask as `writes::notify` does. A
  registration made when a server has published through an entry receives
  the notices of later entries; what came before is in what the client
  reads. After a reconnect FileMaker Pro asks which of its cached items
  changed (op89, network-protocol.md 5.6.11).
- **Exactly once.** A session submits one operation at a time, each with
  the next sequence number, and the session table holds the last number
  and its reply. The leader runs the next number, answers the last number
  again with the recorded reply once its entry is committed, joins a
  request to one that is still waiting, and refuses anything older. So an
  operation a server sends again after a leader change is not run twice.
  This holds within a session. A request FileMaker Pro repeats after it
  reconnects comes from a new session and is a new operation, as it is
  after a dropped connection on one server.

### 7.4 Clients

FileMaker's protocol has no redirect (network-protocol.md 8), and
FileMaker Pro recovers from a lost host by opening the file afresh
(network-protocol.md 6.3, from its code).

**One identity.** Every server of a cluster gives the same server name, id
and version string in its Connect reply, and a certificate valid for the
address clients dial. FileMaker Pro tells hosts apart by name and id
(network-protocol.md 8) and reopens a file only when its creation value
matches (6.3); that value is part of the file.

**Read barrier.** Before a server answers a request, it makes sure it has
published every entry that was committed when the request arrived. The
leader confirms that it still leads by an exchange with a majority.
Another server asks the leader for its committed position, which the
leader gives after that exchange, and waits until it has published through
it. Requests waiting at the same moment share one exchange. So no server
answers from a state older than one any client was already answered from,
and a server cut off from the majority answers nothing. No clock is
involved. On one server the barrier is empty.

**Leader only.** Clients are served by the leader. Every other server
relays an incoming client connection to the leader unread, so a client may
dial any server, or one address in front of them. A leader change drops
every client, and each reconnects. This is the first mode built.

**Every server.** Each server takes clients, reads from its own views
behind the barrier, and sends write operations to the leader. It answers
an operation only after it has published the entry itself. Losing a
server then drops only that server's clients, and a leader change is a
pause in writes for the others.

### 7.5 Catching up, repair and comparison

**Entries.** A server that is behind gets entries from the leader's log.

**State.** When the log no longer reaches back far enough, or the server
is new, it copies a checkpoint the leader pins. The two walk the trees
from the roots down, and a page is sent only when the receiver has no page
with that hash at that place. The receiver checks each page against its
hash and writes index pages with its own page numbers, which the hash does
not cover. Blobs the state names are fetched too. The copy becomes the
receiver's state in one superblock write, at the checkpoint's entry; until
then its old state stands. Entries after it follow from the log.

**Repair.** A page whose bytes do not match the hash its parent holds is
damaged on this server. It is fetched from another server by its place in
the tree and checked the same way. Damage is found by recomputing hashes
from bytes, on reads and by the background job (section 6.12), never by
comparing stored hashes, which a damaged page leaves unchanged.

**Comparison.** The leader writes a `mark` entry from time to time. Each
server computes its state's root hashes at that entry and reports them. A
server whose hashes differ from the leader's first checks its own pages.
If they are damaged it repairs them. If they are sound, the difference is
a defect: the server stops serving, keeps its files for inspection, raises
an alert and copies the leader's state. When a majority agrees against the
leader, the leader steps down and does the same.

### 7.6 Members and witnesses

Servers are added and removed one at a time by a `members` entry, each
committed before the next, from the Admin Console or the command line. A
new server first follows without a vote, and is given its vote by a second
entry once it has caught up.

A **witness** is a member that keeps the log and the blobs its entries
name, and votes. It holds no trees and serves no clients, so two full
servers and a witness survive the loss of any one of them. A witness can
win an election, by the ordinary rules, when it holds entries the
remaining full server lacks. As leader it runs no operation: it writes its
`empty` entry, brings the full server up to date, and hands leadership to
it (Raft's leadership transfer). Clients wait meanwhile. A witness discards
entries and blobs once every full member has them durable.

## 8. Container data

A large value is a node with a header child and pieces of 1,000 bytes
(file-format.md 3.6). The store moves such a node out of the tree into
`blobs/`, named by the SHA-256 of its content, when it is larger than a
threshold and has exactly that layout. The tree keeps a reference with the
length and the header, and a view shows the node's children as they were.
A value with any other layout stays in the tree. Export writes the pieces
back.

- **Before the entry.** A blob is written, flushed and checked against its
  hash before the entry that names it is appended. A server acknowledges
  an entry only when it holds every blob the entry names; blobs travel
  beside the log, not in it.
- **Counted in the state.** Identical content is stored once across files,
  clones and versions, and the state counts the references to it.
- **Removed late.** A blob is removed only at a checkpoint, and only when
  no kept checkpoint, clone, kept entry, open group or operation in
  progress names it.

**Container files beside the `.fmp12`.** A field with external storage
keeps its data in files in a folder beside the file, and the tree holds
references to them ([containers.md](re/containers.md) 3.4 and 8). They are
part of the hosted file. Import takes the folder's files into `blobs/` and
records each one's path in the file's state, as of the same entry as the
tree. Export and backups write them back where FileMaker Server puts them
(admin-console.md, "Backups"). The store does not interpret them;
serving them to clients is
[#21](https://github.com/OpenMaker-Dev/openmaker/issues/21).

## 9. Design history, analysis and deploys

These are commands (AGENTS.md rule 3) in `om-author`'s catalog. The Admin
Console, the CLI and MCP tools call the same operations.

**Objects and revisions.** A design object is a catalog member: a layout,
a script, a table, a field, a value list, a custom function, a theme, a
menu, a privilege set, a relationship, a table occurrence. Its revision is
the `sha256:` of its Design paths' content without modification stamps,
the definition `om-author` already uses ([authoring.md](authoring.md),
"Revisions").

**History.** The entry that changes a Design or Access path also writes,
into the file's state, an event: the entry, the time, the account, the
operation and each object's revision before and after. Each revision's
content is kept once, by its hash; two events that reach the same content
stay two events. History is part of the state, so it does not depend on
how long the log is kept. That covers deploys, `om-author` edits and a
developer's edits from FileMaker Pro alike. For Access paths only the fact
of the change is kept, never account content. History answers: what
changed between two points, who last changed an object, and an object's
content at any revision.

**Text form.** One text file per object: readable text where OpenMaker
decodes the object (scripts in the form `get_script` returns), the rest as
hex under its path, stamps left out. Text must read back to the same
revision; an object for which it does not is written as hex only. Access
paths and Data paths inside a member are never written. The text form is
for diffs, review and an optional mirror that writes each revision into a
folder for git. The store's history is the master copy, since live edits
arrive there.

**Analysis.** From the decoded references between objects, for any
revision: where an object is used, broken references, objects nothing
refers to, references made by name at run time (scripts run by name,
ExecuteSQL text, `Evaluate`), script call chains, and a security review
(accounts without a password, scripts that run with full access).

**Deploy.** Bringing selected design changes from a source into a hosted
file without closing it:

1. **Source and base.** The source is an uploaded `.fmp12`, another file
   in the store, or a revision of the text form; it is held as it is from
   preview to apply. The **base** is the target's state the source was
   made from: the export or clone it started as, which the store recorded,
   or one the administrator names. Without a base the comparison is
   between source and target only, and every difference is shown as one.
2. **Identity.** An object of the source is the target's object of the
   same kind with the same id when the base has that object too. An object
   absent from the base is new in the source. Two environments can give
   the same id to unrelated new objects: a new object whose id the target
   has used for another object is given a new id, with every reference to
   it rewritten, when OpenMaker decodes every place that can refer to it.
   Otherwise it is refused. Names are shown and never decide identity
   alone.
3. **Differences** are found against the base: changed in the source,
   changed in the target, or changed in both, which is a conflict the
   administrator settles per object. Putting an object back to an earlier
   revision is a change like any other. Each difference is classed: a
   member replaced; a table's fields changed; or a change every record must
   follow (a field's type, a stored calculation, an index, a deleted
   field), which runs as a group (section 6.9).
4. **Selection.** The administrator selects. The selection is widened by
   what it needs and refused if it would leave a reference broken. Access
   paths are never deployed. Data paths inside a member keep the target's
   values (a serial field keeps its next value). A changed privilege set
   is shown with the accounts it affects, and one that accounts use cannot
   be deleted.
5. **Apply** is one write operation. It takes, for each kind of object,
   the locks FileMaker Pro takes to save that kind, in its order: Manage
   Database's for tables, fields, occurrences and relationships
   (schema-path.md 4), the Script Workspace's for scripts
   ([scripts.md](re/scripts.md), "Saving scripts to a host"). Under those
   locks it checks that every target object still has the revision the
   preview showed, and fails with a conflict otherwise. It writes the
   members and catalog indexes with new modification stamps
   (schema-path.md 8) and releases the locks with FileMaker's notices
   (schema-path.md 9). A kind whose save sequence has no spec in
   `docs/re/` is not deployed until it has one.

That clients fetch a catalog again on its notice is inferred from
FileMaker Pro's code (schema-path.md 9), and whether a deploy looks to
FileMaker Pro like a developer's live edit is unmeasured (section 14).

**Environments.** A development or test copy of a hosted file is a clone
(section 6.8) under another name. Design moves between environments by
deploy; data and accounts stay where they are.

## 10. SQL

One executor serves FileMaker Pro's finds (op86, `om-query`) and SQL
(`om-sql`). Both read a view and use the file's own indexes.

- **Comparison follows the dialect.** FileMaker's SQL compares text
  exactly and case-sensitively, in UTF-16 code unit order, whatever the
  field's index language ([execute-sql.md](re/execute-sql.md) 6.1), where
  a find matches by the index's language. An index only proposes rows;
  each is checked again with the SQL comparison (execute-sql.md 10.1,
  inferred there).
- **Two dialects, one planner.** FileMaker's SQL for `ExecuteSQL`, and a
  Postgres-compatible dialect for the endpoint below, because clients of
  that protocol send Postgres syntax. Both use section 6 of
  execute-sql.md for comparison, order, null and type rules, so one
  statement gives the same rows in either.
- **Endpoint.** The Postgres wire protocol over TLS on its own port, off
  by default. The database is a hosted file. Sign-in is with the file's
  accounts, needs the `fmxdbc` extended privilege, and the account's
  privilege set applies: record access formulas and per-field access.
  Enough of the Postgres system catalogs is answered for common clients
  and drivers.
- **Types.** A FileMaker field holds whatever was typed. A typed column
  gives the value when it parses as the field's type and null when it does
  not; the stored text is always available as text.
- **Reads.** A statement, or a read-only transaction, runs on one view, on
  any server, behind the read barrier (section 7.4).
- **Writes.** INSERT, UPDATE and DELETE are write operations, so they run
  on the leader: the statement's rows are found again in the leader's
  working state, never taken from the view a follower planned with. A
  statement is all-or-nothing. It fails whole when a row is locked by
  another session, as a script's edit of that record would. The server
  runs auto-enter options, validation and stored calculations (`om-calc`,
  `om_data::rules`), keeps indexes and modification counts, and notifies
  clients, as for a record a server script writes. Script triggers do not
  run. Container, calculation and summary fields cannot be written. A
  transaction that writes more than one statement, and statements that
  change the schema, are refused with an error.
- **OpenMaker's client.** A protocol extension of OpenMaker's own
  (architecture.md, "OpenMaker's own protocol extensions") lets the
  desktop client have the server sort, summarize and run `ExecuteSQL`,
  where FileMaker runs `ExecuteSQL` in the client and fetches through
  finds (execute-sql.md 10.2, inferred there).

## 11. Running it

| Setting | Meaning |
|---|---|
| `--engine file` or `store` | the engine for the whole server; `file` until the maintainer changes the default |
| `--store-dir` | the store folder |
| `--store-cache-mib` | the page cache's size |
| `--export-on-stop` | a folder that gets an export of every file at a clean stop |

- **Moving to the store engine.** A file named by `--database-folder` that
  the store does not hold is imported when the server starts. The original
  is not written again and stays as it was.
- **Moving back.** Export, then start with `--engine file` on the exports.
- **Backups.** Schedules, retention and layout as today; copies are
  exports from a view (section 6.10).
- **Restore points.** The store keeps a checkpoint whole, with the log
  after it, at a configured interval and for a configured time
  (section 6.7). A file is restored by the leader as a **new file with a
  new id**: a clone of the file at the restore point, then that file's
  kept entries up to the chosen entry or time, applied as a group. Old
  sessions and retried operations cannot address the new file.
- **Formats.** Entries, pages and the superblock carry a format version. A
  newer server reads older formats. Until the format is declared stable,
  moving between versions may need export and import.
- **Secrets.** The store folder is readable by the server's account only.
  The store is not encrypted at rest (section 14).

**Administration is commands** (AGENTS.md rule 3). Each has a stable id,
typed parameters and a typed result, needs the administrator's sign-in,
and is unavailable, with the reason, on an engine that lacks its
capability. The Admin Console, the Admin API and the command line call the
same operation.

| Command | Runs as |
|---|---|
| import, export, clone, rename and remove a file; set a restore point; restore a file | entries, run by the leader from any server |
| add, remove or promote a member; hand over leadership; raise the tree format version | `members` entries, run by the leader |
| check every page and entry now; repair a page; copy the leader's state | on one server |
| accept the loss of entries after damage on a single server (section 6.12) | on that server, with the entries named |
| export from a store folder with no server running | the command line only |

## 12. Testing

Reliability is decided here more than in any structure above.

**The engine runs in a simulator.** `om-store` reaches disk, network,
clock and randomness only through traits. The simulator implements them in
memory and runs a whole cluster in one thread from a seed, injecting:
crashes at any point, half-written and lost writes, failed flushes, damaged
and misplaced pages and entries, a full disk, delayed, dropped, repeated
and reordered messages, split networks and clocks that jump. A failing
seed replays exactly. After every step it checks:

1. every published version equals a reference model, an in-memory map of
   paths with a lock table, given the same operations;
2. no operation that was answered as stored is ever lost, and none runs
   twice within its session;
3. servers at the same entry have the same root hashes;
4. a view never changes, and never shows an entry that is not committed;
5. no answer comes from a state older than one already answered from;
6. no two sessions hold locks that conflict;
7. an export of a view, imported again, equals the view node by node;
8. a group is seen whole or not at all;
9. a restore to an entry equals the reference model at that entry.

Clusters with a witness, members joining and leaving, and a leader lost at
each step of each of these are part of the generated schedules.

**Both engines behind one server.** Every server test runs against both
engines, and generated request sequences are sent to both through the real
connection code, with replies compared byte for byte. Both runs take the
host clock, connection ids and nonces from the same seeded source, so
replies that carry them are comparable.

**FileMaker.** Exports pass FileMaker's consistency check
(hbam-writer.md 8) and open in FileMaker Pro. The runs of section 14 use
FileMaker Pro against a cluster.

**Measurements** go to `docs/performance/` with their method: commit
latency and throughput by file size, memory, restart time, and the same
for the file engine.

## 13. Alternatives not taken

| Alternative | Why not |
|---|---|
| PostgreSQL or another outside database as the store | The maintainer wants no dependency on an outside project for storage; the protocol needs the file's own bytes and key order, which an outside store would hold as opaque values anyway |
| Writing changed pages into the `.fmp12` in place (hbam-writer.md 7.1) | Every page write must satisfy FileMaker's checks, the format has no checksums, and atomic commits need a journal beside the file. The bulk writer is simpler and already proven; the file engine keeps it |
| The key tree in memory with a log, and an `.fmp12` as each checkpoint | Memory, restart time and checkpoint cost still grow with the file |
| Pages updated in place with a redo log, as PostgreSQL does | Readers then need row versions and a cleanup job. One writer and copy-on-write pages give readers a fixed version without either |
| A log-structured merge tree | Reads and range scans dominate here; compaction makes latency uneven |
| A hash of each tree that does not depend on its shape | Comparing servers and copying only pages that differ need hashes of pages. Fixing the shape by the entry sequence and a format version gives both |
| A primary with one standby fed by log shipping | No protection against two primaries, and a failover can lose confirmed commits |
| Every server running each operation itself | Needs identical calculation, collation and clock behaviour on every server and across versions |
| Locks kept only in the leader's memory | A failover would empty the lock table while clients still edit records |
| Reads without the barrier, or under a lease kept by clocks | A server that is behind, or a leader that was replaced without knowing, would answer from an old state. A lease is safe only while clocks run at nearly the same rate |
| A table update published as it goes | A view, an export or a backup could hold a table partly converted (section 6.9) |
| A full server that copies a witness's entries before standing for election | A new election rule to prove. A witness that wins by the ordinary rules and hands over needs none |

## 14. Limits and unknowns

- **FileMaker Pro after its host is gone.** That it opens the file afresh
  and keeps a list of its locks is from its code (network-protocol.md
  6.3); what it does with unsaved edits and locks has not been run, and
  where it gets a host's standby addresses is not known. Until it has
  been run, a cluster serves clients from the leader only (section 7.4).
- **FileMaker Pro when a layout it shows, or a script it runs, changes.**
  Unmeasured. Deploys use FileMaker's own locks and notices, so they are
  no worse than a developer's live edit, which is also unmeasured.
- **A transaction across files.** Whether FileMaker Server undoes one
  file's commit when another's is refused is not shown by the capture
  (write-path.md 1.5).
- **A file with deferred field work pending.** An export taken between a
  field catalog's upload and the end of its op73 holds the deferred list
  (section 6.9). OpenMaker finishes that work when it hosts the file
  (`Store::recover`). What FileMaker does on opening such a file is
  unmeasured.
- **Identity of catalog members.** Deploys match objects by kind and id
  against a base (section 9). Whether a member carries an id of its own
  that survives copying between files, which FileMaker's XML export
  suggests, is not specified in `docs/re/`.
- **Encrypted files** cannot be opened
  (`crates/om-hbam/src/container.rs`), so they cannot be imported, and the
  store has no encryption at rest of its own.
- **FileMaker's ODBC and JDBC protocol** is not studied; section 10's
  endpoint is not it.
- **Host time** (op82) comes from each server's clock. Servers of a
  cluster need synchronized clocks; a rule for refusing to serve when they
  drift is not designed. Nothing in sections 6 and 7 depends on clocks
  agreeing.
- **Damage to the last batch on a single server** cannot be told from an
  interrupted write (section 6.12).
- **Two developers' parallel changes** merge by section 9's rules.
  Objects OpenMaker does not decode cannot be given new ids, so such a
  merge can be refused.
- **Copies of production data for development** with records masked or
  sampled are not designed.
