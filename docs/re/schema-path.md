# Schema editing over the network - Manage Database

Status: transaction layouts, ordering and locking complete for creating and
saving tables, fields and table occurrences, and for deleting a table
(2026-10-28). Catalog contents are decoded far enough to build and read them;
the calculation token stream stays opaque. Open items are in section 12.
Each claim carries a confidence (High/Med/Low) and its evidence:

- `om:<rid>` - an exchange in `re/captures/2026-10-28-s06-omserver-n/conn-001`:
  FileMaker Pro 26 against OpenMaker Server, opening File > Manage >
  Database on `test project`, defining a table "Test" with four fields and
  pressing OK twice, then Cancel. OpenMaker Server answered ops 82, 61 and 15
  with status 3, so the requests are FileMaker Pro's own up to the first OK;
  everything after the first op61 is FileMaker Pro reacting to a refusal.
  Extracted per exchange by `re/work/query/tools/capx.py`
  (`re/work/schema/capn/`), dumped with `re/work/schema/tools/dumpx.py`.
- `live:<run>/<tag>` - a transaction sent by the RE probe
  (`re/work/schema/oms`, scripts `re/work/schema/live/s*.txt`) to the lab
  FileMaker Server 26.0.3, file `test project` (file id `0x165`). Request,
  reply and back-channel bytes are in `re/work/schema/live/<run>/<tag>.{req,rep,bc}`.
  Run s0 replayed FileMaker Pro's requests from `om:` one for one.
- `bin:<name>` - the Mac client engine `DBEngine` (x86_64 slice, `nm`
  addresses; `mac:`) or the Linux server engine `libDBEngine.so` 26.0.3
  (DWARF; `nm` addresses). Decompiled with `re/work/download/DecompFix.java`
  (`re/work/schema/dec*.sh`); behaviour is described here, not copied.
- `fmp:<run>` - FileMaker Pro 26.0.3 on macOS saving one Manage Database
  edit in a local copy of the Contacts starter solution, the file compared
  key by key before and after (5.8).
- `tpl:` - the shipped English templates and FileMaker Server sample
  (`re/work/schema/tpl/*.txt`, dumped key by key; field list in
  `re/work/schema/tpl/fields.txt`).

Notation as in write-path.md: integers are big-endian; `[a, b]` is an HBAM
key path with segments in hex; a **key** field in a transaction body is
`[segments as (u8 len, bytes)...][u8 total length]` (`Push(uchar const*)`);
"push order" lists fields in the order the sender appends them, the receiver
pops from the end. Table ids are serial keys `80 xx` (`80 06` = table
`0x8006`), table occurrence (TO) ids `d0 00 xx`, field ids `01`..`7f`
(then `80 80`...), relationship ids `01`...

Catalog keys used throughout (bin `_GLOBAL__sub_I_DBGlobals.cpp`,
re/work/write/kconst.txt):

| key | name | meaning |
|-----|------|---------|
| `[3, 16]` (`03/10`) | `KTableCatalog` | base tables |
| `[3, 17]` (`03/11`) | `KTableAliasCatalog` | table occurrences |
| `[3, fb]` | `KRelationCatalog` | relationships |
| `[T, 3]` | `KCatalog` under a table | the table's fields |
| `[cat, 1]` | `KIndex` | a catalog's index: id counter, name list, ordering, count, mod info |
| `[cat, 5]` | `KItems` | a catalog's members, one subtree per member id |
| `[cat, 1, fd]` | `KDeferred` | field catalogs only: pending data work for the server (7.2) |
| `[cat, fe]` | `KModified` | client-side only: the members changed in this edit session |
| `[08]` | `KGlobal` | lock path of "the schema is being edited"; holds no data |
| `fc`, `fc01`, `fc02`, `fc03` | `KModCount`, `KModUser`, `KModAccount`, `KModTimestamp` | modification count and who/when (section 8) |
| `d8`, `d9` | `KUniqueID`, `KSourceUUID` | 16-byte UUIDs of a catalog or member |

---

## 0. Summary for implementers

FileMaker Pro edits the schema on its own copy and uploads the result. The
server computes nothing about tables or fields: no ids, no names, no index
keys, no mod counts. It provides a clock (op82), stores subtrees where the
client puts them (op86 UploadMultiple, op13), removes a whole table on request
(op86 DeleteTable), and runs the per-record follow-up work the client lists in
the field catalog's deferred list (op73). Locks make the edit exclusive.

| user action | FileMaker Pro sends | the server does |
|-------------|---------------------|-----------------|
| open Manage Database | op60 `[08]` type 9; per catalog it shows: op60 `[cat]` type 9, op86 GetKey `[cat, 1, fc]` + NodeExists `[cat]` (or a download) | grant locks; answer the checks |
| first edit that stamps a time | op82 HostTimeStamp | return the host's local date-time key |
| create a table | op60 `[T, 3]` type 9, op15 `[T, 3]` (missing) | grant; answer "missing" |
| OK | op60 `[T, 5]` type 9 (each table with field changes); per changed catalog: op61 `[cat]` 9 -> 10, op86 UploadMultiple into `[cat, 5]`, op13 `[cat, 1]`, op62 `[cat]` type 10 attention 2; after each field catalog op73 `[T]`; for a deleted table first op60 `[T]` type 11 + op86 DeleteTable; then op62 `[08]`, op62 `[T, 5]` type 100 attention `0x201` | upgrade; copy members / delete listed members; replace the index; release and notify; process deferred field work; delete tables |
| Cancel | op62 for every lock it took | release |

The minimum a server needs so FileMaker Pro can create a table with fields
and save it: op60/op62/op65 on any path (write-path.md 4; already in
OpenMaker Server), **op82** (3.1), **op15** (3.2), **op61** (4.2), **op86
UploadMultiple** (3.4), **op13** direct upload (3.3), **op73** answered with
an empty reply (3.6; deferred work is only needed once tables hold records),
and the op86 GetKey/NodeExists checks without access keys (3.5; already in
OpenMaker Server since commit 9bf56ee of the earlier, private history). Deleting a table adds op86
DeleteTable (3.4). Other connected clients learn of the change from the
attention-2 unlock (section 9).

---

## 1. The model (High unless marked)

**Catalogs and members.** Every schema object lives in a catalog: a node
`[cat]` with an index `[cat, 1]` and members `[cat, 5, id]`. FileMaker Pro's
`DBCatalog` / `DBCatalogMember` classes (bin `mac:DBCatalog::*`
0x1911cfa-0x19184a0) handle all of them alike: base tables `[3, 16]`, table
occurrences `[3, 17]`, relationships `[3, fb]`, and each table's fields
`[T, 3]` (also layouts, scripts, value lists and the rest, not covered here).

**Edit copy.** Opening a catalog for editing (`DBCatalog::DoOpen`, bin
0x19136d8) takes a *bundle lock* on `[cat]` (op60, type 9 for the normal
write mode, 10 for an exclusive mode) and makes sure the client's copy is
current (`DBCatalog::OpenDownload` -> `CopyIfNeeded`, bin 0x19129cc,
0x1911ed4): for a hosted file it either asks for the mod count and existence
(op86 GetKey/NodeExists, 3.5) or downloads `[cat]` with op15 (3.2). Edits go
to the client's copy; each changed member is recorded under the client-side
list `[cat, fe]` (`KModified`, never uploaded).

**Commit.** `DBCatalog::Commit` -> `DoCommit` -> `Upload` (bin 0x19179e0,
0x1913ab8, 0x1912dca), per catalog:

1. stamp the catalog's own mod info (`IncrementModCount`, bin 0x190bad0;
   section 8), which needs the host time (op82);
2. upgrade the bundle lock to exclusive (op61 9 -> 10);
3. upload every member named in `[cat, fe]` in one op86 UploadMultiple
   (`DBCatalog::UploadModifiedMembers`, bin 0x1914700, and
   `DBQueryProgramRunner::UploadMultiple`, bin 0x1b3bfe8): members that still
   exist are copied into `[cat, 5]`, deleted ones go in its delete list;
4. upload the whole index `[cat, 1]` with op13 (`DBCatalog::TransferNodes`,
   bin 0x1912048);
5. release the bundle lock with attention code 2 (`kDBAttnCatalog`), which
   notifies the other clients (op62; bin `DBCatalog::Upload` passes
   `2` unless the transfer mode is 3, x86 at 0x1912ead-0x1912eca).

**Mod counts.** Clients detect stale copies by comparing `[cat, 1, fc]`
(section 8). The server does not maintain any of these values.

**Field data follow-up.** A field change that affects stored record data
(delete a field, change its type, turn indexing on or off, change storage or
repetitions) is listed by the client under `[T, 3, 1, fd]`; op73 makes the
server walk the table's records and do that work (section 7).

---

## 2. Ordering

### 2.1 Opening Manage Database (High: om:264-298, replayed as live:s0)

```
op60  [08]            type 9, timeout ffffffff, txn A   (DBSchema::Open; KGlobal)  -> 0000
op60  [3, 16]         type 9, timeout 0,        txn B   (table catalog bundle lock) -> 0000
op86  GetKey [3,16,1,fc] -> [3,1];  NodeExists [3,16] -> [3,2]   (no access keys)
op60  [3, 17]         type 9, timeout 0,        txn C   (table occurrence catalog)
op86  GetKey [3,17,1,fc] -> [3,1];  NodeExists [3,17] -> [3,2]
op82  HostTimeStamp
```

`DBSchema::Open` (bin 0x1b95da8) first checks that the account has full
access or the "manage schema" privilege (else client error 207), then locks
`[08]` with a fresh transaction id; a conflict (8003) becomes client error
303 `kSchemaLocked`, which is what a second user opening Manage Database at
the same time sees. Each catalog the dialog shows is then opened with its own
transaction id (`B`, `C`, ...). FileMaker Pro used txn `0x2b` for `[08]` and
`0x2c`, `0x2d`, `0x2e` for the catalogs (om:264-294).

FileMaker Server's answers (live:s0): every lock `0000`; the checks
`{[3,1]: {00 = 01 02}, [3,2]: {00 = 01 01}}` for `[3, 16]` (mod count 2,
exists) and `{00 = 01 05}`, `{00 = 01 01}` for `[3, 17]`; op82 a timestamp
(3.1).

### 2.2 Creating a table and its fields in the dialog (High: om:276-298)

Defining a new table reserves nothing on the host; FileMaker Pro only looks
up the new table's field catalog:

```
op90  [T, 3]  (repeatedly: om shows 9 identical requests)       -> missing
op60  [T, 3]  type 9, timeout 0, txn D                           -> 0000
op15  [T, 3]                                                     -> missing (3.2)
op82  (again when more time stamps are needed; at most once a minute, 3.1)
```

The table id `T` is chosen by the client: the table catalog's id counter
`[3, 16, 1, 00]` plus one (`80 05` -> `80 06` in `om:`). No request reserves
it; the `[3, 16]` bundle lock (and `[08]`) keep two clients from picking the
same id. Field ids, TO ids and relationship ids are allocated the same way
from their own catalog's `[cat, 1, 00]`. Defining fields sends nothing.

### 2.3 OK (save) - order (High for the order of catalogs: bin
`DBSchema::Commit` 0x1b96594, om:300-306. Med for FileMaker Pro's requests
after a granted op61: derived from bin, sent in this order by the probe and
accepted by FileMaker Server (live:s1, s2a-s2d), not captured from FileMaker
Pro itself, section 12)

```
op60  [T, 5]   type 9, timeout 2, txn 0      for each table whose field catalog is open (GrabTableLock)
-- for a table deleted in the dialog (DBTableCatalog::DoCommitProcessing):
op60  [T]      type 11, timeout ffffffff     ; op86 DeleteTable T ; op62 [T]
-- table catalog [3, 16]:
op82  (if the last one is more than 60 s old)
op61  [3, 16]  9 -> 10, txn B
op86  UploadMultiple  src spot [2,1] = changed tables, dest [3, 16, 5], dellist = deleted tables
op13  [3, 16, 1]  (the whole index)
op62  [3, 16]  type 10, attention 2, txn B
-- relationship catalog [3, fb]   (only if opened in this session): same four steps
-- table occurrence catalog [3, 17]: same four steps
-- each field catalog [T, 3] that was opened:
op61  [T, 3] 9 -> 10, txn D ; op86 UploadMultiple dest [T, 3, 5] ; op13 [T, 3, 1] ; op62 [T, 3] type 10, attention 2
op73  HostUpdateTable T
-- end:
op62  [08]     type 9, txn A
op62  [T, 5]   type 100 (any), attention 0x201, txn 0    (ReleaseTableLocks)
```

FileMaker Pro commits the table catalog, then the relationship catalog, then
the table occurrence catalog, and stops at the first error; it then commits
every open field catalog regardless and finally releases the table locks
(bin `DBSchema::Commit`). In `om:` OpenMaker Server refused the first op61
(`[3, 16]`) with status 3, so FileMaker Pro skipped `[3, fb]` and `[3, 17]`,
still tried the field catalog (`op61 [80 06, 3]`, refused) and released
`[80 06, 5]` with type 100 and attention `0x201` (om:300-306); pressing OK
again repeated the same five requests (om:310-318).

A catalog that was not changed is not uploaded: `UploadModifiedMembers` sends
nothing when `[cat, fe]` is empty, but the index upload and the mod count
stamp happen for every catalog that was opened in write mode and committed
(Med: from code; a no-change OK was not captured).

### 2.3a OK as FileMaker Pro sent it (High: capture s06-omserver-o, rid 142-250)

The first capture of FileMaker Pro saving against a server that granted
op61 (OpenMaker Server, 2026-10-28 22:09; the maintainer created table
"Test Table" `80 06` with four fields, then saved field changes three more
times). It differs from the derived order of 2.3 in one way: **every
catalog went up whole with op13, and no UploadMultiple was sent.**

```
op60  [08] 9, [3,16] 9 + op86 GetKey/NodeExists, [3,17] 9 + checks, op82
op90  [80 06, 3] (x5, missing)  ; op60 [80 06, 3] 9 ; op15 [80 06, 3] (missing)
op60  [80 06, 5] type 9, timeout 2, txn 0
op61  [3, 16] 9 -> 10 ; op13 dest [3, 16] = {01: index, 05: all members} ; op62 [3, 16] 10, attention 2
op61  [3, 17] 9 -> 10 ; op13 dest [3, 17] = {01, 05}                    ; op62 [3, 17] 10, attention 2
op61  [80 06, 3] 9 -> 10 ; op13 dest [80 06, 3] = {01, 05}              ; op62 [80 06, 3] 10, attention 2
op73  [80 06]
op62  [08] type 9, attention 3 ; op62 [80 06, 5] type 100, attention 0x201, address 0
-- then FileMaker Pro creates the new table's layout (no locks held):
op87  [1f, 05, x] +1 for each of seven file-library icons (x = 03..08, 02)
op86  GenerateMemberKey for the layout catalog [4] (3.4)
```

The op13 payloads were complete catalogs (`[3, 16]` 769 bytes, `[3, 17]`
1832, `[80 06, 3]` 902, each `{01, 05}`), so a server that replaces the
destination subtree (3.3) stores exactly the client's catalog. Each op62
carried message address `00000001`, not 0 (4.3). OpenMaker Server refused
GenerateMemberKey, and FileMaker Pro reported "Unknown Error: -1" and did not
create the layout. Later saves of field changes repeated op60 `[80 06, 3]`,
op15, op82, op61, op13 `[80 06, 3]`, op62, op73. What FileMaker Pro sends
after a successful GenerateMemberKey (the layout upload) is not captured yet.

Capture s06-omserver-p (22:21, a field added to that table, OK) shows a save
that also touched a layout: after the `[3, 17]` and `[80 06, 3]` commits as
above, op60 `[4, 5, 3]` type 9 want-mod-count (reply `01 03`), op60 `[4, 1]`
likewise, op61 `[4, 5, 3]` 9 -> 10, op13 dest `[4, 5, 3]` (the whole layout,
20,113 bytes), op62 attention 2, op61 `[4, 1]` 9 -> 10, then **op13 of a
single value**: dest `[4, 1, 00]`, flags `01` (a value), terminal value
`01 04`, no slots (`N` = 0). OpenMaker Server answered that op13 with a
continuation handle, because it took the empty slot list for an unfinished
multi-part upload, and FileMaker Pro repeated the continuation 18,076 times
until it was force-quit. A server must treat an upload with no slots as
complete.

### 2.4 Cancel (High: om:320-328)

```
op62  [3, 16]  type 9, attention 0, txn B
op62  [3, 17]  type 9, attention 0, txn C
op62  [T, 3]   type 9, attention 0, txn D
op62  [08]     type 9, attention 0, txn A
```

followed by FileMaker Pro's usual close-file `op62` type 5, attention
`0x2000`, empty path (write-path.md 4.3) when the user then closed the file.
Nothing was written; the table id it had picked was never stored.

---

## 3. Transactions

### 3.1 op82 HostTimeStamp (`RCHostTimeStamp`) (High)

Request: the 20-byte header only, empty body (om:274, `maxreturn` 131).
Reply: one key pushed with `Push(uchar const*)` - a one-segment key holding a
`DBDateTimeKey` - and nothing else; there is no status field:

```
08  9f aa 9c  80 05 65 87 30  09
^seglen     date           time    ^u8 total length
```

live:s0/r274_op82 = 2026-10-28 15:12:42 (the server's local wall-clock
time; the host was on CDT). The 8 key bytes are the timestamp index key of
query-program.md 7.3: date `0x800000 | year << 10 | month << 6 | day` in
three bytes (`9f aa 9c` = 2026-10-28), then the time as a number key of the
seconds since midnight (`80 05 65 87 30` = digits 5 4 7 6 2 = 54762 s =
15:12:42; each nibble is digit + 1). Whole seconds only (bin
`RCHostTimeStamp::Perform`, linux 0xf33980: `DateTime::Now()` ->
`DBDateTimeKey::Set`). Three calls within two seconds returned the same
value. A server must answer from its own clock in its local time zone.

Client use (bin `mac:RCHostTimeStamp::Call` 0x1a1a93e): FileMaker Pro asks
for the host time when it stamps catalog mod info (8.2) and for
`Get(CurrentHostTimeStamp)`. It caches the result: within 60 seconds of the
last successful op82 it adds its own elapsed time to the cached host time
instead of asking; when op82 fails it falls back to its local clock plus the
last known offset, so a refusal is not fatal (om:274, 298 and 312 were sent
again because each one failed).

### 3.2 op15 DownloadFromHost (`RCDownloadFromHost`) (High)

The op90 download without the lock. Request (om:296, live:s0):

```
[ u32 N = 0 ][ u64 perform 0 ][ u64 cursor 0 ][ i16 depth -1 ][ u8 copy-key length 0 ][ key path ][ u8 path length ]
00000000 0000000000000000 0000000000000000 ffff 00 | 02 80 06 01 03 | 05
```

`maxreturn` 0x10012a. The resume block and paging are network-protocol.md
5.6.6c. Reply: as op90 (5.6.6) minus the trailing `u16 lockError`:

```
[ slots, N ][ u32 N ][ u64 perform ][ u64 cursor ][ i16 depth ][ u8 copy-key length ][ value, V ][ i16 V ][ u8 node state ]
```

