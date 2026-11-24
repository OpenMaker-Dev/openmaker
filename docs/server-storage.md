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
[#673](https://github.com/OpenMaker-Dev/openmaker/issues/673). The store
engine removes these costs. The file engine stays: it serves local files
in the desktop client (`om_server::LocalHost`,
[architecture.md](architecture.md), "Local files use the hosted path"),
it is the default until the maintainer switches it, and the store engine is
tested against it (section 12).

## 2. The contract

Both engines keep these, and a difference between them is a bug:

1. **The wire.** For the same sequence of requests, a client gets the same
   bytes: downloads, find results, record ids, serial numbers, modification
   counts, lock grants and refusals with their error codes, and
   notifications. FileMaker Pro cannot tell which engine it talks to.
2. **Always an `.fmp12`.** A hosted file can be exported at any time as an
   `.fmp12` that holds exactly its key tree and its page 0, which FileMaker
   opens. Import followed by export changes no key and no value.
3. **Nothing dropped.** Keys and values the server does not understand are
   kept and exported as they came.
4. **Stored means durable.** A reply that says a change is stored is sent
   only after the change is on disk (on a cluster: on a majority of
   servers). A change that cannot be stored is refused or taken back
   whole, and the reply says so.
5. **Explicit refusals.** What an engine cannot do fails with an error,
   never as a silent success (AGENTS.md rule 4).

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
that are children of the value's node (file-format.md 3.6), so no single
value outgrows a page.

Both engines keep this model. The store engine does not convert a file
into tables of its own:

- the protocol works on it: FileMaker Pro downloads subtrees by path in the
  file's own slot encoding ([network-protocol.md](re/network-protocol.md)
  5.6.6), and uploads record subtrees ([write-path.md](re/write-path.md));
- rule 3 of the contract holds without a codec for every catalog;
- export is a walk of the tree.

Besides its tree, a file has its **page 0** (the superblock image the
`.fmp12` writer needs) and its name. Tables, columns and SQL types are
views computed from the tree (section 10), never the stored form.

**Path groups.** For history and deploys (section 9) the paths of a file
are sorted into three groups. The grouping is OpenMaker's and changes
nothing about how a file is stored.

| Group | Holds | Changed by |
|---|---|---|
| Design | tables, table occurrences, relationships, field definitions, layouts, themes, scripts, custom functions, custom menus, value lists, privilege sets, extended privileges | developers |
| Data | each table's records, indexes, record list and counters; container data | users |
| Access | accounts | administrators |

The paths of each group are listed in one place in code, from
file-format.md 3.4 and [schema-path.md](re/schema-path.md) 5. A path in no
group belongs to the file as a whole: it is exported, and never deployed.

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
read lock, as readers do today.

**Write operations.** Every change the server makes is a value of one
type, a write operation: commit records (op70), reserve record ids (op71),
advance a serial (op72), upload to a catalog (op13 and op86's
UploadMultiple and DeleteTable), update a table (op73), store an index
built for a find, store or release container data (op88, op87), an
authoring edit (op240), a deploy (section 9), and taking, upgrading and
releasing locks (op60, op61, op62, op65). One function runs an operation
against a transaction and the file's lock table and returns its result, the
notices for other clients, and what changed. A transaction is a view plus
put, delete and replace-subtree; if the operation fails, nothing it did
remains. Operations on one file run one at a time, in one order.

Writes are values, and not closures over the store, so that they can be
sent to another server (section 7.2), recorded with their result
(section 7.4) and replayed in tests (section 12).

**The engine.** An engine gives views, submits write operations, exports
and imports, and says what else it can do:

