# OpenMaker Server storage

How OpenMaker Server keeps hosted files: its two storage engines, the
interface between them and the rest of the server, and what is built on the
store engine (replication, design history and deploys, SQL). All of it is
OpenMaker's own design. FileMaker Server works differently inside
([hbam-writer.md](re/hbam-writer.md) 6.1), and nothing here is compared with
it except the contract in section 2.

**Status: design.** Only the file engine exists
(`crates/om-server/src/store.rs`), and of the engine interface the write
operations of record writes and locks (section 5 says which). The rest of
the interface, the store engine and everything in sections 6 to 12 are
specified here and not built.
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
   One exception: a file hosted read-only because it holds content the
   writer cannot write is exported as the bytes it was imported from
   (section 6.10), and opens in FileMaker only if those did.
3. **Nothing dropped.** Keys and values the server does not understand are
   kept and exported as they came.
4. **Stored means durable.** A reply that says a change is stored is sent
   only after the change is on disk (on a cluster: on a majority of
   servers). An operation has one of three outcomes: stored, and the reply
   says so; not stored, with nothing of it left, and the reply says so; or
   unknown to the client, when the server stopped, lost its place in the
   cluster or could not confirm a flush before it could answer. In the
   third case the client gets no success reply, and the change is there
   whole or not at all.
5. **Explicit refusals.** What an engine cannot do fails with an error,
   never as a silent success (AGENTS.md rule 4).

Rule 4 is about what the engine does before it answers. A single server
finds damage its disk does to what it confirmed (section 6.12) but cannot
repair it; backups and a cluster do.

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

**Status.** Write operations are built for record writes, locks, schema
changes, container data and authoring edits, with the file engine behind
them (`crates/om-server/src/engine/`): op70, op71, op72, op60, op61, op62,
op65, op13, op73, op86's schema instructions and stored indexes, op87,
op88, and op240's edits and its Save, from clients, and the same
operations of server scripts (reserving a record id, advancing a serial,
releasing a transaction's or a session's locks, storing the indexes a
find or a related-record lookup built). Everything else still changes a
file from its request handler: account edits, a server script's commit
(with the container data it stores and counts) and its record and account
locks, the release of a closed session's locks, the desktop's saved found
sets, and the deferred field work a file is opened with
(`Store::recover`) with its save. Views, `export`, `import` and
`capabilities` are not built.

**Views.** A view is one fixed version of one file's tree. It answers a
node's kind and value, its children in key order from a given segment in
either direction, and a depth-first walk of a subtree. Nothing a view
returned changes later. `om_query::Source`, the download encoder
(`om_hbam::Node::to_wire`), the calculation and script hosts and the
`.fmp12` writer read through views. A view is cheap to take. On the store
engine it never waits for a writer; on the file engine it holds the file's
read lock, as readers do today. Downloads (op90) are plain reads of a view
in both engines. That they take no lock, where FileMaker Server takes a
read lock that waits (schema-path.md 4.5), is how OpenMaker Server works
today and is not part of this design.

**Write operations.** Every change the server makes is a value of one
type, a write operation (`engine::Operation`): commit records (op70,
`CommitRecords`), reserve record ids (op71, `ReserveRecordIds`), advance a
serial (op72, `AdvanceSerial`), upload to a catalog (op13,
`UploadToCatalog`; op86's UploadMultiple and DeleteTable, `ChangeSchema`),
update a table (op73, `UpdateTable`), store an index built for a find
(`StoreIndexes`), store container data (op88, `StoreContainerData`),
adjust a container's reference count, which releases it at 0 (op87,
`AdjustContainerCount`), an authoring edit (op240, `EditDesign`, one
variant carrying the decoded edit), the authoring extension's Save
(op240, `SaveFile`), a deploy (section 9), and taking, upgrading and
releasing locks (op60, op61, op62, op65: `TakeLock`, `UpgradeLock`,
`ReleaseLock`, `ReleaseLocks`). An operation owns its data and names who
asks: the lock user (connection and transaction), the name other users see
for it, for op70 the account, its privilege set at login and those of
the listed records the session was served without their values, for
op73 the account its calculations see, and for an authoring edit the
connection, session name and account its locks and stamps name. op70's
staged records travel in it: the session's temporary area lends them
(`writes::TempArea::lend`) and takes back those the commit did not take.
A commit that was not stored has still taken the records it reached, as
before the interface: the file is put back, the staged edits are gone,
and the client uploads them again to retry. op88's container data is read
from the upload before its operation, within the memory the client may
hold (`--upload-memory-mib`), and travels in it, or, when it is not valid
binary data, why not; the request keeps the charge until `submit`
returns. An authoring edit's request travels as it was decoded, its
definition charged the same way.

Finds and op86 programs run before their operation, as reads of the file
under its read lock, and leave state of the running server outside the
file's tree: the indexes they built on demand (query-program.md 7.1) and
the member keys GenerateMemberKey issued (schema-path.md 3.4) wait in the
store until a write stores them. Whichever write first holds the store
stores all that waits then, whoever left it. So the request that stores
them submits a draft (`engine::Draft`) that names none of them, and the
file engine completes it under the store's write lock, taking what waits
into the operation it runs (`ChangeSchema`, `StoreIndexes`). A recording is
offered each operation by reference, completed and before it runs; that is
what it holds, and a replay submits it as it is. The running server records
nothing, so submitting an operation copies none of its uploaded trees or
staged records. What it does copy is small: the member keys a draft took (a
catalog path and two numbers each), kept so that they go back to waiting if
its run is left by a panic before it writes them, as they stayed waiting
before. The store engine will have to make issuing a member
key and building an index operations of their own.