| case | reply after the slots (live:s0) |
|------|---------------------------------|
| missing `[80 06, 3]` | `00000000` 16 zero bytes `ffff 00 0000 00` (26 bytes, node state 0) |
| node `[3, 16]` | 642 slot bytes = exactly the op90 stream, then `00000282` 16 zero bytes `0000 00 0000 02` |
| terminal `[80 02, 3, 5, 1, 10]` | `00000000` 16 zero bytes `ffff 00`, the 10 value bytes, `000a 01` |

The server takes no lock (bin linux `RCDownloadFromHost::PerformOperation`
0x110d8e0 only sets depth -1 for a non-node source; none of its methods
call the lock manager), which is why FileMaker Pro uses op15 for catalogs it
already holds a bundle lock on: an op90 would wait on that lock (4.5).
FileMaker Pro calls op15 from `DBCatalog::CopyIfNeeded` (bin 0x1911ed4) for
`[cat]` or `[cat, 1]` and from `DBCatalogMember::CopyIfNeeded`/`Download`
for single members.

### 3.3 op13 UploadToHost (`RCUploadToHost`) (High: live:s1, s2a-s2d)

The op14 framing (write-path.md 3.3) with bit 7 of the flags clear: the
payload goes straight into the hosted file at the destination path.

```
[ payload, N ][ u32 N ][ u64 perform 0 ][ u64 cursor 0 ][ i16 depth -1 ][ u8 copy-key length 0 ]
[ terminal value, L ][ i16 L ][ u8 flags ][ dest key path ][ u8 dest path length ]
```

| field | value |
|-------|-------|
| payload | one bracketed slot list (`80 ... 40`, minimal chunk codes, key order; file-format.md 3.6), relative to the destination |
| terminal value / `L` | bytes stored when the destination is a value, not a node; `L` 0 and no bytes for a node |
| flags | bit 7 = 1: upload to the connection's temporary area (op14); 0: into the file. Low 7 bits = the destination's node state: `02` node, `01` value |
| dest key path | segments `[u8 len][bytes]`, then the `u8` total length |

live:s1/a_idx16 (the table catalog index, 340 bytes): body ends
`... 40 | 00000133 | 0000000000000000 0000000000000000 | ffff | 00 | 0000 | 02 | 01 03 01 10 01 01 | 06`.
Reply: the 23-byte empty page `00000000` + 16 zero bytes + `ffff 00`
(depth -1: bin linux `RCUploadToHost::PerformOperation` 0x1106f00 always
sets it). Large payloads are split as in network-protocol.md 5.6.6c.

Server behaviour (bin linux `RCUploadToHost::PerformPopAddition` 0x1106d20,
`PerformAfterDownload` 0x11079c0):

1. Unpack the payload into a temporary position owned by the transfer
   (bit 7 clear) - two-stage, so a half-received upload never reaches the
   file.