```text
view(file)              -> View                    never blocks on the store engine
submit(file, operation) -> result, notices         returns once the change is durable
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
before it returns; a failed save takes the operation back and is the
operation's error. No reply reports success after a failed save. A
leftover `.omsave` from a save that never finished is removed when the
server starts and holds the file's lock.

## 6. The store engine

Crate `om-store`. It knows paths, values, files, entries and pages. It
knows nothing of records, catalogs or the protocol.

### 6.1 On disk

One store folder per server, used by one process at a time (a lock file):

```text
<store>/log/       the change log, in segments
<store>/pages      the page file: superblock slots, then pages
<store>/blobs/     container data by content hash (section 8)
<store>/state      this server's id, and its term and vote (section 7.1)
```

### 6.2 The log

Every change is first an **entry** in the log. An entry has an index (its
position, counted from 1), a term (section 7.1), a kind, the file it
belongs to, a payload, and a SHA-256 hash over the previous entry's hash
and its own content. The chain of hashes shows a missing, reordered or
altered entry.

| Kind | Payload |
|---|---|
| change | one write operation's effects on one file: tree edits (put a leaf, put a list, delete a subtree), lock table edits, notices, the operation's id and its result |
| file | a file added, removed, renamed or cloned |
| members | the cluster's servers (section 7.6) |
| mark | a named point: a release, a restore point |
| empty | a new leader's first entry |

An entry holds **effects**, not the request: the edits the operation made,
as computed by the server that ran it (section 7.2). Applying an entry is a
function of the tree before it and the entry alone.

An entry is **durable** on a server when the log is flushed through it.
Appends wait for no timer: one flush covers every entry appended while the
previous flush ran. Segments are opened for direct, synchronous writes
where the system offers them, so that a failed flush cannot be hidden by
the system's cache (section 6.10).

Entries of different files share the log, so they share flushes, and a
cluster has one election and one replication stream. Each entry names its
file, so one file can be restored or replayed without the others.
FileMaker Pro commits a transaction across files as one op70 per file,
each answered before the next is sent (write-path.md 1.5 and 3.5), so the
protocol gives no all-or-nothing commit across files for a shared log to
keep.

### 6.3 Trees and pages

Each file's tree is a B+tree of fixed-size pages (16 KiB) in the page
file. Its keys are paths in an encoding that sorts as the key tree does:
each segment's bytes with `00` written as `00 ff`, followed by `00 01`. A
subtree is then one contiguous key range, in depth-first order. An item's
value is a kind byte (list, value, or both) and the node's value. Any
segment, any depth and a node that is both can be stored, so that a file
read with such content is kept as it was read (and hosted read-only,
section 6.8); the limits of section 4 are checked where writes are checked
today, not by the tree.

Pages are **copy-on-write**: a change writes new pages for the leaf and
for every page above it, and the old pages stay as they were. An index
page holds, for each child, its first key, its page number, its hash and
the index of the entry that last wrote it. A page's hash is SHA-256 over
its items and, for an index page, over its children's hashes, without
their page numbers. So:

- reading a page checks it against the hash its parent holds, which finds
  damaged pages, pages written to the wrong place and pages never written.
  The `.fmp12` format has no such check (file-format.md 5);
- a tree's root hash depends on its content and shape, and not on where a
  server put its pages. Applying an entry is deterministic, so every
  server has the same root hash for a file at the same entry index;
- the store can tell from a few pages whether anything under a path changed
  since a given entry.

The **superblock** names, for a checkpoint (section 6.6), the entry index
it covers and the root of every file's tree, and carries its own hash. It
is written to one of several slots in turn; the newest valid slot is the
one used.

### 6.4 Writing

1. A connection's handler checks what it can against a view and builds a
   write operation.
2. The file's executor runs operations one at a time against the working
   tree, which includes every earlier operation, and appends each one's
   entry.
3. The log is flushed. On a cluster the entry is also replicated
   (section 7).
4. The entry is **published**: views taken from now on include it, its
   notices go out and its reply is sent.

A view never shows an entry that is not durable, so no client acts on
data a crash can take away. Operations of different files run in
parallel. An operation that must be all-or-nothing, such as an op70 with
many records, is one entry.

New pages stay in memory until a checkpoint writes them, so a commit costs
one share of a log flush and the pages on its path.

### 6.5 Reading

A view is a reference to a file's root at a published entry. Published
pages never change, so the page cache never invalidates anything: it only
evicts, and its size is fixed by configuration. A hosted file larger than
memory is served from disk.

Encoded download replies are cached by file, path and the entry that last
changed anything under the path. A layout or script that many clients
download is encoded once per version.

Long reads hold their view: an export, a backup, a large find or a SQL
query sees one version while writes go on.

### 6.6 Checkpoints and recovery

A **checkpoint** writes the unwritten pages of a published version to free
space, flushes them, then writes and flushes a superblock slot. Pages a
checkpoint no longer references are free for reuse only after that
checkpoint is durable and no view or clone needs them.

Starting up: take the newest valid superblock, then apply the log's
entries after its index. Applying an entry twice gives the same tree. No
step needs an administrator.

The log before the oldest needed checkpoint is kept as long as replication
(section 7.5) or point-in-time restore (section 11) needs it, then
removed.

### 6.7 Clones

A clone is a new file that starts as another file's tree at one version
and shares its pages. Shared pages are counted, and counts are passed down
lazily when a shared index page is copied (the method of Rodeh, "B-trees,
Shadowing, and Clones", 2008). Environments (section 9) are clones.

### 6.8 Import and export

**Import** reads an `.fmp12` through `om-hbam` leaf by leaf and writes its
items as change entries in chunks, then one `file` entry that makes the
file visible. It refuses what the server refuses to host today (encrypted
files, `crates/om-hbam/src/container.rs`), and a file holding content
beyond the limits of section 4 is hosted read-only, as today.

**Export** walks a view in key order into the bulk writer of
[hbam-writer.md](re/hbam-writer.md) 7.2, which writes leaves, then index
levels, then the root, with memory bounded by the tree's depth and not its
size. The result is checked by reading it back and comparing it with the
view path by path. Export is offered:

- at any time from a running server (Admin Console, `om-author`), from a
  view, without holding up writes;
- by backup schedules: a backup keeps FileMaker Server's layout
  ([admin-console.md](admin-console.md), "Backups"), and each file's copy
  in it is an export from one view;
- when the server stops cleanly, into a configured folder;
- by `om-server export`, which reads a store folder with no server
  running.

### 6.9 Long operations

Work that touches many records and need not be all-or-nothing runs as a
job that commits in bounded chunks: op73's deferred field work, an index
build, a field type change, an import. Other operations run between its
chunks. The locks FileMaker's protocol already takes for such work (a
table's `[T, 5]`, schema-path.md 4) give it the isolation FileMaker gives
it. A job's pending work is recorded in the file, as deferred field work is
today (schema-path.md 7.2), so it resumes after a restart.

### 6.10 Storage faults

| Fault | Found by | Response |
|---|---|---|
| The last entry is half written | its hash, with nothing valid after it | cut it off; it was never durable |
| An entry is damaged and valid entries follow, or it is not in the last segment | the hash chain | never cut. On a cluster, fetch it from another server. On one server, stop the store and report; the administrator restores (section 11) or explicitly accepts the loss |
| A page is damaged, misplaced or missing | its parent's hash | on a cluster, fetch it (section 7.5). On one server, reads of that range fail, the file is marked damaged and the event is logged |
| A flush fails | the system's error | stop the server at once and recover on restart. Never retry the flush: the system may already have dropped the data (PostgreSQL's 2018 finding) |
| The disk is full | the write's error | refuse writes, keep serving reads |
| Slow damage | a background job that reads every page and entry | as for the fault found |

## 7. Replication

A cluster is a set of servers with the same log. One server is a cluster
of one, and everything below holds for it trivially.

### 7.1 Consensus

The log is replicated with Raft (Ongaro and Ousterhout, 2014), implemented
in `om-store`: terms, one leader per term, entries committed once a
majority has them durable, an election restriction so that only a server
with every committed entry can lead, a vote check before a server starts
an election, and a leader that steps down when it cannot reach a majority.
A server's term and vote are flushed before it answers.

An entry is published (section 6.4) when it is **committed**. Servers
talk to each other over TLS with certificates both ends check, on a port
of its own.

### 7.2 Who executes

Only the leader runs write operations. It appends each one's effects, and
the other servers apply effects. A server that received a client's request
sends the operation to the leader and waits for the entry.

So the servers need not compute the same result from the same request,
which they could not promise across a rolling upgrade: calculation,
collation and index-key code may differ between versions, and clocks
differ. They only need to apply edits the same way.

### 7.3 Sessions, locks and notices

- **Locks.** A lock is taken, upgraded or released by a write operation,
  and the lock table is changed only by applying entries, so every server
  holds it and a new leader starts with it. The lock table lives in memory
  and is sent with the trees to a server that joins. It is not needed after
  every server has stopped, since no session survives that; a single
  server therefore does not flush for it.
- **Sessions.** A session is known to the cluster by its server and
  connection. When a server is lost, its sessions are treated as silent
  ones: the leader releases their locks after the time a silent session is
  kept (network-protocol.md 6.2 and 6.3). A client that reconnects through
  another server is a new session, as it is after a network drop today.
- **Notices.** An entry carries the notices its operation raised
  (schema-path.md 9, write-path.md 6). Each server delivers them to the
  connections registered with it (op63) when it publishes the entry.
- **Exactly once.** An operation carries its session and a sequence
  number, and its entry records its result. An operation sent again after
  a leader change returns the recorded result and is not run twice.

### 7.4 Clients

FileMaker's protocol has no redirect (network-protocol.md 8), and
FileMaker Pro recovers from a lost host by opening the file afresh
(network-protocol.md 6.3, from its code).

**Leader only.** Clients are served by the leader. Every other server
relays an incoming client connection to the leader unread, so a client may
dial any server, or one address in front of them. A leader change drops
every client, and each reconnects. This is the mode until the FileMaker
Pro runs of section 14 are done.

**Every server.** Each server takes clients:

1. It reads from its own published view.
2. It sends write operations to the leader and answers the client only
   after it has published the entry itself, so a session always reads its
   own writes, and a lock's modification count is never older than the
   lock.
3. A server that has not heard from the leader for a bound holds its
   clients' requests, and closes its sessions after the silent-session
   time.

Losing a server then drops only that server's clients, and a leader
change is a pause in writes for the others.

### 7.5 Catching up and repair

A server that is behind gets entries from the leader's log. When the log
no longer reaches back far enough, or the server is new, it copies trees:
the leader pins a checkpoint, the two compare root hashes and walk down,
and only pages whose hashes differ are sent. A page that fails its hash
(section 6.10) is fetched the same way.

Servers at the same entry index compare root hashes as a standing check.
A difference is a bug or damage; the server that disagrees with the
majority stops serving and repairs itself.

### 7.6 Members and witnesses

Servers are added and removed one at a time by a `members` entry, from the
Admin Console or the command line. A **witness** is a member that keeps
the log and votes but holds no trees, serves no clients and never leads,
so two full servers and a witness survive the loss of any one of them. A
full server that finds the witness ahead of it copies the witness's
entries before it stands for election. That step is an addition to Raft
and is accepted only with its proof in the simulator (section 12).

## 8. Container data

A large value is a node with a header child and pieces of 1,000 bytes
(file-format.md 3.6). The store moves such a node out of the tree into
`blobs/`, named by the SHA-256 of its content, when it is larger than a
threshold and has exactly that layout. The tree keeps a reference with the
length and the header, and a view shows the node's children as they were.
A value with any other layout stays in the tree. Export writes the pieces
back.

Identical content is stored once across files, clones and versions, and
is removed when no published tree, clone or retained version refers to
it. On a cluster a server acknowledges an entry only when it holds every
blob the entry names; blobs travel beside the log, not in it.

Container data FileMaker stores outside the file
([#21](https://github.com/OpenMaker-Dev/openmaker/issues/21)) is a
different thing: it is part of how the file is laid out for FileMaker,
and import and export carry it as FileMaker lays it out.

## 9. Design history, analysis and deploys

These are commands (AGENTS.md rule 3) in `om-author`'s catalog. The Admin
Console, the CLI and MCP tools call the same operations.

**Objects and revisions.** A design object is a catalog member: a layout,
a script, a table, a field, a value list, a custom function, a theme, a
menu, a privilege set, a relationship, a table occurrence. Its revision is
the `sha256:` of its stored content without modification stamps, the
definition `om-author` already uses ([authoring.md](authoring.md),
"Revisions").

**History.** For every change entry that touches a Design or Access path,
the store records the entry, the time, the account, the operation and each
object's revision before and after, and keeps each revision's content
once. That covers deploys, `om-author` edits and a developer's edits from
FileMaker Pro alike. For Access paths only the fact of the change is
kept, never account content. History answers: what changed between two
points, who last changed an object, and an object's content at any
revision. One object or a whole earlier state can be put back as a
deploy.

**Text form.** One text file per object: readable text where OpenMaker
decodes the object (scripts in the form `get_script` returns), the rest as
hex under its path, stamps left out. Text must read back to the same
revision; an object for which it does not is written as hex only. The
Access group is never written. The text form is for diffs, review and an
optional mirror that writes each revision into a folder for git. The
store's history is the master copy, since live edits arrive there.

**Analysis.** From the decoded references between objects, for any
revision: where an object is used, broken references, objects nothing
refers to, references made by name at run time (scripts run by name,
ExecuteSQL text, `Evaluate`), script call chains, and a security review
(accounts without a password, scripts that run with full access).

**Deploy.** Bringing selected design changes from a source into a hosted
file without closing it:

1. The source is an uploaded `.fmp12`, another file in the store, or a
   revision of the text form.
2. Objects are matched by FileMaker's id, then by name. An object of the
   source counts as unchanged when its revision is one the target has had.
3. The differences are shown per object. Each is classed: a member
   replaced; a table's fields changed, which needs the table's lock; or a
   change every record must follow (a field's type, a stored calculation,
   an index, a deleted field), which runs as a job (section 6.9).
4. The administrator selects. The selection is widened by what it needs
   and refused if it would leave a reference broken. Access paths are
   never deployed.
5. The apply is one write operation. It takes the locks Manage Database
   and the Script Workspace take (schema-path.md 4), fails with their
   errors when a client holds one, writes the members and catalog indexes
   with new modification stamps (schema-path.md 8), and releases the locks
   with FileMaker's notices, on which clients fetch the catalogs again
   (schema-path.md 9).

An object of the source that has the id of a different object in the
target is given a new id, and every reference to it is rewritten, when
OpenMaker decodes every place that can refer to it. Otherwise that object
is refused.

**Environments.** A development or test copy of a hosted file is a clone
(section 6.7) under another name. Design moves between environments by
deploy; data and accounts stay where they are.

## 10. SQL

One query engine serves FileMaker Pro's finds (op86, `om-query`) and SQL
(`om-sql`): both read a view, use the file's own indexes and compare and
sort text with `om-collate`, so SQL answers as `ExecuteSQL` does.

- **Two dialects, one planner.** FileMaker's SQL
  ([execute-sql.md](re/execute-sql.md)) for `ExecuteSQL`, and a
  Postgres-compatible dialect for the endpoint below, because clients of
  that protocol send Postgres syntax.
- **Endpoint.** The Postgres wire protocol over TLS on its own port, off
  by default. The database is a hosted file. Sign-in is with the file's
  accounts, needs the `fmxdbc` extended privilege, and the account's
  privilege set applies, record access formulas included. Enough of the
  Postgres system catalogs is answered for common clients and drivers.
- **Types.** A FileMaker field holds whatever was typed. A typed column
  gives the value when it parses as the field's type and null when it does
  not; the stored text is always available as text.
- **Reads first.** Queries run on a view, on any server of a cluster.
- **Writes.** INSERT, UPDATE and DELETE become write operations. The
  server runs auto-enter options, validation and stored calculations
  (`om-calc`, `om_data::rules`), keeps indexes and modification counts, and
  notifies clients, as for a record a server script writes.
- **OpenMaker's client.** A protocol extension of OpenMaker's own
  (architecture.md, "OpenMaker's own protocol extensions") lets the
  desktop client have the server sort, summarize and run `ExecuteSQL`,
  where FileMaker's protocol makes the client download the records.

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
  exports from a view (section 6.8).
- **Restore to a point in time.** A file is restored as a new file from a
  checkpoint and the kept entries of that file up to the chosen entry or
  time. How long entries are kept is a setting.
- **Formats.** Entries, pages and the superblock carry a format version. A
  newer server reads older formats. Until the format is declared stable,
  moving between versions may need export and import.
- **Secrets.** The store folder is readable by the server's account only.
  The store is not encrypted at rest (section 14).

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
   paths, given the same operations;
2. no acknowledged operation is ever lost, and none runs twice;
3. servers at the same entry index have the same root hashes;
4. a view never changes, and never shows an entry that is not committed;
5. an export of a view, imported again, equals the view.

**Both engines behind one server.** Every server test runs against both
engines, and generated request sequences are sent to both through the real
connection code, with replies compared byte for byte.

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
| A primary with one standby fed by log shipping | No protection against two primaries, and a failover can lose confirmed commits |
| Every server running each operation itself | Needs identical calculation, collation and clock behaviour on every server and across versions |
| Locks kept only in the leader's memory | A failover would empty the lock table while clients still edit records |

## 14. Limits and unknowns

- **FileMaker Pro after its host is gone.** That it opens the file afresh
  and keeps a list of its locks is from its code (network-protocol.md
  6.3); what it does with unsaved edits and locks has not been run. Until
  it has, a cluster serves clients from the leader only (section 7.4).
- **FileMaker Pro against a server that is slightly behind.** Unmeasured.
  Section 7.4's rules are designed to make it invisible.
- **FileMaker Pro when a layout it shows, or a script it runs, changes.**
  Unmeasured. Deploys use FileMaker's own locks and notices, so they are
  no worse than a developer's live edit, which is also unmeasured.
- **Encrypted files** cannot be opened
  (`crates/om-hbam/src/container.rs`), so they cannot be imported, and the
  store has no encryption at rest of its own.
- **FileMaker's ODBC and JDBC protocol** is not studied; section 10's
  endpoint is not it.
- **Host time** (op82) comes from each server's clock. Servers of a
  cluster need synchronized clocks; a rule for refusing to serve when they
  drift is not designed.
- **Two developers' parallel changes** merge by section 9's matching and
  renumbering. Objects OpenMaker does not decode cannot be renumbered, so
  such a merge can be refused.
- **Copies of production data for development** with records masked or
  sampled are not designed.