One function, `engine::run`, runs an operation against a transaction (a
view plus put, delete and replace-subtree), the file's lock table
(`engine::Locks`) and an environment (`engine::Environment`), from which
it takes the clock and randomness: an authoring edit's modification
stamps and the UUIDs of what it adds, and the unique ids of the container
data op88 stores. Formulas an operation evaluates are not
covered yet: a privilege set's record limit and the stored calculations
op73 recomputes take today's date, `Random` and their time budget from the
process ([#692](https://github.com/OpenMaker-Dev/openmaker/issues/692)). It returns the
**reply** and the **notices** for other clients (`engine::Outcome`); the
**effects** (edits to the tree and to the lock table) are what it did to
the transaction and the lock table, which keep what the engine needs to
take them back. An operation that does not change the tree gets less than
a transaction (`engine::Hold`): op60 a view, op61, op62 and op65 nothing of
the tree, so that these three do not wait for the store, as before. In the
file engine
the transaction (`engine::Transaction`) is still a thin wrapper of the
store and its journal: it exposes the store's methods the operations use,
and the store itself as the view that derived state is computed from. The
reply is typed, one shape per operation (`engine::Reply`); the caller
encodes it for the wire. Operations on one file run one at a time, in one
order; how far the file engine does that today is under "The file engine
behind it".

Reply and effects are separate, because the protocol separates them. A
refusal can keep effects: op71 refused for a lock another user holds on a
reserved id has still used up the ids, as on FileMaker Server
(write-path.md 3.1). What an engine guarantees is about effects. This is
the contract of the finished interface; what the file engine does for each
operation today, where it still falls short, is listed under "The file
engine behind it":

- the effects an operation returned are stored whole or not at all;
- when the engine knows they were not stored, none is, and the reply is
  the operation's storage error in place of the one it returned;
- when the engine cannot tell (contract rule 4), it sends no success
  reply and takes no further write to the file until it can;
- the reply and the notices go out only after the effects are stored. One
  exception: on a single server, an operation whose only effects are edits
  to the lock table is answered before they are durable (section 7.3).

Of the lock table's edits, a lock an operation took is released again when
its other effects are not stored, and a commit's release of the records it
wrote waits until they are: a commit that is not stored leaves its records
locked. The edits of an operation that only edits the lock table are made
at once.

Writes are values, and not closures over the store, so that they can be
sent to another server (section 7.2), recorded with their result
(section 7.3) and replayed in tests (section 12). A test records a
sequence of operations with their outcomes and replays it against a fresh
load of the same file, to the same replies, notices and tree
(`engine::tests`).

**The engine.** An engine gives views, submits write operations, exports
and imports, and says what else it can do:

```text
view(file)              -> View                    never blocks on the store engine
submit(file, operation) -> reply, notices          returns once the effects are durable
export(view, writer)                                an .fmp12 of exactly that view
import(reader, name)    -> file
capabilities()          -> clones, history, replication, point-in-time restore
```

`submit` is `engine::FileEngine::submit(file, operation) -> Outcome`; the
caller encodes the reply and delivers the notices only after it returns.
There is no engine trait while there is one engine. The other names are
settled when they are built. A feature that needs a capability the running
engine lacks answers a typed "unavailable".

**Derived state.** The decoded catalog, the accounts and the
verified-index marks are computed from a view and cached by the version
they were computed from. They are not part of an engine.

The basis of stored calculations is not computed from the current view. It
is what the stored values were last computed with, per table and field:
kind, data type, the options that change stored values, repetitions, and a
calculation's context and formula (schema-path.md, "Implementation notes
(recalculation)"). It is taken from the file when the file is loaded,
except for a table with deferred field work, which gets none, and again
after each op73 (an authoring edit adds a new field's). op73 compares the
field definitions as they are with it to find the stored calculations to
recompute. So an `UpdateTable` replays to the same result only from the
same basis, for example by replaying the uploads before it from the same
starting file: on a fresh load of the file as it was just before op73, the
basis already holds what op13 changed. Deferred work names a formula change
(`0x40`), so recovery at load still recomputes it. A new stored calculation
is named by nothing, and a server that stops between op13 and op73 never
computes it
([#707](https://github.com/OpenMaker-Dev/openmaker/issues/707)).

**The file engine behind it.** `FileEngine::submit` takes what each request
took before the interface: the store's write lock for op70, op71, op72,
op13, op73, op86's schema instructions and stored indexes, op87, op88 and
op240, its read lock for op60, and for op61, op62 and op65 only the lock
table. It takes the store first and the lock table when the operation
first uses it (op13, op73, op86's operations, op87, op88 and op240's Save
never do), completes a draft right after taking the store, holds both
while it saves, and lets go of the lock table first; the caller then
delivers the notices, taking the file's registrations and then the
mailboxes (the order is in the module doc of `engine`). An authoring edit
holds its locks as op240 always did: it takes the lock table only while
it takes them (and again while it releases them when it is refused after
that), saves under the store alone while the locks keep other clients out
of what it changes, and its locks are released after the store is let
go. op13, op73, op86, op87 and op88 raise no notices: the other clients
learn of a schema change when the client releases its lock
(schema-path.md 9). An authoring edit raises the notices the releases of
its locks would raise.

Operations that change the tree therefore run one at a time. op61, op62
and op65 run under the lock table alone: they wait for an op70 or op71
that holds the lock table while it saves, and can run beside an op72,
which holds only the store. op60 also reads the store, so it waits for
each of the three while it holds the store for writing. A request that
takes a lock first counts the locks its connection holds in every file it
has open, so it can also wait for such a save of another of those files.

op71 now holds the lock table while it saves, as op70 always did, because
the locks it took must be released again if the save fails. That changes
what two overlapping requests can get. Before, op71 saved first and locked
its ids afterwards, so an exclusive lock on the table's records requested
during the save (op61) was granted, and op71 was then refused with its ids
used up. Now the lock request waits for the save and is refused, and op71
gets its ids. Both are orders the two requests could have run in;
FileMaker Server's behaviour here is unmeasured.

`submit` saves as each request saved before:

- op70: the commit's journal (`Hosted::save_commit`). A failed save takes
  the commit back, its records stay locked, and the reply is the storage
  error;
- a client's op71: the new last issued id before the ids are answered
  (`Hosted::save_held`, #927). A failed save takes back the reservation and
  the locks it took, and the reply is the save's error. A server script's
  op71 is saved by the script's commit;
- op72, op13, op86's schema instructions and the indexes a find built
  (a client's or a server script's): the file is saved if it changed, and
  a failed save leaves the change in memory and is not reported, as
  `Hosted::save` does and as op13 dropped the result of `save_held`;
- op73: the same, only when its work changed the file;
- op87 and op88: the same, op88 also when the upload was not valid binary
  data, as `Hosted::save` was called after every op88 that was read;
- an authoring edit that changed the file: its journal
  (`Hosted::save_commit`). A failed save takes the edit back, the reply is
  the storage error (status 800, 809 or 810) saying what the file keeps,
  and its locks are released all the same;
- op240's Save: as op72, its reply then saying whether the file holds
  changes the disk lacks (status 800), or that it is hosted read-only;
- the lock requests save nothing.

op86's schema instructions and stored indexes, and a server script's
stored indexes, were made under one hold of the store and saved under a
second one; `submit` does both under one, so a request that overlaps sees
the file before them or after the save, as it could before. The
thumbnails of a library DeleteTable took are forgotten under that hold,
before the save, where they were forgotten between the two.

op87 and op88 changed the file under one hold of the store and saved it
under a second one, op240's Save saved under one hold and asked whether
the file still held unsaved changes under a second, and op240's edits
asked whether the file is hosted read-only (Field Control also read the
object's field and value list) under a hold of their own before they took
the store for writing. `submit` does each under one hold, so a request
that overlaps sees the file before the operation or after its save, as it
could before, with two differences in a narrow overlap. A change another
request left unsaved between the Save's save and its question (a server
script's reserved record ids, which wait for its commit, or a change
whose own save failed) made the Save answer 800 though its save had
succeeded; now it answers for its own save. And an edit of a file that
op73 left hosted read-only between the edit's question and its hold was
made in memory and refused at its save, with status 800 ("could not be
saved: the file is open read-only") and a save-failure alert; now it is
refused with status 810 ("open read-only") and no alert, as when the file
was read-only before the request. op240 is OpenMaker's own; what
FileMaker Server does with overlapping op87 and op88 is unmeasured. The
memory an op88 upload is charged (`--upload-memory-mib`) was given back
once its data was stored, before the save; it is now given back once
the save is done, so another upload during that save finds that much less
of the budget left.

The follow-up of #673's step 1, once every write goes through `submit`,
changes what the file engine does when a save fails, on purpose. Today
`submit` reports nothing for op72, op13, op73, op86's operations, op87 and
op88, and server scripts' other saves call `Hosted::save`, which reports
nothing either, so a client can be told a serial, catalog or container is
stored when it is not. Behind the interface:

- a save that fails before the rename takes the operation's effects back,
  and the reply is the storage error;
- a save whose folder flush fails after the rename (`Store::save` logs a
  warning and reports success today) has an unknown outcome. The renamed
  file holds the change and may or may not survive a crash. The reply is
  the storage error, readers go on reading the version in memory, which is
  the renamed file's, the file takes no more writes, and an alert is
  raised;
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
<store>/state      this server's id, its start count, its term and vote,
                   the list of log segments, the marker (section 6.3),
                   and whether it is in repair (section 7.5)
```

The start count is raised and flushed each time the server starts, before
it writes or sends anything else. `state` is written as two copies in
turn, each with a number and a hash, and the valid copy with the highest
number counts. A server without a valid copy does not know what it
confirmed, and does not start (section 7.5).

### 6.2 State and positions

The **state** of a store at an entry is everything it answers from:

- for each file: its tree, its page 0, its name and id, whether it is
  read-only and the image kept for it (section 6.10), the list of its
  container files stored outside the `.fmp12` (section 8), and its design
  history (section 9);
- the lock table and the session table, with each server's highest start
  count (section 7.3);
- the number of references to each blob (section 8);
- the cluster's members and the tree format version (section 7.6);
- the groups that are open, with the effects of their parts (section 6.9);
- the index, term and hash of the last entry applied.

All of it is kept in trees in the page file, one per file and one for the
rest. A checkpoint names every root in one superblock (section 6.7), so a
checkpoint is the whole state at one entry, and nothing the state needs
lives only in memory or only in the log. Each server's map of its own free
pages (section 6.7) is not part of the state: servers place pages
differently.

Each server has four positions in its log, each at or before the one
above it:

| Position | Meaning |
|---|---|
| appended | the last entry written |
| durable | the last entry a flushed marker covers (section 6.3) |
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
for lack of space. A segment is used only after its header and its folder
are flushed and `state` lists it. The header holds the store's id, the
segment's first index and the hash of the entry before it. A listed
segment that is missing, or has no valid header, is damage
(section 6.12), whatever the other segments hold.

**Batches and the marker.** The entries appended between two flushes are
a batch: a header with its first index, its entry count, its length and a
hash, then the entries. Appends wait for no timer: one flush covers every
entry appended while the previous flush ran. After a batch's flush
succeeds, the server records the **marker** in `state` (section 6.1) and
flushes it: the index and hash of the batch's last entry, and the highest
committed position it has heard of. An entry is **durable** when the
marker covers it. Only then is it acknowledged to a client or to another
server, or counted in a vote. A commit waits for two flushes, one of the
log and one of `state`. When a leader's log requires a follower to cut
entries the marker covers (section 7.1), the follower first records and
flushes the lower marker, then cuts.

The marker says "everything through here was written and flushed", and it
is kept outside the log on purpose: a segment, or the end of one, can be
lost without taking the record of what it held. That is what lets a
damaged log be read correctly (section 6.12). A log that does not hold,
whole, every entry through the marker has lost something confirmed,
however cleanly it ends. Whatever follows the marker was never
acknowledged to anyone.

Segments are opened for direct, synchronous writes where the system
offers them, so that a failed flush cannot be hidden by the system's
cache.

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
in overflow pages, hashed like any page. An index page holds, for each
child, the shortest key that separates it from the child before, and that
key spills to overflow pages by the same rule.

Pages are **copy-on-write**: a change writes new pages for the leaf and
for every page above it, and the old pages stay as they were. An index
page also holds, for each child, its page number, its hash and the index
of the entry that last wrote it. A page's hash is SHA-256 over its items
and, for an index page, over its children's hashes, without their page
numbers. Each page also carries a checksum of all its bytes, page numbers
included, so damage to a pointer is found in the page that holds it. So:

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
covers, the roots of the state's trees and of the server's own map of free
pages, and the restore points that are kept, with their roots. It carries
a number and its own hash, and is written twice. There are four slots: a
new checkpoint's two copies overwrite those of the older of the two
checkpoints on disk.

### 6.5 Writing

1. A connection's handler checks what it can against a view and builds a
   write operation.
2. The file's executor runs operations one at a time against the
   **working state**: the published state plus the effects of every entry
   appended and not yet published. Before it appends an operation's entry
   it reserves the page space that applying the entry will need; an
   operation that cannot reserve is refused with nothing appended.
3. The entry becomes durable (section 6.3). On a cluster it is also
   replicated until it is committed (section 7).
4. The entry is **published**: views taken from now on include it, its
   notices go out and its reply is sent.

A view never shows an entry that is not committed, so no client acts on
data a crash or a change of leader can take away. Operations of different
files run in parallel. An operation that must be all-or-nothing is one
entry, or one group (section 6.9).

New pages stay in memory until a checkpoint writes them, so a commit costs
its share of two flushes and the pages on its path.

### 6.6 Reading

A view is a reference to a file's root at a published entry. Published
pages never change, so the page cache never invalidates anything: it only
evicts, and its size is fixed by configuration. A hosted file larger than
memory is served from disk.

Encoded download replies are cached by file, path and the entry that last
changed anything under the path. A layout or script that many clients
download is encoded once per version.

Long reads hold their view: an export, a backup, a paged download, a
large find or a SQL query sees one version while writes go on.

### 6.7 Checkpoints, space and recovery

A **checkpoint** writes the pages of the published state that are not on
disk yet and the server's map of free pages, flushes them, then writes
and flushes its superblock copies. Only published entries are ever
checkpointed.

**What is kept.** The two newest checkpoints are kept whole, and so is
every checkpoint held as a restore point (section 11), which each
superblock lists. A checkpoint is retired when its superblock copies are
overwritten, and a restore point when no superblock on disk lists it. A
page is reused only when no superblock on disk names a checkpoint or
restore point that uses it, and no clone, no state copy in progress and no
view open in the process refers to it. The log is kept from the oldest
kept checkpoint, and further back while another server needs it
(section 7.5). So when the newest checkpoint cannot be used, the one
before it still has every page it names and every entry after it. The
page file keeps a reserve that only recovery, checkpoints and the discard
of a group may use, so that a store whose writes have run out of space can
still start, checkpoint and free space.

The **map of free pages** records which pages are free and how many trees
share each page (section 6.8). It is each server's own. Changes to it
since a checkpoint are rebuilt by applying the log.

**Starting up.**

1. Read `state`. With no valid copy the server does not start.
2. Of the two checkpoints on disk, newest first, take the first whose
   superblock copy is valid and whose tree roots read back with the hashes
   it names. Deeper damage is page damage (section 6.12), found when a
   page is read, and does not send the server back to an older checkpoint.
3. Read the log from that checkpoint and check it against the marker: the
   entry at the marker's index must be there, whole, with the marker's
   hash, and so must every entry between the checkpoint and it. A
   checkpoint that covers the marker's entry answers for it, as after an
   installed state (section 7.5). What follows the marker and is whole is
   kept: it is flushed and the marker moved over it before anything is
   published from it. What is damaged or incomplete after the marker is
   cut.
4. Apply the entries after the checkpoint, each once, in order, as far as
   the committed position. On one server that is every durable entry. A
   member of a cluster applies as far as the committed position its
   marker recorded and learns the rest from the leader (section 7.1).
5. Free nothing, in the log or the page file, before step 4 is done.

If the log does not hold every entry through the marker, or neither
checkpoint can be used, the server cannot produce what it confirmed. A
single server stops and reports (section 6.12). A cluster member is **in
repair** (section 7.5), as is one that `state` already records as in
repair.

### 6.8 Clones

A clone is a new file that starts as another file's tree and shares its
pages. A `file` entry makes it, and the clone is the source as it is at
that entry, so applying the log again makes the same clone. Shared pages
are counted in the map of free pages, and counts are passed down lazily
when a shared index page is copied (the method of Rodeh, "B-trees,
Shadowing, and Clones", 2008). Environments (section 9) are clones.

### 6.9 Groups

Some work must appear all at once and is too large for one entry: op73's
deferred field work on a table, an index built for a find, a deploy's
changes to records, an import, a restore. Such work is a **group**: change
entries that name the group as its parts, and a last entry that closes it.

- **Staged, then applied.** A part is written, replicated and committed
  like any entry. Applying it adds its effects to the group's staging
  area in the state, not to the file's tree. The last entry applies the
  staged effects, in order, in one step, and removes the staging area.
  Every view shows the file as it was before the group or as it is after
  it. Because the parts are in the state, a checkpoint or a state copy
  taken while a group is open has all of it.
- **What it changes.** A group declares the paths it changes. While it is
  open, the leader produces no other entry that changes those paths or
  anything above or below them.
- **What it read.** A group also declares the paths its result depends
  on, which other operations may still change: an index built for a find
  depends on its table's records, and a record can be committed while it
  is built. Before the leader appends the last entry it checks, in the
  same step, whether anything under those paths changed since the group
  began (section 6.4). If so it adds parts that bring the result up to
  date and checks again, or discards the group and starts it over.
- **Locks.** A group's first part takes the protocol locks its work
  needs, in the lock table, for the session that owns the group: for op73
  the table's exclusive lock (schema-path.md 7.2), for a deploy the locks
  of section 9. They are held until the group's last entry or its
  discard, each of which releases what the group took in the same entry.
  No other session can take a conflicting lock in between, and the last
  entry checks the group's preconditions in the same step that appends
  it.
- **One leader, one term.** A group is opened and closed by one leader in
  one term. A new leader's `empty` entry discards every group left open,
  and so does a restart of a single server, which begins a new term. The
  leader can also discard a group by an entry, and does when the session
  that owns it is closed.
- **One reply.** Only the last entry completes the operation: it carries
  the reply and the notices and advances the session's sequence number
  (section 7.3). A discarded group leaves nothing, so the same request
  sent again runs again. Work without a session, such as finishing
  deferred field work, starts again from what the file records as
  pending: for op73, the deferred list at `[T, 3, 1, fd]`, which the
  group's last entry removes.
- **Space.** Each part reserves the space for its staging and for its
  later application before it is appended (section 6.5).

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
- the entries appended and not yet published, the groups open at once and
  the size staged in them each have a limit; an operation past one waits
  or is refused;
- space to apply an entry is reserved before the entry is appended
  (section 6.5). A server that applies another's entries and runs out of
  space cannot refuse them: it stops and reports, and is in repair until
  it has space;
- pages waiting for a checkpoint have a budget. Past it a checkpoint is
  forced and operations wait. Pages a group's last entry creates may be
  written before the checkpoint that names them;
- views have a limit in number, and one held longer than a configured
  time by a SQL query is ended, so that old pages can be reused;
- a server too far behind for the log to be worth sending gets the state
  (section 7.5).

### 6.12 Storage faults

| Fault | Found by | Response |
|---|---|---|
| The log after the marker is damaged or incomplete | entry and batch hashes | cut from the first bad entry, and log the cut. Nothing after the marker was acknowledged |
| The log does not hold, whole, every entry through the marker: an entry is damaged, the log ends early, or a segment `state` lists is missing | the hash chain, against the marker and the segment list | never cut. A cluster member is in repair (section 7.5). A single server stops the store and reports; the administrator restores (section 11) or explicitly accepts the loss |
| `state` has no valid copy | its hashes | the server does not start: it does not know what it confirmed. A cluster member comes back only as a new server (section 7.5). A single server is restored (section 11) |
| A page is damaged, misplaced or missing | its parent's hash | on a cluster, fetch it (section 7.5). On one server, reads of that range fail, the file is marked damaged and the event is logged |
| A blob or a kept original image is damaged or missing | the hashes in its reference, checked whenever it is read (section 8) | on a cluster, fetch it. On one server, the read or export fails and the event is logged; damaged bytes are never served or exported |
| The newest checkpoint's superblock or roots cannot be read | hashes | use the checkpoint before it, which is kept whole (section 6.7) |
| A flush fails | the system's error | stop the server at once and recover on restart. Never retry the flush: the system may already have dropped the data (PostgreSQL's 2018 finding). An entry the failed flush covered may be found whole on restart; its outcome was unknown to its client (section 2) |
| The disk is full | a segment, page or reservation that cannot be allocated | refuse writes before appending anything, keep serving reads |
| Slow damage | a background job that reads every page and entry and recomputes its hash | as for the fault found |

**What is not found.** The store finds writes a crash interrupted, damage
to any byte it stored, the loss of a file it listed, and a log that ends
before its marker. It cannot find a device that reported both flushes as
done and then holds neither the end of the log nor the marker that
recorded it: the log and `state` have then gone back together to an
earlier pair that agrees. On a single server such a device loses confirmed
changes without a sign. On a cluster the server concerned believes its
log ends earlier than it does; section 7.1's last rule narrows what that
can do, and the comparison of section 7.5 finds it afterwards.

## 7. Replication

A cluster is a set of servers with the same log. One server is a cluster
of one, and everything below holds for it trivially.

### 7.1 Consensus

The log is replicated with Raft (Ongaro and Ousterhout, 2014), implemented
in `om-store`, with these rules as the paper gives them:

- A term has at most one leader. A server's term and vote are flushed
  before it answers a vote or an append.
- A server votes only for a candidate whose log is at least as up to date
  as its own durable log, and asks the others whether they would vote
  before it starts an election.
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
- The members in force for a server are those of the latest `members`
  entry in its log, committed or not (section 7.6).
- Beyond the paper: a server records, in its marker, the highest
  committed position it has heard of. It does not stand for election, and
  does not vote for a candidate, whose log ends before that position.

A server that restarts publishes only what it knows is committed
(section 6.7). Entries after that may be replaced by a later leader.

Raft assumes that what a server acknowledged stays on its disk. Section
7.5's repair rule is what keeps that true when a disk breaks the
assumption.

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

- **Sessions.** A session is named by its server, that server's start count
  (section 6.1) and its connection; within a start a server numbers its
  connections upward. A `session` entry opens it and one closes it. A
  server writes a `session` entry when it starts, which closes every
  session of its earlier starts and records the start count as that
  server's highest; one that names a lower count does nothing. The state
  keeps, for each server, that count and the highest connection number
  opened in it. An entry that opens a session is applied only when it names
  that count and a higher connection number, so a delayed or repeated entry
  can never open a session again. When a server is lost, the leader closes
  its sessions after the time a silent session is kept (FileMaker Server
  drops a silent session after 125 s; that a lost transport counts as
  silence is inferred, network-protocol.md 6.3). Closing a session releases
  its locks and discards its groups (section 6.9). The leader checks that
  an operation's session is open in the same step in which it runs the
  operation, and refuses it otherwise. A server that learns its sessions
  were closed drops those connections. A client that reconnects is a new
  session.
- **Locks.** A lock belongs to a lock user as today (connection, session
  id and transaction, write-path.md 4.1) within a session as above, and
  lock counts are kept per lock. Locks change only by entries, so every
  server holds the same lock table and a new leader starts with it. On a
  cluster a lock is granted when its entry is committed. A single server
  grants a lock when its entry is appended, without waiting for it to be
  durable, and views do not depend on it: if the server stops, every
  session ends, and the entry written at its next start releases their
  locks. This is the one exception to section 5's rule.
- **Notices.** A change entry carries the notices its operation raised:
  the path, the attention code, the session that caused it and whether
  that session is left out (write-path.md 6, schema-path.md 9). Each
  server delivers them, in entry order, to the connections registered with
  it (op63), applying each registration's mask as `writes::notify` does. A
  registration takes effect between two published entries: it receives
  the notices of every later entry, and what came before is in what the
  client reads. After a reconnect FileMaker Pro asks which of its cached
  items changed (op89, network-protocol.md 5.6.11).
- **Exactly once.** A session submits one operation at a time, each with
  the next sequence number, and the session table holds the last
  completed number and its reply. The leader runs the next number,
  answers the last number again with the recorded reply once its entry is
  committed, joins a request to one that is still waiting, and refuses
  anything older. A group completes its operation only by its last entry
  (section 6.9). So an operation a server sends again after a leader
  change is not run twice. This holds within a session. When a session is
  closed its recorded reply goes with it, and its client, whose
  connection is dropped, has the unknown outcome of section 2. A request
  FileMaker Pro repeats after it reconnects comes from a new session and
  is a new operation, as it is after a dropped connection on one server.

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
published every entry that was committed when the request arrived. After
the request arrives, the leader confirms that it still leads by an
exchange with a majority of the members in force. Another server asks the
leader for its committed position, which the leader gives after such an
exchange, and waits until it has published through it. Requests waiting
at the same moment share one exchange. The view a request reads, or pages
through, is taken after the barrier and kept for the whole reply. So every
answer includes every change that was answered before its request
arrived, and a server cut off from the majority answers nothing. Two
requests in flight at once may be answered in either order. A download
continued over several requests, and a read-only SQL transaction, keep
the view of their first request on purpose; the guarantee is for that
first request. No clock is involved. On one server the barrier needs no
exchange: the server waits only until it has published what was committed
when the request arrived.

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
is new, it copies a checkpoint the leader pins, and only from the leader
of the current term.

- The two walk the trees from the roots down, and a page is sent only
  when the receiver has no page with that hash at that place. The
  receiver checks the bytes of every page it keeps or receives against
  its hash, and writes index pages with its own page numbers, which the
  hash does not cover. Blobs the state names are fetched too.
- The copy is installed like a checkpoint: its pages and blobs are
  flushed and checked, then its superblock is written. Until that
  superblock, the receiver's old checkpoints stand.
- That superblock retires the receiver's older checkpoints, because the
  receiver's log no longer leads from them to the installed entry. The
  receiver keeps its own later entries only when its log holds the
  installed entry with the same term; otherwise it discards its log.
  Entries after the installed one follow from the leader.
- A server that is not in repair never installs a state older than the
  one it has published. After an install its marker is the installed
  entry, or a later entry of its own that it kept.
- A server that installs a state has skipped the entries before it, and
  their notices. It drops its client connections before it serves from
  the installed state; clients that reconnect ask what changed (op89).

**In repair.** A server that cannot produce what it confirmed - its log
does not hold every entry through its marker, neither checkpoint on disk
can be used, or it has no space to apply a committed entry - is in repair.

- **The required end.** Before anything else it records in `state`,
  flushed, that it is in repair and its **required end**: the marker's
  index as it was when it entered repair. The marker is outside the log,
  so the required end is known however much of the log is gone.
- **Nothing lowers it.** Installing a checkpoint, discarding or replacing
  its log, a restart, and any marker written while in repair leave the
  required end as it is.
- **Fenced.** It casts no vote, acknowledges no entry and serves no
  client, across restarts.
- **Leaving.** It clears the record only when it holds a usable state
  and, durable, the log of the current term's leader through at least the
  required end, each entry checked by the hash chain against what the
  leader holds. Whatever was committed with its acknowledgement is in
  that leader's log, so it then holds everything its vote must protect.
- **No valid `state`.** A server without a valid copy of `state` does not
  know its required end, and no evidence on its own disk can establish
  it. It does not start. It comes back only when an administrator removes
  it from the cluster and adds it again as a new server, which has
  acknowledged nothing.

A cluster with a majority in repair waits for an administrator, as a
single server does.

**Page repair.** A page whose bytes do not match the hash its parent
holds is damaged on this server. It is fetched from another server by its
place in the tree and checked the same way. Damage is found by
recomputing hashes from bytes, on reads and by the background job
(section 6.12), never by comparing stored hashes, which a damaged page
leaves unchanged.

**Comparison.** The leader writes a `mark` entry from time to time. Each
full server computes, at that entry, the root hash of every file's tree
and of the state's own tree, and reports them. Maps of free pages are not
compared. A server whose hashes differ first checks its own pages and
repairs any that are damaged. If its pages are sound, the difference is a
defect, and the hashes alone do not say which copy is right:

- servers in a majority that agree keep serving. A server outside it
  stops serving, keeps its files for inspection, raises an alert and is in
  repair. If it is the leader it steps down first. It copies the state
  from the leader, which is then a server of the majority;
- with no majority for either result, as between two full servers, the
  files concerned are closed on all of them until an administrator
  chooses a copy.

### 7.6 Members and witnesses

Servers are added and removed one at a time by a `members` entry, each
committed before the next, from the Admin Console or the command line. A
new server first follows without a vote, and is given its vote by a second
entry once it has caught up. A server uses the latest `members` entry in
its log from the moment it is appended, for elections and for counting a
majority.

A **witness** is a member that keeps the log and the blobs its entries
name, and votes. It holds no trees and serves no clients, so two full
servers and a witness survive the loss of any one of them.

- A witness applies `members` entries and nothing else. It keeps, flushed,
  the members in force and the index, term and hash of the entry before
  the first one it still holds.
- It can win an election, by the ordinary rules, when it holds entries the
  remaining full server lacks. As leader it runs no operation and answers
  no client. It writes its `empty` entry, sends the full server what it
  lacks, and hands leadership to it (Raft's leadership transfer) once
  that server has every entry durable. Clients wait meanwhile.
- For a witness, committing its `empty` entry is all that section 7.1
  asks of a new leader: it has no state to publish.
- It discards an entry, and the blobs only that entry names, once the
  entry is committed and every full member has it durable.
- A witness too far behind for the log gets, from the leader, the index,
  term and hash of a committed entry and the members in force there, and
  the entries after it.

## 8. Container data

A large value is a node with a header child and pieces of 1,000 bytes
(file-format.md 3.6). The store moves such a node out of the tree into
`blobs/`, named by the SHA-256 of its content, when it is larger than a
threshold and has exactly that layout. The tree keeps a reference with the
length and the header, and a view shows the node's children as they were.
A value with any other layout stays in the tree. Export writes the pieces
back.

- **Before the entry.** A blob is written and flushed, its folder is
  flushed, and it is read back against its hash, before the entry that
  names it is appended. A server acknowledges an entry only when it holds
  every blob the entry names in that way; blobs travel beside the log,
  not in it.
- **Counted in the state.** Identical content is stored once across files,
  clones and versions, and the state counts the references to it.
- **Checked when read.** A reference holds the blob's hash and a hash of
  each part of it. Every part is checked as it is served or exported, the
  background job (section 6.12) reads every blob and kept original image,
  and a part that fails is fetched from another server or fails the read.
- **Removed late.** A blob is removed only at a checkpoint, and only when
  no kept checkpoint, clone, kept entry, open group, open view, state copy
  or operation in progress names it. Operations register a blob in a
  table of blobs in use before they rely on it, and the collector removes
  only what that table does not hold, under the same lock.

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
   or one the administrator names.
2. **Identity.** With a base, an object of the source is the target's
   object of the same kind with the same id when the base has that object
   too, and an object absent from the base is new in the source. Two
   environments can give the same id to unrelated new objects: a new
   object whose id the target has used for another object is given a new
   id, with every reference to it rewritten, when OpenMaker decodes every
   place that can refer to it. Otherwise it is refused. Without a base,
   an object of the source is the target's only when kind, id and name all
   agree; one that agrees in id or in name alone is shown to the
   administrator, who says which it is, and is never matched
   automatically.
3. **Differences** are found against the base: changed in the source,
   changed in the target, or changed in both, which is a conflict the
   administrator settles per object. Without a base every difference is
   shown as one. Putting an object back to an earlier revision is a
   change like any other. Each difference is classed: a member replaced; a
   table's fields changed; or a change every record must follow (a field's
   type, a stored calculation, an index, a deleted field), which runs as a
   group (section 6.9).
4. **Selection.** The administrator selects. The selection is widened by
   what it needs and refused if it would leave a reference broken. Access
   paths are never deployed. A member is written path by path: its Design
   paths from the source, its Data paths kept from the target (a serial
   field keeps its next value). A member holding a path that is in no
   group is not deployed over an existing one. A changed privilege set is
   shown with the accounts it affects, and one that accounts use cannot be
   deleted.
5. **Apply** is one write operation, in a sequence of OpenMaker's own, as
   the authoring extension's edits are today (authoring.md, "How writes
   reach the file"). It does not replay FileMaker Pro's requests. For each
   kind of object it takes, all at once, the locks FileMaker Pro holds
   while it saves that kind - Manage Database's for tables, fields,
   occurrences and relationships (schema-path.md 4), the Script
   Workspace's for scripts ([scripts.md](re/scripts.md), "Saving scripts
   to a host") - and fails with their errors when a client holds one.
   Under those locks it checks again everything the preview relied on:
   each target object's revision, the references the selection needs, and
   the accounts bound to its privilege sets. It fails with a conflict if
   any changed. It writes the members and catalog indexes with new
   modification stamps (schema-path.md 8) in one entry, and raises the
   notices FileMaker's releases of those locks raise (schema-path.md 9). A
   kind whose locks and notices have no spec in `docs/re/` is not deployed
   until it has one.

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
  a find matches by the index's language. An index is used only when the
  rows it gives are certain to include every row the SQL comparison
  accepts, as for equality; each is then checked again with the SQL
  comparison (execute-sql.md 10.1, inferred there). An index's order is
  not SQL's, so a range comparison scans.
- **Two dialects, one planner.** FileMaker's SQL for `ExecuteSQL`, and a
  Postgres-compatible dialect for the endpoint below, because clients of
  that protocol send Postgres syntax. Both use sections 5 and 6 of
  execute-sql.md for types, nulls, comparison and order, so one statement
  gives the same rows in either.
- **Endpoint.** The Postgres wire protocol over TLS on its own port, off
  by default. The database is a hosted file. Sign-in is with the file's
  accounts, needs the `fmxdbc` extended privilege, and the account's
  privilege set applies: record access formulas and per-field access.
  Enough of the Postgres system catalogs is answered for common clients
  and drivers. A global field has the value a new session sees.
- **Types.** A FileMaker field holds whatever was typed. A typed column
  gives the value when it parses as the field's type and null when it does
  not; the stored text is always available as text.
- **Reads.** A statement, or a read-only transaction, runs on one view, on
  any server, behind the read barrier (section 7.4). A table with
  deferred field work pending (section 6.9) is refused with an error
  until the work is done.
- **Writes.** INSERT, UPDATE and DELETE are write operations, so they run
  on the leader: the statement's rows are found again in the leader's
  working state, never taken from the view a follower planned with. A
  statement is all-or-nothing. It fails whole when a row is locked by
  another session, as a script's edit of that record would. The server
  runs auto-enter options, validation and stored calculations (`om-calc`,
  `om_data::rules`), keeps indexes and modification counts, and notifies
  clients, as for a record a server script writes. Script triggers do not
  run. Container, calculation and summary fields cannot be written. Each
  write is its own transaction: a write inside BEGIN, and a statement
  that changes the schema, are refused with an error.
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
  after it, at a configured interval and for a configured time (section
  6.7). A file is restored by the leader as a **new file with a new id and
  a name of its own**, in a group: the file as the restore point holds it,
  then its kept entries up to the chosen one. Where the file was made by a
  clone after the restore point, its source is rebuilt the same way first.
  What is rebuilt is the file's whole state: its tree, page 0, container
  file list, kept original image and history, and no session, lock or
  notice. The new file is visible only by the group's last entry. A time is
  turned into an entry by taking the last entry before the first one the
  leader stamped later than that time; where a clock jumped, that is the
  order of the log, not of the clocks. Giving the restored file the old
  file's name is the administrator's step, after removing the old file,
  which closes its sessions; it is what replacing a file with a backup is
  on FileMaker Server, and clients that open it ask what changed (op89).
- **Formats.** Entries, pages and the superblock carry a format version. A
  newer server reads older formats. Until the format is declared stable,
  moving between versions may need export and import.
- **Secrets.** The store folder is readable by the server's account only.
  The store is not encrypted at rest (section 14).

**Administration is commands** (AGENTS.md rule 3), defined in `om-server`,
which owns the store. Each has a stable id, typed parameters and a typed
result, needs the administrator's sign-in, and is unavailable, with the
reason, on an engine that lacks its capability. The Admin Console
(`om-admin`), the Admin API and the command line call the same operation.

| Command | Runs as |
|---|---|
| import, export, clone, rename and remove a file; set a restore point; restore a file | entries, run by the leader from any server |
| add, remove or promote a member; hand over leadership; raise the tree format version | `members` entries, run by the leader |
| check every page and entry now; repair a page; copy the leader's state | on one server |
| accept the loss of entries after damage on a single server (section 6.12); choose a copy after a comparison no majority settles (section 7.5) | on the servers concerned, with the entries or files named |
| export from a store folder with no server running | the command line only |

## 12. Testing

Reliability is decided here more than in any structure above.

**The engine runs in a simulator.** `om-store` reaches disk, network,
clock and randomness only through traits. The simulator implements them in
memory and runs a whole cluster in one thread from a seed, injecting:
crashes at any point, half-written and lost writes, failed flushes, damaged
and misplaced pages, entries and markers, a full disk, delayed, dropped,
repeated and reordered messages, split networks and clocks that jump. A
failing seed replays exactly. After every step it checks:

1. every published version equals a reference model, an in-memory map of
   paths with a lock table, given the same operations;
2. no operation that was answered as stored is ever lost, and none runs
   twice within its session;
3. servers at the same entry have the same root hashes;
4. a view never changes, and never shows an entry that is not committed;
5. every answer to a single request, and to the first request of a
   continued download or read-only transaction, includes every change
   answered before it arrived;
6. no two sessions hold locks that conflict, and no closed session's
   operation runs;
7. an export of a view, imported again, equals the view node by node;
8. a group is seen whole or not at all, and its result agrees with what
   it depended on at the entry that closed it;
9. a restore to an entry equals the reference model at that entry;
10. a server in repair never votes or acknowledges, and never leaves
    repair short of the position it had confirmed;
11. every server starts from any state the faults above can leave, or
    says why not;
12. no damaged blob or page is ever served or exported;
13. a group never completes while another session holds a lock that
    conflicts with the ones it took.

Clusters with a witness, members joining and leaving, state copies, and a
leader lost at each step of each of these are part of the generated
schedules.

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
| One flush per commit, with the next batch's header as the only record that a flush succeeded | The last confirmed batch would have no record, so damage to it could not be told from a write a crash interrupted, and a cluster member could cut an entry it had acknowledged |
| Checking every page of a checkpoint at each start | Restart time would grow with the store. Roots are checked at start, every page when it is read, and all of them by the background job |
| A primary with one standby fed by log shipping | No protection against two primaries, and a failover can lose confirmed commits |
| Every server running each operation itself | Needs identical calculation, collation and clock behaviour on every server and across versions |
| Locks kept only in the leader's memory | A failover would empty the lock table while clients still edit records |
| Reads without the barrier, or under a lease kept by clocks | A server that is behind, or a leader that was replaced without knowing, would answer from an old state. A lease is safe only while clocks run at nearly the same rate |
| A table update published as it goes | A view, an export or a backup could hold a table partly converted (section 6.9) |
| A group's parts kept only in the log | A checkpoint or a state copy taken while the group is open would lack them |
| A full server that copies a witness's entries before standing for election | A new election rule to prove. A witness that wins by the ordinary rules and hands over needs none |

## 14. Limits and unknowns

- **FileMaker Pro after its host is gone.** That it opens the file afresh
  and keeps a list of its locks is from its code (network-protocol.md
  6.3); what it does with unsaved edits and locks has not been run, and
  where it gets a host's standby addresses is not known. Until it has
  been run, a cluster serves clients from the leader only (section 7.4).
- **FileMaker Pro when a layout it shows, or a script it runs, changes.**
  Unmeasured. Deploys raise FileMaker's own notices, so they are no worse
  than a developer's live edit, which is also unmeasured.
- **A transaction across files.** Whether FileMaker Server undoes one
  file's commit when another's is refused is not shown by the capture
  (write-path.md 1.5).
- **A file with deferred field work pending.** Between a field catalog's
  upload and the end of its op73, a view holds the new field definitions
  with the old records and the deferred list, as the protocol leaves the
  file then. SQL refuses such a table (section 10). An export taken then
  holds the deferred list; OpenMaker finishes that work when it hosts the
  file (`Store::recover`), and what FileMaker does on opening such a file
  is unmeasured.
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
- **A single server finds damage and cannot repair it.** It stops or
  fails the reads concerned (section 6.12); backups and a cluster are the
  remedy.
- **A device that loses writes it confirmed**, with every record of them,
  is outside what the store can find (section 6.12).
- **Two developers' parallel changes** merge by section 9's rules.
  Objects OpenMaker does not decode cannot be given new ids, so such a
  merge can be refused.
- **Copies of production data for development** with records masked or
  sampled are not designed.
