# Native record writes in OpenMaker's client

How `om-provider` creates, edits and deletes records on a hosted file. The
protocol and what the server does are specified in
[write-path.md](re/write-path.md); this note records the client's choices and
limits. The client computes field values itself, as FileMaker Pro does
(write-path.md 5.6); [auto-entry.md](re/auto-entry.md) describes the rules it
evaluates.

The hosted Root provider retains the decoded access rules from its own
account's privilege-set reply, as related owners do. Browse's record and
field capabilities and ordinary write checks use that account context;
local providers retain the local host's authoritative account information.
One synthetic OpenMaker Server regression signs into Root as Read-Only:
Create, Edit and Delete are unavailable, an edit returns 200, and a
separate session reads the original value. This is provider-level evidence,
not a native FileMaker or FileMaker Server comparison. Unavailable or
unsupported hosted access metadata still leaves enforcement to the host;
this is not complete hosted privilege coverage.

## Transactions

Each write is one transaction with its own id (write-path.md 1.2), sent in
FileMaker Pro's order:

| user action | transactions | write-path.md |
|-------------|--------------|---------------|
| New Record, then save | op72 for each serial number field (FileMaker Pro sends it after op71; Serial numbers below); op71 (count 1); for each unique field, op60 on `[T, 01, F]` and op86 ExistsInIndex; op14 the record, without `fc`; op70 `[T, 05, R] = 01`; op65 | 1.1, 1.3, 3.1-3.4, 5.4 |
| save an edit | op60 on `[T, 05, R]` asking for the mod count; op14 the whole record with `fc` one higher; op70 `R` = the changed field ids; op65 | 1.1, 4.2, 5.4 |
| Delete Record | op60 on `[T, 05, R]`; op70 `[T, 05, R] = 03`; op65 | 7 |