2. When the upload is complete, if the destination is one of five
   *protected indexes* (the theme, script, font-list, value-list and
   custom-menu-set catalog indexes; `IsProtectedIndex` 0x1106f10), require
   the uploaded data to contain `fc` (`KModCount`) and `KSerNum`; if either is
   missing the copy is skipped silently (debug log "AllowCopyToMainFile()
   returning false") and the reply is still the empty page.
3. Otherwise create the destination path if missing and copy the uploaded
   subtree there (`HBPosition::PutAnchor` + `CopyFrom`), then forward the
   change (`DBNotifications::ForwardSchemaChanges`, section 9).

**The copy replaces the destination subtree** (High: live:s2a). Uploading
the baseline `[3, 16, 1]` over an index that also held table `80 06` removed
`80 06`'s name, ordering and counter leaves: `[3]` was byte-identical to the
first baseline afterwards. Keys absent from the payload disappear, so the
client always uploads the complete index.

FileMaker Pro uses op13 for every catalog index `[cat, 1]` at commit
(`DBCatalog::TransferNodes`, bin 0x1912048), and for single catalog nodes in
a third transfer mode (`[cat, 1, KSerNum]` and `[cat, 1, fc]`, used by file
recovery, not by Manage Database). It never uses op13 for record data
(write-path.md 3.3).

### 3.4 op86 instructions for schema (High: bin, live:s1)

**UploadMultiple, opcode 40 (`0x28`).** Copies members into the file and
deletes members (bin linux `DBQueryProgram::UploadMultiple` 0x10cf190,
client `mac:DBQueryProgramRunner::UploadMultiple` 0x1b3bfe8).

| leaf of `[1, i]` | value |
|------------------|-------|
| `00` | `28` |
| `01` KSrc | a program spot, `01 02 01 01` = `[2, 1]`, holding one child per member to store (member id -> member subtree) |
| `02` KDest | a path **in the file**, raw segments: `01 03 01 10 01 05` = `[3, 16, 5]` |
| `03` KDelList (optional) | a program spot, `01 02 01 02` = `[2, 2]`, holding one value-less leaf per member id to delete |
| `04` KFileId | u32 file id |
| `07` | absent: FileMaker Pro sends no access key for UploadMultiple (`CreateNextInstruction` without `PutGlobalAccessKey`); FileMaker Server runs it (query-program.md 2.4) |

live:s1/a_um16 (whole body):

```
80 20 01 80 20 01 80 01 00 28 03 01 01020101 04 02 010301100105 03 04 00000165 40 40
   20 02 80 20 01 80 28 8006 80 06 10 15 <"OpenMaker schema test"> 06 d8 10 <uuid> 06 d9 10 <uuid>
            03 fb 00000001 0e fc01 09 <"OpenMaker"> 0e fc02 05 <"admin"> 0e fc03 09 <089faa9c8005662810> 40 40 40 40
00000095 0000000000000000 0000000000000000 ffff 00
```

= `{1: {1: {00: 28, 01: [2,1], 02: [3,16,5], 04: 0x165}}, 2: {1: {80 06: {...}}}}`.
Reply: `80 40` and the op86 trailer (`00000002`, 16 zero bytes, `0000 00`).

Server: resolve the file from `04` (missing file: error 100); for each child
`k` of the source spot, replace `[dest, k]` with a deep copy of it
(`HBPosition::CopyFrom`), then for each leaf `k` of the delete-list spot,
remove `[dest, k]`; after each change call `ForwardSchemaChanges(dest)`.
Missing parents of `dest` are created (live:s1/a_umf wrote
`[80 06, 3, 5, 01..05]` into a table that had no `[80 06]` node; s1d
NodeExists confirmed). `01` and `02` must be non-empty (the instruction is
skipped otherwise), so a top-level key cannot be deleted this way. No lock
is taken or checked (FileMaker Pro holds the exclusive bundle lock). The
client builds the program from `[cat, fe]`: a listed member present in its
copy goes to the source spot, one no longer present goes to the delete list.

**GenerateMemberKey, opcode 43 (`0x2b`).** (High: live:s4, s4b; FileMaker
Pro's request: capture s06-omserver-o rid 208.) Issues the next member id of a
catalog to a client that is about to add a member without holding the
catalog's locks; FileMaker Pro uses it to create a layout.

| leaf of `[1, i]` | value |
|------------------|-------|
| `00` | `2b` |
| `04` | file id |
| `0a` KOperand1 | the catalog **in the file**, raw segments: `01 04` = `[4]`, `01 03 01 10` = `[3, 16]` |
| `0b` KOperand2 | present, no value (meaning not established) |
| `0d` KResult1 | a program spot, `01 03 01 01` = `[3, 1]`: receives `{00 = <key string of the new member id>}` |
| `0e` KResult2 | a program spot, `01 03 01 02` = `[3, 2]`: receives `{00 = <key string of the catalog's next mod count>}` |
| `07` | absent: FileMaker Pro sends no access key |

FileMaker Server's answers (test project; `[4, 1, 00]` = `01 04`,
`[4, 1, fc]` = `01 0d`; `[3, 16, 1, 00]` = `02 80 05`, `[3, 16, 1, fc]` =
`01 02`):

| call | `[3, 1]` | `[3, 2]` |
|------|----------|----------|
| `[4]` (live:s4/g_gen1) | `01 05` | `01 0e` |
| `[4]` again (g_gen2) | `01 06` | `01 0f` |
| `[3, 16]` (live:s4b/h_gen16) | `02 80 06` | `01 03` |
| `[4]` again (h_gen4) | `01 07` | `01 10` |

So each call **issues** the catalog's next member id and modification
count: it adds one to the last issued id `[cat, 1, 00]` and to the mod count
`[cat, 1, fc]`, **writes both back into the file**, and returns them. op15
of `[4, 1]` before and after the two `[4]` calls (live:s4/g_41_before,
g_41_after): `00` went from `01 04` to `01 06` and `fc` from `01 0d` to
`01 0f`, nothing else changed; `[3, 16, 1]` likewise went from `02 80 05` /
`01 02` to `02 80 06` / `01 03` (s4b/h_16_1_*); after the third `[4]` call
`[4, 1, fc]` read `01 10` (live:s5/m_41fc). So two clients never receive
the same id, and the client later uploads the catalog's index holding the
id and count it was given. No lock is taken or checked. The serial keys
count as serial keys: `02 80 05` plus one is `02 80 06`.

live:s4/g_gen1 request program and reply:

```
80 20 01 80 20 01 80 01 00 2b 03 04 00000168 02 0a 0104 00 0b 03 0d 01030101 03 0e 01030102 40 40 40
80 20 01 80 02 00 01 05 40 20 02 80 02 00 01 0e 40 40      = {[3,1]: {00 = 01 05}, [3,2]: {00 = 01 0e}}
```

**DeleteTable, opcode 50 (`0x32`); TruncateTable 51 (`0x33`).** (bin
`mac:DBQueryProgramRunner::DeleteTable` 0x1b3f118, linux
`DBQueryProgram::DeleteTable` 0x10d1ca0, `InternalDeleteTable` 0x10d3df0)

| leaf | value |
|------|-------|
| `00` | `32` (delete) or `33` (truncate: delete all records, keep the table) |
| `01` KSrc | the table key, raw segment: `02 80 06` |
| `04` | file id |
| `07` | the access key (2.4 of query-program.md; FileMaker Pro sends it) |
| `0a` KOperand1 | u32 transaction id of the client's lock on `[T]` |

Delete: the server requires a table id below `d0 00 00` (the first key byte
in `80`..`cf`) and an account with full access, deletes the container files
the table references, then removes the whole `[T]` subtree (records,
indexes, the field catalog `[T, 3]`, `[T, 00]`, `[T, 0d]`) in one step. It
takes no lock itself; FileMaker Pro holds `[T]` type 11 around it (4.4).
It does not touch `[3, 16]` or `[3, 17]`: the client removes the table and
its occurrences from those catalogs with UploadMultiple delete lists in the
same save. Truncate takes the table's record locks for the given
transaction instead.

live:s2a/a_deltable (table `80 06`, instruction 1, access key elided):

```
80 20 01 80 20 01 80 01 00 32 06 01 03 028006 03 04 00000165 06 07 10 <16-byte key> 03 0a 00000203 40 40 40
00000032 0000000000000000 0000000000000000 ffff 00
```

Reply `80 40` and the op86 trailer. Afterwards op90 `[80 06]` answered node
state 0 (missing): records, the field catalog and everything else under the
table were gone (live:s2a/v_8006, s2d/d_after_deltable for `80 07`, which had
a record and two field indexes). `[3, 16]` and `[3, 17]` were untouched by
it.

### 3.5 op86 GetKey / NodeExists catalog checks (High: om:268, live:s0)

```
[1, 1] = {00: 2a, 01: 01 03 01 10 01 01 01 fc  ([3,16,1,fc]), 02: 01 03 01 01 ([3,1]), 04: file id}
[1, 2] = {00: 29, 01: 01 03 01 10           ([3,16]),       02: 01 03 01 02 ([3,2]), 04: file id}
```

No access keys. FileMaker Server: `{[3,1]: {00 = 01 02}, [3,2]: {00 = 01 01}}`
(live:s0/r268_op86_check16): the stored mod count key `01 02` and "exists".
FileMaker Pro compares the mod count with its cached copy and downloads the
catalog again (op15) when it differs (`DBFieldCatalog::IsCurrent`,
`DBCatalog::CopyIfNeeded`). Semantics: query-program.md 8.5.

### 3.6 op73 HostUpdateTable (`RCUpdateTable`) (High for the layout; behaviour 7.2)

Request body: the table key, `02 80 06 03` (`Push(uchar const*)`). Reply:
empty (no fields at all). Sent after every field catalog commit
(`DBFieldCatalog::Commit`, bin 0x19ecddc, -> `RCUpdateTable::Call`
0x19ecec8) unless the catalog is in read-only/recovery mode. The server runs
`TableUpdater::DoUpdate` on the table's deferred list `[T, 3, 1, fd]`
(section 7). FileMaker Pro ignores the reply and the outcome.

### 3.7 Other ops (Low; not sent by Manage Database)

- op85 BuildIndex (`RCBuildIndex`): `DBField::BuildIndexIfMissing` asks the
  server to build one field's index when a find or relationship needs an
  index that does not exist and automatic indexing is on. Manage Database
  turns indexing on through the deferred list instead (7.2).
- op89 CompareModCounts (network-protocol.md 5.6.11): FileMaker Pro sends
  its cached `[cat, 1]` and member `fc` values (and record counts and
  library MD5s) when it reopens a file it has cached and when it positions
  on cached records; the host answers which ones changed. Not part of the
  dialog.
- op87 AdjustReferenceCount: container library reference counts.

---

## 4. Locking

Locks, lock users and the lock types are those of write-path.md 4.1
(DWARF `DBLockType`: 9 write, 10 exclusive, 11 exclusive-reject, 100 any).
Schema editing uses these lock paths:

| path | type | timeout | txn | taken by | released by |
|------|------|---------|-----|----------|-------------|
| `[08]` (`KGlobal`) | 9 | `ffffffff` | its own | opening Manage Database (`DBSchema::Open`) | op62 type 9 after the save or on Cancel |
| `[cat]` | 9 (10 for an exclusive open) | 0 | one per catalog | opening a catalog for editing (`DBCatalog::DoOpen`) | op62 type 10, attention 2 after its commit; type 9, attention 0 on Cancel |
| `[cat]` upgrade | 9 -> 10 (op61) | `ffffffff` | the catalog's | committing that catalog | with the bundle lock |
| `[T, 5]` | 9 | 2 | 0 | saving, for every table whose field catalog is open (`DBFieldCatalog::GrabTableLock`, bin 0x19ec938) | op62 type 100, attention `0x201` (`ReleaseTableLock`, 0x19ecc94) |
| `[T]` | 11 | `ffffffff` | its own | deleting a table (`DBTableCatalog::DoCommitProcessing`, 0x1be9c20) | op62 after DeleteTable |

(High for paths, types and timeouts: om:264-306 and bin; Med for the `[T]`
type 11 lock, from bin only.)

**What they exclude.** `[08]` makes Manage Database single-user per file: a
second client's op60 `[08]` gets 8003 and FileMaker Pro reports
`kSchemaLocked` (303) with the holder's name (bin `DBSchema::Open`). The
`[T, 5]` lock conflicts with any record lock in that table (record locks take
intent locks on `[T, 5]`), so a save that changes fields fails with 303 while
another user is editing a record of the table (`GrabTableLock` maps 8003 to
303; it can retry interactively). The exclusive catalog lock (10) keeps every
other lock user from reading or locking the catalog while it is replaced.

### 4.1 op60 on schema paths (High: om:264, live:s0)

The write-path.md 4.2 layout with "want mod count" 0 and lock argument 0:

```
000000016dab37c8 ffffffff 00 | 01 08 02 | 00000000 | 00000001 | 0000002b | 0009     ([08], om:264)
000000016dab3468 00000000 00 | 01 03 01 10 04 | 00000000 | 00000001 | 0000002c | 0009     ([3, 16], om:266)
000000016dab3a38 00000002 00 | 02 80 06 01 05 05 | 00000000 | 00000001 | 00000000 | 0009  ([80 06, 5], om:300)
```

Reply `0000` (granted) or the 8003 form with user list and self-conflict
byte. FileMaker Server never blocks an op60, whatever the timeout
(write-path.md 4.2); in its waiting mode `DBCatalog::DoOpen` retries by
itself after a short sleep (`Thread::Sleep(5)`) for as long as it gets 8003.

**Mod count with the lock (High: live:s5).** Manage Database locks with
"want mod count" 0, but FileMaker Pro sets it when it works on layouts
(capture s06-omserver-p rid 68, 70: op60 `[4, 5, 3]` and `[4, 1]`, argument
= the file id). FileMaker Server then answers with the `fc` child of the locked node,
whatever the node is, and `01 00` when the node has none:

| locked path | reply | the node's `fc` |
|-------------|-------|-----------------|
| `[4, 1]` (layout catalog index) | `0110 02 0000` | `01 10` |
| `[4, 5, 3]` (a layout) | `0103 02 0000` | `01 03` |
| `[3, 16]` (table catalog) | `0100 02 0000` | none |
| `[4]` (layout catalog) | `0100 02 0000` | none |

The record case of write-path.md 4.2 (`[T, 05, R, fc]`, `01 00` without
one) is this rule applied to a record.

### 4.2 op61 Upgrade (`DBRemoteUpgrade`) (High: om:302, live:s0)

Request body, push order (bin `mac:DBRemoteUpgrade::DoUpgrade` 0x1ad894e):

| field | type | value |
|-------|------|-------|
| waiter id | u64 | opaque, as op60 |
| timeout | u32 | `ffffffff` |
| lock path | key | the catalog, `01 03 01 10 04` |
| host file id | i32 | file id |
| txn | u32 | the transaction that holds the lock |
| new type | u16 | `000a` |
| current type | u16 | `0009` |

om:302 `000000016dab3000 ffffffff 0103011004 00000001 0000002c 000a 0009`;
om:304 the same for `[80 06, 3]`, txn `2e`. Reply: `u16` status, `0000`
granted (live:s0/r302, r304). A refusal is answered like op60 (Med: the
server's `DoUpgrade` builds the same conflict list; not provoked live).
See 4.5 for what the exclusive lock then blocks.

### 4.3 op62 releases (High: om:306, om:320-328, live:s0)

The write-path.md 4.3 layout. Values used for schema:

```
00000000 | 00000201 | 02 80 06 01 05 05 | 00000001 | 00000000 | 0064    table lock: type 100 = any, attention 0x201 (om:306)
00000000 | 00000000 | 01 03 01 10 04    | 00000001 | 0000002c | 0009    catalog on Cancel (om:320)
00000000 | 00000002 | 01 03 01 10 04    | 00000165 | 00000301 | 000a    catalog after its commit (live:s2b/b16_unl)
```

The first field is the message address of the releasing session. FileMaker
Pro sent 0 in every op62 captured; with 0 FileMaker Server also notified the
releasing connection itself when it had registered with op63 (live:s2a-s2d:
the notification came back in the op62 reply's own back channel). Whether
FileMaker Pro fills in its own address for a catalog release, which would
exclude it, is not established (Med).

Reply: the 20-byte request header echoed with idle time 0. The attention
mask is what the server sends to the clients registered on the file
(section 9); type 100 (`kDBAnyLockType`) releases whatever type the txn
holds on the path.

### 4.4 Deleting a table (Med: bin; the probe's sequence worked live, s2a, s2d)

Before DeleteTable FileMaker Pro locks `[T]` with type 11 and an infinite
timeout under a fresh transaction, runs DeleteTable with that transaction id
in `0a`, unlocks `[T]`, raises a local delete notification (`0x401`) for the
table's record cache and drops the table's dependency data.

### 4.5 FileMaker Server blocks downloads behind exclusive locks (High: live:s1)

op90 DownloadWithLock takes a type 6 (read) lock on its target with an
infinite timeout (bin linux `RCDownloadWithLock::PerformOperation`
0x110ea30, `DBLockMgr::Lock(..., 6, ..., 0xffffffff)`), and unlike op60 that
lock **waits**. A type 10 lock on the target or a descendant held by any
other lock user, including another transaction of the same connection,
blocks it. live:s1 did exactly that: after op61 `[80 06, 3]` -> 10 (txn
`0x103`), an op90 of `[80 06]` on the same connection never returned. The
stuck request also kept the connection's locks alive after the client went
away: `fmsadmin disconnect` and FileMaker Server's own "forced to close
connection" removed the session, but its locks on `[08]`, `[80 06, 3]` and
`[80 06, 5]` stayed (op60 probes from a new session got 8003 naming
"openmaker (admin)", live:s1c) until the file was closed.

FileMaker Pro never gets into this: it reads catalogs it holds with op15,
which takes no lock (3.2). A server that implements op90 waiting should
not wait forever, and must release a connection's locks when the
connection closes even if a request is still pending; OpenMaker Server's
non-blocking lock manager can answer 8003 (or serve the stale data) instead.

---

## 5. The catalogs in the file

All text values are stored XOR `0x5a` (SCSU; file-format.md 3.5). "Key
string" below means a key with its segment length byte, as stored in values:
table `80 02` = `02 80 02`, field `09` = `01 09`, TO `d0 00 02` =
`03 d0 00 02`. Evidence: test project and the seven English samples
(`tpl:`), FileMaker Pro's own catalog writes in bin, and the live tables of
section 7.

### 5.1 Common catalog index `[cat, 1]` (High)

| key | value | meaning |
|-----|-------|---------|
| `00` | key string | last issued member id (`02 80 05`, `03 d0 00 05`, `01 0d`, `01 03`). The client allocates `00 + 1` and writes it back with the index; ids are not reused (Med: FileMaker never decrements it) |
| `01, <name key>` | key string | name list: one leaf per member, key = the name's collation key (5.6), value = the member id |
| `03, <u32 position>` | key string | display order: position 1, 2, ... -> member id |
| `04` | u32 | member count |
| `d8` | 16 bytes | the catalog's UUID |
| `fc`, `fc01`-`fc03` | | mod count and mod info (section 8) |
| `fd` | node | field catalogs: deferred field work (7.2) |

Relationships have no names, so `[3, fb, 1]` has only `00`, `04`, `d8` and
the mod info.

### 5.2 Base tables `[3, 16]` (High)

`[3, 16, 5, T]` (T = `80 02`...):

| key | value |
|-----|-------|
| `10` | table name |
| `d8`, `d9` | UUID and source UUID (16 bytes each; independent random values) |
| `fb` | u32, `00000001` for a new table (Contacts in test project has 2; meaning not established, Low) |
| `fc01`-`fc03` | who/when; `fc` only after the table was modified after creation |

The table id is also the table's top-level data key `[T]` (section 6).

### 5.3 Table occurrences `[3, 17]` (High for the layout, Med/Low for fields marked)

`[3, 17, 5, O]` (O = `d0 00 02`...):

| key | value |
|-----|-------|
| `02` | definition, below |
| `10` | occurrence name |
| `d8`, `d9` | UUIDs |
| `fb, <other O>` | key string of the relationship that joins this occurrence to `<other O>` (one leaf per directly related occurrence; absent when it has none) |
| `fc`, `fc01`-`fc03` | mod info |

The definition `02` (35 bytes for a local table):

| bytes | meaning |
|-------|---------|
| `02 01 00` | fixed in every sample (format version and data source "this file", Med) |
| `03 02 80 02` | the base table as a key string preceded by its length (`03`) |
| 4 x f32 BE | relationship graph box: x, y, width, height (`20, 20, 131, 226` for Contacts), in points from the graph's top left corner; the height is the box's with every field shown |
| u32 | `height` less the title bar and key-field area, so the field list's height: 44 less with one key field (15 occurrences), 56 less with two (Assignments in Assets and Tasks), 46 less once (Action Items in Meetings) (Med, 18 occurrences of the six shipped starter solutions; what FileMaker uses it for is not known) |
| u32 | display: `0` every field, `1` the key fields only (Med, below); the shipped starter solutions store 0 for their first occurrence and 1 for the rest, which an earlier reading took for creation order |
| 4 bytes | box colour RGBA, `78 78 78 ff` (`77 77 77 ff` in the FMS sample) |

An occurrence of another file's table has a longer data-source key string
in front (file-metadata.md 1.1); the 28 bytes after the two key strings
are the same.

**How FileMaker Pro 26 draws it** [O: FileMaker Pro 26.0.3 on a copy of
the Contacts starter solution, Manage Database > Relationships at 100%,
measured on a 2x screen capture, 2026-11-11]:

- Each box sits at its stored x and y from the graph's top left corner
  and is its stored width wide. Contacts (display 0) is its stored 226
  points tall and lists every field; Addresses, Phone Numbers and Email
  Addresses (display 1) are 38 points tall, title bar and key field only,
  though they store heights of 118 and 154.
- A box, from the top: an 18-point title bar (a light gradient in its top
  6 points), a sunken white key-field area of 12 points per key field
  plus 1, a 5-point band, the sunken white field list down to 5 points
  above the bottom, and a 5-point bottom frame darkening downward. The
  white areas are inset 4 points at the sides. The default colour
  `78 78 78` is drawn as lighter tints of it (the face is grey 194, the
  title highlight 237, the edges 150 to 209) and the bottom edge darker
  (109).
- The title bar holds a small arrow at its left (go to the table's
  layout), the occurrence name at 10.5 points and, 13 points from the
  right, an 8-point raised white button whose glyph shows the display:
  a rectangle divided across its middle for every field, a rectangle with
  a heavy top for key fields only.
- The key-field area lists the fields the occurrence's relationships match
  on, upright, at 13 points from the left, with a small ring 8.6 points
  in from each side a relationship line leaves by. The field list lists
  every field of the base table in creation order, the key fields in
  italic, Photo (a container) in grey; text is clipped at the box.
- A relationship line leaves the key field's row at the box side facing
  the other box, runs 14 points out horizontally, then straight to the
  other box's 14-point stub. Its operator is drawn in a 16 x 12-point
  framed box at the middle of the stretch between the stubs, `=` as two
  bars. The end at Contacts::PrimaryKey (unique-value validation) is a
  plain line; each ForeignKey end (no unique validation) is a crow's foot
  about 4.5 points long and 8.5 points wide at the box.

Index extras in `[3, 17, 1]`:

- `08, <from O>, <to O>` = key string of the first relationship on the
  path from `<from O>` to `<to O>`, for every pair of connected occurrences
  (a next-hop routing table; test project: `[08, d00003, d00004] = 01 01`,
  Addresses reaches Phone Numbers through relationship 1 to Contacts).
- `0e, <table>, ...` = per base table, the fields that depend on other
  occurrences through relationships, with the paths (`00, <field>, <O>,
  <O2>`), dependency kinds (`ff`, `ff00`, `ff02`, `ff03`) and `fffc` (fields
  with validation such as Not empty, 5.8, 5.9). Client-maintained calculation dependency
  data (`DBDepend`); the server never reads it (Low for the sub-structure).

### Field-to-field dragging in the relationships graph

Claris Help documents creating a relationship by dragging from a match
field in one occurrence to a match field in another, and repeating the
gesture for additional match fields. The Add Relationship dialog is an
alternative; double-clicking the relationship edits its criteria and side
options ([Creating and changing relationships](https://help.claris.com/en/pro-help/content/creating-relationships.html)).
This is documented behavior, not a fresh native measurement of the drag.

The schema transaction and stored match pairs remain those in 5.4 and 5.8.
OpenMaker maps a new dragged pair to equality (consistent with Edit
Relationship's initial operator); exact drag modifiers and intermediate
native save timing remain unmeasured. A relationship's existing side
options and sort orders are retained when another match pair is added.
The documented same-occurrence gesture creates a new occurrence; it is
separate from linking two occurrences that already exist.

### Duplicate in the relationships graph

Claris Help [Duplicating tables in the relationships graph](https://help.claris.com/archive/help/13/fmp/en/html/relational.11.16.html)
(FileMaker Pro 13; the current [Working with the relationships graph](https://help.claris.com/en/pro-help/content/working-relationships-graph.html)
names the button "Duplicate selected tables (and relationships) and
selected notes"):
- it duplicates the selected tables, groups of tables, "and any direct
  relationships among them";
- "Any options you have set for the relationship (for example, a cascading
  delete) are also duplicated";
- "Duplication preserves the relative positions between the original
  tables. By default the duplicated tables are placed below and to the
  right of the original tables";
- a duplicate is renamed in Specify Table, and every occurrence needs a
  unique name.

Unknown (not measured): the default name of a duplicate, how far below and
to the right it goes, and what is selected afterwards.

### Arrange tools in the relationships graph

Claris Help ([Working with the relationships graph](https://help.claris.com/archive/help/12/fmp/en/html/relational.11.11.html),
[Resizing tables in the relationships graph](https://help.claris.com/archive/help/13/fmp/en/html/relational.11.18.html)):
- **Align:** one tool aligns "the left edges, centers, or right edges of
  the selected objects", the other "the top edges, centers, or bottom
  edges".
- **Distribute:** spaces the selected objects "horizontally or
  vertically".
- **Resize:** makes the selected tables the smallest or largest width,
  height, or both. For example, smallest width makes them all as wide as
  the narrowest; "Expand any collapsed tables first if you're going to
  resize the height".
- **Inferred from FileMaker's layout tools:** objects align to the one
  farthest out, and distributing divides the space between the two
  farthest apart evenly
  ([Aligning or distributing objects](https://help.claris.com/archive/help/12/fmp/en/html/edit_layout.10.50.html)).
  Their graph equivalents are not measured.
- **Unknown:** what "centers" align to.

### 5.4 Relationships `[3, fb]` (High for the layout, Med for options)

`[3, fb, 5, R]` (R = `01`...):

| key | value |
|-----|-------|
| `02` | `[u8 4][key string of the left O]` `[u8 4][key string of the right O]`, the left-to-right and right-to-left sort specs (`DBSortSpec`, 4 bytes `00000000` each when empty), then the u32 options: test project `00000000 00000000 00000010` = `kCreateRelatedLeftToRight`. Corrected and complete in [file-metadata.md](file-metadata.md) 4.1 (High, 373 relationships) |
| `03, <n>` | join predicate n: `[u8 operator][field key string, left][field key string, right]`; `00 02 01 09 02 01 0a` = left field `09` `=` right field `0a`. Operators 0 `=`, 1 `≠`, 2 `<`, 3 `≤`, 4 `>`, 5 `≥`, 6 `×`, read "left op right" ([file-metadata.md](file-metadata.md) 4.2, executed) |
| `d8`, `d9`, `fc01`-`fc03` | as for every member |

A relationship change also changes the TO members' `fb` lists and the
routing table `[3, 17, 1, 08]` (client-computed), so FileMaker Pro commits
`[3, fb]` and `[3, 17]` together (2.3).

### 5.5 Fields `[T, 3]` (High for layout; option bits named from DWARF)

Index `[T, 3, 1]`: the common keys (5.1) plus

| key | value |
|-----|-------|
| `06, <kind><type << 2><name key>` | a second name list ordered by field type: one byte field kind, one byte data type shifted left by 2 (`01 04` normal text, `01 18` normal container, `01 14` normal timestamp, `02 04` calculation with text result), the name's collation key, then `00 00` (two zero bytes, not three as in `01`) -> field key string |
| `0e, fff0, <F>, <trigger>` = type | the direct triggers of field F: input fields `G` and the events `ff00` (creation), `ff01` ("when empty"), `ff02` (modification), each with a dependency type byte (5.10) |
| `fd, <F>` | u32 deferred work for field F (7.2) |

Member `[T, 3, 5, F]`:

| key | value |
|-----|-------|
| `02` | the 26-byte field definition (below) |
| `10` | field name |
| `03` | comment (text) |
| `05` | calculation: `04` = context TO key string, `05` = the formatted stream, `06` = the Polish stream (calc-encoding.md 1). Used by calculation fields (kind 2, 5.10) and auto-enter calculations |
| `07` | value list of "Member of value list" validation (5.13), and the furigana field and translation (5.9, "Furigana") |
| `08` | lookup definition (auto-enter lookup; below) |
| `0b` | a lookup's constant for "if no exact match, use" (5.13) |
| `09` | summary definition (kind 3) |
| `0c` | next serial number, text (`"1"` for a new serial field; op72 advances it, write-path.md 3.2) |
| `0d` | auto-enter constant data, text |
| `18`, `19`, `20`, `21`, `22` | custom message as text, validation calculation (as `05`), message calculation (as `05`), the range's lower and upper bounds as text (`KMessage`, `KValidateFormula`, `KValidateMessageFormula`, `KMinValue`, `KMaxValue`; 5.13) |
| `29`, `30`, `31` | advanced flags (`u32`), annotation (text) and display-names formula (as `05`); file-metadata.md 1.2 |
| `da` | tags, text (`#_FMI_0` on the default fields FileMaker creates) |
| `fb` | the field's data version, a serial key string; `01 01` on every new calculation and summary field (5.10) |
| `d8`, `d9`, `fc`, `fc01`-`fc03` | as for every member; `fc` only after a later modification |

**The 26-byte field definition `02`** (DWARF `DBField::FieldDefinition`,
`#pragma pack(1)`, integers big-endian):

| off | size | field | values |
|-----|------|-------|--------|
| 0 | 1 | `mFieldType` | 1 normal, 2 calculation, 3 summary |
| 1 | 1 | `mDataType` | 1 text, 2 number, 3 date, 4 time, 5 timestamp, 6 container |
| 2 | 1 | `mAccessLevel` | 0 (1 seen on one stored calculation in a sample; Low) |
| 3 | 1 | `mEntryVariable` | auto-enter creation/modification value (`EntryVariable`): 0 creation date, 1 creation time, 2 creation timestamp, 3 creator name, 4 creator account, 5 modification date, 6 modification time, 7 modification timestamp, 8 modifier name, 9 modifier account; meaningful with option bit 1 |
| 4 | 4 | `mIndexLang` | index/sort language; `0x15` (21) = English in every template, 0 in the older FMS sample |
| 8 | 4 | `mOptionSet` | bit mask, below |
| 12 | 4 | `mValidationSet` | bit mask, below |
| 16 | 4 | `mSerialIncrement` | auto-enter serial increment, `1` |
| 20 | 4 | `mByteLimit` | maximum number of characters for validation, 0 = none |
| 24 | 2 | `mMaxReps` | number of repetitions, 1 |

`mOptionSet` bits (DWARF enum `Options` in `DBField`):

| bit | name | bit | name |
|-----|------|-----|------|
| `0x1` | auto-enter creation/modification value (`mEntryVariable`) | `0x10000` | global storage |
| `0x2` | auto-enter serial number, generated on creation | `0x20000` | unstored calculation (`kNoStorage`) |
| `0x4` | auto-enter constant data (`0d`) | `0x40000` | computed on the client |
| `0x8` | auto-enter calculation (`05`) | `0x80000` | has pure dependency |
| `0x10` | auto-enter from previous record | `0x100000` | primary key |
| `0x20` | always evaluate calculation | `0x200000` | has no dependents |
| `0x40` | furigana | `0x400000` | no style runs |
| `0x80` | `kOverwriteExisting` (set on every `Get(UUID)` primary key; UI meaning Low) | `0x800000` | single-value joins |
| `0x100` | prohibit modification during data entry | `0x8000000` | relaxed word index |
| `0x200` | auto-enter serial number generated on commit, set instead of `0x2` (5.9, E41) | `0x10000000` | relaxed value index |
| `0x400` | auto-enter lookup (`08`) | `0x20000000` | word index (indexing "All" adds it) |
| `0x800` | unmodifiable | `0x40000000` | prevent index |
| `0x1000`, `0x2000` | container stored externally / open storage | `0x80000000` | value index (indexing "Minimal" and "All") |
| `0x4000`, `0x8000` | `kNeedLibraryUpdate`, `kSkipLibraryUpdate` (container library; Duplicate clears both, 5.14) | `0x1000000`, `0x2000000`, `0x4000000` | `kSupplementalField`, `kNoEquiFindSupport`, `kSecureLessFolder` |

`mValidationSet` bits (DWARF `ValidationFlags`): `0x1` by calculation (`19`),
`0x2` calculation always, `0x4` strict (user cannot override), `0x8` not
empty, `0x10` unique, `0x20` existing, `0x40` in range, `0x80` custom message
(`18`), `0x100` member of value list, `0x200` maximum characters
(`mByteLimit`), `0x400` validate always (not only on data entry), `0x800`
one value, `0x1000` strict data type number, `0x2000` four-digit year,
`0x4000` time of day, `0x8000` message by calculation (`20`).

**The lookup definition `08`** (High for bytes 0 and 1 and the keys;
the engine runs l01-l04 of 5.13 complete the samples below). Three
samples:
- the only lookup in the 132 `.fmp12` files shipped with FileMaker Pro
  and FileMaker Server: Assets' Assignments::Name (table `0x8005`, field
  9, options `0x80000400`), the same bytes in every language and on both
  platforms;
- two lookups saved by FileMaker Pro 26.0.3.307 in a disposable file
  [O, local, native dialogs read back]. Both start from occurrence A
  (`d0 00 06`), look up from occurrence B (`d0 00 07`) through `A::k =
  B::k` and copy `B::x` (field 2 of B's table). Both use "if no exact
  match: do not copy". They differ only in "don't copy contents if
  empty", OFF in one and ON in the other.

```text
native, empty OFF: 00 01 | 04 03 d0 00 07 | 00 | 02 01 02 | 00 11 01 00 | 01 | 04 03 d0 00 06 | 00
native, empty ON : 00 00 | 04 03 d0 00 07 | 00 | 02 01 02 | 00 11 01 00 | 01 | 04 03 d0 00 06 | 00
Assets           : 00 00 | 04 03 d0 00 04 | 7f | 02 01 08 | be 33 11 00 | 01 | 04 03 d0 00 05 | 7f
```

| bytes | meaning |
|-------|---------|
| 0 | "If no exact match, then": `00` do not copy, `01` copy next lower value, `02` copy next higher value, `03` use the constant in member `0b` [O-run: l01-l04] |
| 1 | "Don't copy contents if empty": `01` OFF, `00` ON [O: the native pair differs only here]. Assets' `00` is ON [O: Assets' dialog, below] |
| key string 1 | the occurrence looked up from ("Lookup from related table") [O: B], preceded by its length as the base table is in an occurrence's `02` |
| next byte | padding: key string 1 sits in a 6-byte buffer (length byte and up to 5 bytes). `00` native, `7f` Assets (no setting the dialog shows, below) and `5e`, `5f`, `5a` in the engine's runs, so not meaningful [O-run, O]; `00`, `ff` and `24` in the 29 lookups of a solution FileMaker Server hosts, in production use [O, #993] |
| key string 2 | the field copied, of that occurrence's table ("Copy value from field") [O: `B::x`] |
| next 4 bytes | padding of key string 2's 7-byte buffer: `00 11 01 00` native, `be 33 11 00` Assets, `00 00 00 00` in the engine's runs [O-run, O]; 15 forms in that solution's lookups, among them `00 02 00 00`, `68 76 68 00`, `01 04 00 00`, `ff 80 7e 00`, `ff d0 15 00`, `00 02 02 00` and `6e 00 00 00` [O, #993] |
| next byte | `01` in all three [U] |
| key string 3 | the occurrence the lookup starts from ("Starting with table") [O: A] |
| last byte | padding of key string 3's 6-byte buffer: `00` native, `7f` Assets [O-run, O]; `00`, `01`, `20` and `40` in that solution's lookups [O, #993] |

That every byte so placed is padding is [I] from these runs: no form was
read in FileMaker Pro's dialog but the native ones and Assets'. OpenMaker's
readers (the record planner's lookups and Options for Field) read the
three buffers whatever their padding holds, and nothing else loosely.

FileMaker Pro 26.0.3's dialog shows Assets' lookup with the native "empty
ON" sample's settings [O, native reading on a disposable local copy, #549]:
starting with Assignments, looked up from Employees, copying `::Name`, "if
no exact match: do not copy", "Don't copy contents if empty" checked. So
the bytes where Assets differs change no setting the dialog shows. Whether
FileMaker Pro rewrites them when the dialog is closed with OK, and how the
lookup behaves at run time, were not measured. The keys read the same way
[O by the dialog]: from Employees (`d0 00 04`, table
`0x8004`), copying Employees::Name (field 8, a stored calculation),
starting from Assignments (`d0 00 05`). Relationship 1 joins the two
(`[3, 17, 5, d0 00 04, fb, d0 00 05]` = `01 01`).

The native "empty OFF" field also has a member child `26` = `00000000`
that the "empty ON" field lacks and no shipped file has [U]. Not yet
established: the byte before key string 3 (`01` everywhere), and how
FileMaker Pro's dialog fills the padding (the next values come from
LookupValue, query-program.md 8.6, #146).

Examples (test project, templates):

| field | definition |
|-------|------------|
| plain text, not indexed | `01 01 00 00 00000015 00000000 00000000 00000001 00000000 0001` |
| text, indexed (Company) | `01 01 00 00 00000015 80000000 00000000 00000001 00000000 0001` |
| container | `01 06 00 00 00000015 00000000 ...` |
| `PrimaryKey` = `Get(UUID)`, indexed, not empty, unique, strict | `01 01 00 00 00000015 80000188 0000001c 00000001 00000000 0001` |
| `CreatedBy` (creator account, prohibit modification) | `01 01 00 04 00000015 00000101 0000000c ...` |
| `CreationTimestamp` | `01 05 00 02 00000015 00000101 0000200c ...` |
| `ModificationTimestamp` | `01 05 00 07 00000015 00000101 0000200c ...` |
| serial number, indexed (FMS sample) | `01 02 00 00 00000000 80000002 00000000 00000001 ...`, `0c` = next value |
| unstored calculation, number | `02 02 00 00 00000000 000a0020 ...` |
| summary, number | `03 02 00 00 00000015 00000000 ...`, `09` = summary definition |

### 5.6 Name keys (High: every name list of the 66 starter files; FileMaker Pro 26.0.3 checks)

A catalog's name list `[cat, 1, 01]` lists each member under FileMaker's
key of its name, which `M_DBNameList::Update` builds in one fixed language
(5.7: locale type 4, the catalog language) whatever the file's own
language. The key has three levels [O]:

1. **Primary:** each non-zero primary weight as a big-endian u16, from
   FileMaker's table (DUCET 5.2.0 one code point at a time, with its own
   weights for code points DUCET does not list: a CJK unified ideograph is
   its code point, a Hangul syllable its code point + 0x19AD, U+E002
   `ffe0 e002`; query-program.md 7.2). The catalog language tailors one
   character: `ı` keys as `i` (query-program.md 7.2). Space is `020a`.
2. **`00 00`, then the secondary level:** for each element with a non-zero
   secondary weight, one byte, the weight minus 0x1F (`01` for an
   unaccented letter, `28` for U+0308, `2f` for U+0303; its low byte for a
   larger weight: `ß`'s middle element, 0x159, gives `3a`). The level is
   left out when every byte would be `01`.
3. **`00`, then the tertiary level:** for each element with a non-zero
   tertiary weight, one byte, the weight of its lowercase form minus 1
   (`01` for `G` as for `g`, `03 03 1e` for `ß`, `10` for a katakana,
   `0d` for a hiragana). The level is left out when every byte would be
   `01`.

FileMaker's own weights add nothing to the higher levels: in the Japanese
starters' names that mix kanji and kana, only the kana have secondary and
tertiary bytes. The curly marks `‘`, `’` and `“` key as the straight ones
with secondary weight 0x21 (query-program.md 7.2; `”` is inferred to), and
`„` also with tertiary weight 0x04 (a German starter's script
`„Bearbeiter hinzufügen“ abbrechen`). No starter keys any
name in its language's tailoring: German `ö` is `o` with an accent, not
`o e`; Japanese kana have DUCET's weights, not the Japanese ones.

| name | where | key |
|---|---|---|
| `Contacts` | every starter | `123d 138e 136d 1433 120f 123d 1433 1410` `00 00 00` |
| `Größe` | Pro 26.0.3, field `0e` of a German Contacts copy | `12b0 13da 138e 1410 1410 126b` `00 00` `01 01 01 28 01 3a 01 01` `00` `01 01 01 01 03 03 1e 01` |
| `テスト` | Pro 26.0.3, table and occurrence of a Japanese Contacts copy | `2b27 2b21 2b28` `00 00` `00` `10 10 10` |
| `主キー` | a Japanese starter's field | `4e3b 2b1b 11dd` `00 00` `00` `10 01` |
| `[`U+E002`]` | [Guest], every starter | `0301 ffe0 e002 0302` `00 00 00` |

A field catalog's **list by type** `[T, 3, 1, 06]` keys a field by its
kind, its type byte (E38), then the first two levels only, with no `00`
after the secondary level: `Größe` as a Text field is `01 04`, its primary
weights, `00 00`, `01 01 01 28 01 3a 01 01`.

A **name several members hold** (layout separators `-`, folder ends `--`,
a font the font catalog `[25]` lists more than once) is listed once under
its key and, for each other member, under its key followed by that
member's key segment (layout-model.md 10.6): a second layout separator
`07` is `0223 000000 07`, and of the fonts `0001` to `0004` named
"Helvetica Neue" in a Japanese starter, `0001` has the plain key and the
others the key followed by `0002`, `0003` and `0004`.

Case does not change the key, so the name list enforces case-insensitive
uniqueness; accents do change it. FileMaker Pro refuses a second field
`Größe` ("The name ‘Größe’ is already in use. Please provide a unique
name."); a file whose name list holds `Größe` under the primary weights
alone (an earlier OpenMaker's) lets Pro add a second one, and Check
Consistency does not flag that list. Whether Pro takes `Grosse` beside
`Größe`, whose keys differ in the secondary level, is not established.

Evidence: every entry of every name list in the 66 starter files (7,641
entries: tables, occurrences, both field lists, layouts, scripts, themes
`[6]`, the security catalogs `[23, 1]` to `[23, 3]`, fonts `[25]`, value
lists `[33]` and `[65]`), which om-collate's `name_key` reproduces
(`cargo test -p om-collate --test names -- --ignored`); FileMaker Pro
26.0.3 adding `Größe` and `テスト` to disposable starter copies, and
refusing the second `Größe`
([#210](https://github.com/OpenMaker-Dev/openmaker/issues/210)).

Not established, and refused by OpenMaker [I]: the higher levels of
FileMaker's own weights other than a CJK unified ideograph's in a name
that also has accents or kana (taken as none, as an ideograph's); the low
and reversed marks `‚`, `‛` and `‟`; a secondary or tertiary byte that
would be `00`; what FileMaker does with a key longer than the 210-byte
capacity (5.7); a CJK compatibility ideograph (keyed as its canonical
ideograph, 7.2). Taking case off the wide, compatibility, font and circled
capitals' tertiary weights (DUCET's 0x09 to 0x0C as 0x03 to 0x06) is
inferred.

### 5.7 Static corroboration in the arm64 client engine (Med: bin, static only)

An earlier static pass over FileMaker Pro 26.0.3.307's arm64 `DBEngine`
(thinned from the universal framework, SHA-256
`43ddff25a568fe3843b1aaadf4f295ca1c1a20c87ce91add75b990f0175bdb61`;
image-relative addresses, LLVM disassembly and exported symbols; no REA
evidence ids, because Ghidra's start on the large image timed out) agrees
with the live sequence above. It does not by itself show what is sent.

- Relationship definitions `[3, fb, 5, R, 02]` and predicates
  `[.., 03, n]`: `DBRelation::Definition::Read` `0x1954d4c` / `Write`
  `0x1954fe4`, `DBJoinPred::Read` `0x19538c4` / `Write` `0x1953a40`
  (layouts in file-metadata.md 4).
- Opening a member for editing: `DBCatalogMember::DoOpen` (`0x1721acc`)
  joins the catalog root and member key and calls `DBBundleLock::CreateLock`
  (`0x18d74b0`) with lock type 9 (10 for access mode 2), timeout 0, and a
  fresh nonzero logical owner, not a file-wide owner 0 (4.1).
- Committing: `DBCatalogMember::DoCommit` (`0x1721eb4`) brackets the upload
  with `DBCatalog::BeginUpdateCatalog` (`0x172208c`) and `EndUpdateCatalog`
  (`0x17222d0`), which maintain the member count, key order, secondary lists
  and modification counts (5.1, 8). `DBCatalogMember::Upload` (`0x1721380`)
  upgrades the member lock to type 10 with an infinite timeout in its edit
  branch, transfers the member and releases the lock (4.2, 4.3).
- Name keys: `M_DBNameList::Update` (`0x172da30`) builds the name list key
  with `HBAbstractKey::SetTextKeyValue` (`0x142ccc0`), fixed locale type 4
  and a **210-byte key capacity**, through `International::MakeKey`, whose
  Support-framework `KeyBuilder` uses Unicode decomposition and sort
  weights: not UTF-8 or lowercased names (5.6).
  Renaming only a member's `10` leaf would leave the name list stale.
- Field catalogs: `DBFieldCatalog::Commit` (`0x17f0b9c`) commits the catalog,
  releases the table lock and may call `RCUpdateTable` op73 (`0x17f0ca4`;
  7.2): creating, deleting or retyping a field needs that update as well as
  the catalog upload.

### 5.8 FileMaker Pro's own edits, compared key by key (High: `fmp:` runs, 2026-11-11)

`fmp:<run>` - FileMaker Pro 26.0.3 on macOS editing a local copy of the
Contacts starter solution (`DynamicTemplates/Contacts/en.fmp12`) through
Manage Database, driven through its accessibility interface; the file
before and after was dumped key by key (`om-hbam` example `dump_keys`)
and compared. The copy was first opened and closed once, so the diffs hold
only the edit (FileMaker Pro also re-saves layout 3 and records its own
version in `[2, 08]`, `[2, 09]` at the first schema save of a template; not
part of the edit). Each run is one dialog session ended with OK:

| run | edit |
|-----|------|
| E1 | Tables: create "Widgets" |
| E2 | Fields of Contacts: create text "Notes", then container "Attachment" |
| E3 | Fields: select Website, set name "Web Site", comment "The company site", type Number, Change |
| E5 | Fields: delete Website; Tables: rename Addresses to "Locations" with comment "Mailing addresses" |
| E7 | Tables: delete Email Addresses (the default: occurrences kept) |
| E8 | Tables: delete Email Addresses with "Also remove occurrences of these tables in the graph" |

What each wrote:

- **New table (E1).** `[3, 16, 5, 8006]`: `10` name, `d8` UUID, `fb` =
  `00000001`, `fc` = `01 01`, `fc01`-`fc03`; **no `d9`**. `[3, 16, 1]`:
  `00` = `02 80 06`, the name list entry, order position 5, count 5, the
  index's `fc` + 1 and stamp. FileMaker Pro also made an **occurrence of
  the same name** `[3, 17, 5, d0 00 06]`: `02` =
  `02 01 00 | 03 02 80 06 | 24 zero bytes | 77 77 77 ff` (the graph box,
  both words after it and the colour as 5.3 reads them: an empty box,
  which FileMaker Pro places when the graph next shows it), `10`, `d8`,
  `fc01`-`fc03` and no `fc` or `d9`; with its index entries. The
  dependency index got `[3, 17, 1, 0e, 8006]` = `{ff: empty node,
  ff03 = 02 00 0a}` (every table's entry has `ff03 = 02 00 0a`). The new
  field catalog `[80 06, 3]` holds only `01` = `{0e: {fff0: empty node},
  d8, fc01-fc03}` and `05` = empty node: no last id, count, lists or `fc`.
  No layout was made for a table without fields (`[4, 1, fc]` went up by
  one: a member key was issued, 3.4, and not used). No default fields
  were added.
- **New fields (E2).** Member `02` (the 5.5 definition with English,
  `0x15`), `10`, `d8`, stamps; "Notes" also had `fc` = `01 01`,
  "Attachment" none. Index: `00` = `01 0f`, the name list, positions 14
  and 15, count 15, the name list by type (`01 04`... for text, `01 18`...
  for container), `fc` + 2. A container field is also listed in the
  table's dependency entry: `[3, 17, 1, 0e, 8002, fffa, 0f]` = empty leaf
  (`fffa` holds the table's container fields; Contacts' Photo `03` was
  already there).
- **Changed field (E3).** Name list and name list by type entries moved
  to the new name and type (`01 08`... for a number); definition byte 1 =
  `02`; `03` comment added; member `fb` = `01 01` (the field's data
  version, a serial key string) and `fc` = `01 01` added; the table's
  `[3, 16, 5, 8002, fb]` went from `00000001` to `00000002`, and so did
  `[d0 00 02, 07, 01, fe, 80 02]`, the data version a saved found set
  was saved under (file-metadata.md 11).
  So `[3, 16, 5, T, fb]` is the table's **data version**, raised when a
  change affects stored values (type change, deleted field); 5.2's "meaning
  not established" for it is answered for these two changes.
  `[3, 16, 1]` was not stamped although a member changed; `[3, 17, 1]`
  was, with nothing else changed in it.
- **Deleted field (E5).** Member, name list and name list by type entries
  removed; the display order **renumbered without a gap** (positions 7 to
  12 now name what 8 to 13 named); count - 1; last issued id unchanged;
  table data version + 1.
- **Renamed table (E5).** `[3, 16, 5, 8003]`: `10` "Locations", `03` the
  comment (table comments are `03`, file-metadata.md 1.1), `fc` + 1; name
  list moved. FileMaker Pro **renamed the occurrence** that had the
  table's old name too (`[3, 17, 5, d0 00 03, 10]`, its name list entry,
  `fc` + 1).
- **Deleted table, occurrences kept (E7).** The confirmation reads
  "Permanently delete the selected 1 FileMaker table(s) and all of their
  fields and record data?" with an unchecked box "Also remove occurrences
  of these tables in the graph", and Delete / Cancel (Cancel the default).
  `[3, 16, 5, 8005]` and its index entries went (count - 1), and `[80 05]`
  with all its data (DeleteTable, 3.4). The occurrence `d0 00 05`, its
  relationship 3, the routes and the other tables' reach entries **stayed**;
  the table's own dependency entry kept only `00` (its key field's reach)
  and `05/00` (the key marker) and lost `ff`, `ff00`, `ff02`, `ff03` and
  `fffc`.
- **Deleted table with its occurrences (E8).** In addition: the
  occurrence's member and index entries (order compacted, count - 1); the
  relationship `[3, fb, 5, 3]` (`[3, fb, 1, 04]` - 1, `00` unchanged, no
  name or order lists in `[3, fb, 1]`); the other side's
  `[3, 17, 5, d0 00 02, fb, d0 00 05]` (that occurrence's `fc` + 1); every
  route `[3, 17, 1, 08, a, b]` with `d0 00 05` at either end; every reach
  entry `[3, 17, 1, 0e, T, 00, F, O, d0 00 05]` of the other tables; and
  `[3, 17, 1, 0e, 80 05]` whole.

Deleting a field FileMaker Pro asks first: "Permanently delete the selected
1 field(s) and ALL of their field data?" (Cancel / Delete).

More runs, each one session ended with OK (on the same baseline unless
noted):

| run | edit |
|-----|------|
| E11 | Fields: create Timestamp "Stamp", Options > Auto-Enter > Creation (Timestamp) |
| E13 | Fields: Website, Options > Storage > Indexing All |
| E14 | Fields: create Number "Seq" with an auto-entered serial number, and Timestamp "Changed" with Modification (Timestamp) |
| E16 | Fields: Website, Options > Validation > Not empty |
| E20 | Relationships: Specify Table, Contacts as "Contacts 2" (`d0 00 06`); a relationship Contacts::Company = Contacts 2::Company with "Allow creation" on the left |
| E21 | on E20: Specify Table "Phone Numbers 2" (`d0 00 07`); Contacts 2::Title ≠ Phone Numbers 2::Type |

- **Options dialog (E11, E13, E14, E16).** OK in "Options for Field" wrote
  member keys `0c` = "1" (the next serial value, even without a serial)
  and `26` = `00000000` (5.5's unknown key) besides the definition.
  Creation timestamp: `mEntryVariable` 2, option `0x1`; modification
  timestamp: 7, `0x1`; serial: option `0x2`, increment 1; Indexing All:
  option `0xa0000000` (value and word index; the table data version went
  up and the index node `[T, 01, F]` was made, empty, at once); Not empty:
  validation `0x8`.
- **The evaluation lists (E11, E14).** The table's dependency entry orders
  every field with auto-entry or a calculation in `ff, F` = position:
  `09, 0a, 0c, 0e` (creation only), `0b, 0d, 0f` (creation and
  modification), then `08` (Initial, a calculation). A new field moved the
  later positions up. `ff00` lists every field evaluated on creation and
  `ff02` those evaluated on modification, keyed by position + 1, valued
  `01 01 02 01 F`; `ff01` holds the calculation, valued `01 02 02 01 08`.
  A calculation's inputs name it under their own field: `01, <its key>` =
  `01 02 02 01 08`. The field's own `[T, 3, 1, 0e, fff0, F]` lists `ff00`
  (creation), `ff02` (modification) or its input fields, each `01`. The
  whole entry is rebuilt from these by the rules of 5.10, which reproduce
  every run here and in 5.9.
- **Validated fields (E16).** `fffc, F` (empty leaf) lists the fields with
  validation; E11's unvalidated field was not listed. "Prohibit
  modification" cannot be set without an auto-enter option. FileMaker Pro
  also wrote `[3, 17, 1, 07]` = empty leaf (meaning not established) and a
  font member `[19, 5, 7]` "Lucida Grande" (its own interface's font).
- **Specify Table and a new relationship (E20).** The new occurrence was
  put first in the display order (the others moved down; E1's occurrence
  went last), with a placed box `630, 337, 131 x 116`, the word after it
  `0x56`, flag 0 and colour `78 78 78 ff`, and `fc` = `01 03`. Relationship
  `[3, fb, 5, 4]`: `02` = `04 03 d0 00 02 | 04 03 d0 00 06 | 00000000 |
  00000000 | 00000020` (left creation, file-metadata.md 4.1), `03, 1` =
  `00 02 01 05 02 01 05`, `d8`, stamps, no `fc`; `[3, fb, 1]`: `00`, `04`,
  stamp. Each occurrence lists the other in `fb`. Routes were added for
  every pair the relationship connects: from Contacts' side the first step
  is the new relationship (`01 04`), from the others the route they had
  to Contacts. Reach: `[0e, 80 02, 00, 05, d0 00 02, d0 00 06]` and
  `[.., 05, d0 00 06, <every occurrence of the other tree>]`, and every
  existing reach that ran up to Contacts (Addresses' ForeignKey, ...) now
  also reaches `d0 00 06`. The key field got its marker `[0e, 80 02, 05,
  00]` = `03 21 02 01 05`.
- **A `≠` relationship (E21).** The same, with operator byte `01`; the key
  markers are `03 21 ...` for `≠` too. Contacts 2::Title reaches only
  Phone Numbers 2 (`[.., 00, 04, d0 00 06, d0 00 07]`), as the reach of a
  key field is the occurrences beyond its own relationships.

**The occurrences' dependency index `[3, 17, 1, 0e, T]`** (Med, from the
Contacts templates and E7, E8). Per base table:

| key | value |
|-----|-------|
| `00, F, O, X` | empty nodes: key field `F` of the table, used by a relationship of its occurrence `O`; `X` are the occurrences reachable from `O` through it, beyond the first one too (Addresses' `ForeignKey` via `d0 00 03` reaches Contacts and, through it, Phone Numbers and Email Addresses) |
| `F, 00` | `03 21 02 01 F` on each such key field (meaning of `03 21` not established) |
| `F, n` | a dependent of field `F`: `01 02 02 01 G` (type 1, a calculation, field `G`) for `First Name` -> `Initial` (5.10) |
| `ff, F` | `01 nn`: the evaluation position of a field with auto-entry or a calculation |
| `ff00, n` / `ff02, n` | `<type> <kind> 02 01 F`: the fields evaluated on creation / on modification, in order (5.10) |
| `ff01, n` | as `ff00`, the fields evaluated even when their inputs are empty (5.10) |
| `ff03` | `02 00 0a` in every table |
| `fffa, F` | empty leaf for each container field |
| `fffc, F` | empty leaf for a field with validation: E16 added Website when it was given "Not empty", and in the Contacts starter solution every field listed has Not empty or Unique. Auto-entered fields without validation are not listed (E11). Unique and Existing list a field too; Always, strict data type and maximum characters alone do not (5.9) |

The field catalog's own `[T, 3, 1, 0e, fff0, F]` lists what field `F`
depends on: other fields `G` and the events `ff00` (creation), `ff01`,
`ff02` (modification), each with its type (5.10). Routes and reach follow the graph: the
graph is a forest (FileMaker allows one path between two occurrences), so
removing an occurrence removes exactly the entries whose two ends are no
longer connected, which is what E8 removed. How `ff` and `ff00`-`ff02` are
rebuilt from `fff0` is 5.10.

### 5.9 Options for Field, compared key by key (High: `fmp:` runs, 2026-11-11)

Method as in 5.8: FileMaker Pro 26.0.3 on copies of the Contacts starter
solution, one Manage Database session per run ended with OK, the file
dumped before and after. E11, E13, E14 and E16 are 5.8's field-option
runs. These runs end with OK in "Options for Field":

| run | edit |
|-----|------|
| E30 | Website: Auto-Enter > Data "abc"; Title: Value from last visited record |
| E31 | Website: Options opened, only the Storage tab shown, OK |
| E32 | Website: Options opened, only the Validation tab shown, OK |
| E33 | ModifiedBy: Modification (Account Name) unchecked |
| E34 | Website: Validation Always, override off, strict Numeric Only, Existing, maximum 20 characters, custom message |
| E35 | Website: Validation Unique value |
| E36 | Website: Validation Existing value |
| E37 | Website: Validation Always, override off, strict Numeric Only, maximum 20 characters |
| E38 | Website: Indexing Minimal, automatic indexes off; Title: 3 repetitions; First Name: global storage |
| E40 | Website: Creation, then Serial number, Modification, Value from last visited record and Data checked in turn |
| E41 | CreatedBy: Not empty unchecked; new Number "N2": serial on commit, next value 100, increment 5, Indexing All |
| E43 | CreatedBy: Creation (Account Name) changed to Modification (Account Name); new Number "N3": Indexing Minimal |

**The definition** (5.5):
- Creation, Modification, Serial number, Value from last visited record
  and Data are one choice. Checking one clears the others (E40).
- Byte 3 holds the chosen variable: 0-4 on creation, 5-9 on modification.
  It goes back to 0 when neither is chosen (E33).
- A serial on commit sets `0x200` and not `0x2` (E41). Its increment is
  `mSerialIncrement`, and member `0c` holds the next value as text.
- Prohibit modification (`0x100`) stays set when the auto-entered value is
  removed (E33). FileMaker Pro offers it only with a value (E16).
- Validation bits (5.5):
  - Always is `0x400` and strict data type Numeric Only is `0x1000`.
  - Override not allowed is `0x4`. Maximum characters is `0x200`, with
    `mByteLimit` (E37).
  - Unique (`0x10`) and Existing (`0x20`) also turn on the value index when
    the field had none (E35, E36).
- Indexing:
  - Minimal is the value index `0x80000000` and All adds the word index
    `0x20000000` (E13, E38). Automatic indexes off is `0x40000000` (E38).
  - A Number field's All is the value index alone (E41). Minimal is not
    offered for a Number field: E43's click left the definition unchanged.
- Turning an index on raised the table's data version `[3, 16, 5, T, fb]`
  (E13, E34-E36, E38, E41). In these local files the index node
  `[T, 01, F, 10]` (16 zero bytes) was made at once. For a hosted file the
  deferred `0x10` asks for it (7.2).
- Global storage:
  - It sets `0x10000`, and bit 0 of the second byte of the field's key in
    the name list by type `[T, 3, 1, 06]` (`01 04` to `01 05`, E38).
  - It gives the field a data version member `fb` = `01 01`, as 3
    repetitions did for Title.
  - The calculation Initial, which uses First Name, became unstored
    (`0x20000`). The kind byte of its dependency entries went from `02` to
    `52` (5.10).

**Members:**
- FileMaker Pro writes members by tab, when the tab was shown in the
  dialog:
  - `0c` (the next serial value as text, "1" without a serial) when the
    Auto-Enter tab was shown (E30, E33, E41). A Validation-only OK wrote
    nothing but the definition (E32).
  - `26` = `00000000` when the Storage tab was shown (E31; meaning not
    established).
  - The dialog opens on the tab last used, which explains E11's and E16's
    `26`.
- Data is member `0d`, text. An empty Data box writes none (E40).
- The custom message is a calculation in FileMaker Pro 26:
  - validation `0x8000` with `0x80`;
  - member `18` holds the text older versions show: "The custom
    validation message for a field on this layout is not available for
    this version of FileMaker software. To view the message, upgrade to
    the latest version." (E34).

**The dependency index** `[3, 17, 1, 0e, T]` (5.8):
- **The field's events.** The field catalog's `[T, 3, 1, 0e, fff0, F]`
  holds its triggers, type `01`:
  - creation values, serial on creation, last visited and data: `ff00`;
  - modification values: `ff00` and `ff02`;
  - serial on commit: `ff02`.

  The entry goes when it has none (E33).
- **Evaluation order and the lists by event.** `ff`, `ff00` and `ff02` are
  rebuilt from `fff0` (5.10): E30 put Title and Website before
  PrimaryKey's calculated value (each one trigger, by id); E41 put N2 (one
  trigger, `ff02`) after the creation fields and before ModifiedBy and
  ModificationTimestamp (two); E43 moved CreatedBy among the latter. A
  field placed or removed renumbers every later position in `ff`, the
  three lists and the dependents' keys (`01, 07` to `01, 09`, E30). E33
  removed ModifiedBy that way.
- **Validated fields `fffc, F`** (empty leaf):
  - Not empty, Unique and Existing each list a field (E16, E35, E36).
  - Always, strict data type and maximum characters do not (E37).
  - Taking Not empty off a field left with strict only removed its entry
    (E41).
  - E34 also added `fffc, 00, F`: a listed field validated Always is
    listed there too (5.13).

**Not schema.** `[d0 00 02, 07, 01, fe, 80 02]`, an occurrence's
record-level data that records the table's data version, went up with
it (E13, E34-E36, E38). This is a cache keyed by that version [I]; it is
not in the catalogs a schema save uploads.

**Furigana** [O: four saves of a disposable file by FileMaker Pro 26.0.3,
2026-11-19, #1301; read back key by key]. Name was turned on with
Description (field 4) and Full-Width Katakana, then given Status (field 2)
and Half-Width Roman, then turned off.
- "Use Furigana Field" is option `0x40`, the definition's only change.
- The field and the translation are in member `07`, the value list's member
  (5.13). It has two 6-byte buffers and a byte:
  - bytes 0-5: the value list's key as a nested key string (`02 01 00`, no
    list, on this field without one);
  - bytes 6-11: the field that receives the reading, the same way
    (`02 01 04` for field 4);
  - byte 12: the translation.

  A field without `07` got 13 bytes.
- The two saves wrote `02 01 00 00 00 00 02 01 04 00 df 4e 02`, then
  `02 01 00 00 00 00 02 01 02 00 df 4e 05`: only bytes 8 and 12 changed.
  `00 df 4e` is the second buffer's padding past the key, which Pro kept
  [I: stale bytes, as 5.13 found in a value list's padding].
- Turned off, `0x40` alone was cleared; `07` stayed as it was.
- The field's `fc`, `fc01` and `fc03`, and the catalog index's, changed with
  each save, as for any member change.
- Translations: 2 Full-Width Katakana and 5 Half-Width Roman were seen. 1
  Hiragana, 3 Half-Width Katakana and 4 Full-Width Roman follow from the
  menu's order [I], not seen.
- Not established:
  - the dialog's exact entries besides those two;
  - which fields it offers as the target (OpenMaker allows stored text
    fields of the same table [I]);
  - a target field whose id needs more than one byte (inferred from the
    value list's nested key string);
  - whether a non-text field can take furigana;
  - the conversion itself ([#322](https://github.com/OpenMaker-Dev/openmaker/issues/322)).

**Not established:**
- what FileMaker Pro's dialog writes for a value list, a range, a
  validation calculation and a custom message (5.13 has the engine's
  forms);
- index language codes other than 21;
- what `26` holds;
- what FileMaker Pro writes when it removes data or a serial number;
- the deferred work bits it sends for option changes to a hosted file
  (7.2 gives them from the binary).

### 5.10 Calculation fields and the evaluation lists (High: engine runs, 2026-11-13)

Method: FileMaker Server 26.0.3's FMUpgradeTool (`--update`) applied
patches that add calculation fields to the Contacts table (`80 02`) of the
Contacts starter solution; the file was dumped before and after and
compared key by key [O-run]. Runs c01-c20 add one field each (c15 two).
Two batteries added 413 and 25 fields at once: one per built-in function,
constant, operator and `Get` selector, each reading Title, and mixed
cases. The rebuild rule below was then checked against the dependency
entries of all 66 shipped templates, where it reproduces 198 of 199
tables (the other holds the templates' only lookup), and against FileMaker
Pro's own saves E11-E43 (5.8, 5.9) [O]. Statically, committing a field
calls `DBDepend::UpdateDependencies` (libDBEngine.so `nm` 0xebc600), which
rewrites the field's `fff0` entry; `DeleteDependencyTable` (0xebd240) then
drops `[3, 17, 1, 0e, T]` and `RebuildTriggerTable` (0xeb6ac0) rebuilds it
whole [O, bin]. Test vectors: `crates/om-data/tests/fixtures/contacts-calculations.txt`.

**A new calculation field** (c01) writes:
- the member: `02` the definition, `05` the formula (`04` context
  occurrence, `05` formatted stream, `06` Polish stream; calc-encoding.md
  1), `10` the name, `fb` = `01 01`, `d8` and the stamps, no `fc` or `d9`;
- in the index: the last id, the name list, the display order, the count,
  the name list by type `02 <type << 2 | global> <name key> 00 00` (5.5),
  and `fc` + 1;
- its `fff0` entry and the rebuilt table entry below, with `[3, 17, 1]`
  stamped. The table data version does not change. An indexed calculation
  got its index node `[T, 01, F]` (`10`, `11`) at once in the local file
  (c11).

**The definition** (5.5):

| bytes | value |
|---|---|
| 0, 1 | `02`, the result type |
| 2 | `01` on the unstored and global calculations the importer made (c05, c10, c16); the templates, made by FileMaker Pro, have unstored calculations with `00` and with `01` (meaning not established) |
| options | `0x20` with "Do not evaluate if all referenced fields are empty" off (c02); `0x20000` unstored (c05); `0x10000` alone for global (c10); `0x80000000` Minimal, `0xa0000000` All on a text result (c19, c11); `0x40000000` automatic indexes off (c20); `0x80000` when no direct field trigger has type 1 or 2 (c07, c08, the batteries) |
| 16-23 | increment 1, byte limit 0, as for a normal field |
| 24-25 | the number of repetitions (c12) |

**Direct triggers**, `[T, 3, 1, 0e, fff0, F, <key>]` = type [O-run]:
- each field of its own table the formula reads through its context
  occurrence: type `01` where read as a value, `00` where a field
  reference is required (the aggregates, `GetRepetition`, `field[n]`,
  `Extend`, `Last`, `NPV`; calc-encoding.md 5.3), `02` when both;
- `ff00` and `ff01` type `01` when the formula uses `IsEmpty`,
  `GetAsBoolean`, `GetFieldName` or `GetBaseTableName` (anywhere, as in
  `Title & IsEmpty ( "" )`), or when "Do not evaluate if all referenced
  fields are empty" is off and the formula reads a field (c02). None of
  the other built-in functions, constants and operators, nor `If`, `Case`,
  `Choose`, `Let`, `While` or any `Get` selector next to a field, adds one
  (c04, c09, c14, the batteries);
- a formula that reads no field: `ff00` and `ff01` type `00`
  (`Get ( CurrentDate )`, `1`, `Random`; also with the checkbox off), type
  `01` with a function of the list above (`IsEmpty ( "" )`);
- `GetField` adds `ff02` type `01`; `Self` adds the field itself;
- a related field is `00 <occurrence> <field>` (templates; not compared
  further).

**The rebuild** of `[3, 17, 1, 0e, T]` from `fff0` and the definitions
[O-run, O]:
- The evaluated fields are those with a `fff0` entry. A field's closure is
  its direct triggers and, through each evaluated field among them, that
  field's closure.
- `ff, F` = `01 <position>`, ordered by the number of keys in the closure,
  then by id: c04's `{04}` counts 1 and goes after the creation fields;
  c01's `{01, 02}` counts 2 and goes after the modification fields; c06's
  `{08}` reaches Initial's four and goes after it.
- Each event `ff00`, `ff01`, `ff02` and each field `G` lists, keyed by
  position + 1, every field whose closure holds it: c06 is listed under
  `01`, `02`, `08`, `ff00` and `ff01`, while its `fff0` holds `08` alone.
- A listing is `<type> <kind> <F as a nested key string>`. The type is the
  direct trigger's; an inherited one is `01` through `01` edges and `00`
  through a `00` edge (other combinations seen in the battery are not
  reduced to a rule here). A calculation without `0x80000`, and a field
  with an auto-entered calculation, has its type-`00` listings written
  `02` (the battery; 5.13). The kind is the definition's byte 0, with
  `0x40` for an unstored calculation and `0x80` for global storage (c10);
  `0x10` (5.9's `52`) also occurs. A dependent that is itself a
  relational key field (the table's reach `00` has it) carries `0x20` in
  its listings under its input fields: Assets' Assignments lists its
  calculation Sort By, itself a key field (`05, 00` = `03 22 …`), as `22`
  under Date Returned [O, templates].
- The key-field reach `00`, the key markers `F, 00`, `ff03`, `fffa` and
  `fffc` are kept. A table without evaluated fields keeps an empty `ff`
  (5.8, E1).

**Not established:** what FileMaker Pro's own dialog writes in byte 2,
`26` and `fc` for a new calculation (these runs are the engine's
importer); the deferred work FileMaker Pro asks a host for when it adds or
changes a calculation (7.2 gives the rules from the binary);
related, summary and global inputs beyond the forms above; the types
along mixed paths.

### 5.11 Summary fields (High: engine runs, 2026-11-13)

Method as in 5.10: FMUpgradeTool patches on the Contacts table, first
adding the number fields N (`0e`), W (`0f`) and NR (`10`, 3 repetitions)
(run s00), then each of runs s01-s16 the same fields and one summary S
(`11`): every kind of file-metadata.md 5.1, the weighted average (s03), the
standard deviation by population (s08), the subtotaled fraction (s10), the
running total (s12), restarted by Title (s13), the running count (s14), a
repeating field summarized individually (s15) [O-run]. Test vectors:
`crates/om-data/tests/fixtures/contacts-summaries.txt`.

**A new summary field** (s01) writes:
- the member: `02` the definition (byte 0 `03`, byte 1 the result type,
  the rest as a new normal field), `09` the summary definition
  (file-metadata.md 5.1), `10` the name, `fb` = `01 01`, `d8` and the
  stamps;
- in the index what a new field gets (5.8), with the name list by type
  `03 <type << 2>`;
- `fff0, S` = the summarized field with type `00`, and `fffb, S` (an empty
  leaf) in the table's dependency entry, which the rebuild of 5.10 then
  lists like any evaluated field: kind `03`, type `00` (`0e, 05` =
  `00 03 02 01 11`); a summary of an evaluated field reaches its triggers
  through that `00` edge with type `00` (s06, Maximum of
  CreationTimestamp, is listed under `ff00` as `00 03 ...`).

The result type is Number for a total, an average, a count, a deviation
and a fraction, the summarized field's type for a minimum or a maximum
(Timestamp in s06), and Text for a list (s11).

In `09` the bytes of each 6-byte key after its key string are not
initialized by the engine (`02 01 00 00 f8 09`, `02 01 0e 02 63 f6`): only
the length-prefixed part is read.

**The additional field.** A restarting running total (s13) stores its
break field and occurrence in `09` and writes nothing more. The weighted
average (s03) and the subtotaled fraction (s10) also depend on it:
`fff0, S, 00 <occurrence> <field>` = `02`, and in the dependency entry
`00, <occurrence>, <field>, S` = `03`, `00, <occurrence>, ff, S` = `03` and
`<occurrence>, 00` = `03 20 04 03 <occurrence>` (s03: occurrence
`d0 00 02`, field W).

**Not established:** what FileMaker Pro's own dialog writes in `09`'s
uninitialized bytes and in `26`; the deferred work it asks a host for when
a summary changes (7.2 gives `0x40` and `0x80` for a changed summary from
the binary).

### 5.12 Calculations over related fields and aggregates (High: engine runs, 2026-11-13)

Method as in 5.10: runs r01-r15 add calculations over the Phone Numbers
(`d0 00 04`) and Addresses (`d0 00 03`) occurrences and aggregates of the
Contacts table's own fields [O-run]. Test vectors:
`crates/om-data/tests/fixtures/contacts-related-calculations.txt`.

- **Triggers.** A field read through another occurrence than the context
  is the trigger `00 <occurrence key string> <field key string>`
  (`00 03 d0 00 04 01 02` for `Phone Numbers::Number`), type `01` as a
  value and `00` inside a one-argument aggregate (`Count`, `List`, `Sum`).
  A field of its own table in an aggregate is its key with type `00`
  (`Count ( Title )`), and with type `02` when it is read both ways
  (`Sum ( Title ) & First Name` is `04` = `00` and `01` = `01`, so `02`
  where both meet).
- **Storage.** A calculation that reads a related field, or a
  calculation that is not stored, is made unstored (`0x20000`) whatever
  was asked; its byte 2 stays `00` (r01, r06, r15), while an unstored
  storage asked for in the importer writes `01` (r12). `C1 + 1` over
  `C1 = Count ( Phone Numbers::Number )` is unstored and keeps
  `0x80000`: a calculation is pure when every input it reads with a type
  other than `00` is pure itself (r06).
- **The dependency entry.** The closure counts the related key, and the
  rebuild of 5.10 lists the calculation under it this way: `00, <occurrence>,
  <field>, F` and `00, <occurrence>, ff, F` hold the calculation's kind
  byte (`02`, `03` for a summary, 5.11; no storage flags), and
  `<occurrence>, 00` = `03 20 <occurrence as a nested key string>`
  (`03 20 04 03 d0 00 04`). A calculation that reads one through another
  is listed there too (r06, r15). Its listings elsewhere carry the
  unstored kind `42` (r05). Nothing changes in the related table's
  entry, and the context's key field is not an input.

**Not established:** related summary and global fields, and related
calculations' own triggers (a related field that is a calculation was not
run). Lookups and an auto-enter formula's related inputs: 5.13.

### 5.13 Auto-entered calculations, lookups and validation options (High: engine runs, 2026-11-13)

Method as in 5.10: FMUpgradeTool patches add one normal field each to the
Contacts (`80 02`, field `0e`) or Phone Numbers (`80 04`, field `09`)
table of the Contacts starter solution, and the file is compared key by
key before and after [O-run]. Runs a01-a14 add auto-entered calculations,
l01-l04 lookups, v01-v27 validation options. Each result passes
`FMDeveloperTool --checkConsistency` and exports with Save as XML as the
patch described it. Test vectors:
`crates/om-data/tests/fixtures/contacts-option-formulas.txt`.

**Save as XML** (the 2.x grammar FMUpgradeTool reads; element names from
libFMEngine.so's `ObjectTransferFields::XMLSerialize_ValidationOptions`
and `XMLDeserialize_ValidationOptions` and their `kXMLEngine_*`
constants [bin], each form confirmed by a run):

| option | XML |
|---|---|
| calculated value | `<AutoEnter type="Calculated" overwriteExisting="…" alwaysEvaluate="…"><Calculated enable="True"><Calculation>…</Calculation></Calculated>` |
| looked-up value | `<AutoEnter type="Looked_up"><Looked_up dontCopyIfEmpty="…" noMatchCopyOption="DoNotCopy\|NextLower\|NextHigher\|ConstantData" enable="True">[<ConstantData>…</ConstantData>]<FieldReference>…</FieldReference><Context><TableOccurrenceReference …/></Context>` |
| member of value list | `<ValueListReference id="…" name="…"/>` in `<Validation>` |
| in range | `<Range from="…" to="…"/>` |
| validated by calculation | `<Calculated enable="True"><Calculation>…</Calculation></Calculated>`, and `alwaysValidate="True"` on `<Validation>` when "Validate only if field has been modified" is off |
| maximum characters | `<MaximumSize>20</MaximumSize>` |
| custom message | `<Message>text</Message>`, or `<MessageCalc enable="True"><Calculation>…</Calculation></MessageCalc>` |

**Calculated value** (a01-a14):
- Options `0x8`; `0x80` with `overwriteExisting="True"`, that is "Do not
  replace existing value of field (if any)" off (a01, a02); `0x20` with
  "Do not evaluate if all referenced fields are empty" off (a08, a11).
  No `0x80000`, whatever the formula reads.
- Member `05` as a calculation field's (`04` context occurrence, `05`,
  `06`). No `fb` member.
- `fff0, F`: the formula's field inputs by the type rules of 5.10 and 5.12
  (`Self` is the field itself, a07; inside an aggregate `00`, a05) and
  `ff00` = `01`, also with no input (a03, a10). A field reading itself is
  listed under itself keyed `01` rather than by position (a07). Never `ff01`: not for
  `IsEmpty` (a07, a09), nor with the checkbox off (a08, a11).
- A related input adds what 5.12 describes, with the field's own kind
  `01` (a04), and the field catalog node `[T, 3, 1, 0e, fb, F, <context>,
  <occurrence>]` for each occurrence it reads, two relationships away
  included (a12, a13: flat, without the occurrence between).
- That node also makes triggers the rebuild derives, not stored in
  `fff0`: the first relationship on the way from the context to the
  occurrence is the route `[3, 17, 1, 08, <context>, <occurrence>]` (5.3),
  and each of its predicates' fields on the context's side is a trigger
  of type `01` (Phone Numbers::ForeignKey for a12; Contacts::PrimaryKey
  for a04). They count in the closure and are inherited: in a14 the
  auto-entered `Phone Numbers::Number` (closure `{related, ff00,
  PrimaryKey}`) ties with a calculation over three fields and follows it
  by id, and a calculation reading it is listed under PrimaryKey too. The
  rule reproduces these runs, l01-l04 and the templates' only lookup
  (Assets' Assignments) [O-run, O]. A field that repeats while the match
  field does not may get type `02` [U].

**Looked-up value** (l01-l04, Phone Numbers looking up Contacts::Company
through `PrimaryKey = ForeignKey`):
- Options `0x400`; member `08` (the table above), and `0b` = the constant
  text for `ConstantData` (l04).
- No `fff0` entry. The field catalog node `fb, F, <starting occurrence>,
  <looked-up occurrence>`, and the derived trigger above: the field is
  listed under ForeignKey (`05`) and ordered with a closure of one.
- From a parent table (p01, battery s41: Contacts::L2 looking up
  Phone Numbers::Number through the same relationship from Contacts):
  - the same forms, with `fb, 0e, d00002, d00004`;
  - the derived trigger is Contacts' own match field, PrimaryKey (`09`),
    an auto-entered `Get ( UUID )`, under which L2 is listed (`09, 07`);
  - the rebuild of 5.10 with derived triggers reproduces the entry.
- Through a sorted relationship (p02, battery s42: Assets::LA looking up
  Assignments::Note from Assets through the Assets starter's relationship
  sorted by Assignments::Sort By):
  - the same forms, with `fb, 0f, d00002, d00005`;
  - the derived trigger is again the starting match field, Assets'
    PrimaryKey (`0a`), under which LA is listed (`0a, 08`); the
    relationship's sort field adds no trigger;
  - the rebuild of 5.10 with derived triggers reproduces the entry.

**Validation:**
- Member of value list (v01, v19): `0x100`, member `07`. FileMaker Pro's
  files hold 14 bytes: the value list's key string in a 6-byte buffer
  (`02 01 03 00 00 00` for value list 3), then `02 01 00` in a further
  6-byte buffer and 2 bytes, all padding zero (mbs-email, fms-sample-en;
  some files have stale bytes in the padding). The engine wrote 13 bytes
  with stale padding. Fields with the option off keep a `07` (shipped
  samples). The second buffer names the furigana field and byte 12 holds
  its translation (5.9, "Furigana"); `02 01 00` and `00` there mean none.
- In range (v02, v15, v21): `0x40`, the bounds as text in `21` (from) and
  `22` (to), exactly as given (`1/1/2020` and `2020-12-31` alike).
- Validated by calculation (v09-v27): `0x1`, and `0x2` with "Validate only
  if field has been modified" off (v10). Member `19` as `05`. Its inputs
  are `[T, 3, 1, 0e, fff1, F]`, by the type rules of the calculated value
  (`Self` is the field, v09; `00 <occurrence> <field>` for related ones,
  v13, v27; `00` inside an aggregate, v25), with no events: a formula
  without field inputs has no `fff1` entry (v24), `IsEmpty` adds none
  (v23). `fff1` takes no part in the rebuild of 5.10, and related inputs
  here write no relational entries and no `fb` node.
- Custom message: as text, `0x80` with member `18` (v11, v14); as a
  calculation, `0x8000` with `0x80` and member `20` (v12, v26). The
  importer writes no `18` with a calculation; FileMaker Pro 26's dialog
  writes `0x8080` and `18` with the text for older versions (5.9, E34).
  `<Message>` alone, without another validation option, is dropped.
- **Listed fields** (`[3, 17, 1, 0e, T, fffc]`): a field validated Not
  empty, Unique, Existing or by calculation is listed `fffc, F`; such a
  field validated Always also `fffc, 00, F` (v15, v17, v18; E34). A value
  list, a range, a maximum, a strict type or a message lists nothing, with
  or without Always (v01, v02, v08, v19; E37).

Runs a15 and v28 give the same forms from Phone Numbers through its
ForeignKey: an auto-entered `Contacts::Company & "!"` with `fff0` =
`{00 d0 00 02 05: 01, ff00: 01}`, its relational entries and `fb, 09,
d0 00 04, d0 00 02`, listed under ForeignKey; a validation by
`Contacts::Company ≠ ""` with `fff1` = `{00 d0 00 02 05: 01}` alone.

**Not established:** what FileMaker Pro's own dialog writes for these
options (the forms are the engine's importer's; FileMaker Pro wrote 5.8's
and 5.9's), `0x800`, and lookups along more than one relationship.

### 5.14 Duplicate in the Fields tab (Med: bin and Claris help; no capture)

Claris's help: Duplicate copies the selected fields (one or more) in the
current table, "places the duplicated field at the end of the list of field
definitions and adds Copy to the field name" [doc:
`create_db.8.33.html`, the FileMaker Pro 12 help in Claris's archive].
The help does not say how further copies are numbered.

FileMaker Pro 26 (Mac x86_64; `FMDefineFieldsPage::OnDuplicatePushed`,
`0x100350060` in the app; DBEngine for the calls named) does, for each
selected field in list order, inside one task:

1. `DBCatalogMember::Duplicate` (`CreateCopy`): copies the original's whole
   member subtree under the new field's key (`HBPosition::CopyFrom`), then
   rewrites its tags `da` (below), and sets an empty UUID and owner id
   (`SetUUID`, `SetOwnerID`; the commit gives the new member its `d8`
   [I]).
2. The name: the original's, with spaces at either end trimmed
   (`CleanName`), then `MakeDupName` with a 100-character limit, which is
   `DBUniqueName::MakeName` with the duplicate suffix, DBEngine resource
   `(0x81, 0x69)`, " Copy" (`(129, 105)` in the raw string-resource
   dump, docs/re/catalogs/README.md). While the name is taken: find the
   last " Copy" (`FindLastPattern`); at the end of the name, write `2`
   after it; followed by a number n above 0 (`ExtractTrailingNumber`,
   whose 0 also means no number), write n + 1 in its place; otherwise
   append " Copy" (so `Name Copy0` gives `Name Copy0 Copy`). The name is
   checked before any of this, so a trimmed name that is free is the
   copy's name as it is. So `Name` gives `Name Copy`,
   then `Name Copy2`, `Name Copy3`; a duplicate of `Name Copy` gives
   `Name Copy2`. No other character changes: a name calculations write as
   `${…}` gives a copy that needs it too. After 99 numbers, or past the
   limit, a different path runs (not read).
3. `DBField::SetOptions` with the copied options less `0x4000 | 0x8000`
   (`kNeedLibraryUpdate`, `kSkipLibraryUpdate`, 5.5).
4. `DBCatalogMember::Commit`, which writes the new field's catalog entries
   as for any new field.

Then the list is rebuilt and the copies are selected.

**Tags.** `CreateCopy` reads the copy's tags `da` ("#tag " one after
another, 5.5); when it has any, it removes `DBObjectTags::kPrimaryTag`
`^FMI*` and adds `kPossibleSecondaryTag` `^FMI_` when the original had the
first, and removes `kDefaultObjectTag` `_FMI_0` (Support's static tag
strings; the others: `kAutoCreatedObjectTag` `_FMI_1`, `kForeignTag`
`^WA*`, `kAutoCreatedCalcTag` `^WA%`, `kWAStarterTag` `_FMI_WA_STARTER_`,
`kConfigTag` `CONFIG`). A starter's primary key, `#^FMI* #_FMI_0 `,
gives a copy tagged `#^FMI_ `; its other default fields' `#_FMI_0 ` gives
none. Whether an empty result leaves an empty `da` or none is not read
[I: none]. Where `Add` puts the new tag among others is not read.

So the copy holds every member of the original byte for byte (the
definition, its formula `05`, options and their members, a summary's `09`
and a lookup's `08` with the padding bytes FileMaker's files carry, the
next serial value `0c` as it was, a comment, `29`-`31`), but its name,
tags, identity and bookkeeping (`d8`, `d9`, `fc`...). Whether `fb` is kept
or set as for a new field is not read.

**Not established:** a capture or a FileMaker Pro run of Duplicate; the
resource text of the suffix in other languages; what the commit adds to
the copied member.

### 5.15 Deleting a field (High: engine runs, 2026-11-13)

Method: FileMaker Server 26.0.3's FMUpgradeTool (`--update`) applied
patches that delete one field, and the file was dumped before and after
and compared key by key, as in 5.10 [O-run]. The patch form is Claris's
[doc: App Upgrade Tool guide, "About the patch-file"]: `DeleteAction`
holds `<ItemReference UUID="<the field's UUID>" type="FieldReference">`,
and the field's table must be named by a `<BaseTableReference>` child
(`DynamicTemplateXML_Director::ProcessDeleteReferencedItem` calls
`ExtractBaseTableReference` for a field [O, bin]). Without that child, or
with the field named by `Field` or `FieldReference` elements, the tool
reports "Patch File Applied" and leaves the file unchanged. Test vectors:
`crates/om-data/tests/fixtures/contacts-deletions.txt`.

Runs on the Contacts starter solution: d01 Initial (a stored
calculation), d02 CreatedBy (Creation Account Name, validated not empty),
d03 First Name (read by Initial), d04 PrimaryKey (an auto-entered
`Get ( UUID )`, a relationship's match field). Runs on the files of
earlier add runs: e01 the summary S (Average of N) of s02 and e05 its
field N (5.11); e02 V1 of v09, validated by `Length ( Self ) < 10`; e03
A1 of a01, auto-entered `First Name & " " & Last Name`; e04 A1 of a04,
auto-entered `Phone Numbers::Number`; e06 the lookup L2 of p01; e07 V1 of
v17, validated not empty Always (5.13).

**What the deletion writes** [O-run]:
- The member `[T, 3, 5, F]` goes with all it holds.
- The catalog index: the name list and name-by-type entries go, the
  display order closes up (d01: the fields after Initial each move up
  one place), and the count drops by one. The last issued id `00` stays.
- The field catalog's dependency index: the field's `fff0` entry (its
  triggers, also an entry option's `ff00`, d02), its `fff1` entry (e02)
  and its `fb` node (e04, e06) go. The `fff1` and `fb` lists stay, also
  when empty.
- The table's entry `[3, 17, 1, 0e, T]`: the field leaves the validated
  list `fffc` (d02), and `fffc, 00`, the list of fields validated Always,
  which goes when it is empty (e07); and the summary list `fffb`, which
  goes when it is empty (e01). The evaluation lists are rebuilt from what
  remains, as in 5.10, with the derived and relational entries of 5.12
  and 5.13: e04 drops `00, d00004`, `d00004` and PrimaryKey's listing of
  A1. After each of e01-e07 the entry equals the starter's again.
- Record values: the engine runs had no records. FileMaker Pro asks the
  host to delete the field's values with deferred work `0x1` (7.2).

**What the engine leaves** [O-run]: a field another definition reads is
not deleted. First Name, read by Initial (d03), and N, summarized by S
(e05), come out unchanged while the tool reports the patch applied. A
relationship's match field is deleted (d04), but the table's entry keeps
its key marker `09, 00`, with the field's kind cleared (`03 21 02 01 09`
becomes `03 20 02 01 09`), so the relationship goes on naming a field the
file no longer has.

**Not established:** FileMaker Pro's own deletion through Manage Database
beyond 5.8's E5 (a plain field); what it writes for a definition left
reading a deleted field, a relationship sorted by it or a value list of
its values (none was measured) (FileMaker Pro shows `<Field Missing>`,
calc-encoding.md 10); whether a table's `fffc` stays when its last
validated field is deleted.

### 5.16 Calculations over a global or summary field (High: engine runs, 2026-11-14)

Method as in 5.10: FileMaker Server 26.0.3's FMUpgradeTool added fields to
the Contacts starter solution, and the file was dumped before and after
and compared key by key [O-run]. Runs: g01 a global text field G and
`C1 = First Name & G`; g02 G and `C2 = G & "x"`; g03 a number field N,
`S = Total of N` and `C3 = N / S`; g04 `S = Count of First Name` and
`C4 = S`. C3 and C4 have a number result, C1 and C2 text. Test vectors:
`crates/om-data/tests/fixtures/contacts-global-summary.txt`.

- **Triggers.** A global or summary field of the calculation's own table
  is an ordinary direct trigger, type `01` (`fff0`; g01-g04). The
  summary's own trigger is its summarized field with type `00` (5.11).
- **Storage.** The calculation is made unstored (`0x20000`) whatever was
  asked, with byte 2 `00`, as a related input makes it (5.12). None of
  the four has `0x80000`: each reads a field with type `01`.
- **The summary list `fffb`.** A calculation that reads a summary field
  joins the summary in the table's `fffb` list (g03: S and C3; g04: S and
  C4). A global input adds nothing to it.
- **The dependency entry.** The rebuild of 5.10 lists the calculation
  under every input with 5.10's types; C4's inherited type-`00` listing
  under First Name through S is written `02`, as for any calculation
  without `0x80000` (5.10). Its kind byte depends on the input it is
  listed under:
  - under a global field: the definition's byte 0 with `0x50` (`52`;
    g01, g02), as E38 shows for Initial when First Name became global
    (5.8);
  - under a summary field: with `0x40` (`42`; g03, g04);
  - under any other input: byte 0 alone (`02`), though the calculation
    is unstored (C1 under First Name, C3 under N, C4 under First Name).
  A calculation made unstored by a related input instead carries `42`
  under every input (5.12).

**Not established:** a global or summary field of another table, read
through a relationship; a global or summary field read by an auto-entered
or validation formula; a calculation that reads C1-C4 (a calculation over
a global or summary field), or that reads a global or summary field
together with a related field, an unstored calculation or a `Get`
function; a summary field's own listings when a calculation reads it
inside an aggregate. The importer wrote non-zero bytes in slots of a
summary definition that 5.11's runs write as zeros: in g03, where it
added N and S in one patch, and the byte after the summarized field's
key in g04 (`01 02` where 5.11 has `0e 00`); their meaning is not
established.

### 5.17 Auto-entered and validation formulas over a global field (High: engine runs, 2026-11-14)

Method as in 5.16: FMUpgradeTool added to the Contacts starter a global
text field G (`0e`) and a text field (`0f`) with an auto-entered
calculation or a validation calculation over G, and the file was dumped
before and after and compared key by key [O-run]. Runs: o01 A auto-entered
by `G`; o02 A auto-entered by `First Name & G`; o03 V validated by
`Self <> G`; o04 V validated by `First Name = G`. Each was entered "Do not
replace existing value", "Do not evaluate if all referenced fields are
empty" and "Validate only during data entry", the defaults. Each result
passes `FMDeveloperTool --checkConsistency`, and Save as XML exports the
formula as the patch gave it. Test vectors:
`crates/om-data/tests/fixtures/contacts-option-globals.txt`.

- **A global field of the formula's own table is an ordinary input.**
  Everything is written as 5.13 writes a formula over a normal field:
  - the global is a direct trigger of type `01` in `fff0` (an auto-entered
    formula, with `ff00`) or `fff1` (a validation formula, with the field
    itself for `Self`);
  - in the dependency entry the field is listed under G with its own kind
    byte `01`, the same as under First Name (o02). Nothing marks G as
    global: no `0x50`, unlike a calculation field over a global (5.16);
  - the definition takes 5.13's options (`0x8` for the calculated value,
    validation `0x1`), with no `0x20000` and no `0x80000`.
- **A changed global recomputes no record** [I]: no trigger of the
  dependency entry is G's own, so nothing is listed to run when G
  changes. When FileMaker re-evaluates these formulas on a record is not
  measured.

**Not established:** a summary field, or another table's global, in an
auto-entered or validation formula; the other options of these formulas
over a global ("replace existing value", always evaluate, always
validate); a global of another type than text, or with repetitions.

---

## 6. Table data for a new or deleted table (High: live:s1, s2b-s2d)

Creating a table writes nothing under `[T]` except the field catalog
`[T, 3]` the client uploads. There is no request that "creates a table" on
the server; the table exists once `[3, 16, 5, T]` and `[T, 3]` exist. The
other per-table keys appear on demand (write-path.md 5):

| key | appears |
|-----|---------|
| `[T, 00]` last issued record id | at the first op71 |
| `[T, 05, R]` records, `[T, 0d]` record list | at the first committed record (op70) |
| `[T, 01, F, ...]` field indexes | only when the index node exists: op73 creates it for a field whose deferred list entry has `0x10` (7.2, live:s2c) |

**The index node decides, not the option bit.** A field whose definition
says "indexed" (`0x80000000`) gets no index from a record commit unless
`[T, 01, F]` already exists (write-path.md 5.5). live:s2c committed a record
into the new table `80 07` whose fields `01` and `03` were defined indexed:
FileMaker Server wrote the record, `[T, 00]` and `[T, 0d]`, and no
`[T, 01]` at all. The deferred `0x10` + op73 of the next save then built the
indexes it was asked for. So whoever turns indexing on must also make the
server build the index: FileMaker Pro does it with the deferred list (and
op85 when a find needs a missing index, 3.7). For a table created in the
same save the client masks the data bits out of the deferred list (7.2);
how FileMaker Pro then gets the index nodes of a brand-new table's indexed
fields created (an op85 per field, or a deferred `0x10` in the first save) is
not established (section 12).

A new table's field catalog has no `fc` (it was never modified after
creation; Med, from the `IncrementModCount` rule in 8.1).

Deleting a table is DeleteTable (3.4): the whole `[T]` subtree goes,
including the field catalog. The client removes `[3, 16, 5, T]`, every
occurrence of the table in `[3, 17, 5]`, every relationship that used them
in `[3, fb, 5]`, and rewrites the three indexes. Layouts, scripts and value
lists that used the table are not changed by the delete (Med: nothing in
the delete path touches them; FileMaker Pro reports such references as a
missing table).

---

## 7. Work the server does itself

### 7.1 Nothing at creation (High)

The server does not create, number, name or index anything when a table,
field, occurrence or relationship is created: all of it comes from the
client (sections 3 and 5). It does not validate catalog contents either
(live:s1 stored hand-built members unchanged).

### 7.2 Deferred field work: `[T, 3, 1, fd]` and op73 (High for the format
from bin; behaviour per bit from bin, a live op73 in 7.3)

When a field change needs work on existing record data, FileMaker Pro writes
one leaf per field under the field catalog's index before uploading it:

```
[T, 3, 1, fd, F] = u32 BE operation bits
```

(`DBFieldCatalog::PutDeferredOps`, bin 0x19e7010; computed by
`DBField::DoCommitProcessing` 0x19e815e by comparing the field's old and new
definition, and by `DBField::DoDeleteProcessing` 0x19e8fbc). The server's
actions are `TableUpdater::DoUpdate`, `UpdateRecords`, `UpdateRecord`,
`DeleteFieldData`, `ProcessFieldData` and `DeleteGlobalRecordData` (mac
DBEngine 0x19f1220, 0x19f08c2, 0x19f0770, 0x19f03d4, 0x19f050e, 0x19f1018;
the same names in linux libDBEngine.so, `nm` 0xefd800 for `DoUpdate`). Bits,
with the definition fields of 5.5 (from bin):

| bit | set when | server action |
|-----|----------|---------------|
| `0x1` | the field is deleted, or its storage changes (stored, global `0x10000`, unstored `0x20000` in `mOptionSet`) unless the old field was an unstored calculation | delete the value of every repetition from every record (with `0x2`, the global value instead) |
| `0x2` | added to any of `0x1`, `0x4`, `0x8` when the old field had global storage | apply them to the table's global value (`DeleteGlobalRecordData`, all repetitions from the first, or from the new maximum + 1 with `0x4`) instead of walking records |
| `0x4` | the repetition count is reduced and neither `0x1` nor `0x8` is set (not for an old unstored calculation) | delete the repetitions above the new maximum from every record |
| `0x8` | the old field was a container and the data type changes, or with `0x1`/`0x4` on a container | release each deleted value's container library reference (`DBCursor::DecrementLibraryRef`, containers.md) and delete the value; a type change from container deletes every repetition |
| `0x10` | indexing turned on, the index language changed, or any of `0x1`, `0x4`, `0x8`, `0x40` on a field that is indexed after the change | rebuild the field's indexes: `DBIndexSet::StartRebuildIndex` before the walk, every record re-indexed (`DBTransaction::ModifyRow`), `FinishRebuildIndex` after |
| `0x20` | indexing turned off | delete `[T, 01, F]` |
| `0x40` | data type, storage (stored, global, unstored) or repetition count changed (not for an old unstored calculation); on a calculation or summary, the formula or the "always evaluate" option (`0x20`) changed | no change to the field's own values; stored fields that depend on it are recalculated in every record, and indexed calculations among them reindexed (below) |
| `0x80` | kind, data type, auto-enter, validation or formula changed (also set on delete) | none on the server |
| `0x100` | set on delete | none |
| `0x200` | the name or other member data changed | none |
| `0x400` | a normal field (kind 1) changes to date from a type other than date and timestamp | rewrite each stored value as a date (7.2.1) |
| `0x800` | a normal field changes to timestamp from a type other than date and timestamp | rewrite each stored value as a timestamp (7.2.1) |
| `0x1000` | the repetition count is reduced | none by itself (the deleting is `0x4`) |

Deleting a field sets `0x1e0 | 0x1`, plus `0x2` for a global and `0x8` for a
container (`0x1e1` for a normal text field). When a flag of the field
catalog object is set the client clears bits `0x1d`, `0x400` and `0x800`
(mask `0xfffff3e2`); the same flag makes `DBFieldCatalog::Create` add a
member, so it is probably "this catalog is being created" (Low; it would
drop the build-index bit for a new table, see section 6). A data-changing
bit (`0x1`, `0x4`, `0x8`, `0x40`) also bumps the field's data mod count.

After uploading the field catalog FileMaker Pro sends op73 (3.6). The server
then, holding the table lock `[T, 5]` type 9 for the client's session with
transaction 0 (the lock FileMaker Pro already holds, 4; this is why the
client takes it with transaction 0), goes through the deferred list:

1. For each field: rebuild (`0x10`) or delete (`0x20`) its indexes; apply a
   global's work (`0x2`); note a field with `0x40` in a list of changed
   fields.
2. If a field had `0x40`, or any field has a walk bit (`0x10` or one of
   `0xc1d` without `0x2`), it upgrades the table lock to type 11 and walks
   the table's records in one transaction (`UpdateRecords`). Auto-enter is
   off and related-set caching is off for the walk. Before it, the changed
   fields' dependents are looked up (`DBRecordCursor::DeferDependencies`
   0x192ccd8); an indexed stored calculation among them has its index
   rebuilt like `0x10`. If there are no dependents and no walk bit, the walk
   is skipped. Entries without any of `0xc1d` are dropped from the list
   before the walk (`PruneDeferredList`).
3. For each record: re-index for `0x10`; delete for `0x1`/`0x4`/`0x8`
   (`DeleteFieldData`); rewrite for `0x400`/`0x800` (`ProcessFieldData`,
   7.2.1); then recalculate the record's dependent stored fields
   (`DBRecordCursor::DoDeferredDependencies` 0x192cdb4, which evaluates the
   dependency list). The transaction is committed every 512 records and at
   the end.
4. It finishes the rebuilt indexes, saves the global record, deletes
   `[T, 3, 1, fd]` and releases the lock.

Notifications to other clients are held back while it runs
(`StopNotificationProcessing`).

#### 7.2.1 Stored values when a field's type changes (from bin; rendered text observed)

These rules describe a schema change, not a user's Browse entry; the
locally observed Browse rules are in write-path.md 1.4.

A stored value is text in every data type (file-format.md 3: numbers and
dates are stored in their text form; a container stores a reference ending
in U+E000, containers.md). A type change rewrites stored values only for a
change to date or timestamp; otherwise the text stays and is read under the
new type the way calculations convert text (calc-values.md 2.8, 4.4-4.6):

| from \ to | text | number | date | time | timestamp | container |
|---|---|---|---|---|---|---|
| text | - | kept | rewritten (`0x400`) | kept | rewritten (`0x800`) | kept |
| number | kept | - | rewritten (`0x400`) | kept | rewritten (`0x800`) | kept |
| date | kept | kept | - | kept | kept | kept |
| time | kept | kept | rewritten (`0x400`) | - | rewritten (`0x800`) | kept |
| timestamp | kept | kept | kept | kept | - | kept |
| container | deleted | deleted | deleted | deleted | deleted | - |

"Kept" means the stored text is unchanged, so a number field converted to
text shows the digits as stored and a text value converted to number keeps
its letters. "Deleted" is `0x8`: every repetition's value is removed and its
library reference released. A text kept in a field that becomes a container
is read as a container only when it ends in U+E000 (`DBData::Load`, mac
0x1936fe2); what FileMaker Pro shows for other text there is unmeasured.
Date to timestamp and timestamp to date set neither `0x400` nor `0x800`.

**Observed for text and number** (FileMaker Pro 26.0.3, local file; #441,
the research thread's comment of 2026-11-14 05:53Z): an indexed Text field
holding `10`, `2`, `-3`, `4.5` and `N/A` changed to Number, and back.
- **No alert either way:** Change, then OK.
- **Stored values:** all five `[T, 05, record, F]` values stay byte-identical, `N/A` included, and still display.
- **The definition:** only byte 1 (the data type) changes, `01` to `02`; the options and the rest stay, and the version and timestamp leaves update.
- **The by-type list:** the field's key in `[T, 3, 1, 6]` moves from type byte `04` to `08`.
- **The value index:** it is rebuilt, the five text keys replaced by five numeric keys with its counts unchanged; back as text the original keys return byte for byte.
- **Sorting:** ascending, it orders `-3`, `2`, `4.5`, `10`, `N/A` as a number and `-3`, `10`, `2`, `4.5`, `N/A` as text.

What key `N/A` gets in the number index is not recorded ([#246](https://github.com/OpenMaker-Dev/openmaker/issues/246)).

**The rewrite (`0x400`, `0x800`).** For every record and every stored
repetition of the field (the iterator visits existing repetitions only),
`ProcessFieldData` loads the stored text as the new type and validates it
the way data entry is validated (`DBData::Validate(DBDataType const&,
bool, bool)` 0x19385d8 with strict off and time-dropping on, then
`ValidateDate` 0x1937990 or `ValidateTimeStamp` 0x1937bb0):

1. **Clean** the text (`TextBuffer::NumberDateTimeCleaner`, mac Support
   0x2043dc): full-width ASCII forms (U+FF01-U+FF5E) become ASCII,
   U+3001/U+FF64 become `,`, U+3002/U+FF61 become `.`, U+2014 and U+30FC
   become `-`, other white space becomes a space, and leading and trailing
   white space is removed.
2. **Parse** it as a date and time (`DateTime::Set(Text const&, Locale
   const&)` 0x146038), the parser `GetAsDate`/`GetAsTimestamp` use on text
   (`DBData::ConvertToDate` 0x1934fe8, `ConvertToTimeStamp` 0x1935388;
   rules in calc-values.md 4.6). The parse result is one of `kNormal`,
   `kIncomplete`, `kEmpty`, `kInvalid` (DWARF `DateTime::ParsingResults`).
   A date is incomplete when its year has one to three digits or is missing
   (the current year is used), or has a Japanese era letter
   (`DateTimeParsers::GetTheYear` 0x14a736); a four-digit year is complete.
3. **Decide** the new text:
   - to date: a complete date without a time keeps the cleaned text. A date
     with a time, or an incomplete date, gets the time dropped and is
     rendered again in the file's date format with the separator it was
     typed with (`DateTime::GetText`, as `GetAsText ( GetAsDate ( text ) )`).
     Anything else (a time without a date, no date, empty) keeps the
     cleaned text.
   - to timestamp: a value needs a date and a time of day (`kTimeOfDay`).
     With both, a complete value keeps the cleaned text and an incomplete
     date part is rendered again (`GetAsText ( GetAsTimestamp ( text ) )`).
     A date without a time, an hour alone, an elapsed time or a time without
     a date keeps the cleaned text; it is not completed to midnight.
4. **Write** the new text only when its length differs from the stored
   text's (`ProcessFieldData` compares character counts), through the
   record cursor and `DBData::Save` as the new type. So adding a century or
   trimming spaces writes, and a change of the same length (full-width
   digits made ASCII) does not.

Validation errors (500, 501) are ignored: a value that does not parse stays
as stored. A number stored as `37000` is parsed as text, not as a day
number, so it stays (`GetAsDate ( 37000 )` is a date, `GetAsDate ( "12" )` is
error 500). The locale is the one stored in the file
(`DBFileUser::GetLocale` 0x1a21fc4: the file's locale, the engine default only
when there is no file), so a server parses `2.1.03` as 2 January 2003 in a
de-DE file whatever the machine's settings.

Other effects of a type change:

- **Repetitions**: every stored repetition is rewritten by the same rule,
  after the repetitions above a reduced maximum are deleted (`0x4`, 7.2
  step 3).
- **Modification counts**: whether the walk raises the modification count
  of a record whose values it rewrites or deletes is unmeasured.
- **Global fields**: the global value lives in the table's global record,
  which the record walk does not visit, so a global's value is not rewritten
  (from bin: `0x2` is only added to `0x1`, `0x4`, `0x8`).
- **Indexes**: an indexed field gets `0x10` with any type change and is
  re-indexed from the converted values with the new type's keys
  (query-program.md 7.2-7.3).
- **Dependents**: stored calculations that use the field are recalculated
  in every record after the rewrite (step 3 above), and reindexed when
  indexed. Summary fields store nothing and are computed when read
  (inference).
- **Calculation fields**: a calculation's result type change sets neither
  `0x400` nor `0x800` (they need kind 1); its formula or option change sets
  `0x40`, and its values are recomputed as a dependent of itself
  (inference from the dependency step).

Rendered text, from `tools/fms-calc-harness` runs of `GetAsDate` and
`GetAsTimestamp` on FileMaker Server 26.0.3 (observation, 2026-11-11, so the
current year is 2026; whether a value is written is from bin):

| file | stored text | to date | to timestamp |
|---|---|---|---|
| en-US | `1/2/2003` | kept | kept |
| en-US | `01/02/2003` | kept (`GetAsDate` renders `1/2/2003`, but complete dates are not rewritten) | kept |
| en-US | `1/2/03`, `1/2/3` | `1/2/2003` | kept |
| en-US | `1-2-03` | `1-2-2003` | kept |
| en-US | `01-02-03` | kept (the rendered text has the same character length) | kept |
| en-US | `1.2.03` | `1.2.2003` | kept |
| en-US | `1/2/203` | `1/2/0203` | kept |
| en-US | `1/2`, `1-2` | `1/2/2026`, `1-2-2026` | kept |
| en-US | `1/2/2003 13:00`, `01/02/2003 1:30 PM` | `1/2/2003` | kept |
| en-US | `1/2/03 13:00` | `1/2/2003` | `1/2/2003 1:00 PM` |
| en-US | `  1/2/03  ` | `1/2/2003` | `1/2/03` (trimmed, so the length changed) |
| en-US | `１/２/０３` (full-width) | `1/2/2003` | kept full-width (cleaned to the same length) |
| en-US | `１/２/２００３` (full-width) | kept full-width (same length) | kept full-width |
| en-US | `1/2/03 1:30 PM` | `1/2/2003` | `1/2/2003 1:30 PM` |
| en-US | `1-2-03 13:30` | `1-2-2003` | `1-2-2003 1:30 PM` |
| en-US | `1/2 13:30` | `1/2/2026` | `1/2/2026 1:30 PM` |
| en-US | `13:30`, `abc`, `12`, `2/30/2003`, `1/2/2003 13` | kept | kept |
| de-DE | `2.1.2003`, `02.01.2003` | kept | kept |
| de-DE | `2.1.03` | `02.01.2003` | kept |
| de-DE | `2-1-03` | `02-01-2003` | kept |
| de-DE | `2.1.2003 13:00` | `02.01.2003` | kept |
| de-DE | `2.1.03 13:30` | `02.01.2003` | `02.01.2003 13:30` |
| de-DE | `2.1.` | kept (error 500) | kept |

Two cells were not run as written and follow from the rule: `1/2/03 13:00`
to timestamp (rendered like `1-2-03 13:30`) and the trimmed `  1/2/03  ` to
timestamp. The batteries and raw results are in
`re/work/needs-re/441/harness/` (git-ignored).

A server without records support can answer op73 with the empty reply and
only delete `[T, 3, 1, fd]`; records written before the change then keep
values of deleted fields (harmless to FileMaker Pro, which ignores field ids
missing from the catalog) and indexes go stale.

### 7.3 op73 observed (High: live:s2c)

Table `80 07` with one record (fields `01`-`05` stored). The probe's second
save renamed `02` and turned its value index on, deleted `05` and added an
indexed text field `06`, uploading `[80 07, 3, 1, fd]` =
`{02: 00000210, 05: 000001e1, 06: 00000010}` with the index, then sent op73.
Between the op13 and the op73 the deferred list was in the file and nothing
else had changed. op73 then (vec `live/s2c/c_before73`, `c_after73`):

| change | key |
|--------|-----|
| removed field `05`'s value from record `01` | `[80 07, 05, 01, 05]` deleted |
| built the value index of `02` | `[80 07, 01, 02, 00, <01 + key of "OpenMaker schema record">, 01]` (empty leaf = singleton record 1), `[80 07, 01, 02, 10]` = `0000000000000001 0000000000000001` |
| built the value index of `06` (empty in the record) | `[80 07, 01, 06, 00, 01, 01]` (the empty-text key `01`), `[.., 10]` likewise |
| touched the record | `[80 07, 05, 01, fc]` = `01 01` (the record's mod count, write-path.md 5.4, now 1) |
| cleared the work list | `[80 07, 3, 1, fd]` deleted |

No word index was built (the option was value index only). op73's reply was
empty and no back-channel record reached the other registered connection
for the record change. FileMaker Server computes the index keys itself when
it builds an index: the stored key is `01` + om-collate's `text_key` weights
of the value (query-program.md 7.2).

---

## 8. Modification counts and stamps (High)

### 8.1 Values

| key | value | example (test project) |
|-----|-------|------------------------|
| `fc` (`KModCount`) | key string of a serial key, `01 nn` | `[3, 16, 1, fc]` = `01 02` |
| `fc01` (`KModUser`) | text, the user name of the client (`Get(UserName)`, from FileMaker Pro's preferences) | "Duc Ha" (the template's author) |
| `fc02` (`KModAccount`) | text, the account name | "Admin" |
| `fc03` (`KModTimestamp`) | `08` + the 8-byte date-time key of 3.1 (9 bytes) | `08 9f 91 03 80 05 51 91 50` = 2020-04-03 11:20:04 |

`DBCatalogCommon::IncrementModCount(isNew)` (bin 0x190bad0) writes them for a
catalog index or a member on every commit: unless the object is new it reads
`fc` (missing = empty), increments it as a serial key and writes it back;
then it always writes `fc01` (user name), `fc02` (account name) and `fc03`
(op82 host time). So a new member has `fc01`-`fc03` but no `fc` until its
first later modification; existing members in the samples show `fc = 01 01`
after one change. FileMaker Pro 26 also wrote `fc = 01 01` on some members
it had just created (a new table, a new field "Notes", 5.8), and not on
others (the new table's occurrence, field "Attachment"); clients compare
the count only for equality, so either is read the same. The record-level `fc` of write-path.md 5.4 is the same
convention.

### 8.2 Who uses them

- **Staleness check.** FileMaker Pro caches catalogs per open file. Before
  editing it reads `[cat, 1, fc]` with op86 GetKey (3.5) and re-downloads
  the catalog (op15) when the count differs from its copy; field catalogs
  are compared the same way (`DBFieldCatalog::IsCurrent`).
- **Notifications** (section 9) tell other clients which catalog changed;
  they then refetch it.
- **Cache revalidation.** When FileMaker Pro reopens a file it has cached,
  it sends its cached counts with op89 and the host reports the ones that
  differ from the file's (network-protocol.md 5.6.11).
- The server writes none of them and reads them only to answer op89. A
  server that rewrites a catalog on its own (OpenMaker Server's repairs, imports) must increment `[cat, 1,
  fc]` itself, or clients keep their stale copies.

---

## 9. Telling other clients (High: live:s2a-s2d)

Uploads alone notify nobody. In live:s1 the probe released the catalogs with
attention 0, and a second connection registered with op63 (write-path.md
6.1) received nothing in its back channel; the server's own
`ForwardSchemaChanges` call after each op13/UploadMultiple is internal
(a hook used by FileMaker Server's other engines, Med). What other clients
see is the **attention code of the op62 that releases the lock**
(`DBLockInfo::NotifyLevel`, write-path.md 6.2), delivered like a record
change (piggybacked on the client's next reply, or a server `Notify` call
when it is idle, write-path.md 6.4):

| release | code | path in the message | example (back channel of the other connection) |
|---------|------|---------------------|-----------------------------------------------|
| catalog after commit | `0x2` `kDBAttnCatalog` | the catalog, `[3, 16]` | `ff00 \| 00000002 001d \| 00000001 00000000 00000002 0002 001b fffffffc 0004 04 01 03 01 10 0005` (live:s2a/a16_bflush) |
| field catalog | `0x2` | `[80 07, 3]` | `... 00000002 001e ... 05 02 80 07 01 03 0006` (live:s2b/bF_bflush) |
| table lock after the save | `0x201` modify + record scope | `[80 07, 5]` | `ff00 \| 00000201 001e \| ... 05 02 80 07 01 05 0006` (live:s2b/b_flush_end) |

The message is the marshalled `DBMesg` of write-path.md 6.3 (send-to
address, reply-to 0, attention, two items: the client file id from op63 and
the path, total length first). A server should therefore queue, for every
other connection registered on the file, one record per op62 that carries a
non-zero attention mask, with that mask and the lock path. The releasing
connection got the same records when its message address in op62 was 0
(4.3).

On a catalog notice FileMaker Pro drops its cached copy of that catalog and
fetches it again when needed (`DBRemoteNotify` -> catalog cache reset; Low,
not captured); its mod-count check (3.5) catches any change it missed.

---

## 10. Live runs on the lab server and what changed (2026-10-28, test project only)

Probe: `re/work/schema/oms` (`target/release/s <script> <outdir>`), scripts
generated by `re/work/schema/tools/gen_s1.py` and `gen_s2.py`. 16
successful logins, no failed login. Host times from each run's op82.

| run | time | what it did | file changes |
|-----|------|-------------|--------------|
| s0 | 15:12 | baseline downloads; FileMaker Pro's Manage Database requests replayed one for one (om:264-326) | none (locks only) |
| s1 | 15:19 | created table `80 06`: `[3, 16]` and `[3, 17]` committed (member + index), field members uploaded into `[80 06, 3, 5]`; then an op90 of `[80 06]` blocked behind the probe's own exclusive lock (4.5) | table `80 06`, TO `d0 00 06`, five field members; no field index |
| s1b-s1d | 15:22-15:25 | read-only: a second download of `[80 06]` also blocked; op60 probes showed the dead session's locks on `[08]`, `[80 06, 3]`, `[80 06, 5]`; op86 NodeExists showed what s1 had written | none |
| (maintainer) | ~15:28-15:36 | FileMaker Server stopped for maintenance and restarted; this released the stuck locks | not mine: see below |
| s0b | 15:37 | new baseline after the restart | none |
| s2a | 15:37 | DeleteTable `80 06`; UploadMultiple delete lists for `80 06` and `d0 00 06`; `[3, 16, 1]` and `[3, 17, 1]` restored to the s0 bytes | s1 fully removed |
| s2b | 15:37 | created table `80 07` with TOs `d0 00 07`/`d0 00 08`, relationship `04` between them, five fields; op73 | as described |
| s2c | 15:37 | one record in `80 07`; renamed+indexed `02`, deleted `05`, added `06`, op73 (7.3) | as described |
| s2d | 15:38 | DeleteTable `80 07`; delete lists for `80 07`, `d0 00 07`, `d0 00 08`, relationship `04`; `[3, 16, 1]`, `[3, 17, 1]`, `[3, fb, 1]` restored to the s0 bytes | all removed |
| s3 | 15:39 | read-only final comparison | none |

**Final state** (s3 and s2d downloads): `[3]` (all three catalogs with their
indexes) byte-identical to the 15:12 baseline; `[2]` and `[80 02]`
byte-identical to the post-restart baseline s0b; `[80 03, 3]` and
`[4, 1]` identical to s0; `[08]`, `[80 06]`, `[80 07]`, `[80 08]` missing
(NodeExists `[80 06]`, `[80 07]` false). The catalog counters and mod
counts were restored to their baseline values rather than advanced as
FileMaker Pro would (so table id `80 06` will be issued again); nothing of
the probe remains.

**Not mine, seen in the comparisons:** between s0 and s0b `[80 02, 00]`
went from `01 0d` to `01 0e` and a container library `[80 02, 1f]` appeared,
both written by another "openmaker" session at 15:28 (`fc01` "openmaker",
lower case; the probe writes "OpenMaker"), and `[2, 08]`/`[2, 09]` counters
changed with the restart. The stray root keys `[12]` and `[fc]` left by
another probe were not compared or touched.

**Side effects.** From 15:19 to the restart the dead s1 session's locks on
`[08]`, `[80 06, 3]` and `[80 06, 5]` stayed in FileMaker Server: anyone
opening Manage Database on test project in that window would have been
told the schema was locked by "openmaker". The probe's two stale sessions
were removed with `fmsadmin disconnect client` (96, 97); `fmsadmin list
clients/files` was used read-only. No server configuration was changed.
The maintainer's FileMaker Pro (client 65) had test project open until the
restart; the s1 releases carried attention 0, so it was not sent notices.

---

## 11. Test vectors

In `re/work/schema/` (git-ignored):

| path | content |
|------|---------|
| `capn/<rid>-opNNN/{txn,result,backchannel}.bin` | every exchange of om: (FileMaker Pro's requests, OpenMaker Server's replies) |
| `live/s0/r*` | FileMaker Server's answers to FileMaker Pro's Manage Database requests (2.1-2.4): ops 60, 61, 62, 82, 15, 86 |
| `live/s0/x_op15_*` | op15 on a node, a terminal value and a missing key |
| `live/s1/a_um16`, `a_um17`, `a_umf` | UploadMultiple of a table, an occurrence and five fields |
| `live/s1/a_idx16`, `a_idx17` | op13 of a catalog index |
| `live/s2a/a_deltable`, `s2d/d_deltable` | DeleteTable with access key |
| `live/s2a/*_um` | UploadMultiple with a delete list only |
| `live/s2b/*` | a full create in FileMaker Pro's order, with the other connection's notices (`*_bflush`) |
| `live/s2c/c_before73`, `c_after73` | `[80 07]` before and after op73 (7.3) |
| `live/s*/*.slots` | op90 downloads (`v_*` verification, `b_*` baselines) |
| `tpl/fields.txt` | every field definition of the seven English samples and test project |

Each `<tag>.req` is the whole transaction (20-byte header + body), `.rep`
the reply result stack, `.bc` the back channel. A server implementation can
be checked like the write path: apply the s2b/s2c requests to a copy of
test project's key tree and compare with the `v_*`/`c_after73` downloads.

---

## 12. Open questions

1. **Index nodes of a new table.** Which request makes FileMaker Pro's new
   indexed fields (the default `PrimaryKey`) get their `[T, 01, F]` node:
   a deferred `0x10` in the first save, op85 per field, or the first find
   (section 6). Until known, a server should create the index node for every
   field whose definition has an index bit when the field catalog is
   uploaded, or index on commit by the option bits.
2. (Partly answered by 2.3a: FileMaker Pro uploads whole catalogs with
   op13.) FileMaker Pro's own requests after a successful op61 were derived from
   its code and replayed by the probe in that order; they are not captured.
   In particular: the exact message address and attention in its catalog
   op62, whether it sends op82 before each catalog commit, and the order of
   op13 against UploadMultiple for a brand-new catalog.
3. The `[T, 5]` table lock's exact use when fields of several tables change,
   and the client's reaction to its 2-second refusal (303 dialog).
4. ~~Relationship options and predicate operators other than `=`;
   multi-predicate relationships~~: resolved in
   [file-metadata.md](file-metadata.md) section 4. External (ODBC/other
   file) occurrences remain open.
5. Occurrence definition: what FileMaker reads the field-list height for;
   display values other than 0 and 1 (FileMaker Pro offers a third
   state, the name only, presumably 2); how colours other than the default
   grey are tinted; FileMaker's rule for the "one" end of a relationship
   beyond unique-value validation (serial numbers, other operators,
   several predicates), and where a line meets a box that shares the
   other box's column.
6. `[3, 17, 1, 0e]` and `[T, 3, 1, 0e]` dependency structures in full
   (5.8, 5.10, 5.12, 5.13, 5.16 and 5.17 have what is known, including how the
   evaluation lists are rebuilt): the meaning of `F, 00`'s
   `03 21`.
7. Layouts FileMaker Pro creates for a new table (layout catalog `[4]`,
   layout-model.md) and the order of that commit relative to the schema.
8. ~~The calculation token streams `[F, 05, 05]` / `[F, 05, 06]`~~:
   calc-encoding.md and 5.10.
9. op61 conflict reply and the exclusive (type 10 at open) catalog mode.
10. The op85 BuildIndex layout.
11. A type change live (7.2.1 is from bin, with the rendered text from the
    calculation harness): what FileMaker Pro shows for values the rewrite
    keeps (text that is not a date, full-width digits, text in a field that
    became a container), and whether FileMaker Pro itself fills a field
    whose storage becomes global, since the server deletes the record
    values (`0x1`) and does not rewrite a global's value. For a future
    authorized check, fill a Text field with the texts of the 7.2.1 table
    and preserve that baseline. In independent disposable copies of the
    original Text baseline, change one field to Date and another to
    Timestamp, downloading the records after each op73.

**What the maintainer can do to close 1-3 and 7:** with the capture relay running
(README, "Live observations") to FileMaker Server, open test project in
FileMaker Pro, File > Manage > Database: create a table "Capture Test" (the
default fields stay on), add a text field with indexing "All", add a
relationship from it to Contacts, press OK; reopen, rename the text field,
change it to number, delete it, OK; reopen, delete the table, OK. Name the
capture `2026-10-2x-s07-schema`. Then delete any layout FileMaker Pro created
for the table (Layouts > Manage Layouts).

---

## Implementation notes (recalculation)

OpenMaker Server recomputes stored calculations after a schema change, as
FileMaker Server does (2026-10-29: `om-server` `recalc` and
`Store::update_table`, `om_data::rules::recalc`). FileMaker Server
recalculates, in op73's record walk, the stored fields that depend on a
field with `0x40` (7.2, from bin; not captured); points marked (inf) are
OpenMaker's decisions.

**When.** At op73, after the deferred work of 7.2 (deleted fields' values
dropped) and before the index work, in the same pass. The server keeps per
table the *basis* its stored values were computed with: each field's kind,
data type, the option bits that change stored values (global `0x10000`,
unstored `0x20000`, "do not evaluate if all referenced fields are empty"
off `0x20`), repetitions, and a calculation's context and Polish stream
(`[F, 05, 04]`, `[F, 05, 06]`). Names, comments, indexing, auto-entry and
validation are not in it, so renaming a field changes nothing. A field
that is new, deleted, or whose basis differs is stale; so is every field
the deferred list names with a data bit (`0x1`, `0x4`, `0x8`, `0x40`,
`0x80`, `0x400`, `0x800`, `0x1000`; a formula change sets `0x80 | 0x40`,
7.2). Which bit FileMaker Pro sets for a new stored calculation is not
established; the basis catches it either way (inf). Every stored
calculation (kind 2, neither global nor unstored) that is stale, or reads
a stale field directly or through other stale calculations, is recomputed
on every record of the table, in dependency order. The basis is taken when
the file is loaded and after each op73; a table with none (created in this
session) has all its stored calculations recomputed. Stored calculations
cannot read other tables (`CalcMustBeUnstored`, calc-values.md 8.8), so a
change never makes another table's stored values stale.

**How.** `Recalculation` plans per field with the record rules' evaluator
(auto-entry.md, docs/record-writes.md): the formula's own table, stored data and
stored calculation inputs, the `Get` selectors record rules allow, custom
functions that read no fields. A calculation it cannot evaluate (a
related, global, summary, unstored-calculation or container input, a
plug-in, a selector that needs a window, repetitions, a container result,
a formula that names a deleted field) is skipped: its values stay as they
were and the server logs the field and why. A calculation that fails on a
record keeps that record's value; failures are counted and logged once per
field. With "do not evaluate if all referenced fields are empty" the value
is empty when they all are. Auto-enter calculations are not recomputed
(they apply when data is entered) and nothing is validated (inf). `Get`
answers for the account that sent op73 (also as the user name), the
server's clock, and a fresh UUID per record when a formula asks for one.
Values are the calculation's text in the file's formats (`[2, 0d]`,
file-metadata.md 2), as a client's write stores them. A record whose values
change gets them and its modification count `fc` one higher, as op73's
deletion of a field's values does (7.3; inf for recalculation). A
recomputed field with an index node has its value and word indexes rebuilt
from all records once, after the walk.

**Notifications.** None per record, as FileMaker Server holds them back
while op73 runs (7.2) and none reached the other connection in live:s2c
(7.3). Other clients learn of the change from FileMaker Pro's releases: the
field catalog's attention 2 before op73 and the table lock `[T, 5]` with
`0x201` after it (2.3a, 9). op73 finishes before its reply, so the `0x201`
notice always follows the new values.

**Memory.** The file is in memory as its key tree (the server's design);
the walk changes each record in place, one record's text at a time, and
the index rebuild holds one index's keys, as large as the index itself. No
copy of the table is made. The walk holds the store's write lock, so other
clients' requests wait for it.

**Crash safety.** Everything happens in memory under the write lock and is
written by the one save that follows op73: a complete new file written
beside the old one, flushed and renamed over it (hbam-writer.md 6.2). The
deferred list `[T, 3, 1, fd]` is removed in the same change, so the file on
disk holds either the state before op73 (old values, the list present) or
the state after (new values, no list), never half a table. The op13 before
op73 is saved at once, so a server that stops between the two (or during
op73) leaves the new field catalog, its deferred list and the old values.
When such a file is loaded, a table with a deferred list gets no basis and
`Store::recover` runs its deferred work, recomputing all its stored
calculations, before the file is served, and saves (inf: what FileMaker
Server does with a list a stop left is unknown). Recomputing is
idempotent, so finishing the work twice changes nothing more than once.

**What else read the schema (2026-10-29, review fix).** The store counts
schema changes (`Store::schema_generation`: every catalog op13 or op86
replaces, and op73 when its field work changed anything). The store's own
catalog is re-read at each change and the per-table basis at each op73;
the recalculation reads the field catalog, custom functions and
relationships afresh each time. A server script session caches the
schema (record rules, the calculation schema with ExecuteSQL's catalog,
summaries, layouts, scripts): it reads it again before a step when the
count moved (script-runtime.md "Implementation notes (PSOS)"), and rebases
the records it holds on what op73 left, so a script running across a save
writes with the new rules and does not write old stored values back.
Before, a session kept the rules it started with until it ended. Value
indexes are never cached: unique checks, finds and joins read the store.

Test: `crates/om-server/tests/recalc.rs` (FileMaker Pro's save sequence of
2.3a with a formula change, a new dependent calculation, a calculation the
server cannot evaluate, a rename, a deleted input, and a file left with
deferred work; a script paused across a formula change).
