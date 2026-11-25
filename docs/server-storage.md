# OpenMaker Server storage

How OpenMaker Server keeps hosted files: its two storage engines, the
interface between them and the rest of the server, and what is built on the
store engine (replication, design history and deploys, SQL). All of it is
OpenMaker's own design. FileMaker Server works differently inside
([hbam-writer.md](re/hbam-writer.md) 6.1), and nothing here is compared with
it except the contract in section 2.

**Status: design.** The file engine (`crates/om-server/src/store.rs`)
hosts every file. Of the engine interface, the write
operations are built: every change the server makes to a hosted file or
its lock table goes through them, and every save's outcome is reported
(section 5). Views are built, and every reader outside server scripts
and the operations' own reads reads through them: downloads, finds,
logins, thumbnails, op88's check of a library, Describe Security, the
desktop's private host and a schedule's login. `export` is built, and
backups and the desktop's Save a Copy as make their copies with it. What
the engine says of a file that is not a version's (whether it takes
changes, its file on disk) it offers itself. Server scripts and the
operations' own reads still hold the file engine's store (section 5,
"Moving the other readers"). Still to come: `import` and `capabilities`
(section 5). Of the store engine, `om-store` holds the `state` file
(section 6.1), the entries and applying them to the state's trees (6.2,
6.3), the page trees (6.4), the page file with its checkpoints, restore
points and the open that chooses a checkpoint (6.7), and the simulator
(12); `om-server` reads a file's tree in it as a view, imports an
`.fmp12` into one and exports it again (section 5, "The store engine's
file"). No file is hosted on it yet: the log's segments, the store behind
the engine interface, groups, clones, replication and everything else in
sections 6 to 12 are specified here and not built.
[#673](https://github.com/OpenMaker-Dev/openmaker/issues/673) lists the
work. A section is rewritten in place when its part is built.

## 1. Why a second engine

The file engine loads each hosted `.fmp12` whole into memory as its key
tree and writes every change back as a complete new file, renamed over the
old one, while it holds the file's write lock (`Store::save`,
`Hosted::save_change`). That is simple and never leaves a half-written file.
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
   only after the change is on disk (on a cluster: once it is committed,
   which rests on a majority of servers each having acknowledged it
   durable, section 7.1). An operation has one of three outcomes: stored, and the reply
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

**Status.** The write operations are built, with the file engine behind
them (`crates/om-server/src/engine/`), and every change the server makes
to a hosted file or its lock table is one: a client's op70, op71, op72,
op60, op61, op62, op65, op13, op73, op86's schema instructions and stored
indexes, op87, op88, and op240's edits and its Save; a server script's
reservations, serials, stored indexes, commit (with the container data it
stores and counts), Set Next Serial Value, locks on a record and on the
accounts catalog and their releases, account steps and the save at its
end; the release of a session's locks when its connection ends, it logs
out of a file or closes it, or a server script's session ends; the
desktop's own Change Password and its saved found sets; and the deferred
field work a file is opened with, with its save. Only the engine can take
a file's store for writing or change its lock table (`engine::Writer`),
and code elsewhere that tries does not compile; everyone else gets the
lock table to read (`Hosted::lock_table`), and a whole-file copy is an
export (`engine::export`, below), which opens the file on disk only to
read it (`Store::open_saved`). The exceptions:

- test switches: `Hosted::fail_commit_saves` and `Hosted::fail_saves`
  (debug builds, behind `ServerHandle::failing_commit_saves` and
  `ServerHandle::failing_saves`), which set a flag of the store and never
  its tree, and `engine::Writer::for_tests` (test builds);
- reads that hold the lock table: a connection's count of its locks, and
  a debug-build test that holds every lock table to panic on purpose;
- the file's path and folder, which no type can keep from being opened
  for writing: `Hosted::path` is read for log lines, `Get ( FilePath )`,
  the file's size, the Documents folder of a server script and the
  folders backups resolve; `Hosted::at` (and `Slot::file_at`) is read by
  backups, which look up the file's entry, by exports, which open the file
  read-only (`Store::open_saved`), and by the engine's saves. Nothing
  outside the engine's saves writes through either.

Views are built, with the file engine's behind them, and these readers
read through them: downloads (op90, op15, op92), finds (op86), logins
(op80), thumbnails (op96), op88's check of the library it stores into,
Describe Security (op240), the desktop's private host (its suggested
account, stored login and logged-in account) and a schedule's login.
What the engine says of a file that is not a version's (whether it takes
changes and why, which file on disk it last read or saved) it offers
itself ("The engine's state of a file", below). `export` is built
("Exports", below): it copies the file on disk where that holds the
version and walks a view otherwise, and backup schedules and the
desktop's Save a Copy as make their copies with it. Server scripts and
the operations' own reads still hold the store; "Moving the other
readers" below lists them and the plan for them. `import` and
`capabilities` are not built. Of the store engine, its view of one file,
the import of an `.fmp12` into it and its export are ("The store engine's
file", below); no file is hosted on it yet.

**Views.** A view (`engine::View`) is one fixed version of one file's
tree. Nothing a view returned changes later. It answers:

- `node(path)`: one node without what is below it (`engine::Head`):
  whether it opens a list (it was written as a push, or it has children)
  and its own value. A node that opens a list and holds a value is both
  (section 4);
- `subtree(path, limit)`: a node and everything below it, as an owned
  tree's form, refused (`ReadError::TooLarge`) when it would take more
  than the limit its caller states, counted as `om_hbam::tree::memory`
  counts an owned tree. `NO_LIMIT` says that the caller holds it whole
  whatever its size;
- `children_page` and `subtrees_page`: up to a number of the children of
  a node, each alone or with its subtree, in key order or against it,
  from a segment included or excluded (or from the first or the last),
  and whether more may follow. A view may give fewer than asked;
- `walk_page`: up to a number of the nodes of a depth-first walk of a
  subtree, each with its depth below the walk's own node (`Step`), from
  the start or after the node a page ended with, and that node's path
  when more may follow;
- `version()`, and `changed_since(path, version)`: whether anything at or
  below the path changed after that version, never false when something
  did.

Readers use the iterators `children`, `subtrees` and `walk` (on
`dyn View`), which ask the view for the next page when they reach the end
of one: a call through the trait per page of 256, not per node, and no
reader holds more of a list than a page. Every read returns a `Result`.
`ReadError::Unreadable` names a part of the version the engine could not
read (section 6.12); the file engine never answers it.

What a read returns borrows the view (`Cow<'v, _>` for `&'v self`): it
never outlives the view, and a reader that keeps something past the view
copies it. The file engine lends its tree, so it copies nothing a reader
does not ask to own; an engine that reads pages gives owned values and
holds a page only while a call reads it, never for as long as the view
lives.

A reader takes a reading of a file (`FileEngine::read(file) ->
engine::Reading`), which holds one version for as long as the reader
keeps it, and reads through `engine::Read`: the view, the state derived
from it (below), what reads leave waiting for a write (`engine::Waiting`:
the member keys GenerateMemberKey issued and the indexes finds built on
demand, which the file engine takes into a draft, module doc of `engine`,
"Drafts"), and whether the file is hosted read-only. On the file engine
the view (`engine::FileView`, over `engine::TreeView`) holds the store's
read lock, as `Hosted::read_store` did, so a reading costs what that lock
costs and lends the store's own tree. Its version counts the times the
store was taken for writing, and it answers `changed_since` for the whole
file. Versions compare only within one load of a file: a file closed and
opened again is a new store, counting from 0 again, and its derived state
says which load it belongs to (`Derived::instance`). On the store engine a view will never wait for a writer. Downloads
(op90) are plain reads of a view in both engines. That they take no
lock, where FileMaker Server takes a read lock that waits (schema-path.md
4.5), is how OpenMaker Server works today and is not part of this design.

The readers that read through views:

- op90 and op15 (`connection::downloaded`). A list no privilege filter
  applies to is written in the wire form as the view walks it
  (`engine::wire`, `om_hbam::tree::WireWriter`, which gives the bytes
  `Node::to_wire` gives), never held as a tree. A node a filter applies to
  (an account's view limits, `views::ViewFilter`; a restricted account's
  own account member) is copied whole and filtered under the lock and
  written after it, as every download was before, its view limits taken
  once it is copied, as before, so that a limit's formula reads the clock
  at the same point. A key that holds a value is answered with it;
- op92 (`connection::selected`): the parent's own node, and each key of
  the list looked up and copied whole. A key no larger than one before it
  in the list is passed over, so that a list out of order or repeating a
  key selects what walking it beside the children did before;
- op86 (`connection::run_query`, `QuerySource`): `om_query::Source` over
  a read. Its catalog is the derived state's; a node is the view's
  subtree, filtered as before; an index built on demand reads each record
  as the view lists it (`derived::index_on_demand`); GenerateMemberKey
  reads the catalog's index nodes (`derived::issue_member_key`).
  `Source` gained `record_ids`, a table's record list read entry by entry
  (`[T, 0d]`); its default, which the other sources (`HbamFile`, the
  store's own for server scripts) keep, decodes `Source::node`'s node.
  What `om-query` still asks of `Source::node` whole: a field's index (`Indexes::load`: `Index` finds keys by position,
  checks their order once and reads entries by position, so it needs the
  entries at hand), a record's cell (`Indexes::stored_text`), the node
  NodeExists and GetKey name, of which only its own value is read
  (`exec::file_node`), and each node on the destination of
  UploadMultiple, which is measured whole as the program's work
  (`exec::upload_multiple`). The view filter evaluates a view limit on
  the record as stored (`ViewFilter::viewable`, a record whole);
- op80 (`connection::login`): the account the login names, looked up in
  the derived state's accounts (`Derived::account`, "Derived state"
  below) as it was looked up before, by its name in any case. Its password
  is checked (`hosting::check_password`) once the reading is let go, so a
  login holds the store's read lock only for the lookup. What is checked
  and in which order is as before: the login limits, the account, its
  password, hashed once whether or not the name is an account's, and then
  whether the account may log in;