- Each field value goes into the record op14 sends in the form FileMaker
  stores (hbam-writer.md 2.3). Text of one byte per character, up to 1000
  bytes, is one leaf. Other text is a node with a header child of its byte
  length and UTF-16 length: longer text in 1000-byte pieces, and short
  text such as a container's reference (`01` and U+E000), which FileMaker
  Pro does not read as a picture from a leaf (containers.md 1.4, #911).
- Locks are asked for without waiting (timeout 0). A refusal (8003) reports
  who holds the record from the reply's user list (4.2).
- A record of the opened file's own tables is locked as its editing begins,
  as FileMaker Pro locks it (write-path.md 4.1; [#40](https://github.com/OpenMaker-Dev/openmaker/issues/40)): Browse's first
  change of it, and a script's, goes through `record.prepare_edit`, which
  opens the window's record session and locks the record before the change
  is taken. The lock is held until the record is saved or reverted, so
  another client's change of it is refused (301, naming the holder)
  meanwhile. Delete Record of the record being edited deletes it under
  that lock, its edits going with the session, as FileMaker Pro deletes the
  record being edited; a refusal before anything is sent (privileges, a
  file opened read-only) keeps the edits. A session that also holds a
  related record's write or container data is reverted first, which lets
  the lock go before the delete takes it again. A related record, a record
  of another file's table, and edits inside a script's own transaction are
  still locked only to commit them.
- op65 ends every transaction, also after a failure, releasing what it
  holds (4.4); the server also releases a connection's locks when it
  disconnects.
- An edit is refused when the record's modification count, from the lock
  and from the stored `fc`, differs from the count read when the record was
  displayed: another client changed it.
- op70's reply carries no commit status (3.4); OpenMaker Server fails an
  op70 it could not save instead
  ([below](#a-commit-openmaker-server-cannot-save)). After every write the
  client reads `[T, 05, R]` back and compares the values and `fc` it sent;
  after a delete it checks that the record is gone.
- op103 is not sent: FileMaker Pro sends it only after some commits and the
  server's answer carries nothing (3.5).
- A transaction's commit (`om-provider/src/transaction.rs`) runs in two
  stages that can be called apart (#1006). Preparing applies every held
  record's rules and builds the uploads. It sends no op14 or op70, but it
  reserves the serial numbers not taken yet (op72; they stay used whatever
  follows) and reads indexes, related records and container data; the
  transaction's locks stay held. Submitting sends op14 and op70 and reads
  the records back. A failure before op70 means nothing was committed;
  one after it leaves the outcome unknown, and the transaction's container
  references are kept for recovery.
- Each transaction has a kind: a window's commit of a record with its
  related records, or a script's Open Transaction. Both commit alike for
  now. Another file's provider may join a transaction under its id only on
  the same connection, as a handle of it; a second connection, to the
  same host or not, is refused with 3. A commit across files gives each
  file its own part, every part prepared before any record data goes
  (below: a script's transaction across files, and the window's commit of
  a record across files).
- **The windows after Revert Transaction.** A record the transaction
  created goes from every window of the file that shows it, with what the
  window still had pending on it: one committed into the transaction, and
  one a New Record made in it, in any window, that no commit had logged
  yet. The records are known by identity, as the provider's transaction
  holds them new (`DataProvider::transaction_created`), not by what a
  window remembers of its record: a record an earlier transaction saved
  stays, with an edit pending on it, and so does a record a window's
  record session holds apart. OpenMaker discards participating windows'
  transaction drafts and keeps the ordinary drafts they brought in on the
  same record, unless Revert Record discarded them. The window shows the
  record at the same place in the found set as the revert leaves it, or the
  last one when no record is there any more (so the record before a new one
  at the end), and `Get ( RecordID )` answers that record's id
  ([#307](https://github.com/OpenMaker-Dev/openmaker/issues/307)). Which
  record FileMaker shows then is unmeasured [I].

### A window's record session

New Record and Duplicate Record, a script's and the menus', are held,
not saved, until the record is committed, as FileMaker Pro holds a new
record ([#261](https://github.com/OpenMaker-Dev/openmaker/issues/261)). FileMaker Pro sends op71 at New Record,
under a transaction id of its own for that record (write-path.md 1.1,
1.2), and op14 and op70 only when the record is committed. A record left
uncommitted is abandoned with op65, its id used up (5.1).

OpenMaker does the same with the window's record session
(`DataProvider::begin_record_commit`), on the provider's transaction:
- **New Record** opens the session: op71 reserves and locks the record's
  id, and the serial numbers taken on creation are reserved (op72). The
  record's rules are computed then, as a preview computes them (below),
  with a clock and UUID drawn at New Record: its auto-entered values (a
  creator, a creation timestamp, a UUID key) and stored calculations of
  its own fields show on record pages and to calculations, a script's
  among them, before the commit. A later change computes them again.
  Lookups, related values and `Random` are left to the commit. This holds
  for every new record a transaction holds, a script's Open Transaction
  too.
- **Duplicate Record** opens the session before it copies the record, so
  the container references the copy takes are the session's: kept by its
  commit, released by its revert. It copies no auto-entered value: a
  field that auto-enters a variable (a creation timestamp, an account
  name), a serial number, a constant or a calculation that replaces an
  existing value gets the copy's own, as FileMaker's does (auto-entry.md,
  "Serial numbers"; script oracle s46-record-number-ae for a replacing
  calculation,
  [#473](https://github.com/OpenMaker-Dev/openmaker/issues/473)). A
  replacing `Get ( RecordNumber )` is the copy's place after the records
  of an unsorted found set, as s46 measured, as the host's found set is
  when the copy is made; the copy keeps it through its edits before its
  save, and in a script's transaction through Commit Records and later
  writes until the transaction commits it (the transaction's held create
  keeps it), as an auto-entered value is entered when the record is made.
  Another client's change of the found set after that is not counted, as
  a value FileMaker entered is not entered again [I]. In a
  sorted found set the copy has none and its save is refused (3). A
  lookup's value and a calculation that does not replace one are copied.
  In a transaction that skips auto-entry the copy takes every stored value
  of the record but a serial number, and enters none [I]. A table with a field that enters the last visited
  record's value (`0x10`) is refused for these writes, as an unsupported
  field option. The variable and constant cases follow Claris's
  documentation and are unmeasured.
- **On a layout of a related file's table** the related file holds the
  record as one of the session's parts, as it holds a portal row of its
  table (`write_routed_related`): op71 under the session's shared
  transaction, nothing stored until the session commits, and Revert
  abandons it with the session, deleting nothing. Its window reads it
  from that file's transaction: the found set, the page and calculations.
  A refused save keeps it held (#1119); it is never saved at once
  instead.
- **The commit** writes the record, and what was changed through it,
  together (op14, op70) and reads them back
  (`DataProvider::commit_record_session`). Committing a held new record
  without further edits also reloads the window's page, so the next edit
  uses the stored record version. The related files' records it
  wrote, a row of a related file's table typed into the record's portal
  among them, are committed in their files before this file's (#1119).
  Each file takes part in its own way. A related file on this file's
  connection joins the session's held transaction. One on another
  connection takes part with a transaction of its own. Each file's part
  ends once every file has settled, related files first and this file
  last ("Related records of another file", below). What FileMaker does
  when such a save fails partway, and across hosts, is unmeasured. The
  record is computed with the clock and
  UUID drawn at New Record, so a key a script read is the key stored. A
  modification timestamp therefore also takes New Record's time (inferred:
  when FileMaker takes it for a new record is not measured). A refusal
  before any record data is submitted, such as a validation, keeps the session open
  and the record held, and leaves the session as the commit found it
  (`DataProvider::mark_record_session`, `rollback_record_session`): the
  window keeps its changes, and the next commit writes them once. A
  related record the refused commit created or locked stays locked under
  the session, its id used, until the session ends.
- **Revert Record, Delete Record, or closing the window** ends it with
  op65. Nothing of the record was on the host, so nothing is deleted and
  no other client's change can be lost.

**One session per window.** A window's session goes with the window: the
provider keeps each window's open session apart, as FileMaker binds the
transaction to the window (script-runtime.md 8.1), so another window's
writes are committed at once, not held in it. The window's own direct saves
(`DataProvider::create_record`, `update_record`, `delete_record`) are
refused while its session holds the new record; they would otherwise be
held with it and go with a revert. A script's Open Transaction in another
window sets the session aside (`DataProvider::record_session_held`): the
record stays held outside that transaction, as FileMaker keeps other
windows' changes (script-runtime.md 8.5). The window's pages and
calculations still show it; its commit, Commit Transaction run from the
window included, commits it apart in its own session
(`DataProvider::enter_record_session`); Revert ends the session. Once that
transaction ends in the window, the session is open again. The
transaction's steps run from the window leave the record alone
(`WorkerHost::record_apart`, #1103):
- **A failed Commit Transaction**, or a revert of the transaction with the
  window current (Commit Transaction after an error), keeps the window's
  unsaved typing and its held record.
- **The record committed apart** is reported by itself to
  OnWindowTransaction. It isn't among the transaction's records, so a
  later Revert Transaction doesn't forget it from the windows' found sets.
- **Revert Transaction** already reverts in the transaction's own window
  (the runtime selects it for the revert).

What FileMaker does to a window's held record when another window's
transaction is committed or reverted from it is unmeasured [I].

**Leaving the record.** A window that leaves the held record without
committing or reverting it (a found-set change, say) commits its session
the next time it reads its record, as leaving a record commits it;
refused, the session is reverted. Open Transaction commits it first, as
FileMaker does (8.1).

What another client sees of a held record is not measured. Since op70
comes only at the commit, it presumably sees nothing until then
(inferred).

**Limits.**
- **Stored calculations before the commit.**
  - **Script reads.** A script reading the window's own record before its commit reads it as the commit would store it. The provider computes it over the window's unsaved changes and its other unsaved records (`DataProvider::compute_pending`, `WorkerHost::pending_own`, #1104), once per change of those. So after Set Field `Count` to 4, `Doubled = Count * 2` reads 8, as FileMaker Pro 26 read it in a local file (auto-entry.md, "A stored calculation before the commit").
  - **Unknown fields.** A field the provider can't know yet reads its stored value, never empty. With a related record to delete in the same commit, nothing is computed.
  - **Browse** still shows a stored calculation's stored value until the commit.
  - **Measured only for a script's Set Field** on a local file's own stored calculation. The user's typing, Browse and hosted files are unmeasured [I].
  - **On OpenMaker Server** a script sees its open new record's
    auto-entered values and stored calculations from New Record on. A
    change to a cell the rules compute from runs the preview again
    (`RecordPlan::computes_from`, `ServerHost::preview_new_record`,
    [#474](https://github.com/OpenMaker-Dev/openmaker/issues/474)).
    The preview includes the record's creation position in an unsorted
    found set for `Get ( RecordNumber )`, as measured in script oracle
    s46. Set Field of a previewed value enters that value in the record.
    Duplicate Record, the transaction's other records and unique-value
    checks use the record's own cells. The commit validates the row and
    computes its values again using the same clock and UUID. A new
    record's creation and modification timestamps are its creation time
    [I]. A global or related record can change between the preview and
    the commit. The commit also checks whether the record's creation
    position is still valid (see below). A formula with an unavailable
    input can be absent from the preview. An existing record's stored
    calculations read their stored value until the commit.

### A new record's preview

`DataProvider::preview_new_record` computes a new record's rules before
the record is written (`om-provider/src/preview.rs`, [#277](https://github.com/OpenMaker-Dev/openmaker/issues/277)): what a window
would show of a portal row the user began, and what its triggers read,
before the record commit writes it.
- **What it computes.** Auto-entered values, serial numbers generated on
  creation and stored calculations over the cells the write gives
  (`RecordPlan::preview_fields`), with constants, the clock, a UUID, the
  operation's context and the record's own container data. Validation is
  left to the write. The values returned are every cell the record would
  store with a value other than the write's own. A preview call naming an
  earlier preview (`ScriptWrite::prepared`) computes it again over the
  cells it gives now, as after a trigger changed one or a vetoed commit:
  the same id, with what it drew, nothing reserved again.
- **Values read from other records** (a lookup, a related field, a
  self-join's too, an aggregate of related records, and every field
  computed from one) depend on the window's unsaved records. Before the
  commit opens its transaction those are only the window's: its own
  record's typed cells, a parent not created yet, the other rows begun in
  its portals, the portal rows it edited.
  - **Without them** (`preview_new_record`), the preview reads no other
    record and leaves these values out, rather than read the host's stored
    record in their place and show a value the commit computes
    differently.
  - **With them** (`preview_new_record_over`, [#278](https://github.com/OpenMaker-Dev/openmaker/issues/278)), the window names
    every one but the written row, and the preview reads related records
    and lookups through them as Commit Transaction reads the records it
    holds (`HeldRecords`), each computed by its own rules first, as its
    commit would compute it (`RecordPlan::pending_row`), so a stored
    calculation or auto-entered value of a parent reads as the commit
    stores it, not as stored now:
    - a record the open transaction holds new (a window's record session,
      [#261](https://github.com/OpenMaker-Dev/openmaker/issues/261)) from its held cells, the serial numbers it took and the clock
      and UUID drawn when it was held, with the window's cells over them;
    - an existing record from its stored record, or what the transaction
      holds of it, with the window's cells;
    - a row not created yet from its cells, under an id no stored record
      has (a begun row's: what its own preview entered among them).

    The open transaction's other held records that these, or the written
    row, read (by the tables their formulas read, through each other) are
    computed the same way, first, in the order the commit computes them: not
    read as their held cells over the stored row, whose calculations are
    stale. Records of other tables come first, then the written row's
    table's, each in the order given, each reading the ones before it as
    computed.
    A portal row of another file's table reads the opened file's records
    the same way, through its session, computed by the opened file's
    rules. Each is computed by the rules its write will commit it by (the
    open transaction's Skip options), with the context that write will
    hold it with: a held record the window writes again takes the window's,
    unless it was written from a preview.

    A value of a pending record its commit may not store is unknown, field
    by field (`RecordPlan::pending_row`): a value its rules leave out (a
    formula they cannot answer yet, a serial number not reserved), one they
    evaluate from a `Random` draw, from the clock, a UUID or the host clock
    where its commit reads its own (`RecordPlan::volatile_fields`: an
    edit's modification timestamp, a new row's creation values unless its
    clock and UUID are kept), or from an unknown value of a record before
    it, through a relationship or a lookup; and every value computed from
    one, evaluated now or not, since an input that comes out the same now
    may not at its commit. A value it does not evaluate now (a creation
    date an edit keeps, an auto-entered value the write keeps) is the
    stored one. The written row leaves out each field reading an unknown
    value, and each computed from one (`RecordPlan::masked_fields`); the
    rest is read.

    Unknown values are kept by record. A field read through a relationship
    or a lookup is checked on the records the rules read for it; the match
    and sort fields there on every record of their table, since an unknown
    one may relate another record or change which comes first. A field
    reading its own table through a self-join is unknown when the record's
    own value it may read is (#1035). A calculation computed when read or a
    summary is unknown only through what it reads
    (`om_data::rules::unknown_when_read`): its own record's fields (every
    record's, for a summary), other tables' fields on any of their records,
    the match and sort fields on the way, or anything at all when it reads
    what it cannot name (a custom function, `Evaluate`, `GetField`,
    `ExecuteSQL`, its record number or found set); always when it reads the
    clock, a UUID or `Random`. So an edited parent's modification timestamp
    leaves out only what reads that parent's timestamp, and a row of
    another job reads its own parent's. The rules read no unstored or
    summary field of another record (`related_hop`, lookups), so today
    that check is reached by nothing a preview computes; it is there for
    what reads one over pending records. A related-values list is checked
    at the commit only, over the records the commit holds
    (related_lists.rs), and reads no unknown value.

    The window's records are not used at all, and every value read from
    another record left out, when one is a new record of a table other than
    the written row's (a parent the window has not created, whose keys the
    commit may compute), a record of a third file's table, a typed value
    not of its field's type, an imported record, or one reading a record
    that comes after it.
  - **The commit** computes these values over the records it holds, or
    refuses them: a commit holding records of both files is #1006, a
    related record's unresolved calculation dependencies [#276](https://github.com/OpenMaker-Dev/openmaker/issues/276), a
    self-join's own membership #1035.
- **What else it leaves out.** A `Random` draw, which the commit would
  draw again, with what is computed from it. A formula it cannot evaluate
  yet (`RecordPlan::preview_entry`), as a record number the window did not
  give (#1038) or a self-join whose answer depends on the new record's own
  membership (#1035); the write computes such a field again, or refuses
  with 3 as it would without a preview. Serial numbers generated on commit,
  which the write reserves. When a value the user typed into the written
  row does not read as its field's type, there is no preview (`None`), and
  the write refuses it.
- **What it sends.** No op71, op14, op70 or op103. It reserves the serial
  numbers generated on creation (op72) once, now; they stay used whatever
  follows, a gap like a reverted record's. It reads op82 for a formula
  reading `Get ( CurrentHostTimestamp )`, the session's global values a
  formula reads, and the record's own container data; over the window's
  unsaved records, also the stored records and indexes its lookups and
  related fields read.
- **The write reuses it.** The provider keeps a snapshot under the
  preview's id: the reserved serial numbers, the UUID, date, time and
  timestamp, and the operation's context (layout number, privilege set,
  host clock). A write naming it (`ScriptWrite::prepared`) computes with
  them, so a value from inputs no trigger changed is the value the preview
  showed; a changed input is computed again. The record number is the
  creating write's own, none included (#1084): the window numbers the row
  afresh at each commit and gives none where a trigger changed it out of
  the measured shape, so a formula reading it is refused with 3 as without
  a preview, never given the preview's number. Outside a transaction the
  write uses the snapshot up when it sends op71; a refusal before that
  (a related value list, a privilege) leaves it. In a transaction the held
  record takes the serial numbers at New Record (`hold_new`) and keeps the
  clock, UUID and context for Commit Transaction, also when a later write
  of the transaction changes it (which gives only a record number, when it
  gives one); a commit that submits uses the preview up, and a reverted or
  refused one leaves it for the next attempt.
- **Op71 in a transaction.** The held write sends op71 as New Record does;
  a formula the preview left out is refused by Commit Transaction, before
  op14 or op70, as for any held record (the id stays used).
- **Lifecycle.** A preview ends with `DataProvider::discard_prepared`, or
  when the file closes. A reconnect keeps the file's own previews, with
  the serial numbers, clock and UUID they drew, for their records' saves
  ([#16](https://github.com/OpenMaker-Dev/openmaker/issues/16)); a routed one (a row of another file's table) ends with its
  owner's session, and a window's new record with such a row begun is not
  carried across the reconnect (recovery.md). A refused scalar save whose
  new related rows are already reserved can instead carry those owners'
  parked creation state under an all-file no-data receipt. A held new record
  in the opened file is carried in that same group, with its issued values;
  it is not recreated before the related files have been revalidated.
  Explicit reconnect revalidates all files, privileges and existing versions
  before fresh wire IDs, without drawing new serials, creation clock or UUID.
  All drafts wait
  for an explicit save; missing proof or changed versions freeze the whole
  group Unresolved. Container acquisitions carry separate per-file ownership
  receipts with immutable bytes and native library/member identities; unknown
  ownership stays Unresolved without releasing or uploading assets (recovery.md).
  A single ordinary new record with an acknowledged actual table-library upload
  can also reconnect before its first Save, under a baseline captured before
  any record work and unchanged own static permissions, schema and asset proof.
  Missing proof refuses before opening or closing a session. Recovery reserves
  only a replacement ID; Save or Revert stays explicit. This PM-chosen synthetic
  attachment-entry case does not establish a real hosted solution's sequence or
  native FileMaker/FMS behavior. Existing-record, related-part, explicit-
  transaction, conditional-permission, file-library and stored-source recovery
  are not added by this single-record case (recovery.md). The prepared mixed-row
  subtype also carries a new or accepted existing Root with an ACKed upload,
  several accepted related edits and several new logs before first Save
  (recovery.md). Multiple rows and native tables share their actual file's
  transaction and asset ledger.
  An existing Root can also copy an original stored source into a distinct
  container field of the same row/table as its first mutation, then join one
  existing Related attachment owner without a Root upload. This also works
  after both owners have acknowledged uploads. Actual source-cell provenance and own View/Edit proof
  precede op87 +1; only a positive ACK owns the extra reference. Reconnect never
  repeats it, and Revert releases only that extra reference. Missing/changed
  proof or a lost ACK retains the whole group Unresolved (recovery.md).
  Existing and new uploaded cells require a prepared row/first-repetition target;
  scalar membership grants no asset authority. It captures row intents before
  work, retains issued and accepted preview values and validates every owner
  before any replacement, then acknowledges all replacements before ledger
  transfer. Existing Root scalar-first entry uses `record.prepare_edit` before
  mutation, retaining its accepted RID/version and own static Edit proof. Insert
  extends that original footprint and actual library observations; recovery keeps
  the same Root RID. Unavailable recovery preserves ordinary edits without
  permitting a later baseline capture. Existing-only attachment proof remains
  strict.
  A write naming a preview the provider
  no longer holds, one of another table or layout, or one the open
  transaction already holds is refused with 3 before anything is sent,
  rather than store values other than those shown. Ids are not reused, also
  after a reconnect.
- **Related files.** A portal row of another file's table is previewed by
  that file's provider through the table's route, as its write goes
  (related_owner.rs); the opened file maps its preview id to the owner's,
  and to the layout the write goes through. So is a new record of a layout
  of another file's table ([#43](https://github.com/OpenMaker-Dev/openmaker/issues/43)):
  its write naming the preview, outside a transaction or held in one, stores
  what the preview drew, with no layout of the owning file's (but see
  [#949](https://github.com/OpenMaker-Dev/openmaker/issues/949): in a
  script's transaction with Skip auto-enter options, a preview made before
  that file's first write there enters, and reserves, what the commit then
  skips). So is a
  related record of another file's table on such a layout.
- **The desktop window's use.** OnObjectSave when leaving a changed field
  by clicking the layout's background or using keyboard Commit
  (an ordinary commit, #1070), previews the window's begun rows before its
  script runs, after their match fields are set
  (`WorkerHost::prepare_begun_records`). It names the
  window's other unsaved records (`pending_records`): its own record with
  what was typed or set into it, the portal rows edited through it, and its
  other begun rows with what their previews entered, each by its table's id
  in the window's file (`preview_new_record_over`). A record to delete with
  the commit, or a record New Record made and has not saved, can't be named
  so; the window then names none, and the preview reads no other record.
  Rows reading each other (a self-join, an aggregate) are previewed in
  passes, each previewing again, under the same preview, the rows whose
  records changed since, until none did. A cycle is cut after a pass per
  row, and a row still reading changed records shows nothing from its
  preview. While a global field the user typed waits for the commit to
  reach the session (`take_typed_globals`), nothing is previewed and an
  earlier preview's values aren't shown, since the preview would read the
  global's value before it.
  The script reads a row's fields as typed over the preview, Browse shows
  the row that way, and the commit's write names the preview. A row whose
  cells, or whose other unsaved records, changed since (a refused commit,
  then an edit) is computed again under the same preview; one the provider no longer holds is previewed anew, and a
  commit naming one is written once more without it. Revert Record, and
  Delete Portal Row of the row, discard its preview. A Tab or a click into
  another field and a step's Commit Records preview nothing; neither does
  a commit without an OnObjectSave trigger: an OnRecordCommit script then
  still reads the row's auto-entered fields empty.
- **Unmeasured.** When FileMaker computes a begun portal row's auto-entered
  values and match fields relative to OnObjectSave in an ordinary commit is
  measured only for `Get ( RecordNumber )` ([#277](https://github.com/OpenMaker-Dev/openmaker/issues/277)'s native cases); that a
  UUID or a timestamp is drawn once, before the triggers, and kept by the
  commit is OpenMaker's choice. When FileMaker takes a serial number for a
  begun portal row, and whether an abandoned row leaves a gap, is not
  measured.

**The window's own record before its commit** (#1104,
`DataProvider::compute_pending`). Given the write the window would commit
its record with, and its other unsaved records, the provider computes the
record as that commit would: the same pending computation as above
(`pending_one`), the record after the others. It answers every cell the
commit would store, the window's own cells included, and the fields whose
values the commit may not store (a modification timestamp, `Random`, a
value read from another record's unknown one, and what is computed from
one). A script reading a stored calculation of the record before the commit
can read it from there, and an unknown field as before (Scripts lane).
Nothing is sent or reserved. There is no answer for another file's table, a
typed value not of its field's type, a write naming a preview (whose own
context it does not compute with), or records it cannot read as the window
holds them. FileMaker Pro reads a stored calculation computed over a
script's Set Field before the commit (auto-entry.md, "A stored calculation
before the commit": measured for a local file's own stored calculation
only).

## The modification count on OpenMaker Server

The client that commits a record keeps its count (write-path.md 5.4), and
OpenMaker Server stores the `fc` a client uploads as it came, so save
conflicts between two clients are detected as with FileMaker Server. It
stores only a count, though:

- A record whose uploaded `fc` is not a count in FileMaker's form (a leaf
  holding a length byte and a serial key of that length, 2 to 10 bytes) is
  refused as a commit of a record another session holds is: logged, nothing
  written, the lock kept, op70's reply unchanged. The client sees the old
  record when it reads it back. A server script's commit of such a record
  fails with error 3.
- A lock asking for the count of a record that already holds something
  else (a file written by an earlier version, or by another program) is
  granted with `01 00`, the count of a record never modified, as the
  server's scripts read it (0). The next edit stores a count again.
- The server's own writes (server scripts, stored calculations it
  recomputes, values it removes with a deleted field) store the count one
  higher.
- A record whose count is the largest a serial key holds (`u64::MAX`; op70
  stores any well-formed count) cannot count another change, so the server
  makes none: a server script's commit that edits it fails with error 3
  and writes nothing, a stored calculation it recomputes keeps its old
  value on that record, and a deleted field's value stays on it, both
  logged. OpenMaker's client refuses to edit it as well ("modification
  count is exhausted"). A count wrapped around to 0, or left as it was,
  would let a client holding a copy of the record miss the change and
  write over it.

Refusing a malformed count, reporting a stored one as `01 00` and refusing
changes at the largest count are OpenMaker's choices: what FileMaker Server
does in these cases is unmeasured (write-path.md 11).

Container libraries keep counters of their own (containers.md 1.2, 1.3):
the header's last member id and modification count, and each member's
modification count. They come from the file, which a Full Access catalog
upload can set. A store of container data (op88, a server script's) or a
change of a member's reference count (op87, a commit's counting, a record
or field deleted) that would advance one past the largest is refused
before anything changes, with the answer the operation already gives for a
library it cannot write into; a script's commit is taken back whole.
Repetition n of field F is the cell `F * 256 + n` (execute-sql.md): for a
field id above `u64::MAX / 256`, which no FileMaker file has been seen to
hold, no repetition after the first can have a cell, and the server and
ExecuteSQL over local files read them as empty. OpenMaker's client does
not check this yet. Both are OpenMaker's choices; FileMaker's behaviour is
unmeasured.

## A commit OpenMaker Server cannot save

OpenMaker Server saves the file at the end of every op70 commit
(`writes::commit`). When the save fails (a full disk, a folder it cannot
write, content the file cannot hold), the commit is taken back: every
record is put back in memory, the file on disk keeps what it held, the
records stay locked by the committing session, and no other client is
notified. The op70 then fails with the CORBA system exception `MARSHAL`,
`COMPLETED_NO`, instead of the empty page a commit answers, so the client
is never told that a lost commit was done. OpenMaker's client reports it
as an error of the commit, before its read-back.

Failing the request is OpenMaker's choice. op70's reply has no status
field (write-path.md 3.4), and a system exception is the one way the
protocol has to fail it.

- What FileMaker Server answers when it cannot save during op70 is
  unobserved.
- How FileMaker Pro handles `MARSHAL` on op70 is unmeasured. In one
  disposable loopback probe, FileMaker Pro 26 answered a system exception
  on op70 by abandoning the request and starting a reconnect; it did not
  report success.
- A commit the account's privilege set refuses (no set, or one that does
  not allow a record's action) still writes nothing and answers the empty
  page, logged: the client checks privileges before it commits and
  reports 200 itself.

## Locks and notifications on OpenMaker Server

OpenMaker Server bounds the locks and the change notifications one client
can make it keep. The bounds are OpenMaker's choices: FileMaker Server's
are unmeasured.

**Locks.** One connection's requests (op60, op61, op71) may hold 100,000
locks over all its files (`writes::MAX_LOCKS`). A lock its holder takes
again counts once. A request past the cap is refused as a lock conflict
with the requesting session itself: op60 and op61 answer 8003 with the
self-conflict byte set and the session's own name, op71 answers 8003
before it reserves any id. The client reports it as it reports any
conflict. What clients hold:

- a user editing a record: the record, and the index of each unique field
  checked (write-path.md 1.3);
- New Record: the block of ids op71 reserved, at most 25 from FileMaker
  Pro and 1 from OpenMaker's client;
- Manage Database: `[08]`, each catalog it edits and one table lock per
  table whose fields changed (schema-path.md 4). Under it no other
  connection locks a record of the table or commits one, even a record
  it holds no lock on: op70 skips it as a record another session holds;
- a script transaction in the client: one lock per record it changes, until
  it commits. A client transaction that changes more than about 100,000
  records is refused at the cap.

Server scripts run under lock users of their own and are not capped. An
import holds one lock per row until its single commit, as many rows as the
import's source holds (`om_transfer::import::limit`); the script limits
bound how long. The server's own short-lived locks (authoring edits) are
not capped either.

op71 reserves at most 1,000 ids per request: FileMaker Pro asks for 1 or
25 (write-path.md 3.1), and what it asks for while importing into a hosted
file is unmeasured, hence the margin. Its refusals (that count, the lock
cap, ids past the largest, a table node the last issued id cannot go
beneath) all come before it reserves anything, in the order write-path.md
2 lists. op71 then saves the file before it answers, so every id it
hands out is one the file on disk counts as issued (#927): a client that
keeps a new record across a server restart commits it with its id, and no
id it holds is handed out again. When the file cannot be saved, op71 takes
the reservation back and is refused with the save's error (800, 809 or
810), before any id is locked. The server's own scripts and imports reserve
their records' ids without that save: they end with the server, so no id of
theirs outlives a restart, and their commit saves the file once rather than
once per record. Ids reserved and never committed stay used, a gap; no
record has them.

**Notifications.** Changes wait for each other registered connection until
its next Perform reply (write-path.md 6.4; OpenMaker Server makes no
`Notify` calls):

- A reply carries the waiting notifications in order, as many as fit in
  what the request's `maxreturn` leaves after the result; the rest wait for
  the next replies. FileMaker Pro asks small `maxreturn`s for some requests
  (76 bytes for op62, 2,085 for op60 in the captures), so a notification may
  wait for a request with more room. An exception reply carries none.
- At most 50,000 notifications or 2 MiB of them wait for one connection
  (`writes::MAX_QUEUED`, `MAX_QUEUED_BYTES`); 2 MiB is what one reply with
  the largest `maxreturn` carries. Past either bound the queue is
  combined: of identical notifications (same registration, code and path)
  only the latest is kept, and a record's notification `[T, 05, R]` becomes
  one for the table's records `[T, 05]` with the same code. That is the
  path FileMaker Pro gives other clients when Manage Database changed a
  table's fields (schema-path.md 9). OpenMaker's client reads everything
  again on any notification. FileMaker Pro does not (write-path.md 6.5,
  from its code): for `[T, 05]` it drops every cached record on `0x201`,
  ignores `0x101`, so added records do not join its found sets, and on
  `0x401` empties every found set of the table ([#171](https://github.com/OpenMaker-Dev/openmaker/issues/171)). FileMaker Server
  has no bound or combining in the traced queue paths; failed-`Notify`
  behavior is unresolved (write-path.md 6.4). A bulk change by a server
  script (Delete All Records, an import)
  reaches this bound for every client registered on the file.
- When combining leaves more than half of either bound, the connection is
  closed with a GIOP CloseConnection: the session ends as any session ends
  (its locks, registrations and queued notifications released), and the
  client, which cannot miss that, reads the file again
  when it reconnects (OpenMaker's client keeps unsaved edits on screen and
  reconnects when the user asks; network-protocol.md 6.3). Only many
  distinct notifications that are not of records get there: in practice,
  another client announcing changes of thousands of different paths while
  this one sends nothing.
- Dropping notifications would leave a client showing stale data with no
  sign of it, so the server does not.

An op62 announces a change to the other clients only when it releases a
lock the transaction held and its attention code names a change: a scope
from a record (`01`) to a layout object (`07`), with only the add, modify
and delete bits besides (write-path.md 6.2). One op62 therefore makes each
other client receive at most one notification, about a path the sender had
locked, of at most 255 bytes. Session events (a message, closing a file,
disconnecting) and the host serial, local and logout flags are not
relayed; FileMaker Pro was not seen to send them in op62.

## Uploads held and read into trees on OpenMaker Server

A slot list a client uploads takes far more memory as a tree than as
bytes: a node and an allocation for each key, so a list of empty leaves
with short keys takes 30 to 60 times its size. OpenMaker Server reads each
upload into a tree at most once, counts the tree's memory as it reads
(`om_hbam::tree::read_owned`), and charges it, with the upload's bytes
while its parts arrive, to one budget for the whole server. The limits are
OpenMaker's choices; FileMaker Server's are unmeasured.

**The budget.** The memory held for clients' uploads on the whole server,
as bytes and as trees, may not pass `--upload-memory-mib` (default 4096,
`ConnectionLimits::upload_memory`), and one client address may hold a
quarter of it (an IPv6 /64 counts as one address, and an office behind
one NAT address shares its quarter, as it shares the connection limits).
It counts:

- the bytes of every upload sent in parts, as each part arrives, until the
  last part has come and the request has been served;
- the records each connection has staged and not committed, over all its
  files;
- the tree of every upload while it is read and used.

Before an upload is read, as much as reading it may take is reserved (256
times its bytes plus 64 KiB, the reader's worst case being about 125
times, or what the client may still hold if that is less), and the reader
stops at that: no tree grows past what is charged for it. Afterwards a
staged upload keeps what its records take and the rest goes back. What is
charged goes back however it ends: as an upload is served or dropped, as
records are committed, when the file is closed, logged out of or logged
in to again, and when the session ends by `Disconnect`, a dropped
connection, the idle timeout or a panic in the connection's thread.

A real client stages the records of one commit and then commits them,
which empties what it holds, and sends one upload at a time, so ordinary
work stays far below the budget. At the default one address can stage a
commit of about 35 MB of short-key records and upload a container of
about 465 MiB (held while it arrives, then read into a tree of about 1.2
times its size); raise the limit for larger ones. The default is not safe
on a machine with much less than 8 GB of memory: set it lower there, at
the cost of refusing commits and containers that large.

Past the budget a request is refused before anything is staged, committed,
stored or run: its Perform is answered with the CORBA system exception
`IMP_LIMIT`, which the client reports as an error (op14's and op70's
replies carry no status). An op14 is staged whole or not at all. An upload
refused part way is dropped whole, and a client that sends its next part
anyway is refused again.

- **Uploads in parts.** Before a part is refused, the connection drops its
  own idle uploads (ones its client started and left), least recently used
  first. An idle upload on another connection of the same address keeps
  counting until that connection starts another transfer or its session
  ends, so a client that left a large upload unfinished on one file has
  less room on its others until then. The per-connection rules stay as
  they were: one upload of at most 1 GiB, at most 8 transfers and 64 MiB in
  those not in use. Paged downloads are not in this budget but in one of
  their own (below).
- **Paged downloads.** A reply larger than one page (op90, op15, op92,
  op86) is kept, whole, until its client has read its last page. These
  streams are charged to a server-wide budget of their own,
  `--download-memory-mib` (default 2048, `ConnectionLimits::download_memory`).
  Past it, the streams least recently paged, on any connection, are
  dropped, never the one just started, which is held even when it alone
  passes the budget; a client that continues a dropped stream is refused
  with `IMP_LIMIT`. A client reads each download to its end at once, so the
  streams dropped are those clients left unfinished longest. It is not
  split by client address, since many connections behind one NAT address
  each page downloads in ordinary use, and a stream is dropped rather than
  a new download refused, since a refused read shows where a dropped
  abandoned one does not
  ([#145](https://github.com/OpenMaker-Dev/openmaker/issues/145)).
- **Records (op14), commit lists (op70) and subtrees written straight into
  the file (op13)** are read within the reservation. A connection whose
  address's staged records did fill its quarter could not commit them
  until one of its connections logs out or closes the file.
- **Container data (op88)** may take at most twice its bytes plus 64 KiB
  as a tree, and is read within that or what the client may still hold
  beside the upload's held bytes, if that is less. A tree past a
  reservation the client's share cut short is refused as over the budget;
  past twice its bytes it is refused with the status of malformed binary
  data (3) and a message. FileMaker Pro sends a stream in pieces of 1000
  bytes (containers.md 1.5), which take about 1.2 times their size. Storing
  a container takes more than the budget counts: the stream is copied
  twice more while it is checked, and once into the file, so one container
  peaks at about 5.4 times its size.
- **Key lists (op92)** are never read into a tree: the list is checked as
  a tree would check it, before the file is held for reading, and its keys
  are read from its bytes again while the requested children are copied.
- **Query programs (op86)** are parsed once, before the file is held for
  reading, within the reservation and the program's own limits
  (`om_query::program::limits::MAX_TREE`); the program's tree stays
  charged until its reply. What its instructions add while it runs is
  bounded by those limits, not by the budget.

## Reads

A page of records comes from the table's record list `[T, 0d]` (5.3), which
also gives the total, and one op92 for the page's ids (network-protocol.md
5.6.6b). Each write reads the table's field definitions `[T, 3]` again, so
values are computed from the current rules, and single records `[T, 05, R]`.

## Limits

A table is writable only when every field rule is understood
(`om-data/src/rules`, shared with OpenMaker Server's scripts; calculations by `om-calc`):
auto-enter and stored calculations of text, number, date, time and
timestamp results over the record's own fields, constants, auto-entered
dates, times, timestamps, account and user names, the clock, account, user
and UUID `Get` selectors, not-empty, strict, maximum-length and
by-calculation validation, and uniqueness of text fields. Summary and
unstored-calculation fields may be present; they are not edited or
written. Globals and repeating fields may be present too; Browse edits
them as described below (globals as session values, later repetitions in
their own cells). An unstored calculation's option bits are not checked
but for a lookup's, which would write into it: a stray `0x80`
(auto-entry.md) leaves the table writable. A stored calculation's unknown
bits keep it read-only.

An ordinary local Number auto-entry calculation can use `Self` as its
only field input. It reads the proposed value at unsorted New Record or on an
explicit write, including an empty value on a clear. A nonempty explicit
assignment keeps its value when replacement is disabled. This covers
stored results and new-record previews. During a held edit of an existing
record, calculations read the entered value until save. Other creation
paths with auto-entry active and unmeasured Self formulas refuse an
evaluation with error 3. Unrelated existing-record edits are available.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1059)

A local table accepts ordinary record writes to its other fields when a
Container awaits transfer to embedded storage. This covers a
single-repetition field marked `0x6000` (external Open storage) and a
24-repetition field marked `0x4000` (external Secure storage). OpenMaker
keeps the definition and payload references and returns error 3 on a
write to the container itself. Other storage words, repetition counts
and hosted use remain outside this support.
[Open storage comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1060)
[Secure storage comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1083)

In a local file, a replacing 24-repetition Text auto-enter calculation
can read the filename of the same occurrence's 24-repetition Container.
With the measured formula and option word, a new empty record has no
filename cells, and editing an existing source changes only its matching
repetition. OpenMaker refuses direct writes into the calculated Text and
creation with a nonempty source. The deferred-transfer refusal still
applies to source containers awaiting a transfer.
[Native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/39)

In a local file, ordinary New Record applies a constant, a calculation
that reads no fields, or a creation serial to a supported
single-repetition global Text field. A calculation with replacement off
keeps a nonempty global. Constants, replacing calculations and creation
serials overwrite it, including on later creations. A creation serial
also advances the stored next serial. The value goes to the session
immediately and is absent from the new record's cells. Revert Record
leaves the session value in place in OpenMaker. FileMaker's Revert
behavior is unmeasured.

Hosted files, Duplicate Record, portal creation and routed writes still
return error 3 when the global already holds a value. The hosted test
covered an empty global with constant auto-entry only. Import with
auto-entry, new records from a preview and explicit script transactions
remain unsupported for global auto-entry. OpenMaker keeps local globals
in memory for the session. See the native
observations in [auto-entry.md](re/auto-entry.md#auto-entry-into-global-storage)
and [#987](https://github.com/OpenMaker-Dev/openmaker/issues/987).

A calculated global Text field with the measured empty-Self default can
retain its value or append a literal suffix. Ordinary New Record reads
the current session value. Scripted Set Field reads its assigned text.
With replacement off, either operation preserves nonempty text. Clearing
re-enters the default. These formulas require an ordinary local desktop
file and one repetition. Browse typing and persistence across reopen
remain separate. See [the native comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1095).

A local Text or Number calculated auto-enter may read
`Get ( CurrentTimeUTCMilliseconds )` at New Record, including through a
custom function. Each field takes its own samples and replays them
through later edits and commit of a held creation. Applying the same
sampling to duplicate, import and portal creations is inferred.
Evaluating a clock-dependent rule again on an existing record, and
clock reads in hosted writes, validation and stored calculations, are
outside this support.
[Native Browse check](https://github.com/OpenMaker-Dev/openmaker/issues/1055).

A local Text auto-enter calculation can read a single-repetition plain
Text global from another table in the same file, through an unrelated
occurrence. The field is read from the session, independent of the
table's records. A held new record keeps the value read at creation. A
formula that reads a missing or styled session value refuses the write.
FileMaker Pro 26.1.1 copied the value at New Record, including with zero
source rows.
[Claris global-field documentation](https://help.claris.com/en/pro-help/content/global-fields.html)
and [the timing check](https://github.com/OpenMaker-Dev/openmaker/issues/1051).

Local desktop files can auto-enter Creation Time into a single-repetition
Text field when the file and the desktop's configured system time format
use hours without a leading zero, colon separators and a trailing AM/PM
suffix. An unknown or mismatched system format is refused.
The Text field stores the creation-time string, including seconds. Other
type conversions and hosted Creation Time into Text remain unsupported.
[FileMaker Pro measurement](https://github.com/OpenMaker-Dev/openmaker/issues/1035#issuecomment-7185794163).

An ordinary single-repetition Text field in a local file can
auto-enter a direct `$$` variable read. A Text value comes from the
owning file's current script variables, and an unset variable gives an
empty field. OpenMaker keeps the New Record snapshot through its preview
and commit. That timing is inferred when the variable changes between
those two steps. Planning refuses with error 3 when a calculated
auto-entry formula reads a script variable outside this form, including
a bare read in a hosted file. A non-Text value in the supported form
returns error 3 at evaluation. The plan check covers the field's
auto-entry formula itself. Variable reads through custom functions,
validation and stored calculations remain separate.
[FileMaker Pro measurement](https://github.com/OpenMaker-Dev/openmaker/issues/1037#issuecomment-7185794811).

A local stored Text calculation can use a literal `Evaluate` with
an explicit dependency list. OpenMaker compiles a concatenation of
ordinary single-repetition Text fields and plain text during planning.
The measured space form `Char ( 32 )` is also supported. Only the listed
fields trigger a recalculation. Other referenced fields supply their
current values when it runs. An untouched ordinary New Record leaves the
result empty. Creation needs an unsorted local window. Clearing all listed
fields while an unlisted input still holds a value refuses with error 3
when the empty-input guard is on. The expression and its dependencies
are kept in the file.
For a field-supplied text join, the source itself also triggers
recalculation. OpenMaker compiles the current source when a trigger
changes. The source, listed triggers and referenced fields must be
ordinary single-repetition Text fields of the same row without their
own auto-entry or lookup. The write refuses with error 3 if the source
cannot compile to the supported join or is missing when a trigger changes.
Hosted files and other dynamic record-rule expressions remain outside
this support.
[FileMaker Pro measurement](https://github.com/OpenMaker-Dev/openmaker/issues/1072)

**An auto-entered or validation formula may read an unstored
calculation** of its own table ([#39](https://github.com/OpenMaker-Dev/openmaker/issues/39); auto-entry.md, "Formulas over
an unstored calculation or a summary field"). The calculation is
evaluated on the record the write leaves, with its own "Do not evaluate
if all referenced fields are empty"; a change of one of its inputs
evaluates a replacing auto-entered formula again, as a change of a field
the formula reads itself does; and the formula's own "Do not evaluate if
all referenced fields are empty" looks at the calculation's value. Only
an unstored calculation the write can evaluate from the record alone is
read: one that reads another occurrence, a global, summary or container
field, a `Get` function only the write's operation answers, a custom
function that reads fields, or another unstored calculation that does,
keeps the table read-only, as a summary field read by such a formula
still does; so does a chain of unstored calculations more than 32 deep,
past which the calculation host reads them as `?`, or one whose read
would evaluate unstored calculations more than 65,536 times (the host
evaluates one on every read, so a chain that reads each step twice
doubles at each). A loop or an aggregate over repetitions reads one
again, which planning does not count. Each rule shares an allowance of
up to 65,536 unstored evaluations across its empty-input probes and
formula body. Exhaustion during auto-entry or validation refuses the
write with error 7. FileMaker continues evaluating past that limit. A
preview treats a formula over a calculation that draws `Random`, the clock
or a UUID, and a skip that calculation's value decides, as not known until
the commit.

A single-repetition Date field with both Creation Date and a calculated
auto-entry value uses the creation-date rule on creation. An explicit
assignment during scripted creation keeps the assigned date. A held create
that also sets another input to the calculation returns error 3 when the
calculation could replace the date. Updates that would run the calculation
also return 3. FileMaker Pro measured re-evaluation after commit, and the
desktop has yet to implement that path. The combined options with Prohibit
modification or with evaluation enabled for all-empty inputs are
unsupported. FileMaker Pro measured creation in a local file. Applying
the same creation rule on a host is inferred.
[Native measurements](https://github.com/OpenMaker-Dev/openmaker/issues/39#issuecomment-7184309937).

**A custom message** is shown for any rule of its field that a write
breaks, not empty included. One stored as a calculation (`0x8000` with
`0x80`, the formula in `20`: how FileMaker Pro 26 writes every custom
message, schema-path.md 5.9 E34) is evaluated over the record as the write
leaves it, with `Self` the refused repetition's value, wherever the rules
check the record (#602). Where the record is not at hand (a value checked
as it is typed, a host's unique, existing or value-list answer), only a
message that is quoted text joined by `&` is shown. Otherwise the refusal
has OpenMaker's own wording, as it has for a message the rules cannot
evaluate (it reads related fields, a `Get` answer only the write gives, a
summary, an unstored calculation, a container or a repeating global, or
what record rules refuse; the table stays writable) and for one that fails
or gives no text. A message reading a global field has the write carry the
session's globals, as a validation calculation's does. Member `18`, which then holds the text older versions
show, is never shown. That FileMaker evaluates the message in the record
as it does the validation calculation is inferred; what it shows when the
message fails or is empty is unmeasured.

An auto-entered calculation may read its own current value by field name
(#1156). That reference still triggers recalculation on a change to the
field and participates in the skip-all-empty check; replacement and
"do not replace" retain their usual guards. Only the ordering self-edge
is omitted. Stored self-cycles and cycles between different fields remain
refused. This named-reference behavior is inferred from Claris's documented
`Self` auto-entry support (auto-entry.md), without a native comparison.

**A field validated against a related-values list** (one field's values,
"Include only related values starting from", file-metadata.md 6.4) is
checked by the desktop at the commit (`om-provider` `related_lists.rs`).
- **How:** the list is computed as a choice list computes it
  (`FileCalc::value_list_entries`, with the session's global values) for
  the record as the write leaves it. A changed value not among its values
  is refused with 506. Text compares as the list's field keys it in its
  value index, its index language's form, language 0 being that of the
  file that owns the field's table (a related file's own, unknown when its
  locale record does not decode). A text field indexed in a tailored
  language refuses the check with 3 rather than compare in English, where
  `Apfel` would be `Äpfel` ([#571](https://github.com/OpenMaker-Dev/openmaker/issues/571)).
  A destination of another type compares by value, whatever its language. The
  computed list keeps one of the values a calculation compares equal. For
  a text destination, the index keys can distinguish values the list
  groups together (case or accents in Unicode raw or standard, `v` and `w`
  in a Swedish file). If a text value matches only by calculation equality,
  the check returns 3 because deduplication may have dropped an exact match.
  This comparison reads the candidate in the listed field's locale and
  uses the list owner's collation. The check reparses each rendered list
  item in the list owner's locale, which can misread a number retaining
  its original spelling. For example, a US list can read a German `1,500`
  as 1500 and return 3 for that candidate instead of 506
  ([#634](https://github.com/OpenMaker-Dev/openmaker/issues/634)). A refusal
  with 3 ends an import (scripting.md). A numeric destination's miss
  returns 506 and skips the row, even if the stored text compares equal.
- **An existing record** is among its own related records with its
  unsaved values, as F91 saw for a related list FileMaker Pro shows. That
  validation reads the same list is inferred.
- **A list of an unstored calculation** lists what the calculation gives
  on each related record (`FileCalc::read_field_computed`, #1058), the
  written record's over its unsaved values, with the session's account
  and other `Get` answers. A record whose value can't be computed refuses
  the write with 3, rather than leave its values out.
- **The transaction's other records** ([#278](https://github.com/OpenMaker-Dev/openmaker/issues/278)). At Commit Transaction the
  list reads the records the commit has computed before this one as it
  writes them: their auto-entered and calculated values, not their stored
  rows. A record it computes only after this one is not known yet. The
  list is refused with 3 where it reads one: a record of a table its
  relationships find records in or a calculated match key along them
  reads (`om_data::rules::path_reads_unknown`), of the written record's
  own table, or of a table its field reads when it is an unstored
  calculation
  (`om_data::rules::unknown_when_read`: what that calculation reads of its
  own record and others, and anything at all for a custom function,
  `Evaluate`, `GetField`, `ExecuteSQL` or a context `Get`). A held record
  is read with all its cells, so a record that only changed a field the
  list doesn't read still refuses it until it is computed. Calculation
  dependencies can change preparation order; list dependencies alone do
  not reorder records yet. Checked before the
  commit (an imported row's list, at its import and again at the commit),
  every other held record it reads refuses it.
- **Written by a related file for the opened file's window** (#1000), the
  list's calculations read the opened file's tables through its session,
  with that session's global values, as the write's lookups do.
- **In a save across both files** (#1006), the related file's commit comes
  first. Its rules and lists read the opened file's held records as the
  opened file's commit will write them, computed as a preview computes the
  window's records (preview.rs `root_as_committed`, `CallerReads::pending`).
  A value that commit may store otherwise (its modification timestamp, which
  it draws itself, `Random`, what is computed from one) is not read: a list
  reaching it, a lookup or related field the related file's write
  evaluates from it, or a validation calculation reading it that checks a
  value of the write (`RecordPlan::prepare_entry_over`), refuses the save
  with 3. Unknown values are by record, so a related row of another job
  still reads its own parent. A record the opened file deletes in the same
  save is no record there. What the window hasn't saved, outside the
  transaction, isn't read ([#278](https://github.com/OpenMaker-Dev/openmaker/issues/278)). The opened file's commit, which comes
  last, reads the related files' records the same way, as their prepared
  commits write them (#1167, below).
- **Which writes check it:** a user's (`update_record`), a field validated
  Always, and a value the user typed into a record a script's write
  commits (`ScriptWrite::typed`): a related row the window saves, the
  window's record a script commits.
  - A typed value is checked by a user's rules also in a script's commit,
    outside a transaction and at Commit Transaction, unless the commit
    skips data entry validation. The held record keeps which of its cells
    were typed; a script's later write of one takes it back. A value a
    script set is not checked. What FileMaker does here is unmeasured:
    parity-unverified (#1074).
  - A typed edit of a row imported in the same transaction is refused with
    3 when such a list checks it, since that row's list is not computed
    (the record is not named).
  - The rules checked as a value is entered are checked when the user
    types it (`check_typed_entry`); a write's other validation (not empty,
    unique, existing, a validation calculation) applies to a script's write
    already.
  - Whether FileMaker checks an auto-entered value of such a record against
    a related list validated only during data entry is unmeasured: it is
    not checked. A synthetic custom-list case with validation `0x8180`
    saved an auto-entered nonmember without a dialog (auto-entry.md,
    C125 outcome B). Applying that result to a legacy `0x180` definition
    or a related-values list remains inference.
- **Refused with 3, where the list can't be computed exactly:**
  - a new record that would relate to itself, whose own place is
    unmeasured (#1035); this is checked before the record is reserved;
  - a write that doesn't name its record, such as an imported row's later
    edits;
  - a list starting from another table;
  - a list over records the transaction also holds, also at the commit of
    a row imported before them;
  - a list of a field the list's read does not give exactly: a summary,
    a global, a container, or a repeating field;
  - a list of more than 100,000 items.
- **Set Field's own check** has no record to compute the list for, so its
  value is checked at the commit instead (506 at the commit, not from the
  step).
- **OpenMaker Server's scripts** refuse with 3 a value the rules check
  against such a list.

**A field validated against a value list the rules cannot check yet**
(another file's, a calculation, or a second field) leaves the table
writable.
- **A write whose rules check the field's changed value against it is
  refused with 3.** That includes a user's entry and the commit's check of
  an auto-entered value.
- **A write that doesn't check it is written:** a script's write, for a
  field validated only during data entry (s18), and a related record's
  untyped cells, which the window commits with a script's rules.
- **Such lists are not evaluated.** What FileMaker's membership check
  gives for them is not claimed. An auto-entered or validation calculation may read a
global field of its own table, one repetition and not a container: it
reads the session's value (the
window's, or the server script's), which the write carries
(`RecordEnvironment::globals`); a write whose session values were not
given is refused, never computed over an empty global. An auto-entered
or validation calculation (not a stored one) may also read
`Get ( LayoutNumber )`, `Get ( CurrentPrivilegeSetName )` and
`Get ( CurrentHostTimestamp )` ([#39](https://github.com/OpenMaker-Dev/openmaker/issues/39)): the operation making the write
supplies them (`RecordEnvironment::context`, `WriteContext`), and a
formula reading one it left out refuses the write that evaluates it,
never reads it as empty:
- the desktop: the number the window gives the layout written through
  (0 for one the account's set hides); the host's clock from one op82
  HostTimeStamp, sent only when a formula reads it; and the current set:
  the account's for a user's write, the one a script step's window gives
  for a script's (`ScriptWrite::privilege_set`: [Full Access] while the
  step runs with full access, else the account's). An import is given no
  set, so a formula reading it refuses the import's write. A write routed
  into another file's table answers no layout number there, and reads
  that file's own account set and clock: the calling script's full access
  is its own file's.
- OpenMaker Server's scripts: their session's own answers (its window's
  layout, [Full Access] in a step running with full access, the server's
  clock).
- A file's own recalculation and a new file: no window (the layout number
  is empty, as FileMaker answers), no set.

`Get ( RecordNumber )` is the written record's number as its window
answers it at the save (`ScriptWrite::record_number`, `WriteContext`;
auto-entry.md, "Get ( RecordNumber )", #1038). It is given only where
FileMaker's answer is known; nothing else answers it, not even 0 or the
record's place in what the rules read:
- **Measured:** two local cases. A row typed into the creation row of a
  portal that neither sorts nor filters is its place among the portal's
  related records, after the ones related before the save: 1 with none
  before, 3 with two.
- **Measured: records a window makes.** New Record and Duplicate Record
  in a script append the record to the window's unsorted found set, and
  its number is the found set's count before it plus one, wherever the
  current record was (FileMaker Server's engine, auto-entry.md;
  [#298](https://github.com/OpenMaker-Dev/openmaker/issues/298)).
  OpenMaker Server's scripts give that number to their New Record and
  Duplicate Record and keep it through the record's Set Field steps. At
  commit the number is available only if the row is still at that
  position in its table's unsorted found set and it is the sole new
  row in the commit, counting all tables and imports. A rule that
  needs an unavailable number refuses the write with 3. The server
  stages a duplicate with its copied values. The effective write rules
  replace a copied calculated auto-entry value in the preview and at
  commit, including when the result is empty. An import updating a
  staged duplicate protects the cells it maps from auto-entry. With
  auto-entry off it also keeps unmapped copied values. With auto-entry
  on an unmapped calculation that needs the unavailable record number
  refuses the import row (reported by Import Records as 729). These
  import combinations are unmeasured in FileMaker. The desktop's
  Duplicate Record behavior is described above under the window's
  record session.
- **Hosted files** take the same answer, an inference (parity-unverified).
- **Routed writes:** a portal row of another file's table carries the
  calling window's number to that file.
- A stored Number calculation consisting of `Get ( RecordNumber )` can
  use the position from ordinary New Record in a local window, including
  when earlier rows are omitted. An unsorted set appends the row. A sorted
  set inserts it immediately after the current row when that row is known.
  Unrelated edits and later re-sorting leave its stored value alone.
  Automatic re-sorting after commit is a separate difference.
  The same field definition refuses writes
  on hosted files and routed tables. Duplicate Record, portal creation
  and other creates without the local New Record position refuse with 3
  ([#1011](https://github.com/OpenMaker-Dev/openmaker/issues/1011)).
- The number is unavailable for a portal that sorts or filters, a sorted
  desktop window without a known insertion position, and edits of stored records.
- **Edits that evaluate no such formula are written.** An auto-entered
  or stored calculation with no field inputs is evaluated only at creation.
- **The desktop window gives it in the measured shape only**
  (`begun_record_numbers`, `scripting/host/portals.rs`):
  - one record begun in a creation row, the only record of its table the
    commit writes, the window's own record included through a relationship
    to its own table (an edited record of it could change which records
    are related);
  - from the window's stored record, through a direct relationship that
    does not sort, whose match fields the commit neither writes nor can
    change: a match field that is a calculation, a global, or auto-enters
    a modification value, a calculation or a lookup counts as changing
    when the commit changes anything;
  - every portal of that occurrence on the layout neither sorts (`0x8`)
    nor filters;
  - the account's set views every record of the table.

  The number is the relationship's related records before the save, plus
  one. Any other creation that evaluates the selector is refused.
- **Before the save:** an ordinary commit (a click on the layout's
  background) previews a begun row before its field's OnObjectSave script
  runs (a new record's preview, above; [#277](https://github.com/OpenMaker-Dev/openmaker/issues/277)), with the number the window
  gives, so the script reads the number, as FileMaker Pro 26.0.3's did,
  and the commit stores it.

That a hosted file's `Get ( CurrentHostTimestamp )` is the host's op82
clock is inferred (calc-functions.md 6.3 marks it [U]); a local file's is
the local host's, as FileMaker Pro answers. A record takes the context
of the step that last opened or changed it, not of the step that commits
it: a transaction's records on the desktop and on OpenMaker Server, and a
server script's open record after it went to another layout or left a
step running with full access (the last change's: OpenMaker's choice;
a new record written from a preview keeps the preview's, above).
A changed global
recomputes no record, which is inferred from FileMaker's dependency
entries, not measured (schema-path.md 5.17 [I]). A stored calculation
over a global is not one FileMaker stores (5.16) and keeps the table
read-only.

Browse edits stored text, number, date, time and timestamp fields, later
repetitions included, of the layout's own table, and of related
records one relationship away (a portal row's, a related field's first
record, or one the user begins in a portal's creation row), by the related
table's own rules; a table of another file is written by that file
(below). Data globals are edited as session values (below). The window
commits a record's related edits with it through one transaction (`begin_transaction`, as a script's Commit
Records does, script-runtime.md 13.24): each related record is planned with
a script's rules, its typed values checked by a user's entry rules
(`ScriptWrite::typed`), and a refusal of any record writes none. The
related record is locked when the record is saved, not as typing begins
(FileMaker Pro locks as editing begins, write-path.md 4.1); a record
another client changed since the window showed it is refused with 306
(`DataProvider::retain_related_record`; a transaction's lock now checks
the version the window kept, as an edit outside one does). When the window
leaves a record, it drops that record's edit baseline. A number field
stores the text as typed, with
no parsing, reformatting or conversion, as Set Field stores it (script
oracle 13.20: `abc` in a number field is stored as typed). Its entry rules
read the text in the file's own locale (`rules/schema.rs`), as for Set
Field. That FileMaker Pro's own Browse entry stores the typed text the
same way is inferred, not measured.

**Related records of another file.** A portal row or related field of a
table a related file owns, on a layout of the opened file's own table,
is written by that file (`related_owner.rs`, #1000). So is one on a layout
of another file's table, whichever related file owns the record
([#43](https://github.com/OpenMaker-Dev/openmaker/issues/43)): Browse
offers that layout's portals and related fields as on a layout of the
opened file's table (a table of the opened file by its own rules, another
file's by its owner's), and their edits and a script's writes go to the
file that owns each record. Insert into a container of such a related
record of another file's table can fail there with 3: outside a
transaction, its preparation plans the window's own record as the opened
file's ([#957](https://github.com/OpenMaker-Dev/openmaker/issues/957)):
- **The owner writes.** The opened file's Browse edits, a script's Set
  Field into it (one through a relationship that allows creation creates
  the record, its match fields taking the current record's values) and
  Delete Portal Row go to the owning file's own provider. That is the file
  whose schema the table was imported from, on its own host and port. The
  write runs under the account that file was opened with, its privilege
  set, and its record rules and locks; the field ids are its own. That a
  step's full access in the opened file does not reach the related file's
  records is OpenMaker's reading, unmeasured.
  For a hosted related file, the provider retains the access rules from
  that authenticated account's own privilege-set reply. Browse offers
  only supported stored fields that its table permits editing and its field access
  permits modifying. A synthetic OpenMaker Server test uses an editable
  Root account and a Read-Only Related account on one endpoint: Root edits
  succeed, Related offers no edits, its routed write fails with 200, and
  an independent owner read keeps the stored value. This is provider-level
  evidence; FileMaker Server and native FileMaker parity are unmeasured.
  Unavailable or unsupported hosted-set metadata still leaves enforcement
  to the host; this does not provide complete hosted privilege coverage.
- **Fields.** Ordinary stored text, number, date, time, timestamp and container
  fields are written. Container acquisitions run on the owning file's library
  under the ordinary save's transaction; reference adjustments route there too.
  A script's Set Field writes each repetition, by its cell, as the owner
  checks it (`check_scalar_cells`); Browse offers only fields of one
  repetition there ([#951](https://github.com/OpenMaker-Dev/openmaker/issues/951)).
  A global field is no record's: Set Field into one sets that file's
  session value ("A related file's globals", below).
- **Transactions.** Inside the window's commit transaction (or a script's
  Open Transaction), the change is held in the owning file's transaction.
  On the opened file's connection the owning file joins the opened file's
  transaction (`begin_under`): its locks and reserved records are that
  transaction's, as FileMaker Pro holds two files' records under one
  transaction on FileMaker Server (write-path.md 1.5). On another
  connection it opens its own there, with the same options and of the same
  kind.
  - **Commit.** Every file is prepared (its rules, validation and
    versions) before any record data goes. Then the related file is
    committed first and the opened file last, the order write-path.md 1.5
    shows.
  - **Ending** (OpenMaker's choice, not measured parity). Ending a
    participant's part never sends op65 while another may still need the
    transaction's locks. Once every file has settled, the transaction ends
    once in each file that held part of it, related files first and the
    opened file last.
    - **Why one op65 per file.** OpenMaker Server keeps a transaction's
      locks by file (its op65 releases the named file's), so the opened
      file's op65 alone would leave a related file's locks held.
    - **Unmeasured.** Whether FileMaker Server's op65 reaches another file's
      locks, and when FileMaker Pro sends it, is unmeasured: the capture of
      write-path.md 1.5 has no op65 with the transaction's id.
  - **Revert Transaction** ends it the same way.
- **A script's transaction across files** (#1006). Claris documents Open
  Transaction's changes as spanning "multiple tables in multiple database
  files", committed or reverted together. A script's writes in its window
  to the opened file and to related files (related fields, portal rows,
  and a layout of another file's table) are held in each file's part of
  the transaction, as the window's commit holds them.
  - **Commit Transaction** prepares every file (rules, validation,
    versions, locks, reserved records, and the uploads and commit list
    its commit sends, each within one request) before any record data
    goes. A
    refusal in any file reverts every file's part and answers its error:
    nothing is sent, and each file's report says not submitted. Then the
    related files are submitted, the opened file last; if one fails, the
    files after it are not submitted.
  - **The limit of all or none.** Each file is committed by its own op70
    (write-path.md 1.5), so a connection lost, or a host refusing a later
    file, after an earlier file's op70 leaves that earlier file committed.
    Commit Transaction then answers the error with what became of each
    file (`DataError::commit`), and the transaction ends; nothing is undone
    or sent again. The desktop's error alert names each file's outcome
    ("Related: saved. Root: not known whether it was saved."), and the
    window keeps what was typed. What FileMaker does in that case is
    unmeasured.
  - **Revert Transaction** reverts every file's part.
  - **A script's Delete Record** (and Delete All Records, and Revert
    Record of a record the transaction made) on a layout of another file's
    table is held in that file's part, as a deleted portal row is
    (`DataProvider::delete_record_in_script`): the record goes when the
    transaction commits and stays when it is reverted. Commit Transaction
    prepares such a deletion from the table's fields and the record's
    container references alone, as the ordinary deletion does, so a table
    whose rules OpenMaker cannot plan still deletes
    ([#312](https://github.com/OpenMaker-Dev/openmaker/issues/312)). Which
    records the window shows after such a revert is unmeasured.
  - **A script's Duplicate Record** of a record holding a container, on
    such a layout, takes the copy's reference in that file's library
    within that file's part ([#43](https://github.com/OpenMaker-Dev/openmaker/issues/43)):
    the owning file joins the transaction first (`adjust_container`), so
    that Commit Transaction keeps the reference with the copy and Revert
    Transaction gives it back. A copy refused before it is held gives its
    reference back at once, counted as given back before the release is
    sent, so that the revert does not give it back again.
  - **Still refused (3)** inside the transaction, on such a layout: the
    edits, new records and deletions the user makes there while the script
    is paused (the provider's create, update and delete calls,
    [#411](https://github.com/OpenMaker-Dev/openmaker/issues/411)). No step
    of the transaction made them, and the window's OnWindowTransaction
    would report them before the transaction's outcome. The Records menu's
    New Record and Duplicate Record are the exception: they run their
    steps, which run on top of the paused script, so the record (and a
    duplicate's container references) is held in the transaction as a
    script's is. So is a save of the user's edits to a related record there
    (a portal row, a related field), as a portal row's of another file's
    table on a layout of the opened file's table is: the window commits
    related records through the script write path, and the record's own
    edits saved with them go along, where a save of those alone is refused.
    Whether FileMaker holds them so is #411's question.
- **The window's commit of a record across files** (#1006; it begins with
  `DataProvider::begin_record_commit`, a script's Open Transaction with
  `begin_transaction`). It is saved file by file, related files first and
  the opened file last, as write-path.md 1.5 shows FileMaker Pro doing on
  one host. It is not all-or-nothing: Claris documents no such guarantee
  for it, and OpenMaker makes none.
  - **The opened file's commit reads what the related files' commits
    write** (#1167). Every related file's part is prepared first.
    - The records each one holds are read as its prepared commit writes
      them, by the opened file's ids for their tables, over the stored
      ones. This covers its computed values, its new records, and its
      deletions, which are no records.
    - The opened file's lookups, related reads, validation calculations
      and related-values lists read them this way, and they are final
      once prepared.
    - Before, it read the stored records and committed values from before
      the save. A value it computed when a record was written is computed
      again at the commit.
    - The related files' commits come first and read the opened file's held
      records as its commit will compute them. A value of those records
      that is computed from a record a related file holds in the same save
      is therefore unknown there: a related file's rule reading it refuses
      the save with 3, before any record data. Otherwise each file would
      store a different account of the other.
    - A related file's commit can read another held related participant
      when the rule dependencies between those files are acyclic (#1198).
      Each participant is prepared once, after its dependencies, and its
      immutable full rows and deletions are mapped through the reader's
      own table ids. Every preflight still finishes before any record data
      is uploaded; submission and recovery retain the original file order.
      Dependency discovery follows validation-list calculations and their
      relationship match and sort fields. Deleted rows contribute their
      deletion snapshot, with no outgoing calculation dependencies.
      Cyclic or unresolved dependencies refuse with 3. Within a file,
      calculation dependencies also determine record preparation order
      ([#276](https://github.com/OpenMaker-Dev/openmaker/issues/276)); import-time and related-values-list limits remain below.
    - What FileMaker Pro reads here is unmeasured (parity-unverified).
  - **Refused before any record data,** in any file (a rule, a
    validation): the window's record session stays as it was (#1095).
    - Its held records stay held, a new record with its reserved id
      included.
    - New related records staged by the refused attempt are parked with
      their reserved ids, issued serials and creation values (#1174).
      The next explicit save reclaims those reservations; records the user
      drops are not submitted. Revert releases the session.
    - Explicit reconnect can carry this ordinary scalar group under each file's
      own Full Access or proven static custom rules (recovery.md). Custom proof
      retains opaque account/set identities and the pre-rollback action/field
      footprint; all own rules, schema, routes and versions must match before
      any replacement reservation. Conditional, missing, hidden or changed
      proof stays Unresolved. Caller fields remain distinct from immutable
      issued values. Acknowledged uploaded container acquisitions can also
      travel under static custom proof: the owning account needs global
      visibility and unconditional Edit on each acquired member's wire table,
      with unchanged library/member identities, immutable bytes and owned counts.
      All participants pass before reservations, count adjustments or transfer;
      failed cleanup retains the one detached receipt. A zero-owned stored
      reference grants no release authority. Recovery neither copies stored
      sources (including stale-source error 1225) nor reuploads data. An unused
      container field does not prevent scalar recovery. No save is automatically
      replayed. This is synthetic OMS coverage, not measured FileMaker
      lost-connection behavior.
    - Every file's part goes back.
    - The window rolls back what it staged, keeps its changes for
      correction, and saves them once.
  - **Partly applied:** a file committed, or its outcome is unknown, while
    another file's work is not applied. The provider keeps the save
    (`DataProvider::unsettled_save`):
    - the work never submitted stays prepared, with its locks held, and
      the transaction stays open;
    - nothing is sent again by itself;
    - until the save is settled, the window takes no change. Its fields edit
      nothing. Its record and related writes, imports, deletions, global
      values, container data, serial numbers, Commit and a new record
      session or transaction are refused with 3. Giving a container
      reference back, and reading, stay available.
    - the window's reads show the opened file's part of the save while it
      is not known to be applied, so that its cells show what Resume would
      save. For existing related rows, display reads overlay the retained
      participant's final stored cells, including replacing auto-entry and
      stored calculations (#1151). They are matched by window, file identity,
      wire table and record id, without rerunning rules or changing versions.
      Typed drafts remain kept for recovery but do not override that frozen
      intent. Discard reads the host's values again. Other windows and
      independent clients continue to read the host. Unstored calculations
      still evaluate normally; creation-row previews retain their existing
      display path.
    - **The window's report** takes the place of the unsaved-changes bar:
      a line per file (saved, not saved yet, not known whether it was
      saved, not saved (checked), cannot be checked) and the buttons Check
      Again (while an outcome is unknown), Resume Save (once no outcome is
      unknown or unresolved) and Discard Unsaved Changes. It appears when
      a save fails partly applied, from the user's save (its failure says
      so) or a script's commit (the worker reports every window's with the
      script's end, `DataProvider::unsettled_saves`, a script that ends in
      another window, of any file, included), and stays, the window frozen,
      until the save settles. The window edits nothing from the frame it
      learns so, before the provider's record capabilities arrive: no
      typing is taken that settling the save would drop. Revert Record settles it as
      Discard does. Meanwhile the window saves nothing: a save the user
      asks for (Ctrl+Enter, a click on the layout) does nothing, and an
      action that saves the record first (another record, layout or mode,
      closing the window, a replaced found set) is refused with the bar's
      reason, before any trigger. Once settled, every window still connected
      reads its records again. FileMaker has no such report; it is OpenMaker's own.
    - **Check Again** (`DataProvider::reconcile_save`) reads back each
      file whose outcome is unknown and compares it with the values its
      commit computed and with what was there before (`read_back`). Nothing
      is sent but reads.
      - **Applied:** every record holds the saved values.
      - **Not applied:** every record is as before (an edit at the version
        it was locked at, a new record absent, a deleted one there) under
        the save's locks, which every file's transaction still holds. A
        related file on another connection holds its own transaction's end
        too, until the save settles.
      - **Unresolved:** anything else.
    - **Resume** (`resume_save`) sends only the work never submitted or
      proven not applied, in the save's order, from the values its commit
      computed. Nothing is computed again (no auto-entry, serial number or
      plugin step), and no record is created again. It is refused while a
      file's outcome is unknown or unresolved, and a failure keeps the
      save.
    - **Revert Record, or Discard** (`discard_save`), discards what was not
      saved and ends the transaction. The files that saved keep it: nothing
      is deleted or undone.
    - **Container references.** A copy of a file's work whose outcome is
      unknown or unresolved is dropped without releasing them, as an
      ambiguous commit keeps them.
  - **Failed with nothing applied** (the first file's upload failed
    before its op70): as when refused before any record data, every file's
    part goes back, its records held, and the session stays as it was. No
    container reference is released, and no transaction ends.
  - **Closing the file** settles a partly applied save as Revert Record
    does, on every connection that still works. After a lost connection
    (Reconnect), the window stays frozen with its report: every file not
    known to have saved is unresolved, and nothing is sent for the
    transactions the loss ended. Discarding settles it.

  How FileMaker handles a later file's refusal, and two hosts, is
  unmeasured.
- **What a commit did, file by file.** A failed commit's error lists each
  file whose records it held, and whether the commit committed it, sent
  it no record commit (op70; an upload before it, op14, goes only into
  the connection's temporary area, which changes no record, write-path.md
  3.3), or left its state unknown (`DataError::commit`, #1006).
  Files are told apart by the connection each is open on and the host's
  id for it there, never by a name or a number alone: two hosts can hold
  a file of one name, numbered alike. A commit fails unless every file
  committed. On a failure the window keeps every change it made and sends
  none again by itself.
- **Explicit transactions.** Within an explicit transaction, the related
  file's held changes do not show in the opened file's portals until
  Commit, and Set Field's entry check of such a field refuses with 3.

FileMaker Pro was not measured writing another file's related records
from a portal.

OpenMaker currently reads a typed date, time or timestamp with the
field-type-change rule (schema-path.md 7.2.1, `rules/typing.rs`) and the
file's locale. This rule does not fully match FileMaker Pro's measured
Browse entry (write-path.md 1.4):

- the text is cleaned (full-width digits and punctuation made ASCII, white
  space trimmed) and parsed by om-calc's `GetAsDate`, `GetAsTime` and
  `GetAsTimestamp` parsers;
- a date with a four-digit year and no time keeps the cleaned text; a year
  of one to three digits or none is completed and a time dropped, and the
  date written again in the file's format with the separator it was typed
  with (`1/2/26` is stored as `1/2/2026`, `2.1.03` in de-DE as
  `02.01.2003`);
- a timestamp needs a date and a time of day, written again the same way
  when its year is incomplete, except where om-calc cannot read the
  rewritten text back (a ko-KR file's `오후` before the time), when the
  cleaned text is kept; a time keeps the cleaned text;
- text that does not parse is refused with 500 (date, timestamp) or 501
  (time), "Field F requires a valid date.", and nothing is written;
- a four-digit-year validation rule checks the year as typed, before it is
  completed;
- the same holds where the typing reaches the host in a script's write
  (`ScriptWrite::typed`: a related record the window commits, or a record
  a script commits after the user typed into it): the typed cells are
  read and refused this way, the script's own cells stored as set.

FileMaker Pro 26.0.3's local Browse entry is observed in write-path.md
1.4 (#534): it validates on Tab exit, normalizes accepted date and
timestamp text before storing it, and refuses invalid values with a
Revert Field / OK alert. Two differences are reproduced in the desktop:

- Pro refuses Date `1/2/2003 13:00` on Tab exit; OpenMaker leaves it
  in the field until Save Record, then saves `1/2/2003` without an alert.
- Pro stores Timestamp `1/2/26 13:30` as `1/2/2026 13:30`; OpenMaker
  saves `1/2/2026 1:30 PM`, changing the time notation as it completes
  the year.

The OpenMaker file with both values opened in Pro (#534). Other Browse entry inputs
from the Pro table, hosted input and commit traffic, and additional
locale pairs have not been compared with OpenMaker. When Data Entry is
set to always use the system's formats, Pro's observed de-DE/en-US
case interprets the typed date using the system setting; OpenMaker uses
the file's own locale (file-metadata.md 2.2).

Browse edits Text, Number, Date, Time and Timestamp data globals through
session-value operations. A global edit changes no stored record and reaches
no other client. Its editor opens with the session's value, as the field
shows it, not the value the record's page was read with; while that value
is read again (a script set it), the editor takes no typing, so that no
edit begins from the value before. A global's validation is checked as the typing is
applied (`DataProvider::set_typed_global`), in the record's ordinary save
as in a commit after scripts: its entry rules (maximum characters 511,
strict data type 500-502, range 503, a custom value list or a field of
this file 506) and Not empty (509), which holds while any of its
repetitions has a value, as at a record's commit. The check uses the
complete set of proposed repetitions before publishing any of the edits.
The refusal carries the field's
custom message when it is text, or a calculated one that is text alone;
otherwise, and for a date or time it cannot read, OpenMaker's own
wording. What they refuse keeps the draft and the session's value, as an
invalid date does, and a save with several globals sets none of them. A
global validated by calculation, unique or existing value, a
related-values list or another file's field, whose checks are not
implemented for globals, stays display-only. Supported global fields
stay editable while a related file needed by another field's write rules
is still closed. Temporal entry uses the shared typed-global operation and
the file's entry parser: invalid Date/Timestamp (500) or Time (501) text
keeps its draft and leaves the session value unchanged. Complete accepted
Date text survives calculation readback. Time/Timestamp readback uses the
file locale and the layout object's supported data format, rather than
preserving the typed spelling. This follows Claris's documented display
model (script-runtime.md "Globals") as an inferred choice; native global
readback, field-exit timing and local-file GUI parity remain unmeasured.
Scripts retain their separate permissive assignment operation.

**A related file's globals.** A global field of a table a related file
owns is that file's session value, whichever occurrence names it
([#43](https://github.com/OpenMaker-Dev/openmaker/issues/43)).
- **Writes.** Set Field into one, from a layout of any table, and the
  user's typing into one on a layout of that file's table, go to that
  file's own provider (`global_owner`, through the table's route,
  external.rs). It checks the field and its own account's field access
  (201), and a user's typing by its own entry rules, as its own window
  would (with the opened file's tables readable, as for its records'
  entry checks). A layout of that file's table offers the typed globals
  that file's rules and access allow, as a layout of this file's table
  offers its own (`typed_globals`).
- **Readers.** The value is kept in that file's provider, whose rules read
  it (an auto-entry of a record it writes for this file), and in the
  opened file's session values, which its calculations read through the
  occurrence. The shared desktop values (desktop.rs) carry it to that
  file's own window in this session. They are kept by file as external.rs
  tells files apart (host, port and name, or path), so a file of the same
  name on another host keeps its own. Nothing is sent to either host: a
  new session reads the stored value.
- **Evidence.** That FileMaker Pro keeps a related file's global as that
  file's session value, so that its own layouts, calculations and scripts
  read it, is inferred from Claris "Global fields" (a client's global
  values are its own, per file) and from a hosted multi-file solution
  whose startup script sets its data file's globals so; unmeasured in
  OpenMaker's captures.
- **Limit.** A value that file's own window sets afterwards is not read by
  the rules of that file's records this file writes: that file's provider
  here keeps the last value set through it, and its other values are the
  stored ones ([#874](https://github.com/OpenMaker-Dev/openmaker/issues/874)).

A field object showing later repetitions (2 through 255) accepts
Browse typing for these globals and ordinary supported scalar fields, in
the layout's own table or an editable related record. Its numeric cell
retains the field and repetition identity throughout editing, calculation
previews and saves. Own-record first and later repetitions are written
once together; related-record cells use their existing versioned write.
Global repetitions remain session values and run no record transaction.
Account rights, validation and prohibited modification still apply.

Limits: globals validated by calculation, unique or existing value, a
related-values list or another file's field, and container globals, are
not typing targets; calculation fields are not, as
in FileMaker. Typed globals are applied, and so checked, when the record
is committed; FileMaker validates a global as its field is left (Claris
"Field validation"), and its alert there is unmeasured
([#424](https://github.com/OpenMaker-Dev/openmaker/issues/424)). A field object showing several repetitions divides its stored
bounds evenly among them, across or down as it is stored [I: FileMaker
Pro's repetition geometry is unmeasured]. Repetitions above 255 are
outside the write planner's cell representation. Native repetition entry
is unmeasured; container insertion is tracked separately from typing.

**Index keys of a write.** OpenMaker Server and a file on this computer
key text in the form of the field's index language (query-program.md 7.2,
`om_data::rules::written_text_keys`):
- English and English international: primary weights;
- System and Unicode standard: padded;
- Unicode raw: its code units;
- language 0: the file's own language.

Number, date, time and timestamp fields are keyed in the forms of 7.3: an
empty value under `01`, a value with no digit (`N/A`, a date stored as `?`)
under `fe ff`, and a timestamp stored as a date alone under its date and
`fe ff`. A number's text is read as FileMaker reads it, with the file's
decimal and thousands separators (`om_collate::number_text`, the reading
the calculation engine uses too, calc-values.md 2.8): `1,234` is 1234 in
an en-US file, `abc12` is 12, `(5)` is -5. A date or time with digits that
OpenMaker does not read gets no key.

An index whose keys another rule wrote (an earlier OpenMaker build's, or
FileMaker's where OpenMaker's rules still differ or give a value no key)
can hold stale memberships for a record OpenMaker Server changes
([#448](https://github.com/OpenMaker-Dev/openmaker/issues/448)):
- The first write that changes a field's value, after the server starts or
  the schema changes, compares each of the field's indexes with one built
  from every stored record (`Store::index_agrees`).
- In an index that disagrees, every record the commit changes or deletes
  leaves the entries its new value does not give when the commit finishes
  (`Store::sweep_index`). For example, an earlier build indexed `1.5`
  as 1.5 in a German file. The current reader gives both `1.5` and `15,0`
  the key for 15. Editing the record from `1.5` to `15,0` moves its stored
  membership even though its computed keys are equal.
- The index is never rebuilt: records no write touches keep their
  entries as FileMaker wrote them, an overflow `ff 80` membership included.
- Every entry a write or a sweep changes is kept first, so a failed save
  puts each back exactly.
- Before changing a record, the server checks the existing postings at
  its computed old and new keys. If any cannot round-trip through the
  RID writer, the whole commit fails before publication. This includes
  readable forms such as a one-bit bitmap in place of a singleton, even
  when the computed keys are unchanged and only the sweep would add a
  membership. The sweep leaves other postings of unrecognized shape or
  encoding as stored, so a stale membership there can remain. Counts of
  an unknown shape also stay as stored.
- A commit that changes records in a disagreeing index still scans and
  validates every posting under the write lock. If the sweep makes a
  change, recounting reads the resulting postings again.

A new record's id has no prior membership, so creation skips the index
agreement check and the sweep. The server still checks existing postings
at the new keys and refuses the commit if an edit would not round-trip.
An index that a commit empties stays in the commit's plan, so its later
records are still indexed. Until a write changes a record in an index,
the index is read as stored. A find for `=` can miss untouched empty
values in a typed index an earlier build wrote without `01` entries.

Word-index keys break words as FileMaker's English, System and Unicode
standard indexes do (query-program.md 7.2, "Word breaking"): within a
script, across `.` and apostrophes between letters and `-`, `.`, `,`, `:`,
`/` between digits. FileMaker keeps a no-break space in its word. OpenMaker
still separates there (`WordBreaks::ENGLISH`), because an index an
earlier OpenMaker build wrote holds such words apart, and a changed
record's old entries are found by the rules in force
([#448](https://github.com/OpenMaker-Dev/openmaker/issues/448)).
`om_collate::WordBreaks::of_language` holds the joiners each language was
probed to use, the no-break space included. They differ in `.`, `:` and
`/` between digits and in the apostrophe and `·` between letters. They are
tested against the probe's word keys in 52 of the 56 languages probed:
all but Unicode raw, and Vietnamese, Thai and Greek mixed, whose keys of
the probe's values use weights OpenMaker does not build, so their joiners
are taken from the spec unchecked. No key is written with them yet, since
those languages' text is refused
([#300](https://github.com/OpenMaker-Dev/openmaker/issues/300)). Where the probe did not reach, such as a no-break space
beside kanji or alone, the rules are OpenMaker's reading. Unicode raw's
own word breaks are not built
([#395](https://github.com/OpenMaker-Dev/openmaker/issues/395)).

For a text field indexed in any other language they write no key, rather
than English ones a find in that language would miss (#159):
- A commit that would change the field's text where the field has an index
  in the file is refused and changes nothing. That includes a new or
  deleted record with text there.
- op73 builds no such index while a record holds text in the field, and
  so leaves the field without the index it had.
- A query that needs such an index the file does not hold, while a record
  holds text in the field (a find, a join, a value list), is refused
  rather than answered from English keys, which would give another found
  set ([#572](https://github.com/OpenMaker-Dev/openmaker/issues/572)). It
  builds nothing, and keeps none of the indexes it built of other fields
  before the refusal: a later query builds them again. A client gets the
  query's error, 3 (`om_query::QueryError::UnkeyedIndex`; FileMaker
  Server builds every language's index, so its answer is unmeasured).
  Only after that error
  does OpenMaker's desktop client ask which of the indexes it read are
  missing. If one is, it takes its own path for a missing index, as after
  FileMaker Server's empty answer (query-program.md 7.1). For these
  languages that path fails the find with 3
  ([#46](https://github.com/OpenMaker-Dev/openmaker/issues/46)). A server
  script's find, or a join of its Go to Related Record, gives 3. A blank
  match value relates nothing without reading the index. A field that
  holds no text has no keys to differ, and is answered.

OpenMaker's record rules refuse such a write first (error 3, naming the
field and the language). They do so for a stored text field defined with
an index or holding one in the file, also when the change comes from an
auto-entered value or a recomputed calculation. A change that leaves the
field's text as it is goes through.

Not covered:
- The languages FileMaker does not tailor, such as French and Dutch, are
  refused too, though their keys look like the English ones ([#300](https://github.com/OpenMaker-Dev/openmaker/issues/300)).
- The tailored languages wait for their keys ([#299](https://github.com/OpenMaker-Dev/openmaker/issues/299)).
- Within the forms OpenMaker builds, the keys equal FileMaker's for every
  English, Unicode standard and Unicode raw value of the probe
  (`catalogs/index-key-vectors.tsv`). Two choices are OpenMaker's, since
  FileMaker's are unmeasured (query-program.md 13, item 1). In the padded
  form, the first weight of an unlisted code point carries secondary
  weight 0x20 and a second weight carries none, as in UCA. A character that
  DUCET lists with UCA's implicit weights for an ideograph keys as that
  ideograph does in FileMaker: `⼀` U+2F00 as `一` (`4e00`), U+FA0E as
  `ffe0 fa0e`. Whether a record is also listed under `ff 80` follows
  FileMaker only in part ([#177](https://github.com/OpenMaker-Dev/openmaker/issues/177)).

**Furigana.** A normal field with "Use Furigana Field" (option `0x40`)
leaves its table writable. OpenMaker's record rules refuse a write that
changes that field's value (error 3, naming the field and #1302). The
change can come from typing, a script, or an auto-entered value.
FileMaker Pro writes the reading of the text into the field the option
names, which OpenMaker does not do (what FileMaker stores for the option is
#1301), so the reading would be left as it was. Other fields of the record
change as usual. Before, the option made the whole table read-only, as an
unsupported field option. Whether FileMaker Pro fills the reading for a
change by a script, an import or auto-entry is unmeasured.

A number field with unique or existing-value validation keeps its table
read-only, as before. Unique and existing values are checked in the field's
value index, so such a field must be defined for a value index in English (21,
77), System (1), Unicode standard (3) or Unicode raw (2) (auto-entry.md,
Index bits), the forms whose keys OpenMaker builds, or in language 0 when
the file's own language is one of them and its locale record can be read
(a file without one counts as English). A language FileMaker leaves
untailored counts too (Dutch, French, German dictionary, Greek, Italian,
Portuguese, Russian, Hebrew, Serbian, Bulgarian and the Indic languages 59
to 67): its value keys
are English's (query-program.md 7.2; the set is inferred from bin's list of
tailored languages and a probe,
[#46](https://github.com/OpenMaker-Dev/openmaker/issues/46)). A change of
such a field's text is still refused, as its word keys are not built
([#300](https://github.com/OpenMaker-Dev/openmaker/issues/300)). A field in
a tailored language keeps its table read-only
([#299](https://github.com/OpenMaker-Dev/openmaker/issues/299)). The client
and the host build the key in the field's value form
(`om_data::rules::value_text_keys`, query-program.md 7.2), cut to 219
bytes as the index holds it (7.1), so a long value is checked against the
values that share its first 109 weights, or code units in Unicode raw ([#177](https://github.com/OpenMaker-Dev/openmaker/issues/177)).
Raw keys keep case, so `A` and `a` are two values there; FileMaker documents
its Unicode index as case-sensitive, which was not measured.
A value list of a field's values is checked in its source field's value
index the same way: a text source field in a tailored language refuses
the check with 3, on the client and on OpenMaker Server, rather than
keying the value in English, which could both miss the host's key and
match another value ([#571](https://github.com/OpenMaker-Dev/openmaker/issues/571)).
A number, date, time or timestamp source uses its type's index keys,
with dates read in the file's order (query-program.md 7.3). For example,
`5.0` matches a listed `5`
([#591](https://github.com/OpenMaker-Dev/openmaker/issues/591)). Number
sources in files with a decimal separator other than a point are still
looked up as text until #415. This avoids false matches against an index
built by OpenMaker Server, whose number parser keys a German `1.000` as 1.
A value for which OpenMaker cannot build a typed key also falls back to
text. On the desktop, a date or timestamp source with an unreadable
locale record returns 3 because the date order is unknown. A new record's
refusal before its commit (unique, existing value, value list) is the
write's, with its code: the record id it reserved stays unused, as
FileMaker's reserved ids can (write-path.md 2).
A definition marked indexed does not prove that the host has built the
index (schema-path.md 6). Unique, Existing and
field-backed value-list validation therefore read the value-index counts
`[T, 01, F, 10]` with GetKey before ExistsInIndex in the same op86 program
(query-program.md 7.1, 8.4, 8.5), using independent result spots and the
access key of instruction 2 for ExistsInIndex. A missing counts leaf makes
the answer unknown, including when that query builds the index for later
queries. Existing indexes need no record scan.

When counts were missing, the client scans the source table's committed
records in batches of 128, excluding the current record where required,
and compares the same text keys, including every repetition and
stored line, empty keys, accents, case and significant whitespace
(`om-query::keys::record_value_keys`, also used by PR65's find fallback).
It keeps a unique field's index lock throughout this check until the
transaction ends; pending transaction duplicates are still checked among
the held records before upload, by the same keys: a held value is checked
by its key against the other held values' keys and against the keys the
other held records' values put in the index, one per stored line, so
`green` and `green` CR `purple`, or the same multiline value, in two held
records are 504. A write's changed repetitions of a unique field take its
index lock once and ask their distinct keys together, up to 64 in one
ExistsInIndex (query-program.md 8.4), and the fallback scan runs once for
the keys the host could not answer. Entry, import and skip-validation rules
are unchanged. The scan reads at most 100,000 records, 1,000,000 repetition
cells and 64 MiB of record replies; exceeding a limit refuses the check
with an explicit unsupported error (3), never an absence result. It also
refuses unreadable cells, record nodes or source definitions. Requested
batch IDs are reconciled with the returned rows. A missing row or source
cell is confirmed with an independent op90 record read before it can mean
absence or an empty value; a genuinely disappeared record is skipped.
Confirmation replies count toward the same byte budget, and a refused or
unreadable confirmation fails the check while the unique lock stays held.
A confirmation must finish exactly its requested subtree; trailing data
cannot establish an empty source value.
Missing-index fallback is
limited to stored text keyed in its value form
(`om_data::rules::value_text_keys`): English (21, 77), System (1), Unicode
standard (3), Unicode raw (2), a language FileMaker leaves untailored, or
language 0 resolved as above. A field-backed value list is checked in its
source's value form too; a text source in a tailored language refuses the
check with 3, with the host's index or without (above,
[#571](https://github.com/OpenMaker-Dev/openmaker/issues/571)). A stored
number, date, time or timestamp source is scanned in its type's keys,
with dates read in the file's order. Text fallback compares whole stored
repetitions and skips absent cells. A source with more than 255
repetitions is refused because a later repetition would read another
field's cell. Tailored-language text, global, summary and unstored
sources are unsupported without an index. A stored text source that
prohibits index creation can be scanned on each
check. Field-backed lists still require a local source without a related
filter or second display field; CreateValueListEx is a separate path.

Repetition n of field F is stored under the cell `F * 256 + n`
(execute-sql.md), for repetitions 2 to 255. For a field id above
`u64::MAX / 256`, which no FileMaker file has been seen to hold, no
repetition after the first can have a cell. The record rules (`om-data`,
which OpenMaker Server's scripts share) and the client's finds, joins and
unique scans read those repetitions as empty, never as the cell the key
would wrap around to. This is OpenMaker's choice; FileMaker's behaviour is
unmeasured.
Auto-enter lookups are performed for exact matches using the
exact-match form FileMaker Pro 26.0.3 was seen to save and run
(auto-entry.md, Lookups; schema-path.md 5.5), with any padding in its key
buffers: as FileMaker Pro 26 writes it, as the Assets starter solution
holds it (Assignments::Name, whose differing bytes FileMaker Pro's dialog
shows as the same settings; #549), and with the other padding a solution
FileMaker Server hosts holds (#993). Its option bytes, and the `01` before
the starting occurrence, must be as FileMaker Pro writes them. It holds:
- "if no exact match: do not copy", with "don't copy contents if empty"
  on or off;
- "if no exact match: use" for a Text target, with either empty-copy
  setting. The stored constant is copied on a miss. A blank fallback
  (member `0b` absent) clears the target on a miss. If the relationship
  finds an empty related value, the empty-copy setting decides whether
  to clear or preserve the target. FileMaker Pro's local Text case was
  measured ([results](https://github.com/OpenMaker-Dev/openmaker/issues/1020#issuecomment-7185793871)).
  Its immediate Set Field timing is still outside the commit-time
  lookup path, and hosted fallback behavior is unmeasured. A fallback
  with a replacing auto-entered `Get ( UUID )` match key refuses with 3;
- a stored text, number or date field of one repetition with no other
  auto-entry, copying a stored text, number or date value of one
  repetition (a data field or a stored calculation), a date field only
  from a date: the related value's stored text as it is, as FileMaker
  Server's engine copies it (auto-entry.md, Lookups, battery s43). These
  are the types both hosts read with their stored text. A related date
  field holding text that is no date refuses the write with error 3 (the
  desktop reads it as text, OpenMaker Server's scripts as `?`; FileMaker
  copies the text). Times and timestamps lose their stored text when read
  (`01:02:03.500` reads as `1:02:03.5`), and a date field holding other
  text reads as text in calculations where FileMaker reads an invalid
  date (`+ 1` gives 1/1/0001), so lookups of times and timestamps and of
  text or numbers into a date field keep the table read-only ([#235](https://github.com/OpenMaker-Dev/openmaker/issues/235),
  [#236](https://github.com/OpenMaker-Dev/openmaker/issues/236));
- a field of another table of the same file, through one relationship
  whose predicates are all `=` on stored text match fields, unsorted or
  sorted only by the related table's own data fields and stored
  calculations, each in its own or a custom value list's order, text not
  in Japanese.
  The record's match fields may be supported stored Text calculations
  of one repetition, nonglobal, with complete dependencies (#1194), plain
  fields without auto-entry, or ordinary Text keys with a supported
  calculated auto-enter. A calculated auto-enter key can read the same row.
  Unstored/global keys, unsupported or unavailable dependencies and
  dependency cycles remain refused.

How OpenMaker performs it:
- A write that sets a match field, also to the value it had, reads the
  first related record through the host's own related reads: the first
  in the relationship's sort order, as FileMaker Server's engine copies
  it (battery s42), or in creation order through an unsorted one (battery
  s41 measured "also to the value it had"). The source is the provider's
  calculation source, whose value index narrows the candidates, or
  OpenMaker Server's session records. Plain keys can be read before the
  write is computed. A calculated source key is computed first; the
  evaluator demands a read with that final key before copying or running
  dependent calculations. Lookup match dependencies participate in cycle
  detection and previews. Missing or deferred computed keys never become
  successful empty reads. Creation and edits to a calculated key's inputs
  trigger this path. One local native two-table case with stored Text
  calculation keys (`0xa0000020`) updated the key and copied Text at input
  field exit, before a deliberate blank-layout click (auto-entry.md,
  Lookups). It does not isolate an implicit commit or establish hosted
  behavior, general related calculations, same-value input writes, or
  `0xa0000000` all-empty behavior. Same-value input relookup remains
  inference, as does extending the native result to those conditions.
- When auto-entry changes a Text key, OpenMaker reads the lookup with that
  final key. An input edit that retains the key under "Do not replace
  existing value" also retains the prior looked-up value. FileMaker Pro
  exposes those changes at New Record and Set Field, and OpenMaker uses
  the commit-time lookup path. A same-value write to the input of an
  auto-entered key does not rerun its lookup. OpenMaker refuses Relookup
  Field Contents through that input with error 3. FileMaker has not been
  measured for these two cases.
  [Native measurement](https://github.com/OpenMaker-Dev/openmaker/issues/1036#issuecomment-7185794524).
- A write that sets the lookup field itself keeps that value. Which came
  last is not known; this is OpenMaker's choice.
- A write that fires a lookup it could not read is refused with error 3.
  A server script stopped or out of time while its lookup reads the
  related table ends the read at the next record, list or 500-record
  download with error 1, before anything is copied or written.
- Scripts and imports that perform auto-entry fire lookups as a user's
  entry does. That is unmeasured.
- The looked-up value appears when the record is written, not when Tab
  leaves the match field as in FileMaker Pro.
- A new record whose key is auto-entered as `Get ( UUID )` does not fire
  the lookup. FileMaker fires it with the new key, which no related
  record holds, so it copies nothing either (s41).
- The same holds for a new record given another's key, as a duplicate
  is, when the key's auto-entry replaces it with a new UUID. An import,
  which keeps the key it writes, fires the lookup with that key.
- OpenMaker Server's scripts give the rules a Set Field of a match field
  as a change even when the value stays, so the lookup is performed, as
  s41 measured.
- Which of several matching records FileMaker copies from through an
  unsorted relationship is unmeasured.

Any other lookup keeps its table read-only:
- the next lower or higher value (#146), or a fallback into another field type;
- any other unknown byte of the definition, or a mix of the two stored
  forms;
- several relationships;
- a relationship sorted by a field of another table, a summary field or
  an unstored calculation (which OpenMaker's sort orders as empty), by
  a summary field's or a value list of field values' order (which it
  ignores), or by text in Japanese, by the level's, the field's or the
  file's language (which it orders otherwise than FileMaker in places,
  calc-values.md, "Japanese"): the first record would not be FileMaker's;
- other predicates or field types.

Auto-entered and validation calculations may read related fields
(auto-entry.md, "Calculations over related fields"), in the form lookups
are performed in: another table's stored value of one repetition, through
one relationship of `=` predicates, each of two fields of one type, text
or number ([#39](https://github.com/OpenMaker-Dev/openmaker/issues/39)): a stored field of the record's table, not global,
including a supported stored calculation (#1204), and a stored field of
the other, auto-entered or not (a serial key is matched by its stored
value). Unsorted or sorted as a lookup's may be. Local match fields join
the reader's dependencies so a calculated key runs after its inputs and
before the related read and its consumers. A prefetched answer with other
keys is blocked; only a relationship the expression actually follows
demands another read, within the existing retry/cancellation budgets.
A calculated source key whose evaluation has not settled is also blocked.
A calculation skipped because all its inputs are empty with `0x20` clear
has a final empty value; that is not an unfinished key.
Deferred, missing, global, unstored, cyclic or unsupported inputs never
become a successful empty answer. The related record contributes its
stored value, including when that field has auto-entry; the read does not
run the related field's formula. Native automatic timing for this specific
case remains inference (auto-entry.md, Calculations over related fields).
This does not establish arbitrary calculated joins or a complete
application creation plan.
- **In a related file** (#1139). For a table of the opened file, the
  related files read by its stored calculations, auto-entered and
  validation calculations, looked-up values and validation value lists
  open before the write is planned, read from the field definitions the
  planner uses. A table of an already-open related file whose field
  catalog is unread waits in the same way: the write reads that catalog
  before planning, while record capabilities defer without reading it.
  An unstored or global calculation, which a write does
  not compute, opens nothing. They open as a layout's do before it is drawn, and their
  records are read on their own files' connections.
  - A typed value's check and a script's held write plan the same way.
  - The window's record capabilities (New Record, edits), which the window
    asks for before it draws a layout, open nothing. A plan refused while
    such a file waits is offered as the plan would offer it, from the
    fields' definitions (`rules::user_editable`). The write then opens the
    file and plans before anything is sent, and a refusal fails it with the
    window's typing kept. On a production-like solution's first layout the
    write rules read no file the layout does not.
  - A related record's editable fields (a portal row's) are offered the
    same way, its own write opening the file and planning first.
  - A file that does not open leaves the rule refused as reading a missing
    occurrence, as before.
  - A related file's own write planned for the opened file ([#39](https://github.com/OpenMaker-Dev/openmaker/issues/39)) reads
    the opened file's tables, and also its own other related files that
    the opened file has open (#1160).
    - The owner's plan imports them as it imports the opened file, and its
      rules read them through the opened file's sessions to them, lent
      for the call, as the opened file's own session is.
    - On demand, the opened file first opens those of them the owner's
      rules read.
    - A data source with several paths names the file hydration's search
      of them found. A path whose file refused the account, was not
      reached or was not tried yet ends that search, so the occurrence
      stays unresolved and a rule reading it is refused, even when a later
      path names a file open through another data source.
    - A third file held by the same record session stays in the schema.
      At hold time its nondeleted held values are provisional and unknown:
      related auto-entry remains deferred, and a validation needing an
      unknown value refuses explicitly. Final preflight reads immutable
      prepared peer rows, including computed cells and deletions, in either
      edit order when the dependency graph is acyclic (#1198). No peer is
      prepared at hold time. Unresolved and cyclic reads remain refused.
    - The record capabilities of a layout of the owner's table, and of the
      opened file's fields of it one relationship away, plan over that
      plan without opening anything, and defer as above while such a file
      waits.
    - A file the opened file could not open leaves the rule refused.
- **A match field the write computes itself** (auto-entered on the
  record's side, #1035). The related records are read with the key the
  write leaves before it is computed.
  - A formula that then follows the relationship by another key, the one
    the write computed, asks for it. The record is read again with that
    key and computed again, at most four times on the desktop: once for
    each key computed from what another such read gives.
  - A branch that does not follow the relationship needs nothing. One
    that does, past those rounds, refuses the write with 3.
  - OpenMaker Server's scripts do not read again: such a write of theirs
    that follows the relationship is refused with 3.
  - That FileMaker follows the relationship by the computed key is
    inferred from F91, where a related value list followed the unsaved
    key (file-metadata.md 6.4).
- **A self-join** (a relationship to another occurrence of the record's
  own table, #1035).
  - **What is measured:** a new record's auto-entry reads a self-join that
    no other record matches as empty (auto-entry.md, "A new record's own
    self-join"). It is answered so (`RecordPlan::answers_own`) when all of
    these hold:
    - the formula is the field's auto-entry, and the record is new;
    - no other stored or held record is related;
    - the commit holds no other record of the table;
    - the relationship's keys on both sides are final: fields the write
      gives, or computed fields whose evaluation has passed without being
      deferred or omitted. C126 measured a computed non-empty key with no
      other row.
  - **Older matching rows (C126 follow-up, s47).** A new row's auto-entry
    reads the rows committed before it with its key through an unsorted
    equality join, never itself: aggregates and the first related record
    read them in creation order. FileMaker Pro's local witness read one
    older row (computed key, an aggregate); FileMaker Server's engine read
    zero to three older rows of records a script made (given keys, an
    auto-entry that replaces an existing value; auto-entry.md, "A new
    record's own self-join"). Both sides' keys must be final and the
    commit must hold no other row of the table: a desktop commit's held
    records, or, on OpenMaker Server, the other records of a script's
    transaction. The s47 allowance requires an auto-entry that replaces
    an existing value. A calculation that keeps a value from creation
    retains the earlier C126 allowance for one older match with a computed
    key and an aggregate. A join on several equal predicates is read the
    same way, an inference. Reads by position (`Last`, `GetNthRecord`) are
    outside these allowances.
  - **A match field on the record's side left empty** relates nothing, the
    record included, since an empty match value never matches
    (file-metadata.md 4.3). This holds also where the write computes that
    key: its value is final by then. A reader of a key left out is left out
    too, and one read with other keys is not answered. Only a field the
    record's row holds counts, not a global or an unstored calculation.
  - **Measured, not answered yet:** an edited record reads itself, with
    the values being written, where its committed key matches the key being
    written, and not otherwise; a second new row of one transaction does
    not read the first (s47). OpenMaker refuses membership-dependent
    reads for edited records with 3. For a new row with another pending
    change in its table, the server preview omits the result if it needs
    the self-join, and the commit refuses that read with 3. An empty
    match key is still answered as above.
    [#297](https://github.com/OpenMaker-Dev/openmaker/issues/297)
  - **An unsorted group join with unequal row keys** supports direct
    first-related field reads in do-not-replace auto-entry at commit. The
    predicates compare two Text fields with themselves across occurrences
    of the same table, one with equality and the other with inequality.
    On a local file, creation may leave the group key empty. An
    existing record may join a group when one field-setting step assigns
    its previously empty group key and the written and stored row keys
    are single-line. Both versions of the row must be excluded by the
    relationship's comparison. FileMaker enters the result at Set Field.
    OpenMaker stores it at commit for this sequence. A later input edit
    or clearing the key before commit could otherwise store a different
    value, so those sequences return error 3. Hosted and routed writes
    using the grouped self-join return 3.
    [Native measurements](https://github.com/OpenMaker-Dev/openmaker/issues/297#issuecomment-7184309703).
    This join also supports the replacing Number formula
    `If ( Peer::Primary = 1 ; 0 ; 1 )` with a replacing UUID row key on
    the same table. Ordinary New Record in an unsorted local found set
    shows `1` before the group is assigned. After Set Field assigns the
    group, a script calculation reads the recomputed result before
    commit. Commit stores the result for the final group and row key. A single
    field-setting step can assign an existing record's nonempty group
    again or move it to another group. An unrelated field edit keeps the
    stored result. OpenMaker computes the exact single-field assignment
    for script reads before commit and evaluates it again at commit.
    Where the pending assignment falls outside that supported sequence,
    the affected result returns calculation error 3 instead of a stale value.
    Explicit edits of the result or the stored row key and clearing the
    group are refused. Hosted, routed, and other creation paths are
    refused before record creation or serial reservation.
    [Replacing native measurements](https://github.com/OpenMaker-Dev/openmaker/issues/297#issuecomment-7191674029).
  - **What is unmeasured:** sorted and other non-equality joins with
    matches, stored and validation formulas, and portal rows made in one
    commit.
  - **Its stored row is never read as a related record.** Every write names
    its record (`rules::Written`): new, as reserved by New Record, or
    stored. The stored row holds the values the write replaces.
  - **A related set the record cannot be in is answered:** neither as
    written nor as stored does it relate to the record as written, by the
    relationship's own comparison (numbers as numbers, text as its index
    keys). Its answer is then the other records', whatever FileMaker does
    about a record's own membership. This holds only while every match
    field it is compared by, on either side, has its final value:
    - one the write gives;
    - or a computed one whose turn in the evaluation has passed and that
      was not left out.

    A global or an unstored calculation, which the record's row doesn't
    hold, never counts. So a later record of a group, whose computed key
    on one side differs from its own computed key on the other, reads the
    group's records. Equal keys require one of the measured new-row
    auto-entry cases above or an answer unaffected by its own membership;
    other self-dependent reads remain refused.

    So is one where its membership cannot change the answer:
    - the first related record of an unsorted self-join when a record made
      before it is related, since that record is first in creation order
      either way. A record not reserved yet is the newest; a reserved or
      stored one has its id's place, since another client may make a
      record after the reservation.
    - fields given to aggregates, which skip empty values, that the record
      leaves empty, stored and as written. An auto-entered
      `Max ( Self-join::f ) + 1` with "do not replace", computed only
      while `f` is empty, is one.
  - **Anything else that follows the self-join** refuses the write with 3,
    and only a branch that follows it does: the first row of a new group
    outside the measured case above, a field read by its place (`Last`, `GetNthRecord`), an aggregate of a
    value the record has, or a sorted self-join's first record.
  - **A write that doesn't name its record** answers no self-join. So does
    a row imported earlier in the same transaction, and later edits of an
    imported row. Their rules see what the import left, not the stored row.
- A related field passed where a field is required reads every related
  record instead, in the relationship's order (#1036). That covers
  aggregates such as `Max`, `Count`, `Sum` and `List`, and `Last` and
  `GetNthRecord`. More than 10,000 related records refuse the write with 3.
  That such a formula is evaluated when the same match fields are set as
  for a plain read is inferred: s40 measured a plain read only.
  - Every record read this way carries the match and sort fields of
    each relationship the formulas read its table through. A plain read
    through another occurrence of the same table then still finds the
    first record its own sort gives.
  - A relationship to such a table sorted by a later repetition keeps
    the table read-only.
  - `LookupNext` reading a related field in any argument keeps the table
    read-only: with nothing related it falls back to the host's index.
- A number match field is matched as a number: `7.0` relates to 7, as a
  number index keys them (query-program.md 7.3). That FileMaker matches
  a non-integer the same way is inferred.
- In a transaction the read sees the records the transaction holds over
  the stored ones (`HeldRecords`): a parent made in the same transaction,
  its serial key taken at New Record, is found, an edited record reads
  as edited, a deleted one is not found. Nothing is committed for it. At
  Commit Transaction prepares records after the records their rules need,
  using first-change order among ready candidates. The rule reads identify
  actual related records, including different rows of the same table;
  match and sort fields that are still computed also defer the reader.
  Only rules that can run for this write demand final match keys; an
  unchanged field's inactive validation does not. A held serial cell that
  the writer will replace with its reservation stays provisional for
  readers until that record is prepared; script overrides keep their value.
  Read-only discovery uses deterministic preview values for calculated
  match keys, with the held creation clock, UUID and reserved serials;
  it reserves nothing and draws no random values. Each record's final
  preparation runs once. Its immutable complete row then replaces the
  provisional held cells for later readers. All preflights finish before
  any record upload. Cyclic or unresolved reads refuse with error 3,
  without a fixed-point calculation or a fallback to stale stored values.
  A deleted record supplies its deletion and evaluates no formula.
  An imported row reading a related record the transaction made or
  changed (an imported one only once changed since) is still refused:
  its rules run before Commit. Related-values-list ordering limits are
  described above. FileMaker's cross-record transaction evaluation order
  is unmeasured; reading related records as Commit writes them is
  OpenMaker's choice, not verified parity.
- An auto-entered calculation is evaluated again when its match fields
  are set, as on FileMaker Server's engine (s40). A change to the related
  record does not evaluate it.
- A validation calculation reads the related record as it is at the
  write: with nothing related, `Phone Numbers::Number ≠ ""` refuses it
  with 507.
- A write whose related record was not read is refused with error 3.
- A match field set to the value it had evaluates the formula on
  FileMaker's engine (s40); OpenMaker evaluates it when the write changes
  the value. Which of several related records is first is unmeasured.

Any other formula over related fields keeps its table read-only: paths
of more than one relationship,
relationships sorted in an order OpenMaker cannot reproduce (as for
lookups above), and match fields of other types (dates, times,
timestamps), of two types, or global on the record's side.

A record the opened file writes into a related file's table (a portal
row or related field, #1000, or a layout of that table, [#43](https://github.com/OpenMaker-Dev/openmaker/issues/43)) is planned
by that file's rules over its own schema with the opened file's tables
imported ([#39](https://github.com/OpenMaker-Dev/openmaker/issues/39)). Its occurrences whose data source found the opened file,
by its only path or one of a list, are bound to them, matched by host,
port and name or by path, never by name alone. A Set Field's entry check
plans the record the same way; its occurrences of other files stay unresolved, and a formula
reading them keeps the table read-only. Its formulas read the opened
file's tables through the opened file's own session and account, so that
account's record access applies; whether FileMaker reads them through the
related file's own connection to the opened file is not measured. In a
save across files the related file reads a record the opened file holds
as the opened file's commit will write it (above: in a save across both
files).

Serial numbers, generated on creation or on commit, are taken from the
host for each new record (auto-entry.md, Serial numbers): op72 advances
the field's next value by its increment and returns the value the record
takes (write-path.md 3.2). OpenMaker Server, and so files on this
computer, serve op72 under the file's write lock and save the file at
once, with the next value as text in `[T, 3, 5, F, 0c]`, where FileMaker
reads it: FileMaker Pro and OpenMaker continue one sequence.
- The last run of digits is advanced and keeps its width, however long
  it is: a value that stayed as it was would go to two records. Text
  without digits gets the increment appended. Both are OpenMaker's
  choices; FileMaker's handling is unmeasured.
- A serial generated on creation is taken when the record is created,
  as FileMaker Pro takes it right after op71, so a script reads it before
  the commit: by New Record and Duplicate Record, a script's or the menus',
  inside a script transaction or the window's record session (held with
  the record), and by New Record in OpenMaker Server's scripts. Otherwise
  the client creates a record when it first commits it (a record written
  in one step outside any transaction), and takes both kinds then, op72
  before op71, so a
  write the rules refuse uses up no record id. A held new record's
  on-commit serial is taken at its commit, and shows then, as FileMaker
  Pro shows it.
- A duplicate takes new values: Duplicate Record in Browse, and a
  script's on the client and on OpenMaker Server. A value a script or an
  import sets in the field is kept, as Set Field's after New Record is in
  FileMaker, and a serial is taken anyway: the sequence moves past it,
  as it does in FileMaker, where the serial was taken before the script
  set the field (for an import, unmeasured).
- A value is never given back: a write refused after its serials were
  reserved leaves a gap, as a reverted transaction does in FileMaker.
- An existing record whose on-commit serial field is empty gets no value
  when it is edited. What FileMaker does is unmeasured.
- A serial in a date, time, timestamp or container field, in a repeating
  field, with an increment below 1, or with another auto-entered value
  keeps the table read-only. So does a number serial with unique-value
  validation, as any number field with it does (above).

Entry from the previous record, repeating
calculated fields, globals (but for
the auto-entered and validation calculations above, of the table's own
globals of one repetition that are not containers), summaries, unstored calculations or related fields
(but for the auto-entered and validation calculations above), plug-in
functions, custom
functions that read fields and `Get` selectors that need a window (but for
the three above) are not supported; such tables stay read-only. A save
whose record upload is larger than one request is refused before any
record data is sent (multi-part uploads are not supported). A
calculation that fails when evaluated refuses the write. Deleting needs no
field rules and is available on every table.

Record pages and the records a write returns carry every unstored
calculation, summary and global field, evaluated on the provider
(`om-provider/src/calc.rs`).

## Tests

`om-provider/tests/om_server.rs` and `om-gui/tests/live_edit.rs` run these
paths end to end against `om-server` hosting a copy of test project
(`-- --ignored`, with the git-ignored fixture). Unit tests in
`om-protocol/src/write.rs` rebuild the captured op14 and op70 bodies.
The non-ignored `finds_and_transactions.rs` tests use only synthetic
loopback files, including delayed op18 index creation, cold/warm controls,
literal native key oracles, transaction refusals, scan limits and entry
options.