- op96 (`connection::thumbnail`): whether the account's view limits
  withhold the table's container data, and whether the library holds the
  member (`containers::thumbnail_key`, the member's own node); then, when
  no thumbnail of it in that box is cached, its master image
  (`containers::thumbnail_image`, `thumbnail::master`: each image stream
  in turn, read whole, until one is there). The cache is looked up
  between the two reads, outside both. The image is rendered and cached
  after the reading is let go, as before, and the cache still forgets a
  library's thumbnails when a write releases or empties the library. A
  thumbnail read before such a write and cached after it outlives that,
  as it did before
  ([#741](https://github.com/OpenMaker-Dev/openmaker/issues/741)); keyed
  by the version each member was read at, as an engine whose views do not
  wait for writers needs it anyway, the cache would not keep it;
- op88's check that the session may store into the file's own library
  (`connection::container_library_writable`): the base tables of the
  derived state's catalog;
- Describe Security (op240, `security::catalog`): each record of the
  accounts `[23, 1, 5]`, privilege sets `[23, 2, 5]` and extended
  privileges `[23, 3, 5]`, listed one key at a time and decoded as it is
  read, one record held at a time, with only the members it decodes, each
  read as its decoder reads it: heads and keys first, a member's subtree
  only where a decoder uses it, and nothing the reply discards. A value
  its decoder takes as one (an account's kind `02`, status `0a` and set
  `0b`, a set's record flags `02` and lifetime `0c`) is read as the
  member's own value, and its children, a value in pieces that decodes as
  none, never; a name or keyword as its own value or else its pieces; an
  extended privilege's sets `05` as the keys of its children, all the
  reply takes of them, nothing below those keys. A name, a keyword or the
  sets of one record may take what one reply carries (1 MiB,
  `MEMBER_LIMIT`), as stored: the keys and values of what the decoder is
  given of it (a text member's own value or its pieces, each set's key
  and own value), not the probe for a first child or the rest of the page
  a listing fetched. One past it refuses the request. The limit is on the
  stored bytes read for a member the reply uses, input from a file (rule
  6 of AGENTS.md), not on the reply: a name stored in pieces past it
  refuses the whole catalog even where its text would be short (SCSU) or
  not decode at all (a bad chunk header), where reading it whole left that
  one name out. FileMaker's own security editor is not known to write a
  member of that size (unmeasured). Never an
  account's password `06` or its `04`. Before, it borrowed the three lists
  whole from the store;
- the desktop's private host (`LocalHost`): the account its account
  catalog's header names (`[23, 1, 1, 16]`, one node), looked up in the
  reading's accounts, which a client suggests (`suggested_account`); the
  same name and the stored password `06` (one node each) of File
  Options' "Log in using" (`stored_login`); and the facts of the account
  a desktop logged in as, from the reading's accounts
  (`authenticated_account`), which it read from the published accounts
  before. The stored password is read as before: lent by the file
  engine's view, which copies nothing, and a value in parts assembled
  into a copy that is wiped when dropped; a view that hands over its own
  copy (the test view) has it wiped when dropped too;
- a schedule's login (`schedules::log_in`): its account, looked up in the
  reading's accounts, which it read from the published accounts before.

Every write request's check of whether the file is hosted read-only
(`connection::writable`, and op72's), and the desktop host's read-only
reasons and its Change Password, ask the engine for the file's state
("The engine's state of a file", below), as a server script's account
steps do.

Every subtree these readers ask for whole is asked for with `NO_LIMIT`:
each was read whole and without a limit before, and a limit would refuse
what was served.

A read that fails reaches the client as an error, never as a reply that
looks whole. A download (op90, op15, op92) is answered with the CORBA
system exception `PERSIST_STORE`: its reply has no field for it, since its
lock error is reported apart from its data, and a missing node tells the
client the key does not exist. A thumbnail (op96) is answered with the
same exception: its status says only that there is no thumbnail
(`THUMBNAIL_MISSING`), as for a member the library does not hold. A find
fails as a program that cannot read the file fails
(`om_query::QueryError::File`, the error slot's code `ffff`). A login to a
file whose accounts could not be read ("Derived state" below) is
answered with FileMaker's error 802, "Unable to open file"
(`connection::LOGIN_UNREADABLE_FILE`): not 212, which a client takes for
a wrong account or password and counts toward locking the account out,
and not a success. Its password is not checked, it does not count toward
the server's login limits (as a right password for an account the file
does not admit does not), and no failed login is recorded in the event
log, since no credentials were refused. Describe Security is refused with
802 too (`om_protocol::authoring::status::UNREADABLE`, with the message
"The file's security catalog could not be read."), never answered with a
catalog that lacks what could not be read; a member larger than one reply
carries refuses it with status 3, as a reply past that size is. A schedule's run whose file's
accounts cannot be read is aborted with 802, its event "Schedule "…"
aborted; its file's accounts could not be read (error 802).", as the
client's login is answered, not as an account that cannot log in (212).
The desktop's private host returns the error from each of its reads
(`std::io::Error`), and the desktop's provider answers it with 802, "The
file could not be read: …": opening the file, which reads the suggested
account and, unless the user asks, the stored login, fails with it, as
does loading a file that just logged in, which reads its account's facts.
The read is logged as an error. All of these are OpenMaker's choice; what
FileMaker Server answers when it cannot read its file is unmeasured.

These readers hold the store's read lock as long as they did, with two
differences, both shorter and both within one hold, so each still reads
one version: a download no filter applies to is written in the wire form
under the lock, where its subtree was copied under it and written after
it, and op92 looks up each key it was asked for, where it walked every
child of the parent. A find's check before it runs of whether the file
is hosted read-only (for a program that changes the file), and its
question afterwards of whether it left work waiting, each take a reading
of their own, as each took the store before. Both read the file's state,
not its view, so they are read once and not through `Reading::serve`
(below), whose two runs other readers could change it between. op96
takes one reading for what it took the store twice for (the view limits,
then the member), and so reads one version.

A login takes the store's read lock, which it did not before: it read a
copy of the accounts that had a lock of its own. So a login now waits
while a write holds the store (an operation and its save of the file), as
the client's next request after it, a download, already did. An unknown
account and a wrong password wait alike, and their logins still differ
only by the outcome: each is one lookup in the same list and one hash.
A schedule's login and the desktop host's account facts take the store's
read lock too, which they did not: they read the published accounts. The
desktop host's suggested account and stored login, and Describe
Security, took it before. The one reader of the accounts that does not
read through a view is a server script's (its account facts, "Derived
state" below), which reads them as before, without waiting for the
store.

Each of these reads may run twice (`Reading::serve`, below): the login's
lookup, op96's two reads, op88's list of tables, Describe Security's
lists, the desktop host's three reads and a schedule's lookup change
nothing they read. GenerateMemberKey's read alone runs once
(`Reading::serve_once`), since it issues keys.

A second view, for tests only (`engine::copies`, debug builds), shows
that readers do not lean on the file engine's: it serves the same tree but
hands out every node as an owned copy, gives children and walks three at
a time, and fails every read that reaches a path it was told to fail (the
node at the path and everything below it, and a listing or walk of such
a node whether it is there or not, as a damaged page fails the reads
that reach it). A server switched to it (`Server::checking_reads`, and
`LocalHost::checking_reads` for a desktop's private host) answers each
read above through the file engine's view and then through this one,
from the same version, and panics when the answers differ; it computes
its derived state through this view, once for each load of a file and
its catalog again each time the file engine reads it again, and checks it
against the file engine's, failing the check when it cannot compute it.
The accounts it reads through itself only for a reader of them
(`Reading::serve_with_accounts`: a login, a schedule's login, the desktop
host's suggested account and account facts), for that read alone, wiping
its copies of the account records once decoded (`engine::Wiped`); for any
other reader its derived state holds no accounts, so one that read them
without asking would fail its check
(`engine::tests::the_test_view_reads_the_accounts_only_for_a_reader_of_them`),
and the test view copies no password verifier for it (Describe Security
included). The test servers of `om-server`, `om-provider` and
`om-author` and of `om-gui`'s hosted-file tests, and the private hosts
`om-server`'s local-file tests open, are switched to it; the private
hosts the desktop opens for itself in `om-provider`'s and `om-gui`'s
local-file tests are not. A
read it was told to fail (`ServerHandle::failing_reads`) is answered
through it alone, so that a test sees what the client gets
(`tests/indexes.rs`, `a_failed_read_reaches_the_client_as_an_error`;
`tests/psos_containers.rs`, `thumbnails_read_through_views_and_fail_as_downloads_do`;
`tests/local.rs`, `the_local_hosts_failed_reads_are_errors`, through
`LocalHost::failing_reads`; `tests/schedules.rs`,
`a_run_whose_accounts_cannot_be_read_is_aborted_unable_to_open`;
`om-author`'s `security_that_cannot_be_read_is_refused`);
there a derived state it cannot compute is the file engine's, since
derived state is computed when a file is loaded and when its schema
changes, not by the read that fails. Accounts it cannot read for a reader
of them are unreadable instead, as an engine holds them ("Derived state"
below), and every login is answered as for a file whose accounts could
not be read
(`tests/logins.rs`,
`a_login_whose_accounts_cannot_be_read_is_answered_unable_to_open`).

**Write operations.** Every change the server makes is a value of one
type, a write operation (`engine::Operation`):

- records: commit records (op70, `CommitRecords`), reserve record ids
  (op71, `ReserveRecordIds`), advance a serial (op72, `AdvanceSerial`), a
  server script's commit (`CommitScriptRecords`) and Set Next Serial Value
  (`SetNextSerial`);
- locks: take, upgrade and release them (op60, op61, op62, op65:
  `TakeLock`, `UpgradeLock`, `ReleaseLock`, `ReleaseLocks`), a server
  script's lock on a record or on the accounts catalog and its release
  (`TakeScriptLock`, `ReleaseScriptLock`), and the release of a session's
  locks when it ends (`ReleaseSessionLocks`);
- schema: upload to a catalog (op13, `UploadToCatalog`; op86's
  UploadMultiple and DeleteTable, `ChangeSchema`), update a table (op73,
  `UpdateTable`), store an index built for a find (`StoreIndexes`), and
  finish the deferred field work a file records as pending
  (`FinishDeferredWork`);
- container data: store it (op88, `StoreContainerData`) and adjust a
  member's reference count, which releases it at 0 (op87,
  `AdjustContainerCount`);
- design: an authoring edit (op240, `EditDesign`, one variant carrying the
  decoded edit), the authoring extension's Save (op240, `SaveFile`, which
  a server script's end and the save of deferred work at opening submit
  too) and a deploy (section 9);
- accounts: Change Password, Reset Account Password, Enable Account and
  Delete Account (`ChangePassword`, `ResetPassword`, `EnableAccount`,
  `DeleteAccount`);
- the desktop's saved found set (`SaveFoundSet`).

An operation owns its data and names who asks: the lock user (connection
and transaction), the name other users see for it, for op70 the account,
its privilege set at login and those of the listed records the session
was served without their values, for op73 the account its calculations
see, for an authoring edit the connection, session name and account its
locks and stamps name, for a server script's operations its session's
lock user, whose connection no client has, so that its notices reach
every client registered on the file, and for an account edit whether it
may manage the account and the names its stamps carry. A server script's
lock is not op60: it reads nothing of the store, no cap applies, and the
script host answers a conflict with the step's error (301 for a record,
as a client's lock is refused; 310 for the accounts catalog,
script-runtime.md 2.3). Its commit carries its records as the script
host finished them, with the container data its steps set: the host
applied the records' rules and checked the session's privileges before,
so the operation names no account. The release of the accounts catalog
carries the notices of the change the account step made (schema-path.md
9), as op62 names the change its release tells of; the account edit
itself raises none. The record ids a server script reserves wait unsaved
for its commit, so its end submits op240's Save, which saves those of
records it did not commit.

The passwords an account edit was given are not part of its operation:
they travel beside it to `run` alone (`engine::Credentials`,
`FileEngine::submit_with`), and appear in no log line or `Debug` output.
So whatever keeps operations (a recording, a log, another server) never
has them, and a replay supplies them again. The engine clears what it
holds: the `engine::Password` wrapper each password is copied into when
the caller builds the edit, which is never copied again and is wiped
when dropped, the old one once checked and the new one once hashed. The
text the caller copied it from stays the caller's to clear. On the
desktop that is the Change Password dialog's fields and the request
that carries them to the provider (`om_gui`'s `ChangePasswordDialog`
and `PasswordPair`), and the old password the provider keeps for a
change due at sign-in (`om_provider`), all wiped when dropped. In a
server script it is the step's calculation results and any variable
that held them (`om_scripts`), which are not wiped. A log will keep an account edit's effects
(the account member as rewritten, with its salted verifier, as the file
holds it) and its history only the fact of the change (section 9). A
salt the random source cannot give refuses the edit as unwritten, as
before.

op70's staged records travel in it: the session's temporary area lends
them (`writes::TempArea::lend`) and takes back those the commit did not
take. A commit that was not stored has still taken the records it reached,
as before the interface: the file is put back, the staged edits are gone,
and the client uploads them again to retry. op88's container data is read
from the upload before its operation, within the memory the client may
hold (`--upload-memory-mib`), and travels in it, or, when it is not valid
binary data, why not; the request keeps the charge until `submit` returns.
An authoring edit's request travels as it was decoded, its definition
charged the same way.

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
staged records. What it does copy is small: what a draft took of what
reads left waiting (a catalog path and two numbers for each member key, a
table, a field and a kind for each index), kept so that it goes back to
waiting if the operation is not stored or its run is left by a panic. The
store engine will have to make issuing a member key and building an index
operations of their own.

One function, `engine::run`, runs an operation against a transaction (a
view plus put, delete and replace-subtree), the file's lock table
(`engine::Locks`) and an environment (`engine::Environment`), from which
it takes the clock and randomness: an authoring edit's modification
stamps and the UUIDs of what it adds, an account edit's stamps, status
time and password salt, the unique ids of the container data op73, op88
and a server script's commit store, and what the formulas it evaluates
read (below). Each operation draws from it in a fixed order, written down
in the module doc of `engine` ("The environment"), so that a seeded
replay draws the same. It returns the
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

**Formulas.** The formulas an operation evaluates (a privilege set's
record limits in op70, the stored calculations op73 and the deferred work
finished at opening recompute) read the world through their record
environment (`recalc::record_environment_in`). It is drawn from the
operation's environment in this order:

- a UUID;
- one clock reading. It answers the date and time `Get` selectors, and
  its UTC date gives the year a date without one takes. A reading outside
  FileMaker's dates answers those selectors with error 500, as an
  out-of-range date is (calc-values.md 4.1), never with the system clock;
- eight bytes of the random source, which seed a generator for `Random`,
  FileMaker's (calc-functions.md 3.9), so that each operation draws its own
  sequence;
- a stopwatch, on which a record limit's 100 ms budget is measured. The
  host's measures the monotonic time that elapsed. A seeded environment's
  answers virtual time that advances by a fixed step at each reading, so
  that the budget is met after the same number of the evaluator's checks
  on every machine.

op70 draws them once, when it checks the first listed record that exists
or that it creates, also for an account whose access is unconditional. An
empty commit, or one that lists only deletions of records that do not
exist, draws nothing. A recalculation over a table (op73, and the
deferred work finished at opening, table by table) draws once. When its
calculations read `Get ( UUID )`, each record gets its own UUID and clock
reading, but the first record's generator, date for conversions and
stopwatch serve the whole recalculation.

What runs before an operation, outside it, builds the same record
environment from the host's clock and random source
(`recalc::record_environment`, `engine::HostEnvironment`): the script
host's own calculations and the rules it applies to a server script's
records before it submits their commit, and a download's view filter, a
read, which takes its date for conversions once for the filter.

Formulas still read the process in these places
([#702](https://github.com/OpenMaker-Dev/openmaker/issues/702)):

- `CryptEncrypt` and `CryptEncryptBase64` draw their salt from the system.
  A record limit may call them, so one that compares their output need
  not replay to the same reply.
- The size and attributes of a PDF that a formula makes
  (`GetWidth ( Base64Decode ( ... ; "a.pdf" ) )`) come from a worker with a
  wall-clock deadline. A record limit may read them, so on a slow machine
  such a limit can fail.
- `CryptGenerateSignature` blinds with the system's randomness. Its result
  does not depend on it.
- A server script's own calculations are answered by the script's host
  from the system clock and the process's `Random`, among them an unstored
  match key that a script's lookup reads.
- Text typed outside a formula (lookups' related values and keys, held
  records, typed entry) takes today from the system clock, which matters
  only for a year the text leaves out.

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
`view` is `engine::FileEngine::read(file) -> Reading`, above. `export` is
`engine::FileEngine::export(file, kind, target, progress) -> Exported`,
below. There is no engine trait while there is one engine. The other
names are settled when they are built. A feature that needs a capability
the running engine lacks answers a typed "unavailable".

**Exports** (`engine::export`). An export writes one version of a file as
an `.fmp12`, in one of two ways; the engine says which
(`engine::Source`, from `Reading::export_source`):

- **The file on disk**, when the disk holds the version. It is opened
  under the store's read lock, which is let go at once, and copied after
  it. On the file engine that is a version a save wrote, or the file as it
  was loaded, with nothing changed since, opened where the store can tell
  it is that file (the descriptor a desktop's host holds locked, or the
  entry at the file's path while it is the one the server read or last
  saved there,
  [#468](https://github.com/OpenMaker-Dev/openmaker/issues/468)).
  "Nothing changed since" is the store's record of unsaved changes, clear
  only while memory is known to equal the disk ("The record of unsaved
  changes", below): exact after a save that is not written, the change
  taken back whole, the parents it created and a record held as a value
  included (`engine::tests::an_export_after_a_failed_save_holds_what_memory_holds`,
  `a_deletion_taken_back_puts_the_record_back_as_it_was`), and set after
  a panic, whatever it left
  (`a_panic_in_deferred_container_work_leaves_nothing_and_counts_as_changed`). A file
  hosted read-only for content the writer cannot write is always its file
  on disk, the bytes it was loaded from (section 2, rule 2); while changes
  its refused saves left wait in memory, an export of the version held is
  refused before anything is written (`ExportError::Unsaved`), and a
  backup copies the file on disk with a warning, as it always did. The
  copy goes from descriptor to descriptor, by the kernel where the system
  can (`copy_file_range`), as backups always copied, and is checked
  to be as long as the file was when it was opened, the file not having
  grown past that. It is not read back: that is what FileMaker Server's
  Verify option of a backup schedule adds
  ([#158](https://github.com/OpenMaker-Dev/openmaker/issues/158)). No view
  is read: the file is what a checked save wrote, or what the server
  loaded.
- **Written from the view**, when the disk lacks the version (changes in
  memory that no save wrote) and for a compacted copy
  (`engine::export::Kind::Compacted`): a depth-first walk of the view, in
  key order, into `om_hbam::writer::NodeWriter`, every node with its kind
  and value, empty lists included, and page 0 as the file was loaded
  (derived state, below) with the clean-close flag set and the change
  number counted on from the one the next save starts from
  (hbam-writer.md 5.1), so that it is what that save would write. It is
  read back with `om_hbam`'s streaming reader (`stream::NodeStream`) and
  compared with a second walk of the view, node by node (path, kind and
  value, a value over 1,000 bytes as the writer splits it) and page 0 as
  written. A store that cannot tell its file on disk is the one it holds
  (no lock, no identity) writes every version, a saved one from the
  change number its save started from, so that it is the saved file byte
  for byte.

A difference, a file that does not read back, a file on disk that changed
while it was copied, a read of the view that fails (`ReadError`) or a
refusal of the writer is a typed error (`engine::ExportError`).

**Where an export goes** (`crate::staged`, which the desktop's Save a Copy
as shares). Into a folder, one of two ways:

- **Unnamed, then linked.** On Linux with `/proc` mounted, on a file system
  that makes unnamed files (`O_TMPFILE`: ext2, ext3, ext4, XFS, Btrfs,
  tmpfs and others), the export is an unnamed file, written, given its
  permissions, flushed, then linked to its name from its descriptor, which
  fails when the name is taken. It is complete under its name or absent,
  and never replaces an entry.
- **Under its name**, everywhere else, as copies were made before exports:
  the file is created under its final name, only where nothing is, and
  written there. That covers other Unix systems (macOS), file systems
  without unnamed files, and, for an export, a file system that refuses
  the link because it has no hard links, where the complete unnamed file
  is copied into the newly created name, a part at a time, stopping when
  asked (a backup's cancel). A crash, a stop or a failure while it is
  written can leave a partial file under its name, as before exports. The
  desktop's copy is never made this way: a refused link is its error, and
  nothing is under the name, as before exports.

Nothing removes a name. After the name is given the folder is flushed;
when that fails the export is complete under its name, the file stays,
and the export says so (`ExportError::NotFlushed`): whether the name
survives a crash is not established; if the file's length or identity
cannot then be read, the export reports that instead, and the file stays
complete under its name either way. A backup then goes on: it flushes
every folder of the backup before naming the backup and fails if one
cannot be, as it always did. The desktop logs it and keeps the copy, as
it always did. What else a failure leaves:

- before the name is given, on the unnamed way: nothing;
- the name taken: the entry there, untouched, and nothing else;
- on the way under its name: what was written of the file, under its
  name, as before exports (a backup's run removes its unfinished backup
  folder, with the file in it).

A file that fails while the view is walked is dropped after the store's
read lock is let go. An export can also fill a file its caller made
(`engine::export::Target::File`, the desktop's): it must be empty, open
for reading and writing at any position and not for appending (refused
before anything is written on Unix: a walk writes its pages at offsets),
and the caller's alone while it is filled. It is written from its first byte and
checked to be as long as the export. Windows has no folder descriptors
here: an export to a folder fails as unsupported, and backups are not
made on Windows. Memory is what the kernel's copy takes, or what the
writer and reader hold (a few pages and the open key path) and a page of
the view's walk; never a part of the file.

**The file engine's limit.** Its view holds the store's read lock for as
long as the view lives, so a written export holds up every write to the
file while the view is walked, written and checked: 740 ms for a file of
105 MB and 3.5 s for one of 524 MB (medians of three,
[performance/export.md](performance/export.md)). A copy holds it for an
open (12 µs). That is why a version the disk holds is copied and never
walked, and why backups copy the file on disk whatever memory holds. An
engine whose views do not hold up writes will not have this limit. A
change committed during an export is in it whole or not at all: a copy
reads the file it opened, which a later save renames a new file over, and
a walk holds the store, so the commit waits for it.

Exports are made by backup schedules (`backups`: each file's copy is the
file on disk as its last save left it, `FileEngine::stored` then
`engine::export::copy`, as backups copied it before, with a check) and the
desktop's Save a Copy as (`LocalHost::prepare_copy`, into its unnamed
destination: the current version, or a compacted copy). Not yet: an export
at any time from a running server, at a clean stop, and `om-server export`
(section 6.10).

**Limits of exports.**

- Another program that writes into the hosted file in place, keeping its
  length, gives a copy that passes the length check: OpenMaker's saves never
  write in place, but nothing stops another program, as before exports
  ([#724](https://github.com/OpenMaker-Dev/openmaker/issues/724)).
- Under the store's read lock, a copy opens the file's folder by its path,
  the file in it without following a link, and reads the file's identity,
  length and permissions: calls that can wait on a slow file system. The
  server's store holds no descriptor of its file to clone instead; the
  desktop's holds the locked one, which a copy could clone and read by
  position, where it now opens the path again (a permissions change since
  the file was opened fails that open).
- What another holder of a caller's file writes while an export fills it
  cannot be seen: it can be overwritten, or make the export wrong.
- On systems other than Unix a caller's file open for appending is not
  refused: a walk into it would put its pages in the wrong places. The
  only caller, the desktop's Save a Copy as, is Linux-only and passes its
  own unnamed file.

**The store engine's file** (`engine::paged`). The store engine's first
code in the server: one file held in a tree of `om-store`'s pages, read
as a view, filled from an `.fmp12` and written back as one. No file is
hosted on it, and nothing of its write side is built.

- **The view** (`PagedView`) reads one version of a tree, named by its
  root, through any page store (`om_store::page::PageStore`): the page
  file, through the roots a pin hands over, or a store in memory. Every
  page it reads is checked by the hash its parent holds; one that does
  not read or check fails the read that reached it with
  `ReadError::Unreadable`, naming the path read, and is never data. Each
  call opens the version again from its root (the page store's cache
  answers) and holds a page only while it reads it; what it returns is
  owned. It answers every call as the file engine's view of the same
  file does, page for page: the same children and walk steps in a page,
  the same "more may follow" (a full page of children looks one child
  further to say), and subtrees built with exactly as many places as
  they hold, so that a limit counts them as it counts the file engine's
  tree of the same file (`om_hbam::tree::memory` counts capacity). A
  listing or walk that meets a read that fails partway gives what came
  before it with "more may follow", never "no more" (a failed look-ahead
  past a full page answers "more" too), and the next call reaches the
  failed read first and fails, or reads what is there. Its version is the index of the
  last entry that changed the tree, its root page's stamp, read when the
  view is made; `changed_since` is the tree's own
  ([server-storage-format.md](server-storage-format.md) 2.8). The `View`
  trait needed no change.
- **Import** (`paged::import`) reads an `.fmp12` with
  `stream::NodeStream` and puts every node, with its kind and value,
  empty lists included, into a new tree, in batches of at most 4,096
  nodes and 4 MiB (checked before a node joins a batch; a node larger
  than the byte limit alone is a batch of its own), each one unit of work
  of the page store committed on its own (`paged::Units`), so that what
  it holds does not grow with the file. It goes through no log and no
  entry: its caller gives the entry indexes its batches take, and before
  it writes anything it works out from the file's length that they all
  fit below the last index an entry can have. When the store's write
  path takes it, its batches become the parts of a group whose last entry
  adds the file (sections 6.9, 6.10). What it accepts, an export gives
  back: an export can be written, for a file whose export numbers fewer
  than 2^32 pages (the writer's page numbers are 32 bits, and an export
  can number more pages than its source, which matters only near the
  format's 16 TiB) and whose change number has the room for them; it
  holds the same nodes, kinds and values; and imported at the same entry
  in the same batches, it has the same root hash (a page's hash covers
  the entry that wrote it, so other entries or batches give another root
  hash over the same nodes). That second import has two boundaries:
  page 0's change number, which an export advances by the pages it
  writes, so the export of a file within that many of the bound below
  may itself be refused; and the entries, whose check is worked out from
  the file's length, while an export can be a page or more longer than
  its source, so at a first entry within a few pages' worth of the last
  index an entry can have, the export's re-import may be refused. Accepted
  means exportable once. So it refuses, typed (`paged::ImportError`) and
  with the tree it made dropped again:
  - a file that does not read, an encrypted one among them, whose pages
    do not decode, and five shapes `om_hbam`'s streaming reader refuses
    and the file engine's reader (`HbamFile`) reads: a leaf chain that
    ends before its stream closes the root list (a file cut short), one
    that goes on after it closed the root list, a page that does not push
    again the path left open before it, a value given to a key after its
    children, and a page larger than 64 KiB (`om_hbam::stream`). The file
    engine hosts such a file from the tree its reader reads, exports the
    file's own bytes while nothing changed, and a save writes the tree in
    the writer's form. None of FileMaker's 66 templates is refused;
  - a file holding what the file writer cannot write, or writes in
    another shape: a list that carries an empty value
    ([#754](https://github.com/OpenMaker-Dev/openmaker/issues/754)),
    which the file engine keeps in memory and its writer writes as a bare
    list, and a value over 1,000 bytes held whole, which the writer writes
    in pieces. The file engine hosts such a file, read-only where its
    writer cannot write it, and the store is to keep it as its original
    image (section 6.10, not built);
  - a node whose entry in an export's index could cost more than 1,019
    bytes (section 6.11): past it the file writer's index can grow past
    the 16 levels it writes, and an export would fail. The bound is
    conservative, so some files past it could be written (a chain of five
    empty lists with 219-byte segments). The file engine hosts such a
    file, and its saves fail only where the index does;
  - a page 0 the writer does not write: not one 4,096-byte page, or a
    change number above `u64::MAX - u32::MAX`, which leaves no room for
    the 2^32 pages an export may number from it. That is a worst case: a
    file whose export numbers fewer pages is refused too. The file engine
    hosts such a file, and a save of it fails, before it writes anything,
    only when its pages pass the room the change number leaves;
  - an item past the bounds of section 6.11, which no file the reader
    reads holds.
- **Page 0** comes back with the import (`Imported::page_zero`), as read.
  The file engine keeps it in its derived state; the store keeps it in
  the file's row of its own tree (server-storage-format.md 5.3), which
  the entry that adds the file writes. Until the import goes through
  entries, its caller holds it.
- **Export** (`paged::export`) is the file engine's written export
  (`export::write_checked`, which `write_walked` calls with a reading's
  view and page 0): a walk into the streaming writer with the imported
  page 0 and the change number it holds, read back and compared with a
  second walk. It is the file engine's compacted copy of the same file,
  byte for byte. A read that fails fails it; what is then left under the
  export's name is what every export leaves ("Where an export goes",
  above, and `engine::ExportError`;
  [#761](https://github.com/OpenMaker-Dev/openmaker/issues/761)).

**Derived state.** What readers need of a file besides its tree is one
value, `derived::Derived`: the decoded catalog, the file's accounts, the
file's locale and the order of its stored dates, two facts not in the
tree that compiling its calculations needs, the file's name and whether
it is served to a desktop client, and one an export needs, the file's
page 0 as it was loaded (`Derived::page_zero`; its change number is the
engine's, per version). It is computed from a view
(`Derived::new`, `derived::read_catalog`, `derived::read_accounts`) and
carries the version it was computed from. It is not part of an engine; an
engine decides when to compute it. The file engine computes it when it
loads a file and reads the catalog again after each change of the schema,
at the moments the store always read its catalog again
(`Store::reread_catalog`); the locale and the date order stay those the
file was loaded with, as before. Every reading shares the store's current
value (`Arc`), and the catalog within it is shared too, so replacing the
accounts copies no catalog. An engine with versions keeps it by version,
and gives it to a later view when `changed_since` says no path it was
computed from changed.

The accounts are each account record of `[23, 1, 5]`, read whole one at
a time (a `subtrees` listing), with the privilege set it names from
`[23, 2, 5]`, read whole once, decoded as before (`hosting::read_accounts`,
`hosting::FileAccount`). The file engine decodes them again at the end of
each operation that moved the schema generation, before its save, under
the store's write lock (`Store::follow_accounts`): the moments a copy of
the accounts beside the store was decoded again before, when a write let
go of a store whose generation had moved, but before the save rather
than after it. So an account edit (Change Password, Reset Account
Password, Enable Account, Delete Account), an op13 upload or op86 schema
change of the security catalog, and an authoring edit (op240, none of
which changes an account or a privilege set yet) replace the accounts in
the step that changes the file, and a save that is not written puts back
the accounts from before the operation with the rest of the derived state
(the journal keeps it, `Journal::schema_from`), where before they were
decoded again from the tree put back. A write outside an operation (the setup of a file, a
test's) has them decoded when it lets go of the store. Accounts that
cannot be read are held as such (`Derived::accounts` is a `Result`), not
replaced by the last ones read, which could admit an account since
deleted or a password since changed; every login to the file is then
refused (Views, above). The file engine reads them from its tree, which
never fails.

Every reader of the accounts reads them through a reading but one: a
server script's account facts and Change Password's check of the
account's kind (`Hosted::accounts`, `Hosted::account`), which a
calculation asks for (`Get ( AccountType )`) while the script host may
hold the store already, and must not take its lock again. They read the
derived state of a version published with that version
(`hosting::Published`): the version the store holds when a write lets go
of it and that version's derived state, the same `Arc` every reading of
it shares. It is published while the write still holds the store's write
lock, after the write, its save or roll-back and its accounts, and
before the lock is let go (`StoreWriteGuard`'s drop). So a reader that
takes the store after a write finds the version that write published,
which a reading checks in debug builds (`FileEngine::read`), and a reader
of the published state gets the last version a write let go: while a
write runs, the one before it, which every reader of the store had until
then. Its lock is its own, taken after the store's by a writer and alone
by a reader, never the other way.

The reads of a file through a view and derived state are functions beside
it (`derived`): the catalogs, an index built on demand
(`index_on_demand`, `written_form`, `index_entries`), the next member key
of a catalog (`stored_member_key`, `issue_member_key`, `issued_keys`,
`member_key_refusal`), and the inputs
of a calculation and a privilege set's record limit (`with_calc_inputs`,
`record_limit`). The store's own work calls them with a view of its tree,
so readers and operations share one implementation.

Still kept by the store, outside derived state: the basis of stored
calculations (`Store::basis`, below), the verified-index marks
(`Store::verified`), the schema generation (`Store::schema_generation`,
which server scripts cache by and which says when the accounts are
decoded again), the file's language as its locale record is now
(`Store::file_language`), and the read-only flag and the file's identity
on disk.

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

**The engine's state of a file.** Some of what the server asks of a
hosted file is not a version's: whether it takes changes from its
clients and why not, whether it takes no more changes at all, and which
file on disk the engine last read or saved for it. The engine offers them
itself (`engine::state`, on `FileEngine`), not through a view or `Store`:
`read_only(file)` (the first cause: opened read-only, content the writer
cannot write, deferred work refused), `writes_refused(file)` (a save
whose outcome was unknown) and `file_on_disk(file)` (its device and
inode). They describe the engine's hold of the file on disk and of its
clients, not the content of a version. Two versions with the same tree
can differ in them: a save renames a new copy over the file and changes
its identity on disk and nothing in the tree. And a reader holding an
older version must be told how they stand now: a backup protects the file
on disk now, and a client is refused a change by the file as it is now,
whatever version it read. An engine with versions keeps them once for
each file. The file engine keeps the read-only cause and the identity in
its store and reads them under the store's read lock, as `Hosted` did
before; whether the file takes no more changes it keeps beside the store
(`Hosted::writes_refused`), read without waiting for a writer. Their
writes outside an operation are the engine's too: the identity of the
file it loaded, set before anyone can reach the file
(`FileEngine::loaded_from`), and the read-only hold of a file whose
deferred work a panic left part way (`FileEngine::hold_read_only`).

The callers: the desktop host's read-only reasons (`LocalHost::read_only`,
in the same order of precedence: the reason it was opened with, then no
more changes, then the engine's cause) and its Change Password, a server
script's account steps, every write request's read-only check
(`connection::writable`, op72), the event a file's opening records, a
slot's file on disk for backups (`Slot::saved_file`) and for a close
(`Slot::finish_close_if_drained`), and the check that no other slot hosts
the same file (`hosting::hosted_as`). Whether a file is closing, and which
slot serves it, are the hosting's (`Slot`), as before.

**Moving the other readers.** The readers below, but those marked moved,
still hold the store (`Hosted::read_store`, or
`Transaction::view() -> &Store` inside `run`), or read the accounts the
store published (`Hosted::accounts`, `Hosted::account`). Each is listed
with the `Store` helpers it reads through, what it reads (one node, a
list a node's children make, or a subtree whole), and what it needs that
views and derived state do not give yet. No connection handler is among
them, nor anything of the hosting, the desktop's private host, the
authoring extension or schedules: outside the file engine itself
(`engine`, `Hosted`'s store and its guard), only server scripts and the
operations' own reads take `Store`. They move in this order, the smaller
first, each with the test view's runs of its tests:

1. The security catalog, the desktop's private host and schedules: moved
   (Views, above).
2. Server scripts and their host (`scripts/host.rs` and `scripts/host/`).
   `Schema::load` and `refresh_schema` (`Store::catalog`, `locale`,
   `file_language`, `schema_generation`, and the design catalogs `[3]`,
   `[4]`, `[16]`, `[17]` whole, `design::design_catalogs`): derived state
   plus a calculation schema, which should join it, cached by version.
   `committed_records` (`[T, 0d]` decoded, else `[T, 05]` listed), and
   `with_record_view` and `committed_node` (one record whole).
   `field_holds` and `value_held_elsewhere` (`Store::value_keys`, then one
   entry of a value index by key: `children` from that key). `global_value`
   (`[T, 08, F]` whole). `query_store` (`om_query::Source for Store` and
   `Store::has_indexes_on_demand`): the find path's source over a read, as
   op86 has now. `query` (`Store::scratch`: a store holding the
   transaction's records alone): needs a view that lays a script's
   uncommitted records over a version, which no view gives yet.
   `design::next_serial_value` (`Store::serial_value`'s node, one node),
   `containers.rs`' `value`, `data` and `pending_tokens`
   (`Store::container`, `member_data`: a library member whole), `get.rs`'
   `privilege_set` (the security catalog's set `[23, 2, 5, P]` whole),
   `related.rs`' `index_lookup` (an index entry by key), and the session's
   account facts (`ServerHost::authenticated_account_info`, Change
   Password's check of the account's kind: the published accounts, which
   a calculation reads while the host may hold the store). Needs: the
   overlay view, the calculation schema in derived state, and the script
   session's cache keyed by a design version rather than the store's
   schema generation; once a session reads through a reading of its own,
   its account facts come from that reading's derived state, and the
   published accounts can go.
3. Backups and exports: moved ("Exports", above). A backup takes one
   reading for one open of the file on disk (`FileEngine::stored`), as it
   opened the file under the store before, and copies it after;
   `LocalHost`'s `prepare_copy` is an export of the current version or a
   compacted copy.
4. Admin commands and the hosting state: moved. They read the engine's
   state of a file ("The engine's state of a file", above), not a view.
5. The operations' own reads inside `engine::run`, through
   `Transaction::view() -> &Store` and the transaction's delegates
   (`has_table`, `has_record`, `last_record_id`, `node`): a commit's
   records (one record whole each, `writes::commit`, with
   `Store::record_limit` and `views::hidden_fields`), op71's and op72's
   counters (one node), op13's and op86's refusals (`put_refusals` over
   the tree: each node on a path, and a chunked value whole), op73's
   recalculation (every record of a table, listed with each record whole,
   and the basis), the stored indexes' upkeep (`Store::index_agrees`,
   `reindex_plan`: index entries by key, and whole indexes for a check),
   op87's and op88's library members, op240's catalog members and their
   indexes (`authoring::edit`: one member whole, a catalog's index), and
   the account edits (`[23, 1, 5]` listed, one account whole). Needs: a
   transaction that is a view plus put, delete and replace-subtree, which
   reads its own writes; the basis of stored calculations and the
   verified-index marks in derived state, by version; and `om-query`'s
   `Index` over a seekable listing instead of a whole index, which the
   finds above need too before an engine with pages serves them.

For finds, what `om-query` still asks whole (see "Views") moves with the
store engine: a `Source` method for the head NodeExists and GetKey read,
a measure of a subtree for UploadMultiple's work without holding it, and
an index read by key and position over `children` listings.

**The file engine behind it.** `FileEngine::submit` takes what each request
took before the interface: the store's write lock for op70, op71, op72,
op13, op73, op86's schema instructions and stored indexes, op87, op88,
op240, a server script's commit and its Set Next Serial Value, the
account edits and finishing deferred work, its read lock for op60, and
for op61, op62, op65, a server script's lock and its release and the
release of a session's locks only the lock table. A saved found set
takes the store's write lock only when no one holds it
(`FileEngine::try_submit`), so that a window closing never waits on a
request; otherwise it is not kept, as before. It takes the store first
and the lock table when the operation first uses it (op13, op73, op86's
operations, op87, op88, op240's Save, Set Next Serial Value, the account
edits, deferred work and a found set never do; a server script's commit
takes it at its start, as it always held both), and completes a draft
right after taking the store. op70, a client's op71 and a server script's
commit, which take the lock table, hold it with the store while they save
and let go of it first. The caller then delivers the notices, taking the
file's registrations and then the mailboxes (the order is in the module
doc of `engine`). An authoring edit holds its locks as op240 always did:
it takes the lock table only while it takes them (and again while it
releases them when it is refused after that), saves under the store alone
while the locks keep other clients out of what it changes, and its locks
are released after the store is let go and before the reply goes out.
op13, op73, op86, op87 and op88 raise no notices: the other clients
learn of a schema change when the client releases its lock
(schema-path.md 9). An authoring edit raises the notices the releases of
its locks would raise. A server script's account step takes its lock on
the account catalog (`TakeScriptLock`) before it submits the edit and
releases it (`ReleaseScriptLock`) after `submit` returns, as it always
did; the release carries the notices of a catalog change when the edit
changed it, built before the lock was taken, and the edit raises none.
The desktop's own Change Password is not stamped and tells no one. When
the store's write guard is let go, the file's accounts are decoded again
from the store, so a changed password counts from the next login on.

Operations that change the tree therefore run one at a time. op61, op62
and op65, a server script's locks and their releases, and the release of
a session's locks run under the lock table alone: they wait for an op70,
an op71 or a server script's commit that holds the lock table while it
saves, and can run beside an op72 or any other operation that holds only
the store. Beside an authoring edit's
save they see the edit's locks, so a lock request on what the edit
changes is refused with the conflict, as it was before. op60 also reads
the store, so it waits for every operation that holds the store for
writing. A request that
takes a lock first counts the locks its connection holds in every file it
has open, so it can also wait for such a save of another of those files.

**Overlapping requests.** Each operation takes what its request took and
saves as it saved, so a request that overlaps another sees what it could
see before, except in the cases below, one paragraph per part of the
work.

op71 now holds the lock table while it saves, as op70 always did, because
the locks it took must be released again if the save fails. That changes
what two overlapping requests can get. Before, op71 saved first and locked
its ids afterwards, so an exclusive lock on the table's records requested
during the save (op61) was granted, and op71 was then refused with its ids
used up. Now the lock request waits for the save and is refused, and op71
gets its ids. Both are orders the two requests could have run in;
FileMaker Server's behaviour here is unmeasured.

`submit` saves at the points each request saved before, so that with
saves succeeding the file is written as often as before, and reads every
save's outcome ("Failed saves", below):

- op70: its commit. A save that fails takes the commit back and its
  records stay locked; the staged records the commit did not reach go
  back to the session's temporary area, as when it is refused (those it
  reached are gone, and the client uploads them again);
- a client's op71: the new last issued id, before the ids are answered
  (#927); on a read-only file the save fails. A failed save takes back
  the reservation and the locks it took. A server script's op71 saves
  nothing: its commit, or the Save at its end, saves it;
- op72, op13, op86's schema instructions and the indexes a find built (a
  client's or a server script's), op87 and op88: the file, when it holds
  changes the disk lacks and is not hosted read-only (op88 also when the
  upload was not valid binary data, as after every op88 that was read).
  A failed save takes the operation back;
- op73: the same, only when its work changed the file;
- an authoring edit that changed the file: its commit. A failed save
  takes the edit back, the reply is the storage error (status 800, 809 or
  810) saying what the file keeps, and its locks are released all the
  same;
- op240's Save: the file, when it may be written, the reply then saying
  whether the disk holds every change the file holds (status 800, 809 or
  810 when it does not), or that it is hosted read-only. A server
  script's end submits it, and a failed save there is the script's
  error;
- a server script's commit and its Set Next Serial Value: their commit, as
  op70. A failed save takes the change back, a commit's records stay
  locked, and the step fails with the save's FileMaker error
  (`hosting::save_error`);
- an account edit that changed the file: its commit. A failed save takes
  the edit back and the reply is the edit's "not written" error with the
  save's message (3 for a script step);
- a saved found set: its commit, as the desktop saved it. A failed save
  takes it back and is logged as a warning, with no alert;
- finishing deferred work: the work and its save are one operation, which
  saves when the work changed the file (`Hosted::recover`). A server runs
  it as it loads the file, before the file is served; a local file when
  an account it admits first logs in, so that a local file closed before
  that is left as it was. A failed save takes the work back: the file is
  served as its disk holds it, the work list there and the stored values
  as stored, read-only, as when the work cannot be done
  (`Store::recover`), and the failure is logged and alerted. Hosting the
  file again tries again. Where the work stands is one state, under a
  mutex of its own: due, begun, or ended with an outcome (saved, nothing
  to do, taken back, or unknown). Checking it and doing the work are one
  step under that mutex, so a login that comes while another login's work
  runs waits for it to end before it is answered: no login to a local
  file is answered, and no record served to it, before the work ended.
  The work is marked ended only once the operation has an outcome. A run
  a panic left (the engine took back what it did, so the file is as its
  disk holds it, the work pending) leaves the state begun; the next login
  finds it so and gets the file read-only, as when the work cannot be
  done, logged and alerted, never as if it were recovered. On the
  desktop a failed save of the work makes the file read-only with a
  reason of its own (`local::ReadOnly::DeferredWork`), and a save whose
  outcome is unknown (this one or any later one) with another
  (`ReadOnly::NoMoreChanges`): what the desktop is told when the login
  that opened the file ends, and at every later check of the provider, is
  the file's state then (`LocalHost::read_only`). The window shows the
  reason given when it opened and does not change after a later save
  freezes the file; the provider refuses the writes all the same. A
  compacted copy
  saved before any login (Save a Copy As, which needs no login) is the
  file as its disk holds it, its work list included, so the copy finishes
  the work when an account first logs in to it. (Before, the work was
  finished in memory and saved by a separate Save, whose failure left it
  served and unsaved, and a local file finished it when it opened.);
- the lock requests, a server script's locks and their releases, and the
  release of a session's locks save nothing.

A server script's commit, lock, release and end, and a session's end,
take what they took, in the same order, and save as they did, so a
request that overlaps one of them sees what it could see before. One
thing is timed differently. The log lines that say a change was made are
written once it is stored (`Transaction::once_stored`), after the engine
lets go of the lock table and before it lets go of the store. op70, op71
and a server script's commit wrote theirs before they let go of the lock
table, op70 and the script's commit one per record right after releasing
that record's lock. So a request that takes only the lock table (op61,
op62, op65, a server script's lock or release, the release of a session's
locks) can now run while those lines are written, where it waited for
them. It finds the change stored and the records released either way; a
request that takes the store still waits for the lines. The commit's
container data takes its new members' unique ids from the engine's
environment, which on the running server is the system's random source
it took them from before. The release of a server script's locks at its
end, which was op65's for all of its connection's locks, is now the
release of a session's locks, which does the same.

op86's schema instructions and stored indexes, and a server script's
stored indexes, were made under one hold of the store and saved under a
second one; `submit` does both under one, so a request that overlaps sees
the file before them or after the save, as it could before. The
thumbnails of a library DeleteTable took are forgotten under that hold,
before the save, where they were forgotten between the two.

op87 and op88 took the store once for their change or refusal and again
for the save, op240's Save saved under one hold and asked whether the file
still held unsaved changes under a second, and every authoring edit but
ReplaceScriptSteps and CreateField asked whether the file is hosted
read-only under a hold of its own before it took the store for writing
(Field Control also read the object's field and value list under
another). `submit` does each under one hold, so a request that overlaps
sees the file before the operation or after its save, as it could before,
with these differences in a narrow overlap:

- A change another request left unsaved between the Save's save and its
  question (a server script's reserved record ids, which wait for its
  commit, or a change whose own save failed) made the Save answer 800
  though its save had succeeded; now it answers for its own save.
- The other way round, a Save whose save failed was answered Saved when
  another request's save succeeded between its save and its question; now
  it answers 800 for its own failed save, and a later save may still
  write the change.
- An edit that asked first, of a file that op73 left hosted read-only
  between the edit's question and its hold, was made in memory and
  refused at its save, with status 800 ("could not be saved: the file is
  open read-only") and a save-failure alert; now it is refused with
  status 810 ("open read-only") and no alert, as when the file was
  read-only before the request. ReplaceScriptSteps and CreateField never
  asked first: they try the save and are refused with 800 and the alert,
  as before.

The checks of the request alone that ReplaceScriptSteps, CreateField,
CreateLayout, PlaceField, PlaceText, PlaceButton, PlaceButtonStep and
ChangeParts made before taking the store (decoding a script definition, a
name, a label, a button's step or parameter, a kind of part) are now made
under the store's write lock, which is held that much longer; their
replies are the same. op240 is OpenMaker's own; what FileMaker Server does
with overlapping op87 and op88 is unmeasured. An op88 upload's encoded
bytes are freed once they are read, before the operation is submitted,
where they were freed after the data was stored. What the upload is
charged (`--upload-memory-mib`), the encoded bytes and the tree read from
them, was given back once the data was stored, before the save; it is now
given back once the save is done, so another upload during that save
finds that much less of the budget left.

An account edit read the clock before it took the store, once for its
stamps and once for its status time, and refused a new password of more
than 100 characters before it took the store. It now reads the clock
once, under the store's write lock, and refuses an overlong password
there too. The replies and what the file holds are the same; a stamp or
status time can be later by as long as the edit waited for the store. A
server script's account step now builds the edit, with the passwords it
was given, before it takes its lock on the account catalog, as it builds
the notices its release carries, so the lock is held for the edit alone.
The deferred work finished at opening and a saved found set are as
before: the first runs before anyone can reach the file, the second only
when no one holds the store.

**Failed saves.** Each save has one of the three outcomes of contract
rule 4 (section 2), and `submit` reads it for every operation
(`FileEngine::submit`, `Hosted::save_change`, `Store::save`):

- *stored*: the outcome is the operation's own;
- *not stored*: the save stopped before it renamed its new copy over the
  file (`store::SaveError::NotWritten`), so the file on disk is as it was
  and the copy the save made is gone. Everything the operation changed is
  taken back, and the reply is its storage error, with no notices
  (`Outcome::not_stored`);
- *unknown*: the rename succeeded and flushing the folder after it failed
  (`store::SaveError::Unknown`). The renamed file holds the change, which
  may or may not survive a crash. The reply is the storage error, with no
  notices; the tree keeps the change, as the renamed file holds it, and so
  does everything that follows from it (a commit's records are released);
  and the file takes no more changes (below).

After the rename nothing takes the change back. The save records that it
replaced the file, and that its outcome is unknown until the folder's
flush confirms it, at the rename, before anything that can fail or unwind,
a log line included (`Store::save`): the save is counted, the store counts
nothing as unsaved, and the file takes no more changes until the flush
succeeds. A guard that handles an unwind takes a change back only when no
save replaced the file since the change began (`Store::roll_back_unwound`,
which compares the saves the journal began with): a save left by a panic
after its rename leaves memory as the file holds it, and the file taking
no more changes.

An operation that changed nothing of the tree (op72 reading a serial,
op87 with a delta of 0, an account edit that leaves the account as it
was) still saves where its request saved before, which writes the changes
others left waiting: a server script's reserved record ids, the deferred
work of a local file before its first login. When that save fails it is
answered as it would have been, the waiting changes wait on, and the event
log says so; op240's Save answers that the disk lacks them.

*Taking a change back.* Every operation that changes the tree writes into
one journal (`store::Journal`, which `engine::Transaction` holds), which
keeps what each change replaced; `Store::roll_back` puts it back, newest
first, byte for byte and kind for kind, the root's list flag included: a
record as it was kept too, never through the checks of a client's write
(`put_record`, which makes a record a list and refused one held as a
value, leaving it deleted), the index entries, record lists and counts it
moved being nodes the journal kept before it. Each entry keeps,
with the node a change writes, the first of its parents the change
creates (as absent), so that taking it back removes what it created: the
first record of a table takes its record list `[T, 05]` back with it.
Whenever a change puts a child under a node that exists, the journal keeps
that node's own kind and value: a key that is not a list yet (one holding
an empty value, or nothing) becomes one as the child goes beneath it, so
the first such key on the way is kept whole (`Journal::save_parents`), and
taking the change back puts it back as that key. Every writer keeps its
changes this way, op72's and Set Next Serial Value's serial node under its
field definition too (`Store::kept_before`). A key that becomes a list
drops an empty value of its own (`OwnedNode::ensure`), as the writer
writes such a list: memory holds what the file will. A key that already is
a list and carries an explicit empty value (a push slot with a zero-length
value, which the page format can hold and the reader gives as an empty
value, from a file or an op13 upload) is kept so in memory, while a save
writes a bare list: that one form differs between memory and the file
([#754](https://github.com/OpenMaker-Dev/openmaker/issues/754); no
FileMaker Pro 26 template holds it). The
journal keeps what the change itself decided, never a second account of
it: op88's choice between the member that already holds the data and a
new one is made once (`library::store_plan`), and so is op87's between
counting and deleting (`library::adjust_keys`). What it keeps:

| Operation | What the journal keeps |
|---|---|
| op70, a server script's commit | each record as it was, the record lists and index entries and counts it changed, a script's library entries (as before) |
| op71 | the last issued id `[T, 00]` |
| op72, Set Next Serial Value | the field's serial node, and its definition whole when it is not a list yet |
| op13, op86's UploadMultiple and DeleteTable | the subtree each write replaced or deleted, moved, not copied, and the first parent a write created |
| op86's GenerateMemberKey | the catalog index keys it advanced |
| a find's indexes | the index nodes it put, absent as a rule |
| op88 | the library header and the new member's absence, or the counts of the member that already held the data; a new library's absence |
| op87 | the member's counts, or, when it is deleted, the member and the header |
| op73, the deferred work finished at opening | each record before its first change, the work list, the global values it drops, the indexes it rebuilds or drops (moved), the library entries it releases or stores, and the table's basis of stored calculations |
| an authoring edit, an account edit, a found set | the catalog members and nodes it replaced, and the parents it created (as before, the parents added) |

What follows from the tree is taken back with it: the decoded schema the
store reads is put back as it was before the operation's first change to
a catalog (`Journal::schema_from`), not read again from the tree put back
(a catalog that does not decode is served with the schema last read, and
reading it again would keep the one the operation decoded), and the
accounts with it; the basis of stored calculations is put back too, and
the store's schema generation advances, so the verified-index marks and
a server script's schema are derived again. The locks
the operation took are released and the releases that waited for it
dropped (`Locks::take_back`); an authoring edit's locks are released all
the same. The member keys and indexes a draft took from what reads left
waiting wait again, for the next write. Thumbnails of the libraries a
deleted table took or a script's commit emptied are forgotten, a new
field's basis is counted and the log lines that say a change was made are
written only for a change the file holds (stored or unknown). A run left
by a panic is taken back the same way, its locks released and what it
took of what waits put back, and so is one left by a panic as its accounts
are decoded after it (`engine::file::unwound`); the store then counts as
changed since its last save (below).

One seeded test checks this invariant alone
(`engine::tests::every_change_not_stored_leaves_every_node_and_flag_as_it_was`):
it generates trees whose records, field definitions, library and account
entries, their lists and catalogs' indexes take every shape a file admits
(missing, an empty value, a value, a value in pieces, an empty list), runs
every kind of operation on each, first with a panic injected at each
change it keeps in a journal, then with its save failing before its
rename, and compares the whole tree and every flag (the schema, the
accounts, what waits, the locks, the record of unsaved changes, the
read-only and no-more-changes states, the saves, the file on disk) with
what they were before; and while the store counts nothing as changed,
memory with the file on disk. Nine trees run by default, 2,000 seeded ones
in an ignored run. Its trees are written by OpenMaker's writer, so none
holds a list with an explicit empty value (#754).

*The record of unsaved changes.* The store keeps whether memory may hold
what the file on disk lacks (`Store::has_unsaved_changes`). Clear, it
means that memory is known to equal the file on disk, and an export then
copies that file rather than walk the tree ("Exports", above); but for a
list that carries an explicit empty value, which memory keeps and a save
writes as a bare list ([#754](https://github.com/OpenMaker-Dev/openmaker/issues/754)). Every
change of the tree sets it, and a save that renamed its new copy over the
file clears it, its folder flush confirmed or not. A roll-back of a change
whose save was not written puts back what it was before the change: the
change is taken back exactly, each node and record as it was kept, and
every change of the operation is in its journal (deferred work's journal
of each record it recomputes is taken back with it when the work is left
by a panic, `Store::guarded`, and absorbed into it otherwise). A run left
by an unwind is taken back the same way but leaves the record set,
whatever it was (`Store::roll_back_unwound`, in every unwind handler: the
engine's, `Transaction::guarded`, `Store::guarded`): a panic can leave
what no journal holds, so memory is no longer known to equal the disk;
the next save writes the tree, and an export walks it. A member-key draft
whose keys a write already passed changes nothing and sets nothing.

What a journal costs, on the file of a million records measured in
[#673](https://github.com/OpenMaker-Dev/openmaker/issues/673) (522 MB on
disk, 1.6 GB of tree in memory, 3.8 s to rebuild and 0.9 s to write a
save): a record commit keeps the records it writes, as before; op71, op72,
op87 and op13 a node or two of their own size; op86's DeleteTable of that
table keeps the table, moved, so its 1.6 GB are freed after the save
rather than at once; a find's index nothing; op88 a copy of the library's
header (a key and an MD5 per member). The largest is op73: each record
whose values it rewrites is copied before its change, so recomputing a
stored calculation on every record, or dropping or converting a field's
values in every record, holds a second copy of the records changed, about
1.5 GB on that file, until the save returns, beside the tree and the
save's 0.5 GB image; building or dropping an index copies nothing. Taking
the change back costs a put per kept node.

The other way to take a change back, discarding the tree and loading the
file again from disk, costs nothing while an operation runs but a full
load on each failure (2.2 s on that file, all readers waiting, and the
memory of the load), and is exact only when the file on disk holds the
tree as it was before the operation. It does not while changes wait
unsaved (a server script's reserved record ids until its commit, the
field indexes taken out after a save failed on content the file cannot
hold), and it reads the disk that
just failed a write. A journal needs neither, op70 and the edits kept one
already, and a commit refused part way (a write the file cannot hold) is
taken back without a load: one mechanism for every operation.

*The reply.* What FileMaker Server answers when it cannot write a hosted
file is unmeasured (it would need a server whose disk fails), so each
reply is OpenMaker's choice, made from what the request's reply can
carry. The FileMaker error is the save's (`hosting::save_error`): 809
"Disk/volume is full" for a full disk, a quota or a size limit, 810
"Disk/volume is locked" for a read-only file system, else 800 "Unable to
create file on disk".

| Request | Not stored, or unknown | What the reply can carry |
|---|---|---|
| op70 | a system exception (MARSHAL), as before; not stored, the records stay locked and the staged records the commit did not reach go back to the temporary area | no status (write-path.md 3.4) |
| op71 | status 800, 809 or 810, the key `[00]` | a status (write-path.md 3.1) |
| op72 | status 800, 809 or 810, an empty value | a status (write-path.md 3.2) |
| op13 | a system exception (MARSHAL), as op70; the log says it was not stored, or, unknown, that it may or may not be stored (`connection::upload_not_stored`) | no status (schema-path.md 3.3) |
| op73 | a system exception (MARSHAL), as its refusals | an empty reply (write-path.md 3.6) |
| op86 with schema instructions (UploadMultiple, GenerateMemberKey, DeleteTable) | the error slot `[00] = 02 <800, 809 or 810>` alone | the error slot (query-program.md 11.2) |
| op86 without them, a server script's find | the program's answer: what it found waiting and could not store (indexes finds built, member keys other programs reserved) waits again for the next write, logged | (a program that changes nothing stores nothing of its own) |
| op86 with GenerateMemberKey, once its own write was submitted | the file taking no more changes: the error slot with 800; a key it issued not in its catalog's index: the error slot with its own write's code (800 when it had none to fail); else the program's answer (below, "Keys a request issued") | the error slot |
| op87 | `ffffffff` | the failure count (containers.md 5.3) |
| op88 | status 800, 809 or 810, the save's message as the error text, no member | a status and a text (containers.md 3.2) |
| op240, an edit | status 800, 809 or 810 with what the file keeps; unknown: status 65535 (`OUTCOME_UNKNOWN`), which `om-author` reports as `outcome_unknown` | OpenMaker's own extension (docs/authoring.md) |
| op240, Save | status 800, 809 or 810; unknown: status 65535 | as above |
| a server script's commit, Set Next Serial Value, and the serial numbers New Record and Duplicate Record take | the step's error 800, 809 or 810 | Get ( LastError ) |
| a server script's end | its result's error 800, 809 or 810 (op30's reply, a schedule's last error), whatever its last step's was | op30's error |
| an account edit | "not written" with the save's message; unknown: an outcome-unknown error, which a script's log and the desktop's message say may or may not be in the file (3 for a script step either way) | as its refusals |
| the desktop's found set | not kept, a warning in the log | — |

*Keys a request issued.* GenerateMemberKey issues a key while the
program reads the tree (`derived::issue_member_key`), and the key waits
until a write takes it (`engine::Waiting`): the program's own draft, or
another request's write that held the store first and stores it with its
own. The request keeps the ids it issued, with their catalogs, and once it
has submitted its own draft it answers from one look at the file, under
one reading of it (`derived::issued_keys`, `IssuedKeysAre::error`):

- the file takes no more changes (a save's outcome was unknown, the one
  that wrote the keys or any other): the error slot with 800, whether or
  not its keys are on disk;
- else a key it issued is above its catalog index's last id
  (`[C, 01, 00]`): no save stored it (one failed and the key waits again,
  or the file became read-only and no write takes it), or a write dropped
  it (its catalog no longer able to take it) or put the counter back below
  it: the error slot a key not stored answers, with its own draft's
  error, or 800 when it had none that failed;
- else the index covers its keys: a program whose only write is its keys
  answers its own answer, also when its own draft failed and another
  request's write stored the keys before the look; a program with catalog
  writes of its own that failed keeps that failure.

This is enough for what it has to show, that no outcome that must not be
answered as success passes unseen. An unknown outcome freezes the file
for good (until it is hosted again), so the look at the end sees it,
whoever's save it was and whenever it came before. Any save that stored
nothing leaves the index below the key (a save that fails puts back what
it took), so the look sees a key not stored, whoever's write it was. It
reads only the index and the file's state, so no state per request is
kept in the store, and a request that ends by a panic leaves nothing
behind. What it establishes is that the catalog's counter covers the key,
not that no one else was given the id: op240's Create Layout allocates
layout ids from the index and the members without seeing the keys that
wait, so it can give out an id a request reserved, and the look then
answers stored
([#738](https://github.com/OpenMaker-Dev/openmaker/issues/738)); a write
whose own catalog changes put the counter back below a stored key lets
the same id be issued again
([#736](https://github.com/OpenMaker-Dev/openmaker/issues/736)). Both
allocations are older than this.

*No more changes.* After an unknown outcome the store refuses every later
save (`Store::writes_refused`), and `submit` refuses every operation that
would change the file before running it, so nothing is drawn from its
environment, with its storage error (code 800) saying that the file takes
no more changes until it is hosted again; op240's Save answers status
65535, as whether the disk holds what the file holds is what is unknown.
Whether an operation would change the file is decided from its value: an
op72 that neither replaces nor advances the serial (flags 0), an op87
with a delta of 0 and a find's draft with no index to store read, and
are answered (`engine::file`, `changes_file`). An op86 program with
schema instructions is refused before it runs, with its error slot, so
that it reserves no key that could never be stored; one without them
runs. What a draft took waits again. Reads, op60 and the operations that
change only the lock table go on. One alert is raised (`Event::save_failed`: the
file may or may not keep the change and takes no more until it is hosted
again), and the administrator's list of files gives the reason in the
file's `error` with its status unchanged (`FileInfo::error`;
admin-console.md). Closing the file and opening it again, or a restart,
loads it from the renamed file, which clears the state. The no-more-changes
state is held in the store (checked under its write lock) and once more in
the file (`Hosted::writes_refused`, copied from the store's as each write
lets go of the store, an unwound one too, and read without a lock), so that
the list of files does not wait for a busy store.

*A leftover copy.* A save that stops part way (the process or the machine
stopped) leaves `<file>.omsave`. A save creates its new copy only where
nothing is (an exclusive create through the file's folder), so while
anything is at that name every save of the file fails before its rename:
the change is not stored, it is taken back, the reply is the storage
error (800) and the log line and the alert say which entry is in the way
of the save's new copy. Nothing in OpenMaker removes the entry, or
anything else at that name: a file it did not create in the same call is
never unlinked.

- OpenMaker Server hosts a file opened with anything at its save's copy
  name as it hosts any file, and raises an alert that names the entry's
  path (`Event::save_copy_in_the_way`; `hosting::load_hosted`,
  `store::copy_beside`, which looks in the file's folder without
  following a link, and counts an entry it cannot look at). Every change
  then fails at its save as above, loudly, and that ends by itself when
  the administrator removes the entry: the next save works, with no need
  to close and open the file. Deferred field work found as the file is
  opened ends as the recovery rule says: its save fails, the work is
  taken back and the file is served read-only, as its disk holds it
  (above, "finishing deferred work").
- The desktop opens such a file read-only (`ReadOnly::CopyInTheWay`), as
  at #715's head, says why in the window and logs the entry's path. It
  looks with `symlink_metadata` on the path, so an entry it cannot look
  at counts as nothing there (its save would then fail as above).

What a save checks of its own copy is the base's, unchanged here: after
writing and before renaming, and again before removing the copy after a
failure, it checks that the copy is still the entry it created (the same
device and inode; where there is no file identity, as on Windows, only
that a regular file is there), and then renames or removes it in a
second call, so a replacement between the two is not seen.

OpenMaker Server holds no lock on the files it hosts, as before: another
OpenMaker that opens the same file is not kept out
([#724](https://github.com/OpenMaker-Dev/openmaker/issues/724)).

## 6. The store engine

Crate `om-store`. It knows paths, values, files, entries and pages. It
knows nothing of records, catalogs or the protocol.

### 6.1 On disk

One store folder per server, used by one process at a time. It holds no
symbolic links, and its names are lowercase ASCII, so that no two can be
one file on a file system that ignores case (`om_store::sys`):

```text
<store>/lock       locked by the running server (flock on Unix), never removed
<store>/log/       the change log, in segments
<store>/pages      the page file: superblock slots, then pages
<store>/blobs/     container data by content hash (section 8)
<store>/state      this server's id, its start count, its term and vote,
                   the run of log segments in use, the marker and the cut
                   count (section 6.3), whether it is in repair
                   (section 7.5), and the mark of a copy passed over
<store>/incidents  what the store was approved to give up, each recorded
                   before it acted (so a record can describe an act
                   that never ran), until an administrator acknowledges
                   it (section 6.12)
```

The start count is raised by one and flushed each time the server starts,
before it writes or sends anything else but a flush of the store folder
and the copy of `state` it read, written back unchanged (below); an open
cut short can skip a
value, and no value is issued twice. `state` holds two copies in fixed
slots, each with a number and a hash, and the valid copy with the highest
number counts. A marker advance writes the next number in place over the
slot that does not hold the current copy and flushes: one write and one
flush per commit batch. Every other change - the start count, the term
and vote, the segment run, the cut count, the repair record, a marker
that goes back - is written to both slots, one after the other, each
flushed, before it counts. Losing either slot therefore costs at most one marker advance:
step 3 of section 6.7 re-establishes its index and hash from the log when
the log's tail is whole, and its committed position equals the durable
one on a single server and is learned again from the leader in a cluster
(step 4, section 7.1). Losing a slot and the log's tail together can lose
that batch, which the open below records as possibly lost, and one slot changed on the device while a write to the
other is cut short can leave no valid copy. An open first writes the
copy it read back into its slot and flushes it, because an earlier
process's failed write or flush can leave the system's cache showing a
copy the device does not hold. When an open had to pass over a copy
(damaged, or not readable), it cannot tell whether that copy held a newer
marker. After the flush above, which leaves the copy it read as it was,
it writes into the slot it passed over a copy that says one was passed
over, with the raised start count, flushes it, and then writes the same
into the first slot. Every
later start honors that mark, however whole the two copies then are,
until the update of `state` that settles it, which is written to both
slots: the marker moved over a whole batch that followed the later of it
and the checkpoint's entry in the log, or, when the open finds none, the
server recorded in repair on a cluster (section 7.5) and the loss
reported on a single server (section 6.12). An open that cannot reach the
marker fails before any of this, and the mark stays. The open changes nothing in
the log before the mark is durable. While the mark is set, an open first
walks the log writing nothing, only to decide whether it must record a
possible loss first; what it then writes and publishes comes from its
own walk, as any open's does. A
crash therefore cannot hide that a copy was lost (server-storage-format.md
1.3). On a single server the loss is reported by recording it in the
incident journal (section 6.12) before the mark is settled. The repair
record a cluster member needs, in repair with a required end not yet
known (section 7.5), is not built: it waits for replication. A server
without a valid copy does not know what it confirmed, and does not start
(section 7.5). The format and what the copies protect against are in
[server-storage-format.md](server-storage-format.md) section 1
(`om_store::state`).

### 6.2 State and positions

The **state** of a store at an entry is everything it answers from:

- for each file: its tree, its page 0, its name and id (the index of the
  entry that added it), whether it is read-only and the image kept for
  it (section 6.10), the list of its container files stored outside the
  `.fmp12` (section 8), and its design history (section 9);
- the lock table and the session table, with each server's highest start
  count (section 7.3);
- the number of references to each blob (section 8);
- the cluster's members and the tree format version (section 7.6);
- the groups that are open, with the effects of their parts (section 6.9);
- the index, term and hash of the last entry applied.

All of it is kept in trees in the page file, one per file and one for the
rest ([server-storage-format.md](server-storage-format.md) section 5.3).
A checkpoint names every root in one superblock (section 6.7), so a
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
belongs to (by id), a payload, and a SHA-256 hash over the previous
entry's hash and its own content. The chain of hashes shows a missing,
reordered or altered entry.

| Kind | Payload |
|---|---|
| change | one write operation's effects on one file: tree edits (put a leaf, a list or a list with a value in place of what was there; delete a subtree), lock table edits (set, remove, release a session's locks on the file), notices, the operation's session and sequence number, and its reply. It may be part of a group (section 6.9) |
| session | a session opened or closed, or a server started (section 7.3) |
| file | a file added, removed, renamed or cloned |
| members | the cluster's servers, or the tree format version (section 7.6) |
| mark | a named point: a restore point, a release, a comparison of servers (section 7.5) |
| empty | a new leader's first entry, and a restart's when groups were left open: it discards every open group (section 6.9) |
| group | a group discarded (section 6.9) |

An entry holds **effects**, not the request: the edits the operation made,
as computed by the server that ran it (section 7.2). Applying an entry is a
function of the state before it and the entry alone. Each entry is applied
once: lock counts and share counts are not safe to apply twice.

**Segments.** The log is a sequence of files of one fixed size
(`om_store::log`; the byte formats and the order of every write are in
[server-storage-format.md](server-storage-format.md) section 3). The size
is a setting, 64 MiB by default; a segment keeps the size it was made
with, so after the setting changes, segments of other sizes remain in the
log. A segment's space is allocated when it is created, ahead of need, so
that an append never waits for an allocation where that can be avoided
and never fails half way for lack of space: an append that needs a
segment the disk has no room for is refused before anything of it is
kept. A segment is used only after its header and its folder are flushed
and `state` lists it. The header holds the store's id, the segment's
first index and the term and hash of the entry before it. A listed
segment that is missing, or has no valid header, is damage (section
6.12), whatever the other segments hold. An entry never spans segments.
Its payload is bounded by another setting, 16 MiB by default, which must
fit in an empty segment, and in any case by the format, 64 MiB; a batch
that does not fit in what is left of a segment is written as two batches,
the second at the start of the next segment.

**Batches and the marker.** The entries appended between two flushes are
a batch: a header with its first index, its entry count, its length and a
hash, then the entries. Appends wait for no timer: one flush covers every
entry appended while the previous flush ran. After a batch's flush
succeeds, the server records the **marker** in `state` (section 6.1) and
flushes it: the index and hash of the batch's last entry, and its
committed position: the last entry of its own log known to be committed.
An entry is **durable** when the
marker covers it. Only then is it acknowledged to a client or to another
server, or counted in a vote. A commit waits for two flushes, one of the
log and one of `state`; a batch that reaches a new segment waits for two
writes to the log and for both slots of `state`, since the new segment is
listed with the marker. When a leader's log requires a follower to cut
entries the marker covers (section 7.1), the follower records the lower
marker and a raised **cut count** in both slots of `state`; from then the
cut is done. Every batch carries the cut count it was written under, and
no batch written before a cut is read after it, even when the cut entries
are sent again exactly and what followed them on disk chains again. A
follower cuts only with a committed position given since it last
started: after a restart the one its marker recorded can be one advance
behind (a lost copy of `state`, section 6.1), so until the leader gives
it again the log refuses every cut, and then any at or before it. What
the caller supplies is never a value of the follower's own choosing, and
never past its own log: on a cluster the lower of the current leader's
committed position and the last entry of the follower's log known to match
the leader's (Raft's rule for a follower's commit index); on a single
server the last entry of the flush. The log refuses a committed position
past its own entries. The bound is what lets a follower cut stale entries
it holds from an old term: had it recorded the leader's committed
position while they were in its log, it would refuse the cut that removes
them for good. On a single server nothing cuts.

**An unreadable end.** When the log cannot read a place after its marker,
it ends there for that start, cuts nothing and takes no appends
([server-storage-format.md](server-storage-format.md) section 3.6). On a
cluster the leader's consistency check has the follower cut from the entry
after the last one it read, under the rule above; a cut from further on
removes nothing. What followed that place may have been acknowledged, so
the member is in repair until it holds the leader's log again (section
7.5). On a single server the store stops and reports (section
6.12), and the administrator opens it again later, restores, or accepts
the loss of whatever may follow that place. Accepting it discards only what
lies after the last entry the start verified, never an entry the marker
covers, so it needs no committed position. The confirmation rule keeps a
cut from reaching entries the server confirmed before a restart made it
forget; past the marker such entries exist only when a lost copy of
`state` took the marker back one advance (section 6.1). Giving them up is
then the loss the administrator accepts: a single server has no leader to
give a committed position, and the one it recorded is at or behind the
marker. A cluster member never gives up an unreadable end on its own:
its leader has it cut (section 7.1).

The marker says "everything through here was written and flushed", and it
is kept outside the log on purpose: a segment, or the end of one, can be
lost without taking the record of what it held. That is what lets a
damaged log be read correctly (section 6.12). A log that does not hold,
whole, every entry through the marker has lost something confirmed,
however cleanly it ends. Whatever follows the marker was never
acknowledged to anyone, unless the marker itself is one advance behind,
which a lost copy of `state` can make it (section 6.1): the batch after
it may then have been acknowledged, whether or not the log still holds
it.

Segments are written with synchronous writes (`O_DSYNC` on Linux;
elsewhere each write is followed by a flush of the file's data), and that
write is the log's flush: it returns once the device holds the batch, and
a failure is reported by the write itself, so that it cannot be hidden by
the system's cache. What follows the marker at a start is kept or cut by
whole batches (section 6.7).

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

Each tree is a B+tree of fixed-size pages (16 KiB) in the page file,
built by `om_store::tree` over the page store interface
`om_store::page::PageStore`; the byte format and the shape rules are in
[server-storage-format.md](server-storage-format.md) section 2. A
file's keys are paths in an encoding that sorts as the key tree does: each
segment's bytes with `00` written as `00 ff`, followed by `00 01`. A
subtree is then one contiguous key range, in depth-first order. An item's
value is a kind byte (list, value, or both) and the node's value. A path
has at most 64 segments, which bounds the ancestors one edit can create.
An item too large for a quarter of a page keeps its first 1,024 bytes in
the page and the rest in overflow pages, hashed like any page. An index
page holds, for each child, the shortest key that separates it from the
child before, and that key spills to overflow pages by the same rule.

Pages are **copy-on-write**: a change writes new pages for the leaf and
for every page above it, and the old pages stay as they were. An index
page also holds, for each child, its page number, its hash and the index
of the entry that last wrote it. A page's hash is SHA-256 over its items
and, for an index page, over its children's hashes, without their page
numbers. Each page also carries a checksum of all its bytes, page numbers
included (SHA-256 over the page's hash and the bytes the hash leaves out,
cut to 64 bits), so damage to a pointer is found in the page that holds
it. So:

- reading a page recomputes its hash from its bytes and checks it against
  the hash its parent holds, which finds damaged pages, pages written to
  the wrong place and pages never written. The `.fmp12` format has no such
  check (file-format.md 5);
- the store can tell from a few pages whether anything under a path changed
  since a given entry: from the index pages along the two ends of the
  path's key range, at most two a level, and no leaf. The answer is never
  "no" when something under the path changed, and can be "yes" when only
  another path in the same page did. The cache of section 6.6 and the
  check of section 6.9 rely only on the first: a false "yes" costs a cache
  miss or another pass;
- a tree's root hash depends on its content and its shape, and not on
  where a server put its pages.

**One shape per entry.** How pages split, merge and pack is fixed by the
**tree format version**, which is part of the state. Every server applies
entries with the same version, and the version changes only by a `members`
entry once every member supports the new one (section 7.6). Applying an
entry is deterministic: the shape is a function of the sequence of edits
alone, never of page numbers, timing or what is cached, so every server
has the same root hashes at the same entry. Every page an entry writes
carries the entry's index, and an entry that changes a tree writes its
root. A tree is never rebuilt in another shape except by an entry.

The trees keep their pages in the **page file** (`om_store::pagefile`;
[server-storage-format.md](server-storage-format.md) section 4). Pages
committed and not yet written by a checkpoint are held in memory apart
from the **page cache**, which cannot evict them. The cache holds pages
read from disk, up to a configured size, least recently used first out.
It is keyed by page number and hash, so a page number that is reused
never returns the page that was there before; a page never changes once
it is on disk, and a number freed for reuse is dropped from the cache.

The **superblock** names, for a checkpoint (section 6.7), the entry it
covers, the tree format version and the first page of the checkpoint's
**meta stream**, by number and hash. The meta stream holds the roots of
the state's trees by name, the restore points that are kept (each with
its own meta stream) and the server's own map of free pages, and every
page of it is checked by the hash that names it, so a copy of the
superblock has a fixed size however many files the server hosts. A copy
carries the checkpoint's number and its own hash, and is written twice.
There are four slots in two pairs: a new checkpoint's two copies go into
the pair the newest checkpoint does not hold, over the older
checkpoint's copies when there is one.

### 6.5 Writing

1. A connection's handler checks what it can against a view and builds a
   write operation.
2. The file's executor runs operations one at a time against the
   **working state**: the published state plus the effects of every entry
   appended and not yet published. Before it appends an operation's entry
   it reserves the page space that applying the entry will need, and
   applies the entry to the working state in its one unit of work; an
   operation that cannot reserve is refused with nothing appended. Then
   the entry is appended to the log.
3. The entry becomes durable (section 6.3). On a cluster it is also
   replicated until it is committed (section 7).
4. The entry is **published**: views taken from now on include it, its
   notices go out and its reply is sent. Entries appended before one
   flush are all published by it, or none.

If anything fails after an entry's unit of work committed (its append,
the flush, the marker), the working state is ahead of what is durable and
cannot be taken back: the store stops and is opened again, which rebuilds
the state from the durable log. `om_store::store` does these steps for
one server, driven by its caller
([server-storage-format.md](server-storage-format.md) section 6).

A view never shows an entry that is not committed, so no client acts on
data a crash or a change of leader can take away. Operations of different
files run in parallel. An operation that must be all-or-nothing is one
entry, or one group (section 6.9).

New pages stay in memory until a checkpoint writes them, so a commit costs
its share of two flushes and the pages on its path.

### 6.6 Reading

A view is a reference to a file's root at a published entry, with a pin
that keeps that version's pages while the view lives. Published pages
never change, so the page cache never invalidates anything: it only
evicts, and its size is fixed by configuration. A hosted file larger than
memory is served from disk. A view never reads the working state.

Encoded download replies are cached by file, path and the entry that last
changed anything under the path. A layout or script that many clients
download is encoded once per version.

Long reads hold their view: an export, a backup, a paged download, a
large find or a SQL query sees one version while writes go on.

### 6.7 Checkpoints, space and recovery

A **checkpoint** writes the pages of the published state that are not on
disk yet and the server's map of free pages, flushes them, then writes
and flushes its superblock copies. Only published entries are ever
checkpointed: the page file checkpoints its newest version, so the
engine applies an entry to the trees when it publishes it, or pins the
published version while it applies ahead and checkpoints only once
everything applied is published. A checkpoint is of one exact version:
it covers every change of its entry and of those before, and none after
(the page file refuses the rest; server-storage-format.md 4.6). The
engine calls the page file as 4.6 sets out: one caller applies entries
in order, one at a time, with one open reservation and one unit of work
an entry (everything the entry changes, in every tree, committed once;
the page file holds each tree's root and changes it only then), and
another checkpoints and reclaims. The one that checkpoints
has the applier take no new entry for the moment of a checkpoint's first
check and plan, and keeps a restore point only while no reservation is
open; otherwise the page file refuses the checkpoint, or the keep, for
as long as the applier does not pause. The order of the writes and
flushes, and what a crash at each step leaves, are in
server-storage-format.md 4.6.

**What is kept.** The two newest checkpoints are kept whole, and so is
every checkpoint held as a restore point (section 11), which each
superblock lists. A checkpoint is retired when its superblock copies are
overwritten, and a restore point when no superblock on disk lists it (a
dropped restore point is no longer listed by the next checkpoint, and is
retired by the one after it). A page is reused only when no superblock on
disk names a checkpoint or restore point that uses it, and no clone, no
state copy in progress and no view open in the process refers to it:
every page that leaves the newest version is recorded with the version
that wrote it and the one that released it, and is reused only when no
checkpoint on disk, restore point listed, checkpoint being written or
**pin** lies between the two. The engine pins the version a view, a
state copy or a restore point reads, for as long as it uses it: the
newest (`pin_newest`, also for an entry that changed nothing, which has
no version of its own), or a checkpoint or restore point on disk. Pins
are in memory, so after a restart only what the superblocks on disk name
is kept. The log is kept from the oldest
kept checkpoint, and further back while another server needs it
(section 7.5). So when the newest checkpoint cannot be used, the one
before it still has every page it names and every entry after it, as far
as the device kept them. An open on the newest checkpoint does not need
the entries before it, and reaches it past them; it checks those it walks
(the ones in the segment that holds the entry after it) and reports where
they stop checking, and whether the checkpoint before could replay past
that place, without failing (server-storage-format.md 3.6). The segments
wholly before that one are not read at open: the background check of
section 6.12 ("Slow damage") is what finds damage there. The
page file keeps a reserve that only recovery, checkpoints and the discard
of a group may use, so that a store whose writes have run out of space can
still start, checkpoint and free space; the room a checkpoint's meta
stream needs is set aside beyond it. A reservation the disk cannot hold
is refused before anything is written.

The **map of free pages** records which pages are free, which are dying
and from when to when they were reached, and how many trees share each
page (section 6.8). It is each server's own, and is written whole in
each checkpoint's meta stream, as of that checkpoint. A subtree dropped
whole is released by its root and taken down lazily, a page at a time.
Changes to the map since a checkpoint are rebuilt by applying the log:
the same entries applied from the same checkpoint, with the same
checkpoints, pins and reclaims between them, give the same map, page
numbers included. A dying page that a map on disk still records as
needing its bytes keeps them until every checkpoint on disk records it
settled (server-storage-format.md 4.5).

**Starting up.**

1. Read `state`. With no valid copy the server does not start.
2. Take the newest checkpoint on disk: the one with the highest-numbered
   valid superblock copy, whose meta stream reads back and agrees with it
   and whose tree roots read back with the hashes it names. The open
   gives nothing up (#750). If the device fails a read of a slot or of
   that checkpoint, or that checkpoint's bytes do not check, it writes
   nothing and reports which checkpoint (or slot), why, and what giving
   it up would lose: the restore points it lists that the checkpoint
   before it does not. A read the device fails says nothing of the bytes,
   which a later read may return whole, so the server is started again
   later. Giving a checkpoint up and opening on the one before it is a
   separate, explicit call, given the approval the report carries: what
   to give up, what goes with it, and the checkpoint to land on, each
   checkpoint named by an identity that differs when a number comes again
   (unless the contents are the same in every byte, and then so is the
   loss). The call does exactly
   that or nothing, so the server records the report durably before it
   makes the call, and the loss it recorded is the loss that can happen.
   The store makes that call by itself for damage, after recording the
   approval, with the restore points that go with it, in its incident
   journal (section 6.12); for a read that keeps failing it stops and
   reports, and an administrator's explicit call records the approval and
   then makes it. The
   checkpoint used has its copy written into both of its slots, and the
   other pair keeps only its predecessor's copy: what an open writes over
   is a slot that held nothing usable (bytes that do not check, or a copy
   of a checkpoint whose pages are free), and the explicit call writes
   over the slots of what it gave up, before any of its pages can be
   written over. Deeper damage is page damage (section 6.12), found when
   a page is read, and does not send the server back to an older
   checkpoint.
3. Read the log from that checkpoint and check it against the marker: the
   entry at the marker's index must be there, whole, with the marker's
   hash, and so must every entry between the checkpoint and it. A
   checkpoint that covers the marker's entry answers for it, as after an
   installed state (section 7.5). What follows the marker and is whole is
   kept: it is flushed and the marker moved over it before anything is
   published from it. What is damaged or incomplete after the marker is
   cut; a place after the marker that cannot be read ends the log there
   for this start instead, with nothing cut (section 6.3). A cluster
   member whose log ended so, or whose start had passed over a copy of
   `state` (or found the mark of an earlier start that had) and found no
   whole batch after the marker, is in repair (section 7.5).
4. Apply the entries after the checkpoint, each once, in order, as far as
   the committed position. On one server that is every durable entry. A
   member of a cluster applies as far as the committed position its
   marker recorded and learns the rest from the leader (section 7.1).
5. Free nothing in the log before step 4 is done. The page file frees
   during step 4 what normal running frees, which is only what no
   checkpoint or restore point on disk reaches, so applying the entries
   again never needs more space than the first time
   (server-storage-format.md 4.8). When the pages waiting for a
   checkpoint pass their budget during step 4, the last entry applied,
   which is durable, is checkpointed and step 4 goes on.
6. Write this start's own `session` entry (section 7.3), the first write
   of every start, and publish it. It is part of the recovery for space:
   when the pages waiting for a checkpoint or the disk refuse it, the
   recovered version, which is durable, is checkpointed first, which
   frees what the checkpoints and restore points on disk no longer need.

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
Nothing observed of FileMaker works this way (it commits such work in
pieces, below): groups, their entries, their rules and their bounds are
OpenMaker's own design. The entries and what they do to the state are
built (format sections 5.2 to 5.8, and 4.8 for space); which operations
use a group, and what is marked *not built* below, are not.

- **Two kinds.** A group either changes existing files, a **staged**
  group, or makes a new file, a **new file** group. Changes inside an
  existing file cannot show before the group ends unless they are kept
  aside, so the first kind stages them and its close applies them. A new
  file is shown by nothing until it is published, so the second kind
  writes its parts straight into the file's tree and its close only
  publishes the file. An import (6.10), a restore and a clone are of the
  second kind; op73's field work, an index built for a find and a
  deploy's changes to records are of the first.
- **Entries.** A staged group's first part opens it: the group's id is
  the part's own index, and the part declares the paths the group
  changes, one or more, in one or more files. Later parts add edits.
  Parts carry tree edits only. The closing entry carries no edits of its
  own: it applies the staged ones and may carry the operation it
  completes, with its reply and notices. A new file group is opened by a
  `group` entry (*new file*) whose index is the id of the group and of
  the file; its parts are changes to that file; a `group` entry
  (*publish*) closes it, giving the file its name, flags and page 0. A
  `group` entry discards a group of either kind, and an `empty` entry
  discards every open group.
- **Staged, then applied.** A staged group's part is written, replicated
  and committed like any entry. Applying it adds its edits to the
  group's staging tree in the state, not to the file's tree. The close
  applies each file's staged edits, in order, with one tree apply, and
  drops the staging tree, all in its one unit of work. Every view shows
  the file as it was before the group or as it is after it. Because the
  parts are in the state, a checkpoint taken while a group is open has
  all of it, and a reopen finds the group whole or not at all. A staged
  group has at most 65,536 parts and stages at most 1,048,576 edits and
  64 MiB of them as they encode: the most edits one change carries and
  the largest payload, so that its close holds no more in memory than
  the largest change decodes to (measured for the worst shape found,
  1,086 MiB). A part past a bound is refused, typed, naming it (format
  5.6).
- **Written in place, then published.** A new file group's open makes
  the file's tree, empty, under the group's name, `group:<id>`, which no
  reader of files asks for, and no catalog row or name. Each part is
  applied to that tree as any change is, reserving only its own
  application, and keys that arrive in order take the append path. The
  publication writes the catalog rows and the name, and renames the tree
  `file:<id>`, in one unit, a fixed amount of work whatever the group's
  size; a name taken refuses it, typed, with nothing changed. Until then
  the file is no file to any reader, by its tree's name: the version has
  no `file:<id>`. A discard, by an entry or a restart, drops the tree,
  whose pages go back to the free pages. The group's declared path is the
  whole new file, and no entry but the group's parts reaches it.
- **What a view shows of an open group.** A view, current or a restore
  point's, lists only the store's tree and the files' trees
  (`View::roots`); a group's tree is not among them, and `View::root`
  gives no root for it. Of an open group a view shows the rows of the
  store's tree: that the group is open, its kind, how many parts it has,
  the bytes and edits it staged, the pages it holds, and its declared
  paths (for a new file, the whole file under its future id). Not its
  staged edits, and not the file it is making.
- **The staging is the parts' edits.** The staging tree, `group:` and the
  group's id, keeps each part's edits as the change carries them, keyed
  by file, part and chunk (format 5.3): a part appends at the end of its
  file's items, and the close reads them back in order. A staged copy of
  each file's tree, to which the parts would apply their edits and whose
  declared subtrees the close would put in place of the file's, would
  make the close cheapest: it would write only the paths down to those
  subtrees. It needs one tree to share pages with another (a clone, 6.8,
  whose shared pages no entry records yet) and a graft of key ranges
  between trees, and neither is built; clones are outside this work. So
  the close applies every staged edit. It writes what one entry with all
  of the group's edits writes, each page once (format 2.7), which is
  less than the parts' edits applied one entry at a time would write,
  and it holds them all in memory while it does.
- **Cost, measured** ([performance/store-groups.md](performance/store-groups.md)).
  For N parts of one 4 KiB put each, a staged group's close wrote 3
  pages for N = 1, 109 for 100 and 10,672 for 10,000, about one page a
  part, in 0.9, 2.9 and 325 ms, and the group held 431, 6,272 and 590,372
  pages. A new file group's publication wrote 5 pages in 107 ms or less
  at every size up to 110 MB (26,856 parts), and the group held 509 pages
  whatever its size. Its parts wrote what the same changes to an
  existing file write: the page file at 110 MB was 132 MiB with
  4,000-byte values, and 483 MiB with 4 KiB values, each of which takes
  an overflow page of its own
  ([#802](https://github.com/OpenMaker-Dev/openmaker/issues/802)).
- **What it changes.** A group declares the paths it changes. While it is
  open, an entry that changes one of them, or anything above or below
  one, is refused when it is applied, with a typed refusal, the same on
  every server (format 5.6); so is the removal of a file it declared a
  path in, and another group's open that declares a path meeting one of
  its paths. An ancestor that a put makes, or that it changes from a
  value to both, is not counted: it changes nothing at or below a
  declared path. The leader is to produce no such entry; the refusal is
  the check that it did not.
- **What it read.** *Not built.* A group also declares the paths its
  result depends on, which other operations may still change: an index
  built for a find depends on its table's records, and a record can be
  committed while it is built. Before the leader appends the last entry
  it checks, in the same step, whether anything under those paths changed
  since the group began (section 6.4). If so it adds parts that bring the
  result up to date and checks again, or discards the group and starts it
  over.
- **Locks.** *Not built*: a close's lock edits are refused in this build.
  A group's first part takes the protocol locks its work needs, in the
  lock table, for the session that owns the group: for op73 the table's
  exclusive lock (schema-path.md 7.2), for a deploy the locks of
  section 9. They are held until the group's last entry or its discard,
  each of which releases what the group took in the same entry. No other
  session can take a conflicting lock in between, and the last entry
  checks the group's preconditions in the same step that appends it.
- **One leader, one term.** A group is opened and closed by one leader in
  one term. A new leader's `empty` entry discards every group left open,
  and so does a restart: the store's open, when the state it recovered
  has groups open, appends an `empty` entry before its start's entry
  (format 6.2). The restart begins a new term, one above every term the
  store recorded, in which that entry and the start's are written;
  `state`'s term, which replication's votes use (section 7.1), is not
  raised by it. The leader can also discard a
  group by an entry; doing so when the session that owns the group is
  closed is not built.
- **One reply.** Only the last entry completes the operation: it carries
  the reply and the notices and advances the session's sequence number
  (section 7.3). A discarded group leaves nothing, so the same request
  sent again runs again. Work without a session, such as finishing
  deferred field work, starts again from what the file records as
  pending: for op73, the deferred list at `[T, 3, 1, fd]`, which the
  group's last entry removes. The close's reply is built; the operations
  are not.
- **Space.** The entry that opens a group holds the room for the entry
  that will end it, and a staged group's part holds the room for its
  later application (format 4.8), before it is appended; a part's own
  writes are reserved as any entry's are. The end's room counts what
  that entry writes in the store's tree: for a staged close, the
  session's sequence and a reply of the largest size; for a
  publication, the catalog rows of the largest size and the tree's new
  name; it is a bound at the store tree's height when the group opens,
  plus one, and an ending entry that finds that tree taller extends past
  it as any entry does, and can be refused as full on a disk with no
  room left ([#787](https://github.com/OpenMaker-Dev/openmaker/issues/787)'s
  kind of limit). The entry that ends a group (a close, a publication, a
  discard, an `empty` entry) takes what the group holds as its
  reservation, so a discard is funded by what it releases and runs on a
  disk that cannot grow; what it writes past that may take the reserve.
  What it does not take, and the dropped tree's pages, go back to the
  free pages. The log keeps room at the end of its segment too, while
  groups are open, for a discard of each and the next start's `empty`
  entry and its own (format 6.3): an entry that only gives back never
  needs a segment a full disk cannot give, and every other entry, a
  close and a publication included, leaves that room and is refused for
  the disk when only it is left. The holds are recorded in the group's
  row, so a reopen holds them again before it replays, and the
  accounting holds across checkpoints and reopens with a group left
  open. The bound a
  staged part holds is `entry_bound` of its edits at the file tree's
  height plus one: our own choice, loose, and not a proof
  ([#787](https://github.com/OpenMaker-Dev/openmaker/issues/787)). It
  covers the close while the file's tree grows at most a level beyond
  what the parts' own edits are counted to add; a close that needs more
  extends from free pages and the reserve as any entry does, and can be
  refused as full. One 4 KiB put holds 59 pages, about 55 times what its
  share of the close writes
  ([#786](https://github.com/OpenMaker-Dev/openmaker/issues/786)), so a
  staged group of 10,000 such parts grows the page file to 9.9 GiB while
  it is open, and staging it took 18 to 21 s against 6.3 s with no holds.
  The close holds the staged edits in memory, within the bounds above
  ([#788](https://github.com/OpenMaker-Dev/openmaker/issues/788)). A new
  file group holds only the room for its publication or discard.

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
that makes a new file (6.9, the second kind), whose publication makes the
file visible: the parts write the file's tree in key order, on the append
path, and nothing is staged or written twice. It refuses what the server
refuses to host today (encrypted files, `crates/om-hbam/src/container.rs`)
and a file with an item beyond the store's bounds (section 6.11). Built
so far: the walk into a tree in batches, each a unit of work of the page
store outside the log (`engine::paged::import`, section 5).

A file holding content the writer cannot write (section 4) is hosted
read-only, as today. The store keeps such a file's original image whole,
checked by its hash, and its export is that image. That is not built:
import refuses such a file. A file that cannot be read to its end is
refused.

**Export** walks a view in key order into the bulk writer of
[hbam-writer.md](re/hbam-writer.md) 7.2, which writes leaves, then index
levels, then the root, to a file it can seek in, with memory bounded by
the tree's depth and not its size. It writes page 0 as imported, with the
clean-close flag set and the change number advanced as the writer does
(hbam-writer.md 5.1). The result is checked by reading it back and
comparing it with the view node by node, kinds included. `om-hbam` reads
a file a page at a time (`stream::NodeStream`, which gives its nodes in
key order, kinds included) and writes one a page at a time
(`writer::NodeWriter`, which takes its nodes in key order, kinds included,
and writes the bytes of `writer::rebuild`). Built for one file's view
(`engine::paged::export`, section 5); none of the offers below is.

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
- a file's paths are held to the smaller of the store's bound above and
  what the file writer writes (`om_hbam::writer`): segments of at most 219
  bytes, 16 deep, and an entry of at most 1,019 bytes in the writer's
  index (`MAX_INDEX_ENTRY`, a slot for each segment and 4 bytes, and 3
  more for a list). Within that last bound every index page but the last
  of its level holds four entries or more, since the writer closes a page
  only when the next entry does not fit and four entries of 1,019 bytes
  fill its 4,076 bytes, so 16 levels index 4^16 = 2^32 pages, every page
  a file can number (the derivation is at `MAX_INDEX_ENTRY`). For four
  entries it is tight: at 1,020 bytes four entries no longer fit a page.
  For a file it is conservative: past it an index page may hold fewer
  than four entries, and a file fails only when its index's fan-out is
  too small for its pages (at two entries a page, 16 levels index 2^16
  pages), so some files past it could be written. FileMaker's 66
  templates reach 155 bytes,
  the tests' fixtures 34. Import refuses a file past it, so that what the
  store holds can always be exported; the store's write path will have to
  keep every change within it too
  ([#764](https://github.com/OpenMaker-Dev/openmaker/issues/764));
- an entry is at most as large as the request limits the server already
  enforces allow (`ConnectionLimits::upload_memory`), and at most what one
  log segment holds (section 6.3); larger work is a group, whose parts
  have a fixed largest size;
- the entries appended and not yet published, the groups open at once and
  the size staged in them each have a limit; an operation past one waits
  or is refused;
- space to apply an entry is reserved before the entry is appended
  (section 6.5). A server that applies another's entries and runs out of
  space cannot refuse them: it stops and reports, and is in repair until
  it has space;
- pages waiting for a checkpoint have a budget. Past it the page file
  refuses reservations as "checkpoint needed", a checkpoint is forced and
  operations wait. Pages a group's last entry creates may be written
  before the checkpoint that names them;
- views have a limit in number, and one held longer than a configured
  time by a SQL query is ended, so that old pages can be reused;
- a server too far behind for the log to be worth sending gets the state
  (section 7.5).

### 6.12 Storage faults

| Fault | Found by | Response |
|---|---|---|
| The log after the marker is damaged or incomplete | entry and batch hashes | cut after the last entry of the last whole batch kept, or at the later of the marker and the checkpoint's entry when none is (which can be before the first bad entry), and log the cut. Nothing after the marker was acknowledged, unless the same start passed over a copy of `state`, or found the mark of an earlier start that did (below) |
| The log does not hold, whole, every entry through the marker: an entry is damaged, the log ends early, or a segment `state` lists is missing | the hash chain, against the marker and the segment list | never cut. A cluster member is in repair (section 7.5). A single server stops the store and reports; the administrator restores (section 11) or explicitly accepts the loss: first shown what would be kept and given up (a plan that writes nothing to the log; making it runs an open's first steps, which raise the start count and can give up a damaged newest checkpoint, recording it first), then that plan done exactly, recorded first, or refused. It can be refused at two points. Before anything is written: a fresh plan differs (the log changed since), or the journal cannot take the record; nothing of the acceptance remains, and the start's first steps do. After the decision is durable: the record, the files set aside, the tail copied, the files lengthened and `state` holding the decision remain, and the open that follows fails when its own walk does not find the log as planned (`AfterGivingUp`, with the decision recorded); the next open starts from that decision. The plan is made from the checkpoint the store opens on: the chain from it through every segment that still reads is kept, only what cannot be checked is given up, and every file given up is kept, set aside under a name no open removes (server-storage-format.md 6.2) |
| The log cannot be read at a place after the marker | the device's error | the log ends there for that start, cuts nothing and takes no appends (section 6.3). A cluster member is in repair and is cut there by its leader (sections 7.1 and 7.5). A single server stops the store and reports; the administrator opens it again, restores (section 11), or explicitly accepts the loss of what may follow |
| One copy of `state` is damaged or cannot be read, and the other is valid | its hash, the device's error | the valid copy counts and is written to both slots with a mark that one was passed over (section 6.1). Its marker can be one advance behind, which the server cannot tell. The open first walks the log writing nothing, to decide. When it finds a whole batch after `N`, the later of the marker and the checkpoint's entry, the marker is moved over it, and no entry the lost advance can have covered is lost (a committed position recorded alone can still be forgotten, and is learned again). Otherwise, whether the log's tail was cut or it ends at the marker, a cluster member is in repair (section 7.5), and a single server records in its incident journal that no whole batch was found after `N` (a read that failed there counts as not found), so whatever it confirmed after `N` may be lost, before it writes `state`, and reports it until acknowledged. That record covers whatever the open then finds after `N`, which can be a whole batch a read failed to find before. An open whose first walk found a whole batch there and whose own walk does not fails before writing `state`, having written again only bytes it had just read and checked (a checked prefix of the batch it then finds incomplete), and the loss is recorded. A marked open that cannot reach the marker fails before any of this, and records nothing |
| `state` has no valid copy | its hashes | the server does not start: it does not know what it confirmed. A cluster member comes back only as a new server (section 7.5). A single server is restored (section 11). A folder that holds a store's log or page file and no `state` at all is the same fault, and is never created over; only what a creation cut short left is (server-storage-format.md 6.1) |
| A page is damaged, misplaced or missing | its parent's hash | on a cluster, fetch it (section 7.5). On one server, reads of that range fail, the file is marked damaged and the event is logged |
| A blob or a kept original image is damaged or missing | the hashes in its reference, checked whenever it is read (section 8) | on a cluster, fetch it. On one server, the read or export fails and the event is logged; damaged bytes are never served or exported |
| The newest checkpoint's meta stream or roots do not check | hashes | the page file's open writes nothing and reports the checkpoint and the restore points giving it up would lose; the store records that in its incident journal and then gives it up by itself, opening on the checkpoint before, which is kept whole (section 6.7); when the damage changed meanwhile, it records and gives up what the page file reports then (at most eight rounds), or the open fails |
| The device fails a read of a slot or of the checkpoint the open is choosing | the system's error | the open fails and writes nothing to the page file (its first steps have raised `state`'s start count, and, when the journal was lost, recorded that); start again later. A read that keeps failing is given up only by an administrator's explicit call, which records what it gives up and then opens on the checkpoint before (section 6.7) |
| A flush fails | the system's error | stop the server at once and recover on restart. Never retry the flush: the system may already have dropped the data (PostgreSQL's 2018 finding). An entry the failed flush covered may be found whole on restart; its outcome was unknown to its client (section 2) |
| The disk is full | a segment, page or reservation that cannot be allocated | refuse writes before appending anything, keep serving reads. A start whose own entry finds no room after a checkpoint does not open, and says to free space |
| Slow damage | a background job that reads every page and entry and recomputes its hash | as for the fault found |

**Recorded first, reported until acknowledged.** Everything the store
gives up is written to its incident journal (`<store>/incidents`,
server-storage-format.md section 7), flushed, before it is given up: a
checkpoint of the page file and the restore points that go with it, by
the store or by an administrator's call; entries of the log, and the
segment files set aside, by an accepted loss; whatever may follow an
unreadable end; and the last batch confirmed after a passed-over copy of
`state`. One act is one record, which says what was approved and about to
be done: a crash, or a call that then does nothing, leaves a record of an
act that never ran. Every start that opens reports every incident no
administrator has acknowledged; a start that fails reports the incidents
it recorded itself before failing (`OpenError::AfterIncidents`), and the
others are read with the store closed (`Store::read_incidents`). A start
that must record and cannot (the journal full of incidents not
acknowledged, a record too large, a write that fails) stops with the
journal's error, having done nothing of the act it would have recorded;
its first steps may have run (the start count raised, the page file's
slots settled, a damaged newest checkpoint given up and recorded). A lost
journal at its size limit has no room for the record of its loss: the
start fails as full until an administrator reads the journal and
acknowledges it. A record that says the same as one not acknowledged is
not written again: an act stopped after its record, and tried again at
every start, is recorded once. Acknowledging is an explicit call that
acknowledges everything a read showed the administrator, and is refused
when the journal reads otherwise now (an incident recorded since, a
change the read can see). What the read could not see goes with it,
unseen: a place the device failed to read at both reads, with any record
there, and the bytes past the read's limit. The journal is the store's own and is read without the log or
the page file; one that is missing, holds no record, or is cut short or
damaged does not stop a start that can record, which says what it found.

**What is not found.** The store finds writes a crash interrupted, damage
to any byte it stored, the loss of a file it listed, and a log that ends
before its marker. It cannot find a device that reported both flushes as
done and then holds neither the end of the log nor the marker that
recorded it: the log and `state` have then gone back together to an
earlier pair that agrees. On a single server such a device loses confirmed
changes without a sign. On a cluster the server concerned believes its
log ends earlier than it does, and nothing on its disk says so: the
repair rule of section 7.5 does not apply to it. Section 7.1's last rule
narrows what it can do. The comparison of section 7.5 shows the loss
only while that server's state still differs from the others'; once it
has caught up again, or has helped elect a leader without the entry,
the hashes agree.

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
  log from there: never at or before its committed position or the entry
  of the checkpoint it started from, and only once it has heard a
  committed position from the leader since it started, because the one
  its marker recorded can be one advance behind (section 6.1). The
  position it records is the lower of the leader's and the last entry of
  its own log it has checked against the leader's (section 6.3).
- A follower whose log ended at a place it could not read (section 6.3)
  has nothing readable there to compare. Once the leader has checked the
  follower's last readable entry against its own log, the follower cuts
  from the entry after it, under the same conditions, whether or not the
  leader has an entry to send for that index.
- The leader counts servers only for entries of its own term. An entry is
  **committed** when the leader holds acknowledgements for it from a
  majority, its own durable copy counting as one, and it is of the
  leader's term, or when a later entry is committed. An acknowledgement
  says that the sender's marker covered the entry when it was sent. A
  server can lose the copy afterwards, so a majority of copies need not
  exist at one moment; the fence of section 7.5 keeps a server that sees
  such a loss from helping to elect a leader without the entry.
- Beyond the paper: every message between servers carries the sender's
  start count (section 6.1). When the leader sees a higher count from a
  follower than it had seen, it forgets what it had recorded that
  follower to hold, learns it again from the follower's answers, and
  ignores messages of a lower count from then on. An entry already
  committed stays committed. This shortens the time in which a leader
  counts a copy that a restart has lost; it cannot end it, since an
  acknowledgement sent before a restart can arrive before the leader
  hears of the restart. Safety does not rest on it but on the fence of
  section 7.5.
- A new leader's first entry is `empty`. It runs no operation and answers
  no client's request until that entry is committed and published, so
  that its state, with the session and lock tables, holds everything
  committed before its term. Replication, and its answers to a server in
  repair (section 7.5), do not wait for that: the acknowledgement that
  commits the entry may have to come from a server that is leaving
  repair.
- A leader that cannot reach a majority steps down.
- The members in force for a server are those of the latest `members`
  entry in its log, committed or not (section 7.6).
- Beyond the paper: a server records, in its marker, its committed
  position: the last entry of its own log known to be committed (section
  6.3). It does not stand for election, and does not vote for a candidate,
  whose log ends before that position.

A server that restarts publishes only what it knows is committed
(section 6.7). Entries after that may be replaced by a later leader.

Raft assumes that what a server acknowledged stays on its disk. When a
server can see that its disk broke that assumption, or may have, section
7.5's repair rule keeps it from voting and acknowledging until it holds
the leader's log again. A loss the server cannot see is outside that
rule (section 6.12, "What is not found").

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
  connections upward. A `session` entry opens it and one closes it; one
  that closes a session not open does nothing. A server writes a
  `session` entry when it starts, which closes every session of its
  earlier starts and records the start count as that server's highest;
  one that names no higher count does nothing. The state
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
  lock counts are kept per lock; the store keeps each lock under its
  session and file, with its key, user and value as bytes it does not
  interpret, so that closing a session, or removing a file, releases
  them in the same entry (server-storage-format.md 5.3). Locks change
  only by entries, so every server holds the same lock table and a new
  leader starts with it. On a
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
So is one that may have lost what it confirmed without its marker showing
it.

- **The required end.** Before anything else it records in `state`,
  flushed, that it is in repair and its **required end**: the marker's
  index as it was when it entered repair. The marker is outside the log,
  so the required end is known however much of the log is gone. The next
  point is the case in which the marker itself cannot be trusted.
- **May have lost what it acknowledged.** The marker can be one advance
  behind what a server confirmed when a copy of `state` was damaged or
  could not be read at a start (section 6.1). The log's tail normally
  makes up for it: a whole batch after the marker is kept and the marker
  moved over it (section 6.7, step 3). A server is in repair when it
  cannot rule out that this failed:
  - a start passed over a copy of `state`, or found the mark of an
    earlier start that did (section 6.1), and the log held no whole batch
    after the marker: its tail was cut as damaged or incomplete, or it
    ended at the marker;
  - or the log ended at a place it could not read (section 6.3),
    whatever `state` showed.

  Neither sign says that an entry was lost. A crash that tears a write of
  `state` which covers no new batch (a term, a vote, a start count, a
  committed position recorded alone) can leave a damaged copy beside a
  log that ends at the marker, with nothing lost.
  So does damage to the older of the two copies, and so can an earlier
  start's cut tail whose bytes are still in the segment. A place that
  cannot be read may be one where nothing was written. The server cannot
  tell these from a lost advance whose batch is damaged, gone, or reads
  as never written, so it is fenced in all of them. That costs
  availability, never an entry: one fenced server of three recovers from
  the other two without anyone's help when they can reach each other and
  it, and with two of three fenced the cluster waits for an
  administrator.

  A batch that was flushed and is gone without a trace, with both copies
  of `state` whole and agreeing with the shortened log, shows no sign.
  That is the loss section 6.12 says cannot be found.

  The marker of such a server does not give its required end. A server
  is fenced, as below, from the open that finds a sign, and what must
  outlast a crash differs for the two. A passed-over copy would leave no
  trace once the copies are whole again, so its mark in `state` is
  durable before the open changes anything in the log (section 6.1), and
  every later start honors it. A place that cannot be read is still
  there for the next start to find; if a crash comes before the record
  below is durable and the place then reads, that start judges the log
  as it reads it, and nothing was passed over unseen. The
  update of `state` that removes the mark is the one that settles it: it
  moves the marker over a whole batch the log held, or records the server
  in repair with its required end not yet known. A log that ended at a
  place it could not read is recorded the same way before the server
  answers any other server. "Not yet known" is a state of its own, not an
  index. A server so recorded sends the leader a request
  that carries its start count. It takes an answer only from a leader of
  a term at least its own, given to that request: the last entry of the
  leader's log at the answer. It records that as its required end before
  it stores anything from the leader. The answer follows this start, so
  whatever this server acknowledged in an earlier start and that was
  committed before that leader's term, or by that leader, is in that log:
  a leader's log holds every such entry, and this server, fenced since
  the start, gave no vote that could elect one that does not. The answer
  proves no more than that. An entry committed later, or by a leader of
  a higher term that neither of the two has heard of yet, is protected by
  the servers that acknowledged it to that leader, not by this one. What this server acknowledged and was never
  committed needs no protection, so the end can lie before entries it
  once acknowledged.
- **Nothing the server does lowers it.** Installing a checkpoint,
  discarding or replacing its log, a restart, and any marker written
  while in repair leave the required end as it is. Only a leader can
  replace it. The log of the leader a server recovers from can end before
  the required end, when the entries up to it were never committed and
  that leader does not have them: the server would wait for an index
  that may never come. It then takes as its required end the last entry
  of that leader's log, given in answer to a request that carried its
  start count, by a leader of a term at least its own, by the argument
  above. A server that learns of a higher term while it recovers records
  that term first and begins again with that term's leader: what it had
  checked against the earlier one no longer counts toward leaving.
- **Fenced.** It casts no vote, acknowledges no entry and serves no
  client, across restarts.
- **Leaving.** It clears the record only when it holds a usable state
  and, durable, the log of the current term's leader through at least the
  required end, each entry checked by the hash chain against what the
  leader holds; a state installed from that leader at or past the
  required end counts for the entries it covers. The check is made
  against the leader of the term the server is in when it leaves, not
  the one that gave the required end. Whatever was committed with its
  acknowledgement before that leader's term, or by that leader, is in
  that leader's log, so it then holds what its vote must protect; what a
  leader of a higher term has committed since is protected by that
  leader's own majority.
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
| accept the loss of entries after damage, or of what may follow a place the log cannot read, or give up a checkpoint whose read keeps failing, on a single server (section 6.12): for damage, the plan of what would be kept and given up is shown first and accepted as shown, or refused when the log changed since; each is recorded in the store's incident journal before it is done; acknowledge the incidents a start reports; choose a copy after a comparison no majority settles (section 7.5) | on the servers concerned, with the entries or files named |
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
10. a server in repair never votes or acknowledges: from the open that
    found the sign, and across a crash from the moment the mark of a
    passed-over copy or the repair record is durable (section 7.5). It
    leaves repair only as section 7.5 says, also when the leader changes
    after its required end was given. No leader is ever elected whose log
    lacks a committed entry;
11. every server starts from any state the faults above can leave, or
    says why not;
12. no damaged blob or page is ever served or exported;
13. a group never completes while another session holds a lock that
    conflicts with the ones it took.

Clusters with a witness, members joining and leaving, state copies, and a
leader lost at each step of each of these are part of the generated
schedules. Checks 2 and 10 are promised for the faults the store finds
(section 6.12), and check 2 only while a copy of the change survives on
a server that the others can reach: finding a loss does not bring an
entry back. When too few copies survive, what is checked is that no
leader is elected without the entry and that the servers concerned stay
in repair and say why. A schedule that injects a fault the store cannot
find (a flushed write that leaves no trace, together with the marker
that recorded it) is checked only against what section 6.12 says of it.

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
