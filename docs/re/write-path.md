# The record write path - create, edit, commit, delete, lock, notify

Status: complete for creating, editing and deleting records of a normal
FileMaker table over the network (2026-10-28). Open items are listed in
section 11. Each claim carries a confidence (High/Med/Low) and its evidence:

- `cap:<rid>` - an exchange in `re/captures/2026-10-27-s03-direct/conn-000`
  (FileMaker Pro 26 creating and editing records in a hosted solution on
  FileMaker Server 26). Extracted per exchange by
  `re/work/query/tools/capx.py` under `re/work/query/cap03/`.
- `om:<rid>` - an exchange in
  `re/captures/2026-10-28-s06-omserver-e/conn-001` (FileMaker Pro against
  OpenMaker Server, whose op71 answered status 3).
- `omn:<rid>` - an exchange in
  `re/captures/2026-10-28-s06-omserver-n/conn-001` (FileMaker Pro 26 creating
  related records in portals of test project on OpenMaker Server).
- `live:<run>/<tag>` - a transaction sent by the RE probe
  (`re/work/write/omw`, scripts `re/work/write/live/s*.txt`) to the lab
  FileMaker Server, file `test project` (file id `0x165`), table `80 02`
  (Contacts). Request, reply and back-channel bytes are saved as
  `re/work/write/live/<run>/<tag>.{req,rep,bc}`.
- `vec:<name>` - a test vector in `re/work/write/vectors/` (section 10).
- `bin:<name>` - the Linux FileMaker Server engine `libDBEngine.so` 26.0.3
  (DWARF; addresses are `nm` values) or the Mac client engine `DBEngine`
  (`mac:`, x86_64 slice; addresses are `nm` values).

Notation: integers in transaction bodies are big-endian (network-protocol.md
4.2). `[a, b, c]` is an HBAM key path (file-format.md 3) with each segment
written as its bytes in hex. Record ids, table ids and field ids are
`HBSerialKey` segments (query-program.md 2.6): `01`..`7f` are themselves,
`83 4e` is `(0x834e & 0x3fff) + 0x80` = 974, `c4 19 51` is
`(0xc41951 & 0x1fffff) + 0x4080` = 285,137.
A **key** field in a transaction body is written with `Push(uchar const*)`:
the key's segments (`[u8 seglen][seg]...`) followed by a `u8` total length,
e.g. table `80 02` = `02 80 02 03`, the path `[80 02, 05, 04]` = `02 80 02 01
05 01 04 07`, the empty path = `00`.
"Push order" lists fields in the order the sender appends them; the receiver
pops them from the end (network-protocol.md 4.2), so the last field listed is
read first.

---

## 0. Summary for implementers

Creating a record in FileMaker Pro is four or five transactions; editing is four;
deleting is three. Record data changes only in op70; op71 and op72 only
advance counters (`[T, 00]`, a field's next serial value).

| step | FileMaker Pro sends | the server does |
|------|---------------------|-----------------|
| New Record | op71 HostCreateRecord (table, count) | reserve `count` record ids after the table's last-issued id `[T, 00]`, lock them for the client, return the first |
| (auto-enter serial) | op72 HostGetSerial (table, field, increment) | read and advance the field's next serial value `[T, 03, 05, F, 0c]`, return the old value |
| typing | nothing | - |
| Commit | op14 Upload (record subtree) | store the uploaded record in the connection's temporary area |
| | op70 HostCommit (commit list) | for each listed record: write it from the temporary area into `[T, 05, R]` (new/modified) or remove it (deleted); update `[T, 0d]` and every field index `[T, 01, F, ...]`; notify other clients |
| | op103 TransactInfo (file list) | nothing observable; answer `0x15` |
| (unique field) | op60 Lock (`[T, 01, F]`, type 9), op86 ExistsInIndex | lock the field's index for the transaction; answer whether another record holds the value (1.3) |
| edit an existing record | op60 Lock (record path, type 9) | grant or refuse the record lock; return the record's modification count |
| leave the record / close | op65 KillAllLocks (transaction) or op62 Unlock | release the transaction's locks |

The temporary record id `0x8000` seen against OpenMaker Server (section 2) is
not a protocol feature: it is what FileMaker Pro falls back to when op71
fails. When op71 succeeds the client uses the id the server returned from the
start and never sends a temporary id.

The minimum a server must implement for New Record + Commit to work
end-to-end in FileMaker Pro: op71 (3.1), op14 into a per-connection
temporary area (3.3), op70 with the file updates of section 5, op103 (3.5),
op65 (4.4). Edit adds op60 (4.2) and op62 (4.3); op72 is needed only for
fields with an auto-enter serial number. Other connected clients only see the
change if the server also sends the notifications of section 6.

---

## 1. Ordering (High)

### 1.1 New record (cap:914-948, the solution's tables `80 01`/`80 02`)

```
op71  HostCreateRecord   table 80 02, count 1,  txn 1       -> record 83 4e
op72  HostGetSerial      table 80 02, field 06, inc 1        -> "<serial>"
op14  Upload             record 83 4e (field 06 = "<serial>") to [80 02, 05]
op70  HostCommit         [80 02, 05, 83 4e] = 01 (new),  txn 1
op103 TransactInfo       files {0x84}, txn 1                 -> 0x15
op71  HostCreateRecord   table 80 01, count 25, txn 0x3a     -> first record c4 19 51
op14  Upload             record c4 19 51 (4 fields)
op70  HostCommit         [80 01, 05, c4 19 51] = 01,  txn 0x3a
op65  KillAllLocks       txn 0x3a                     (user left the new record)
op60  Lock               [80 01, 05, c4 19 51], type 9, txn 0x39  -> mod count 01 00
op14  Upload             the whole record, now with field fc = 01 01
op70  HostCommit         [80 01, 05, c4 19 51] = {changed field ids}, txn 0x39
op60  Lock               same record, txn 0x3d -> mod count 01 01
...
```

The official client creates the record on the server at New Record time, not
at commit time: op71 is sent the moment the user clicks New Record (om:74),
and the uploaded record is committed the moment the user leaves it. A record
created with op71 but never committed is simply abandoned (its id is used up;
section 5.1).

A script or a related record creation (portal, lookup) can produce several
creates and commits in one transaction across several files; op103 then lists
every file id of the transaction (cap:1010: files `0x108`, `0x84`).

### 1.2 Transaction ids

Every write transaction carries a client-chosen 32-bit **transaction id**
(`txn` below). It groups the locks taken for one record-editing session: op71
and op60 take locks owned by the txn, op70 commits under it, op65 releases
everything the txn holds. FileMaker Pro allocates a new txn per record
session (cap: `0x01`, `0x39`, `0x3a`, `0x3c`, `0x3d`, ...; live probes used
arbitrary values). A server must treat txn as an opaque key scoped to the
connection (4.1).

### 1.3 Unique fields (High for the order, omn:160-240)

Before it uploads a record whose field `F` must hold a unique value,
FileMaker Pro write-locks the field's index node `[T, 01, F]` (query-program.md
7.1) for the record's transaction and asks the server whether another record
already holds the value (op86 ExistsInIndex, query-program.md 8.4). It keeps
that lock until the record is committed, then releases it with op62:

```
op71  HostCreateRecord  table 80 04, txn 0x16                -> record 01
op60  Lock              [80 04, 01, 06], type 9, txn 0x16    -> 0000 granted
op86  ExistsInIndex     table 80 04, field 06, the new PrimaryKey's index key,
                        exclude {01 = 01, 02 = 01}           (8.4)
op14  Upload            the new records
op70  HostCommit        [80 04, 05, 01] = 01, ...,  txn 0x16
op62  Unlock            [80 04, 01, 06], type 9, txn 0x16
```

omn:176 is the lock (body `00000001 6dab3e58 | ffffffff | 00 | 02 8004 01 01
01 06 07 | 00000000 | 00000001 | 00000016 | 0009`: no mod count, argument 0),
omn:178 the check, omn:194 the unlock; omn:208-224 repeat the pattern for table
`80 03`, field `0b`. The program carries the instruction's access key (2.4 of
query-program.md) and puts the key in upload spot `[2, 1]`, the records to
ignore in `[2, 2]` and the answer in `[3, 3]`. By inference the lock
serializes uniqueness checks of one field between clients, so two records
with the same value cannot both pass the check before either is committed.

### 1.4 FileMaker Pro Browse entry of date, time and timestamp fields (observed locally)

In FileMaker Pro 26.0.3, leaving a field with Tab validates its typed value
before the record is closed. On a disposable local en-US file, each accepted
value below was read back from the record after closing the file. The record
stores text, including the displayed year and separator; it does not keep
the original typed text in these cases.

| field | typed | displayed and stored |
|-------|-------|----------------------|
| Date | `1/2/26` | `1/2/2026` |
| Date | `1/2` (test year 2026) | `1/2/2026` |
| Date | `01-02-03` | `1-2-2003` |
| Date | `１/２/２００３` (fullwidth text pasted) | `1/2/2003` |
| Time | `13:30` | `13:30` |
| Timestamp | `1/2/26 13:30` | `1/2/2026 13:30` |

Date entries `1/2/2003 13:00`, `abc`, `2/30/2003` and `13:30` were
refused on field exit, as were Time `abc` and Timestamp `1/2/2026` or
`13:30` alone. All offered **Revert Field** and **OK**. The exact alert
text, respectively, was:

- Date: The value of this field must be a valid date in the range of
  years 1 to 4000 and should look like “12/25/2013”.
- Time: The value of this field must be a valid time (hours, hours and
  minutes, or hours, minutes and seconds) and should look like
  “23:09:03”.
- Timestamp: The value of this field must be a valid timestamp in the
  range of years 1 to 4000, include a valid time and should look like
  “12/25/2013 23:09:03”.

After **Revert Field**, none of those rejected values was stored. With
**Strict data type: 4-Digit Year Date** validation on the Date field,
`1/2/26` was refused on field exit, before its ordinary year expansion.
The alert said: This field is defined to contain a valid date in the
range of years 1 to 4000 and should look like “12/25/2013” (four-digit
year). You must enter a valid value. It offered the same two buttons.

File Options > Text > Data Entry also changes the interpretation of a
typed date in FileMaker Pro; the de-DE file/en-US system observation is in
file-metadata.md 2.2. The type-change rewrite in schema-path.md 7.2.1
uses different write rules: Browse entry above normalized `01-02-03` and
fullwidth digits even though the text length did not change. Hosted input
and commit traffic, other date/time forms and other locale pairs remain
unmeasured. Evidence: the local FileMaker Pro 26.0.3 observations on
issue #534.

---

### 1.5 Two files in one transaction (cap:948-1010)

One connection of FileMaker Pro 26 to FileMaker Server 26, two hosted files
(file ids `0x84` and `0x108`), one body transaction id `0x3c` (High for the
order, decoded offline from the retained capture; #1006):

```
op60  Lock              file 0x84,  type 9, txn 0x3c       -> granted        cap:948
op71  HostCreateRecord  file 0x108, count 1, txn 0x3c      -> record key     cap:952
op14  Upload            file 0x108, flags 82 (temporary)   -> empty page     cap:1002
op70  HostCommit        file 0x108, flags 01, txn 0x3c     -> empty page     cap:1004
op14  Upload            file 0x84,  flags 82 (temporary)   -> empty page     cap:1006
op70  HostCommit        file 0x84,  flags 01, txn 0x3c     -> empty page     cap:1008
op103 TransactInfo      files {0x108, 0x84}, txn 0x3c      -> 0x15           cap:1010
```

- **One transaction.** The edited record's lock in one file and the reserved new
  record in the other are held under the same body txn, and each file's records
  are committed with their own op14 and op70 under it.
- **Order.** The file with the new record is uploaded and committed first,
  the other last; op103 lists the two files in that order.
  [I: `0x84`, opened first and locked for an edit, reads as the window's file,
  and `0x108`'s new record as a related row.]
- **Txn fields.** op14 carries no txn field. The request header's transaction
  ids differ from request to request and are not the body txn.
- **Not decoded.** 25 other requests in that span (19 op90 reads, three op86,
  one each of op78, op63 and op80) were not decoded.

**Not shown:**
- what produced it (a window's commit or a script's transaction);
- a refused later commit, rollback or atomicity;
- how the transaction's locks are released. No op62 or op65 with txn `0x3c`
  is in the retained connection.
- two hosts.

The two empty-page replies do not show that both writes were applied
together. OpenMaker's coordinator follows this order
([record-writes.md](../record-writes.md), "Related records of another
file").

The capture does not establish how a third participant's held rows are
read by another participant's auto-entry, validation or lookup rules, nor
how cyclic dependencies are handled. OpenMaker's dependency-ordered
preflight and immutable peer snapshots (#1198) are its own implementation
choice, tested with synthetic files, not observed FileMaker behavior. No
new wire operation or commit-order parity is inferred from that test.

## 2. Record ids and the "temporary id 0x8000" (High)

**There is no temporary record id when op71 works.** op71 asks the server for
`count` new record ids; the server returns the first and the client uses it
for the new record immediately (in the op14 upload, the op70 commit list, and
every later download). In cap:914-1064 no request names a record outside the
blocks the server reserved.

**What OpenMaker Server saw.** om:74 sent op71, OpenMaker Server answered
status 3 (unavailable). FileMaker Pro then kept the new record client-side
and, for about a minute, re-fetched the table's records: 20 op90 for
`[80 02, 05, "", "", ""]` (three empty key segments, i.e. "the record with no
id yet") and then 2,218 op90 for `[80 02, 05, 80 00]` plus 891 op92
`[80 02, 05]` with key list `{80 00}` (om:158 onwards). `80 00` is a
serial-key segment whose value is 128 (query-program.md 2.6); it is the
client's placeholder for a record the server never created, built by the
client's local record cache after the failure. The loop ends only when the
user gives up. The fix is to implement op71; the client's placeholder needs no
server support.

**How ids are assigned.** Record ids of a table are allocated from the
table's **last-issued record id** at `[T, 00]` (section 5.1), strictly
increasing, never reused: op71 with count `n` returns `last + 1` and stores
`last + n` (live:s1/a_op71: `0103` -> `0104`, id `04`; live:s3/a_op71x3:
`0104` -> `0107`, first id `05`). Ids of records that are deleted, or reserved
and never committed, are not reused (after all probe records were deleted the
value stayed at its high-water mark, section 9).

**Reserved-but-unused ids.** FileMaker Pro asks for 25 ids at a time for some
tables (cap:928, count 25) and then commits several of them without another
op71 (cap:1022-1026: one op71 with count 25 for file `0x111`, then
`[80 01, 05, c7 7f 6d]`, `... 6e`, `... 6f` committed together). So op70 must
accept a new record whose id lies inside a block reserved by an earlier op71
on the same connection, and the client numbers the following records itself
(first id + 1, + 2, ...): live:s3/a_commit2 committed `06`, reserved by a
count-3 op71 but never returned to the client. FileMaker Server does not check
new-record ids against reservations at all: live:s8 committed record `09` as
new (it had been created and deleted earlier) without any op71. A server
should accept a new record for any id up to `[T, 00]` that has no record, and
should refuse (or at least not corrupt the file for) ids above it.

**OpenMaker Server** (its choices; FileMaker Server's answers are
unmeasured): `[T, 00]` comes from the file, and a catalog upload can set
it. op71 checks, in this order, and refuses before it reserves anything: a
count of 0 or over 1,000 (status 3); more ids than the connection may
still lock (8003, 4.1); ids that would pass the largest a serial key holds
(`u64::MAX`, status 3), so ids never wrap around to those records have;
and `[T]` holding a value that `[T, 00]` cannot go beneath (status 3;
every store write is checked against the tree as it is, script-runtime.md,
implementation notes). op71 then saves the file, so the new `[T, 00]` is on
disk before the reply hands the ids out (#927): FileMaker Pro 26.0.3 keeps
a new record and its id through a server restart (its automatic
reconnect, a fresh Connect, Open and Login) and commits it later with that
id, and op70's reply cannot report a record it leaves out (3.4). A save
that fails takes the reservation back, `[T, 00]` as it was, and op71 is
refused with the save's error (800, 809 or 810, `save_error`) before any id
is locked, so no id is issued that a restart could hand out again. op70
leaves out a new record whose id another record already has, as it leaves
out one above `[T, 00]`, so a new record never replaces one. How FileMaker
Server keeps a reservation across its own restart is unmeasured.

---

## 3. Transactions

### 3.1 op71 HostCreateRecord (`RCCreateRecord`) (High)

Request body, push order (bin `mac:RCCreateRecord::Call` 0x192d810;
cap:914, cap:928, live:s1/a_op71):

| field | type | meaning |
|-------|------|---------|
| table key | key: `[seg bytes][u8 len]` | the base table id as a one-segment key, e.g. `02 80 02 03` |
| count | u32 | number of record ids to reserve (1, or 25 for some tables) |
| txn | u32 | transaction id that will own the new record's lock |

`02 80 02 03 00000001 00000001`: the key is pushed with `Push(uchar const*)`
= the key's segment bytes `02 80 02` (`[u8 seglen 02][80 02]`) followed by a
`u8` total length `03`.

Reply, push order (bin `RCCreateRecord::Perform` 0xe948d0):

| field | type | meaning |
|-------|------|---------|
| (conflict only) self conflict | u8 | when status is `0x12e`: 1 if the conflict is with this connection |
| (conflict only) user list | string | when status is `0x12e`: names of the users holding the conflicting lock |
| status | u32 | 0 = created; otherwise a DBError (below) |
| record key | key: `[seg bytes][u8 len]` | the first new record id as one segment, e.g. `02 83 4e 03` |

The client pops the key, then the status, then (only for `0x12e`) the user
list and the self-conflict byte.

Note the status is a **u32** here (most ops use u16) and the record key is
pushed *after* it, so the key is popped first. Examples:

| vector | request body | reply | meaning |
|--------|-------------|-------|---------|
| cap:914 | `028002 03 00000001 00000001` | `00000000 02834e 03` | record `83 4e` |
| cap:928 | `028001 03 00000019 0000003a` | `00000000 03c41951 04` | record `c4 19 51`, 25 reserved |
| live:s1/a_op71 | `028002 03 00000001 000101a1` | `00000000 0104 02` | record `04` |
| live:s3/a_op71x3 | count 3 | `00000000 0105 02` | records `05`, `06`, `07` reserved |
| live:s3/a_op71_badtable | table `80 09` | `0000006a 0100 02` | 106 = table missing; key `[00]` |

Server behaviour (bin `RCCreateRecord::HostCreateRecord` 0xe93780; live):

1. Resolve the table; if it does not exist answer 106 with the key `01 00`
   (one segment `00`).
2. Take an intent-exclusive lock (type 4) on the table's `[T, 05]` node for
   the duration of the call.
3. Read `last = [T, 00]` (a serial-key value, section 5.1; missing = 0),
   write back `last + count`.
4. Lock each reserved record path `[T, 05, last + i]` (i = 1..count) with
   type 9 (`kDBWriteLock`, section 4) for `txn`, so no other connection can
   create or edit them while the client fills them in (live:s3/b_lock6_nowait:
   8003 on a reserved id). If a lock is refused the status is that lock error.
5. Answer status 0 and the record key `last + 1`.

No record data, index or rid-list entry is written: after op71 the record
does not exist (live:s1/a_after71_rec: op90 of `[80 02, 05, 04]` answers node
state 0, missing; `[80 02, 0d]` unchanged). Only `[T, 00]` changes.

Errors: 106 table missing (live), 0x12e (302 = table locked) with a user list
when the table lock is held by another user (from code). The create privilege
is checked by the client before it sends op71 (`DBCursor::GetCreateAccess` in
bin `mac:DBRecordCursor::CreateKeys`); no privilege check was seen in the
server's handler (Med). For ODBC/external tables the client does not send op71
(it answers 3 itself).

### 3.2 op72 HostGetSerial (`RCSerialize`) (High)

Gets (and advances) the next value of a field's auto-enter serial number.
FileMaker Pro calls it right after op71 for each auto-enter serial field,
before the user sees the record (cap:916).

Request body, push order (bin `mac:RCSerialize::Call` 0x19f1fd4):

| field | type | meaning |
|-------|------|---------|
| table key | key | `02 80 02 03` |
| field key | key | the field id, e.g. `01 06 02` (field `06`) |
| value | string | the value to store when `flags` has bit 0 (`kReplace`); empty otherwise |
| increment | i32 | serial increment, normally 1 |
| flags | u32 | bit 0 `kReplace` = set the next value to `value`; bit 1 `kIncrement` = advance; 0 = read only |

Reply, push order (bin `RCSerialize::Perform` 0x1000180):

| field | type | meaning |
|-------|------|---------|
| (conflict only) self conflict | u8 | when status is `0x12f` (303) |
| (conflict only) user list | string | when status is `0x12f` |
| status | u32 | 0 ok, 102 field missing, 303 schema locked |
| value | string | the serial value before advancing (the value for this record) |

The client pops the value, then the status (then the conflict fields).

Examples:

| vector | request body | reply | meaning |
|--------|-------------|-------|---------|
| cap:916 | `028002 03 0106 02 [empty string 00000000] 00000001 00000002` | `00000000 <6 bytes> 00000006` | value: a six-digit serial; the next becomes that number + 1 |
| live:s3/a_op72_read | field `01`, flags 0 | `00000000 6b 00000001` | "1": a field with no serial node reads as "1"; nothing written |
| live:s3/a_op72_missing | field `7f`, flags 0 | `00000066 00000000` | 102 field missing, empty value |

(The value in each string is SCSU + XOR 0x5A with a trailing u32 length,
network-protocol.md 4.2; the six bytes of cap:916 are the six digits.)

Server behaviour (bin `RCSerialize::HostSerialValue` 0xfff820): lock the
field definition (type 9 on `[T, 03, 05, F]`; a conflict maps to 303), read
the field's next serial value from `[T, 03, 05, F, 0c]` (`KSerialValue`,
stored as the field's text, cap:910 shows the serial field's `0c` = the same
six digits), then:
flags & 1: store `value`; flags & 2: compute the next value by incrementing
the **text** by `increment` (FileMaker serials keep their text: "INV0099" +1
= "INV0100"; the increment applies to the rightmost run of digits, Med, from
the function name `DBDataArg::IncrementSerialNumber`) and store it. Return the
value read before the update. A field without `0c` reads as "1".

### 3.3 op14 Upload List To Host Temp Location (`RCUploadListToHostTempLocation`) (High)

Uploads the record (all its stored fields) into a per-connection temporary
area on the server; op70 then commits from there. op14 never changes the file
(live:s1/a_after14_rec: the record is still missing after op14).

op14 is an upload/download op: its request and reply use the resume block of
network-protocol.md 5.6.6c. Request, push order:

```
[ payload, N ][ u32 N ][ u64 perform 0 ][ u64 cursor 0 ][ i16 depth -1 ][ u8 copy-key length 0 ]
[ u16 terminal length ][ u8 flags ][ dest key path ][ u8 dest path length ]
```

| field | value in every observed request | meaning |
|-------|----------------------------------|---------|
| payload | slot list | the uploaded subtree, relative to the destination (below) |
| terminal length | `00 00` | length of an inline value when uploading to a terminal key; 0 here |
| flags | `82` | bit 7 = upload into the connection's **session temporary area** (not the file); low 7 bits = destination node state (2 = non-terminal) |
| dest key path | `02 80 02 01 05` = `[80 02, 05]` | the table's records node |
| dest path length | `05` | |

So the full addition is `00 00 82 02 80 02 01 05 05` (cap:920, every op14 in
cap and live).

The payload is one bracketed slot list (file-format.md 3.6; the op90 wire
form): one child per record, keyed by record id, holding one leaf per stored
field. Values are the stored field bytes (text = SCSU ^ 0x5A, numbers and
dates as their text). Example cap:920: `28 834e 80 04 06 <6 bytes> 40 40` =
`{ 83 4e: { 06: <the six-digit serial> } }`. The upload holds **every** stored field of the
record, not only the changed ones (cap:944: 54 field ids), plus field `fc`
(section 5.4) once the record has been committed before. Several records may
be uploaded in one op14 (cap:1024: three records; live:s3/a_up2: two).

Reply: the empty download page `[N 0][perform 0][cursor 0][depth -1][copy key
length 0]` with no addition (`00000000` + 16 zero bytes + `ffff 00`, 23 bytes).
Large uploads are split as in 5.6.6c (not observed).

Server behaviour: unpack the payload into a temporary key tree belonging to
the connection (bin `RCUploadToHost::PerformPopAddition` 0x1206d20 sets
`mUploadToSessionTempPos` from bit 7; `RCConnection::StorePositionAllocation`
keeps it). Later op14 uploads on the same connection merge into the same
area (FileMaker Server keeps one temporary area per guest; op70 deletes
records from it as it commits them). The data stays there until op70 commits
it or the connection closes.

op14 always sets bit 7 (bin `mac:RCUploadListToHostTempLocation::Call`).
op13 RCUploadToHost shares this request layout; with bit 7 clear it writes
straight into the file at the destination path (bin
`RCUploadToHost::PerformAfterDownload` 0x12079c0; schema and layout editing,
refusing protected index paths). FileMaker Pro does not use op13 for record
data (not in any capture).

### 3.4 op70 Host Commit (`RCCommitRecords`) (High)

Commits records from the connection's temporary area into the file.

Request, push order (bin `RCCommitRecords::CallPushAddition` 0x11c0bb0):

```
[ commit list, N ][ u32 N ][ u64 perform 0 ][ u64 cursor 0 ][ i16 depth -1 ][ u8 copy-key length 0 ]
[ u8 flags ][ u32 txn ]
```

`flags` was `01` in every request (`kCompleteBeforeReturning`; the server
asserts it). Bit 3 (`8`, `kExclusiveRowAccess`) is defined but not observed.
`txn` is the transaction id of the record session.

The **commit list** is a bracketed slot list keyed `[T, 05, R]`:

| form | meaning | example |
|------|---------|---------|
| leaf `R` = `01` | new record (`kNewRow`) | cap:922 `80 28 8002 80 20 05 80 09 834e 01 40 40 40` |
| node `R` with value-less leaves `F` | modified record; the leaves are the ids of the fields that changed | cap:940 `{8001: {05: {c4 19 51: {04, 811c, 8145, 81ed, 8255, 8256}}}}` |
| leaf `R` = `03` | deleted record (`kDeletedRow`) | live:s2/a_del `80 28 8002 80 20 05 80 01 04 03 40 40 40` |

One list may hold several records of one table (cap:1026, three new records)
and, by the grammar, several tables.

What FileMaker Server does with an entry in the modified form for a record
the file does not hold (one another user deleted, or an id never created) is
unmeasured: FileMaker Pro marks its new records `01` and locks a record before
it edits it, so it sends neither, and reports an edit of a record another user
deleted to its user as a missing record (error 101; not captured against
FileMaker Server). OpenMaker Server refuses such an entry, and with it the
whole commit, whatever its id: only an entry marked new creates a record, and
only under an issued, free id (OpenMaker's choice until measured,
[#840](https://github.com/OpenMaker-Dev/openmaker/issues/840); it never
brings back a deleted record, nor lets a client plant a record under an id
another client's next new record gets).

An entry, other than a deletion, naming a record with nothing uploaded for
it (op14, 3.3) asks FileMaker Server for nothing that was measured.
OpenMaker Server refuses it, and the whole commit, as the ordering above
(0, 1.1) shows FileMaker Pro uploading every record it commits new or
changed ([#839](https://github.com/OpenMaker-Dev/openmaker/issues/839);
OpenMaker's choice, which a FileMaker Pro commit of a record it did not
upload would show wrong). Value `02` (`kModifiedRow`) is the
engine's internal state for a modified record; on the wire a modified record
is the node form.

Reply: the empty page (23 bytes) as for op14. Errors are not reported in
the reply: FileMaker Server answered the same 23 bytes when it could not apply
a change (live:s7, below); they are only logged server-side
(`**LOCKERR: CommitTable error ...`).

What the server does for each entry is section 5.

**Locks and op70 (High for the observations, Med for the rule).** For each
record, op70 takes the record's write lock for its own lock user
(connection, session, txn of the op70), copies or deletes the record, and
releases that lock with the notification code (bin
`HostTableCommit::CommitTable` 0x11be170). Consequences seen live:

- No lock held by anyone: the commit applies (live:s5, a modify and a delete
  under a txn that never sent op60).
- The record is locked by the committing txn (the normal case): the commit
  applies and the lock is gone afterwards. Observed for a new record: right
  after the commit another txn of the same connection was granted it
  (live:s7/a_lock_after_create_commit). For a modified record this follows
  from the code path, not from a clean live test (Med).
- The record is locked by a **different** lock user (another connection, or
  another txn of the same connection): FileMaker Server updates the field
  indexes (5.5) but skips writing the record, sends no notification, and
  still answers success (live:s7/a_commit_mod, committed under txn `10303`
  while txn `10302` held record `09`). This leaves the indexes describing
  data that was never written. **A server must not do this**: refuse the
  commit of that record (and leave the file untouched) when another lock user
  holds it.

FileMaker Pro always commits under the txn that holds the lock, and sends
op65 when the user leaves the record (cap:934), so releasing on commit and
releasing on op65 both work with it.

### 3.5 op103 TransactInfo (`RCDBTranInfo`) (High)

Sent once after the commits of one transaction, listing the files involved.

Request body, push order (bin `mac:RCDBTranInfo::Call` 0x1d347ce): `i32 file
id` per file, then `u32 count`, then `u32 txn`. cap:924: `00000084 00000001
00000001`; cap:1010: `00000108 00000084 00000002 0000003c`.

Reply: a single u32, `00000015` in every capture and live probe. 0x15 = 21
(`kNotSupported`): FileMaker Server's `HostDBTranInfo` asks its file-set
manager to group the files and answers 21 when it does not (always, for
hosted files). FileMaker Pro ignores the value. **Answer `00000015`.**

### 3.6 op73 HostUpdateTable (`RCUpdateTable`) (Med)

Request body: the table key (`02 80 02 03`, bin `mac:RCUpdateTable::Call`
0x19f1c6a). Reply: an empty result stack (the server pushes nothing, bin
`mac:RCUpdateTable::Perform` 0x19f1cea). The server runs a `TableUpdater`
over the table (by name: re-processing of the table after a schema change;
not read further). FileMaker Pro did not send op73 in any capture, and it is
not part of the record write path. The one live probe (live:s9) sent a
malformed key (`02 80 02` without the total length) and got the empty reply
with no change to the table. A server can answer the empty reply and do
nothing (Med).

### 3.7 Ops not used for record data

| op | use | evidence |
|----|-----|----------|
| 13 RCUploadToHost | direct upload into the file (schema/layout) | not in captures |
| 15 RCDownloadFromHost, 93 RCDownloadFromTemp | downloads | not in the write path |
| 88 RCUploadBinaryData | container data | not observed |
| 89 RCCompareModCounts | cache validation: which cached catalog entries, records and library members changed (network-protocol.md 5.6.11) | capture s06-omserver-p; live 2026-11-11 |
| 91 RCUploadWithLock | upload + lock, `u16` lock error reply addition | not observed |
| 85 RCBuildIndex | build a field index (field id, options, account) | not observed |
| 50/51 in op86 | DeleteTable / TruncateTable (delete all records) | query-program.md |

---

## 4. Locking

### 4.1 Lock model (High)

Locks are kept per **lock user** = (connection, session id, txn) on **lock
paths** = (file, key path). Lock types (DWARF `DBLockType`): 1-4 intent
locks (notify, read, write, exclusive) taken on ancestors, 5 notify, 6 read,
7/8 read-intent, **9 write** (record editing), 10 exclusive, 11/12 reject
variants. A lock user's locks are released together by op65, one by one by
op62, and all of a connection's locks when it disconnects.

Rules observed live (s3, s4, s7):

| situation | result |
|-----------|--------|
| txn T locks record R (type 9) | granted |
| same T locks R again | granted (re-entrant) |
| another txn of the same connection locks R | 8003 with self-conflict 1 |
| another connection locks R | 8003 with self-conflict 0 and the holder's name |
| lock on a reserved id (op71 by another connection) | 8003 (op71 holds type 9 on reserved ids) |
| lock on an id that has no record and no reservation | granted, empty mod count |
| T committed R with op70 | R is free afterwards (3.4) |
| T released with op65 | others can lock R |
| T locked R twice, then one op62 | R still held (lock counts are kept per lock, Med; live:s4) |

**When FileMaker Pro locks (Claris help).** FileMaker Pro locks a record on
the host when a user begins editing it, not when the edit is committed:
"Two people can't edit the same record at the same time"
([Sharing files on a network](https://help.claris.com/en/pro-help/content/sharing-files.html)),
and "any attempt to modify a field or a record will also attempt to open
the record for editing"
([Open Record/Request](https://help.claris.com/en/pro-help/content/open-record-request.html)),
failing with 301, "Record is in use by another user"
([error codes](https://help.claris.com/en/pro-help/content/error-codes.html)).
The lock request is op60 type 9 (4.2; section 1 lists it for editing an
existing record).

**How many locks a client holds (Med: captures, code).** A user editing a
record holds one lock, and one per unique field it checks (1.3); New
Record holds the ids op71 reserved, 1 or 25 in the captures (3.1, 13
requests); Manage Database holds `[08]`, the catalogs it edits and a table
lock per table whose fields changed (schema-path.md 4). Inside a scripted
transaction FileMaker Pro presumably holds a lock on every record it
changed until the transaction ends (inferred from the record locking
above; not captured). Whether FileMaker Server limits how many locks a
connection holds is unmeasured.

**OpenMaker Server** (its choices; [record-writes.md](../record-writes.md)
has the reasons): one connection's requests hold at most 100,000 locks
over all its files. A new lock past that is answered 8003 with
self-conflict 1 and the session's own name; an op71 past it 8003, before
any id is reserved (section 2 lists op71's checks in order). Server scripts
lock under lock users of their own, which are not capped. A connection's
locks are released when its session ends, however it ends.

### 4.2 op60 Lock (`DBRemoteLock`) (High)

Request body, push order (bin `mac:DBRemoteLock::DoLockCall` 0x1ad7e24,
server `mac:DBRemoteLock::Perform` 0x1ada728):

| field | type | meaning |
|-------|------|---------|
| waiter id | u64 | the address of the client's wait block (`DBLockBlock`), opaque; the official client sends `00000001 6ef4xxxx`; any value works |
| timeout | u32 | seconds to wait; `ffffffff` = wait, 0 = do not wait (FileMaker Server answers at once in both cases, see below) |
| want mod count | u8 | 1 = lock arguments follow and the record's modification count is returned |
| lock path | key | `[T, 05, R]` as a key: `02 80 02 01 05 01 04 07` |
| lock argument | i32 | with "want mod count" 1: the file id (`DBLockArgs`); otherwise 0 |
| host file id | i32 | file id (equals header `fileid`) |
| txn | u32 | owning transaction |
| lock type | u16 | 9 for record editing |

cap:936: `00000001 6ef4f5d8 | ffffffff | 01 | 02 8001 01 05 03 c41951 09 |
00000084 | 00000084 | 00000039 | 0009`. The server pops them in reverse
(type first).

Reply, push order:

| status | fields (push order) |
|--------|---------------------|
| 0 granted | `[mod count key][u8 len]` (only when "want mod count" was 1), then `u16 0` |
| 8001 must wait | `u16 0x1f41` only (client waits for an op66 LockFinished) |
| other (8003 conflict, 301, ...) | `u8 self-conflict`, `string user names`, `u16 status` |

Examples:

| vector | reply | meaning |
|--------|-------|---------|
| cap:936 | `01 00 02 0000` | granted; mod count key `01 00`, length 2 |
| cap:942 | `01 01 02 0000` | granted; mod count `01 01` (record modified once since) |
| live:s3/b_lock7 | `00 0000` | granted; record does not exist, mod count empty (length 0) |
| live:s3/b_lock6_nowait | `00 352a3f34373b313f287a723b3e37333473 00000011 1f43` | 8003; held by "openmaker (admin)"; not self |
| live:s4/a_lock1_other | `01 352a...73 00000011 1f43` | 8003; self conflict |

The user-name string is "session name (account)" of each holder. **The mod
count** returned is the record's `[T, 05, R, fc]` value (5.4) wrapped as a key:
`01 00` for a record never modified after creation (no `fc` yet), `01 01`
after one modification, and so on.

Server behaviour: FileMaker Server never blocks an op60: it tries the lock
without waiting (`DBLockMgr::LocalLockNonBlocking`) and answers 8003 on any
conflict, whatever the timeout. The client tells the user who holds the
record from the 8003 user list; 301 (`kRecordLocked`) is the error FileMaker
reports to scripts for this case, mapped on the client: no op60 reply in the
captures or live probes carried 301 (Med).

One anomaly, explained: live:s2/b_lock_conflict (B locking a record A held,
timeout 0) was answered `00 0000` = granted with an empty mod count, and B's
next lock with another txn then failed with self-conflict 1 (user "? (?)").
That probe request was malformed: its body is 37 bytes, one more than every
other op60 here, with the transaction id written as five bytes (`00 00 02 02
b1`). Popping from the end, FileMaker Server read a shifted host file id,
lock argument and path, so it locked some other path and granted it. It did
not reproduce with well-formed requests (s3, s4: every cross-connection
attempt answered 8003 with the holder's name). 8003 is the correct answer
(High).

### 4.3 op62 Unlock (`DBRemoteUnlock`) (High)

Request body, push order (bin `mac:DBRemoteUnlock::DoUnlockCall` 0x1ad8fa6):

| field | type | meaning |
|-------|------|---------|
| message address | 4 bytes | `DBMesgAddr` (a u32 session id); `00000000` |
| attention mask | u32 | notification codes of the lock being released |
| lock path | key | |
| host file id | i32 | |
| txn | u32 | |
| lock type | u16 | |

19 bytes for an empty path (cap:882 `00000000 00002000 00 00000096 00000000
0005`). The reply is the request's 20-byte transaction header echoed with
`idletime` 0 (network-protocol.md 5.6.10), so the u16 the client pops is 0.

Two uses:
- release one record lock: live:s4/a_unlock1 `00000000 00000000 02 80 02 01
  05 01 01 07 00000165 00010a01 0009` (type 9, record `01`). That txn had
  locked the record twice and still held it afterwards (4.1).
- FileMaker Pro sends op62 type 5, attention `0x2000`
  (`kDBAttnChainingLogout`), empty path, txn 0 when it closes a file. The
  recordings hold 26, every one a close: no later request of the session
  names the file. 19 are in s03, one per file it had opened (cap:882 and
  cap:1150-1184); 6 are in s06-b, -e, -n, -q (two) and -r, against
  OpenMaker Server; 1 is in s07, when the host asked it to close the file
  (network-protocol.md 6.5). No Logout follows any of them. After the one
  that closed the last file it had open, FileMaker Pro sent `Disconnect`
  at once in s06-b, -e, -n and -q (the first); it sent nothing more before
  the recording ends in s03, s06-q (the second) and s06-r; in s07 it sent
  nothing more in that session, which the host ended with its own
  `Disconnect` call. No op62 on a file's root in another form was
  recorded. Answering with the header echo is all the reply needs (High);
  OpenMaker Server also closes the file in the session, for this form only
  ([admin-console.md](../admin-console.md), "Connection registry").

### 4.4 op65 KillAllLocks (`DBRemoteKillAllLocks`) (High)

Request body, push order (bin `mac:DBRemoteKillAllLocks::DoKillAllCall`
0x1ad9e1a): `i32 host file id`, `u32 txn`, `u8 all`. With `all` 0 the server
releases every lock of (this connection, txn); with `all` 1 every lock of the
connection on that file (live:s4/a_kill_all, txn 0). The reply is the request
header echoed (as op62). cap:934: `00000084 0000003a 00` -> `41 00 0000
00000026 00000001 00000084 00000000`.

FileMaker Pro sends op65 when the user leaves a record it created or edited
(cap:934 after the first commit of record `c4 19 51`).

### 4.5 op61 Upgrade, op66 LockFinished, op64 (Med, from code; not observed)

- op61 `DBRemoteUpgrade` (bin `mac:DBRemoteUpgrade::DoUpgrade` 0x1ad894e):
  push order `u64 waiter`, `u32 timeout`, `lock path key`, `i32 host file`,
  `u32 txn`, `u16 new type`, `u16 current type`; reply as op60 without the
  mod count. Not used by FileMaker Pro for records.
- op66 `DBRemoteLockFinished` completes an op60 that answered 8001 (must
  wait) once the lock is granted or refused (`DoLockFinishedCall(DBError,
  DBLockContinuation*)`). FileMaker Server never answered 8001 (4.2), so a
  server that does not block never needs op66 (Low for its layout).
- op64 is not registered (the catalog lists 60-63, 65, 66); -64 is the
  `Notify` transaction class of the back channel (section 6) and -67
  `NotifyOfConflict`.

### 4.6 Lock errors (High for codes, DWARF `Codes`)

| code | name | where |
|------|------|-------|
| 0x1f41 8001 | `kLockMustWait` | op60 when the server would block (not sent by FileMaker Server) |
| 0x1f43 8003 | `kLockConflict` | op60, op71 (reserved id), with user list + self-conflict byte |
| 0x12e 302 | `kTableLocked` | op71 with user list |
| 0x12f 303 | `kSchemaLocked` | op72 with user list |
| 0x12d 301 | `kRecordLocked` | client-side mapping of a record conflict |
| 0x132 306 | `kModIdDoesNotMatch` | client-side, when a commit's mod count is stale (Med) |
| 0x507b | | client side: connection gone |

---

## 5. What the server writes to the file

Paths below are relative to the table id `T` (`80 02` for test project
Contacts). Section 7 of query-program.md describes the index *formats*; this
section specifies when each key changes. Everything was observed by
downloading `[T]` before and after each live step
(`vec:table-states/NN-*.flat`).

### 5.1 Next record id `[T, 00]` (High)

`[T, 00]` holds the table's **last issued** record id as a one-segment key
without its total length: `[u8 seglen][serial key segment]` (`0103` = id
`03`; a 3-byte id would be `03 c4 19 51`). op71 with count n returns the next
id and stores `last + n` (3.1). It never decreases. Baseline `0103` (records
1..3 exist); after the probes `010b` (ids `04`..`0b` issued once). A table
that never had a record has no `[T, 00]` (Addresses, Phone Numbers in test
project); treat that as 0 (Med: the first id of such a table was not
observed).

### 5.2 Record data `[T, 05, R]` (High)

A committed new or modified record is copied from the temporary area into
`[T, 05, R]` **as uploaded**: the server writes the uploaded field leaves
verbatim. The whole record subtree is replaced, so a field missing from the
upload disappears (Med: from code, bin `HostTableCommit::CommitTable` copies
the record node with `HBPosition::CopyFrom`; FileMaker Pro always uploads
every stored field, so this was not tested live). It does not evaluate calculations, auto-enter
options, validation or field types: FileMaker Pro computes all of them
(creation/modification timestamps, account names, serial numbers, the
primary-key UUID, stored calculations) and uploads the results. Example
(live:s1): the probe uploaded 8 fields for record `04` and exactly those 8
leaves appeared under `[80 02, 05, 04]`.

A deleted record's whole `[T, 05, R]` subtree is removed (live:s2/a_del).

### 5.3 Record-id list `[T, 0d]` (High)

`[T, 0d]` (`KMasterList`) is the RID list (query-program.md 5.1) of every
record of the table. A new record's id is added, a deleted record's id is
removed; a modify does not touch it. Live sequence:

| state | `[80 02, 0d]` |
|-------|---------------|
| records 1, 2, 3 | `{00 = 0000000e}` |
| + 4 | `{00 = 0000001e}` |
| + 5, 6 (4 deleted) | `{00 = 0000006e}` |
| 1, 2, 3, 10 (`0a`, another user's record) | `{00 = 0000040e}` |

**Writer rule** (bin `DBBitmap::Store` 0x1126610; verified by re-encoding all
11,786 RID lists in the captures, `vec:ridlist_rule.py`): split record ids
into windows of 3,968 ids (124 u32 words, window `w` starts at `w * 3968`).
In each window, scan the words; a run starts at the first non-zero word and
ends before a gap of 4 or more zero words or at the window end (trailing zero
words are dropped). A run of one word with exactly one bit set is written as
a **singleton** leaf (key = that record id, no value); any other run is a
**bitmap** leaf (key = id of the run's first bit position, i.e.
`window start + 32 * first word`; value = the run's words, u32 BE, bit 0 =
lowest id). Keys are serial keys. An empty list is an empty node. So a single
record `04` alone would be the leaf `04` with no value, and the test-project
list above is the bitmap leaf `00 = 0000040e`. The single-record table that
FileMaker Server stored as `{01: ""}` on disk is the singleton form; both
forms appear in files and a reader must accept any mix. The same rule is
used for every RID list the server writes (indexes, query results).

### 5.4 Record modification count `[T, 05, R, fc]` (High unless marked)

Field id `fc` (252) of a record is its **modification count**, the record's
`KModCount` (schema-path.md 8.1). Its form is a key string of one serial
key: `[u8 length][serial key]`, the length byte equal to the key's length
(1 to 9 bytes, query-program.md 2.6), so 2 to 10 bytes in all: `01 01` = 1,
`01 7f` = 127, `02 80 00` = 128. Every `fc` in the captures and probes has
this form (cap:454 shows `0101`..`0106` in the hosted solution's records).

FileMaker Pro writes it, not the server: the client increments the count
and includes it in the op14 upload of a modified record (cap:938 `fc = 01
01`, cap:944 `01 02`, cap:1006 `01 03`). A newly created record has no `fc`
(cap:920, cap:930). The server stores it like any other field (5.2): each
live probe that uploaded a count (live:s2, s5, s7, each the locked count
plus one, `01 01`) found exactly that value in the record afterwards
(vec:table-states/02). op60 returns it to the client as the lock's mod count
(4.2; `01 00` when absent), framed with a one-byte length, so the reply
cannot carry a value longer than 255 bytes.

What FileMaker Server does with an uploaded `fc` that is not the locked
count plus one, or not a count at all (a length byte that does not match,
more than 10 bytes), is unmeasured: no capture or probe sent one. That it
stores whatever was uploaded is inference from the code path (5.2:
`HostTableCommit::CommitTable` copies the record node; Low). A server should
not store a value that is not a count: no FileMaker client writes one, and
one longer than 255 bytes could not be returned by op60. Refusing such a
record, and answering op60 with `01 00` for one already in a file, is a
server's choice, not observed FileMaker behaviour (section 11).

### 5.5 Field value indexes `[T, 01, F, ...]` (High for the rule; text keys see query-program.md 7.2)

For every **indexed** field `F` of the table (a field is indexed iff the node
`[T, 01, F]` exists; FileMaker Pro turns indexing on, the server never adds an
index on its own during a commit), the server keeps:

| key | content | change on commit |
|-----|---------|------------------|
| `[T, 01, F, 00, k]` | value index: RID list of records whose field `F` has index key `k` | add R under each of the record's keys (new), remove it (delete), both for the old and new keys (modify) |
| `[T, 01, F, 01, k]` | word index (text fields whose index is "all" words) | same, per word key |
| `[T, 01, F, 10]` | 16 bytes: u64 records indexed, u64 distinct value keys | recomputed |
| `[T, 01, F, 11]` | 16 bytes: u64 word entries, u64 distinct word keys | recomputed |

The key `k` is the field value's index key in the field's index language
(query-program.md 7.2-7.3: text `01` + collation weights, numbers `80` +
digits, dates, timestamps), one per line of a text value, cut to its first
219 bytes; a record with a cut key is also listed under `ff 80`
(query-program.md 7.1). A record whose field is empty is indexed under the
empty-value key: `01` for text (Company `[80 02, 01, 05, 00, 01]` lists every
record) and is counted in `10`; FileMaker Server indexes an empty field even
though the record stores no leaf for it. When the last record leaves a key,
the key's node is removed.

Live examples (vec:table-states):

- new record 04 "OpenMaker write test" / "Create": First Name gets
  `[80 02, 01, 01, 00, 01 138e 13b3 126b 136d 135f 120f 131e 126b 13da 020a
  148d 13da 12ec 1433 126b 020a 1433 126b 1410 1433, 04]` (the whole value as
  one key, spaces = `020a`), `[.., 10]` `3,3` -> `4,4`; Company and Job Title
  (empty) add 04 to their `01` key (`0000000e` -> `0000001e`).
- modify Last Name "Create" -> "Edit": the key `01 123d 13da 126b 120f 1433
  126b` loses record 04 and is removed, `01 126b 1250 12ec 1433` is added.
- delete: every index entry of the record is removed and the counts drop
  (vec:table-states/03-after-delete-04 equals the baseline except `[T, 00]`).

**Which fields the server re-indexes on a modify.** The op70 node form lists
the ids of the fields that changed; FileMaker Server updates the indexes of
those fields, comparing the record's old stored value (in the file) with the
new one (in the temporary area) (bin `HostTableCommit::UpdateIndexModify`).
It does this **before** it takes the record's write lock, so a commit whose
lock upgrade fails leaves the indexes changed and the record unchanged (3.4).
That is how the RE probe left an orphaned Last Name entry and an empty key
node in test project (live:s7) until it was repaired (section 9). A server
should take the record lock first, then re-index every indexed field whose
stored value differs between the old and new record (equivalent for a
correct client, robust against an incomplete list), then write the record,
and remove a key node once its RID list is empty.

**Counts `[T, 01, F, 10|11]`** (Low): the first u64 grows with index entries
(records times keys for that field), the second with distinct keys, but
FileMaker Server does not keep them exact: after the probes Last Name read
`5, 3` for 4 records and 4 distinct keys. Recompute both on each change;
FileMaker Pro reads them only as statistics (inference).

Index keys of calculation and summary fields: unstored calculations have no
index; stored calculations are computed by the client and uploaded as values
(5.2), and are indexed like any field.

### 5.6 Creation and modification metadata, auto-enter (High)

There is no server-side auto-enter. Creation/modification timestamp and
account fields (test project `0a` CreatedBy, `0b` ModifiedBy, `0c`
CreationTimestamp, `0d` ModificationTimestamp, `09` PrimaryKey = `Get(UUID)`)
are ordinary stored fields the client fills in and uploads. Timestamps are
stored in their text form (`10/28/2026 12:54:55 PM`); account names as text.
Serial numbers come from op72 (3.2). The server writes nothing else per
record: no per-record creation time, no record-level owner.

### 5.7 Keys that do not change on record commits (High)

`[2]` (file status, including `mInternalCreation` at `[2, fe]`),
`[T, 03]` (field catalog, except the serial value `[T, 03, 05, F, 0c]` written
by op72), the superblock global counter and page `mModCount` are page-level
bookkeeping of the storage engine, not observable over the wire: two
downloads of `[2]` before and after all probes were identical. A server that
writes the file with om-hbam must keep the page/B-tree invariants of
file-format.md 5 but needs no extra record bookkeeping.

### 5.8 Order of updates inside one op70 (Med)

For each table of the commit list, FileMaker Server locks the table's index
node, then for each record: reads its old row (file) and new row (temporary
area), updates the master list (add for new, remove for delete), updates the
field indexes, takes the record's write lock and copies (new, modified) or
deletes (deleted) the record node, removes the record from the temporary
area, and releases the lock with the attention code, which queues the change
notification (section 6). Section 5.5 gives the safer order a new server
should use.

---

## 6. Change notification

### 6.1 Registration: op63 RequestNotification (High)

FileMaker Pro registers for notifications once per opened file (op63,
network-protocol.md 5.6.9). Body, push order (bin
`mac:DBRemoteRequestNotification::DoRequestNotification` 0x1ad859a):

| field | type | value |
|-------|------|-------|
| attention mask | u32 | `ffffffff` (all codes) |
| message address | 4 bytes | `00000001`: the client's session id to address notifications to |
| lock path | key | `00` = empty path, the file root (notify on everything in the file) |
| host file id | i32 | file id |
| client file id | i32 | the client's own handle for the file: `-4` for the first file, then `-6`, `-8`... |

Reply `00 00`. The server records a type-5 (notify) lock on the file root for
this connection with that address, mask and client file id; the connection's
locks (this one included) are dropped when it disconnects.

### 6.2 What triggers a notification (High)

Every applied op70 entry notifies the connections that registered on the
file, except the committing one (bin `DBLockInfo::NotifyLevel` skips the
commit's `mExcludeAddress`; the probe's committing session never registered,
so this exclusion is from code). A commit that was not applied (3.4) sends
nothing (live:s7). The attention code (DWARF `DBAttnCode`) is the change kind
OR the scope:

| code | name |
|------|------|
| `0x100` | `kDBAttnAdd` (new record) |
| `0x200` | `kDBAttnModify` |
| `0x400` | `kDBAttnDelete` |
| `0x001` | `kDBAttnRecord` (scope: a record) |
| `0x002/3/4/5/6/7` | catalog, schema, schema version, field index, field change, layout object change |
| `0x10` `0x11` | `kDBAttnMessage`, `kDBAttnEndMarker` |
| `0x30` `0x40` `0x50` `0x60` `0x70` | `kDBAttnMaxIdleTime`, `kDBAttnCloseFile`, `kDBAttnCloseAll`, `kDBAttnDisconnect`, `kDBAttnDisconnectAll` |
| `0x800` `0x1000` `0x2000` | `kDBAttnHostSerial`, `kDBAttnLocal`, `kDBAttnChainingLogout` |

The administrator's messages use `0x810`, `0x850`, `0x40` and `0x870`
(network-protocol.md 6.5).

Observed: new record `0x101`, modify `0x201`, delete `0x401` (live:s2, s3).
op71 alone does not notify (the reserved ids are invisible until committed).

### 6.3 The notification message (High)

One notification is a **marshalled DBMesg** (bin `DBMesg::Marshall`
0x112d620, `DBLockInfo::NotifyLevel`):

| off | size | field |
|-----|------|-------|
| 0 | 4 | send-to session id (u32 BE): the address from op63 (`00000001`) |
| 4 | 4 | reply-to session id: 0 |
| 8 | 4 | attention code |
| 12 | 2 | number of items (2) |
| 14 | 2 | offset of the last item's length word (`0x1e`) |
| 16 | 4 | item 1: the client file id from op63 (`fffffffc` = -4) |
| 20 | 2 | item 1 length `0004` |
| 22 | n | item 2: the changed record's path in the in-memory key form, total length **first**: `07 02 80 02 01 05 01 04` = `[80 02, 05, 04]` |
| 22+n | 2 | item 2 length (`0008` = n) |

Items are stored data-first with a u16 BE length after each item; the header
field at offset 14 points at the last length word. Example (live:s2,
modify of record 04 by another connection, 32 bytes):

```
00000001 00000000 00000201 0002 001e | fffffffc 0004 | 07 02 80 02 01 05 01 04 0008
```

The path is the record (`[T, 05, R]`) for record changes; for other
attention codes it is the changed node (schema keys etc., not observed).

### 6.4 Delivery (High)

The server queues the message on the target connection and delivers it in
one of two ways (bin `RCConnection::PackBackChannel` 0x14ed9a0,
`DBNotifications::SendMesg`):

1. **Piggyback**: in the `backchannel` out sequence of the next Perform reply
   to that client (network-protocol.md 5.6.3b). The sequence then starts `ff
   00` and holds one record per queued message: `[u32 BE message code =
   attention code][u16 BE length][length bytes = the marshalled DBMesg]`.
   live:s2 (vec:live-notify-piggyback-modify):
   `ff00 | 00000201 0020 | 00000001 00000000 00000201 0002 001e fffffffc 0004 0702800201050104 0008`.
   With nothing queued the sequence is the usual `fe <cpu> <0..3 random>`.
2. **Call**: when the client is idle the server calls the client's callback
   object (the RCFMP12 object the client passed to Connect, over the same
   bidirectional GIOP connection). It first sends a GIOP **LocateRequest**
   for object key `\xffomniINSPOA\xffFMPOA\0FMRPO` (vec:live-notify-locate;
   the client answers LocateReply OBJECT_HERE), then a `Notify` request
   (`RCFMP12::Notify(ULong connectionid, string sessKey, in OctetSeq parms,
   out OctetSeq backchannel)`, response expected) whose `parms` is exactly
   the piggyback form above (`ff 00` + records). Several notifications are
   batched into one call (vec:live-notify-call-add2: two records, `0x101`
   for `05` and `06`). The CDR body is `u32 connectionid` (observed
   `0x661d1707`, by inference the client's connection id), the string
   `sessKey` (empty: length 1, a NUL), then `parms`. The client answers
   NO_EXCEPTION. The probe sent an empty `backchannel` sequence
   (`00000000`) and FileMaker Server accepted it; FileMaker Pro puts its
   CPU report there, `fe <cpu> <0-3 random bytes>` (network-protocol.md
   6.5).

Queued messages go out with the next Perform reply to that client, or by a
server-initiated Notify call once the connection has been quiet (below).
Live, a client that sent a request within about a second of the commit got
the piggyback (live:s2 modify, s7), one that stayed quiet longer got the
call (live:s2 delete, s3). A server may use either form; the piggyback needs
no server-to-client GIOP request and is the simpler one to implement (the
client then learns of changes on its next request; FileMaker Pro tickles
every 60 s while idle, network-protocol.md 6.1).

**Correction to network-protocol.md section 6:** a back-channel sequence
starting `ff 00` is this list of `{u32 message code, u16 length, bytes}`
notification records, not a session/idle table; the `fe` form is the CPU
report (`fe <cpu> <0-3 random bytes>`).

**FileMaker Server's queue (High: bin, libDBEngine.so and libSupport.so;
not measured).**

- **No bound found in traced paths.** Each connection has one queue of
  back-channel messages
  (`RCConnection::mBackChannelQ`, a `MsgQ`). `DBNotifications::SendMesg`
  (bin @0x1051450) appends one message per notification: the attention
  code and the marshalled DBMesg. `MsgQ::PostMsg` (libSupport @0x21e860)
  links it at the end of a list with no limit on messages or bytes.
  No bound, combining, replacement or dropping was found in the traced
  enqueue and delivery paths. The queue belongs to the connection and, by
  inference, ends with it; behavior when delivery fails remains unresolved.
- **Not part of `maxreturn`.** `RPO_i::Perform` (bin @0x14eee00) runs the
  transaction under the request's `maxreturn` and then fills the out
  `backchannel` with `RCConnection::PackBackChannel` (bin @0x14ed9a0),
  which takes no size from the request. FileMaker Pro asks small
  `maxreturn`s for some requests (76 bytes for op62, 72 for op63, 29 for
  op65 and 2,085 for op60 in every captured request, s03, s06); they do not
  hold back the back channel.
- **About 32 KiB per reply or call.** With nothing queued,
  `PackBackChannel` writes the CPU report. Otherwise it writes `ff 00`
  and takes queued messages in order while the sequence is shorter than
  `0x8000` bytes, so the last message may pass 32 KiB. The rest wait for
  the next reply or call.
- **The push.** A pass over the connections
  (`RCConnectionList::TickleAConnection`, bin @0x14fbc30) makes a connection
  eligible to push when its last request or call ended 250 ms ago or more
  (`cmp $0xfa`), messages are queued and no call to that client is in
  progress. `RCConnection::PushBackChannelMsgs` then sends one `Notify`
  call with the packed back channel. How often the pass runs is not
  established, so 250 ms is an eligibility threshold, not a delivery time.
- **A client that does not take them.** The traced queue paths retain
  messages for later delivery in about 32 KiB calls. No slow-client bound
  or disconnect was found in those paths. The failed-`Notify` paths were
  not traced, so their retention or disconnect behavior remains unresolved.

**OpenMaker Server** (its choices; [record-writes.md](../record-writes.md)
has the reasons) delivers by piggyback only. A reply carries the queued
messages in order, as many as fit in what `maxreturn` leaves after the
result; the rest wait for the next replies. At most 50,000 messages or
2 MiB wait for one connection. Past either bound the queue is combined: of
identical messages only the latest is kept, and a record's message
`[T, 05, R]` becomes one for the table's records `[T, 05]` with the same
code (the path FileMaker Pro uses for a table's records after a field
change, schema-path.md 9). When that leaves more than half of either bound,
the connection is closed with a GIOP CloseConnection, so that its client
reads the file again. It relays an op62's attention code (6.2) only when
the op62 released a lock the transaction held and the code is a scope
`01`..`07` with only the add, modify and delete bits besides. This differs
from FileMaker Server: no bound or combining was found in its traced
enqueue and delivery paths, and failed-`Notify` behavior remains
unresolved. Its back channel is outside `maxreturn`; FileMaker Pro
ignores `0x101` for `[T, 05]` and empties every found set of the table for
`0x401` (6.5, [#171](https://github.com/OpenMaker-Dev/openmaker/issues/171)).

### 6.5 What the receiving client does (High: bin; not captured)

From the Mac client engine (DBEngine x86_64; addresses are `nm` values).

**Receiving.**

1. `RPO_i::Notify` (mac 0x1d4368c) and every reply's back channel go to
   `RCConnection::ProcessBackChannel` (mac 0x1d439b2). `fe` carries the
   server's CPU byte. `ff 00` hands each `{code, length, bytes}` record to
   `sBackChannelCallback`, `DBNotifications::ProcessBackChannel` (mac
   0x1afbf68).
2. `DBNotifications::ProcessBackChannel` unmarshals the DBMesg and queues it
   for the engine's notify thread (`DBEngineController::DBNotifyThread`),
   which passes it to the session's `DBNotifications::InsertMesg` (mac
   0x1afbb6a).
3. In `InsertMesg`, a code with `0x800` (`kDBAttnHostSerial`) names the file
   by its host file id, which is replaced by the client's own.
4. A code of record scope (low byte `01`: `0x101`, `0x201`, `0x401`) stops in
   `InsertMesg`. The first segment of the path names the table. When the
   client holds no table proxy for it (by inference, it has not used the
   table), the message is dropped; otherwise the change is recorded in the
   table's pending lists (`DBRecordNotifications::AddNotification`,
   mac 0x1afb2d6 and 0x1afb3fa):
   - **`[T, 05, R]`** (three segments) puts R in one of four lists:
     - modified, if the code has `0x200`, unless "all modified" is already
       pending;
     - else added, if it has `0x100`;
     - else deleted, if it has `0x400`;
     - else other.
   - **`[T, 05]` or `[T]`**:
     - `0x200` marks the whole table modified;
     - `0x400` without `0x200` empties the four lists and marks the whole
       table deleted;
     - `0x100` alone records nothing.
5. Other codes (catalog changes, administrator messages,
   network-protocol.md 6.5) go to the session's message lists.

**Applying.** The pending lists are applied the next time the client
processes the table's notifications (`DBTableProxy::ProcessNotifications`
and `InternalProcessNotifications`, mac 0x1c58ca6, 0x1c58e02). That happens
from `DBSession::ProcessNotifications` (by inference the application's idle
processing) and at the start of some engine operations. The record-id list
below is the client's copy of the table's `[T, 0d]` (5.3).

| notification | cached records | record-id list and found sets | then |
|--------------|----------------|-------------------------------|------|
| `[T, 05, R]` `0x101` | - | R is added to the record-id list and appended to every record set that follows it (`IsMasterSet`): the table's own record set, and found sets in a state not resolved (`DBResultSet::IsMasterSet`); related sets do not follow it | change `0x101` |
| `[T, 05, R]` `0x201` | R's cached copy is purged (`PurgeRecords`) | unchanged; summaries over R are flushed | change `0x201` |
| `[T, 05, R]` `0x401` | R's cached copy is purged | R leaves the record-id list and every record set (`PruneRecordSets`) | change `0x401` |
| `[T, 05]` `0x101` | - | nothing | - |
| `[T, 05]` `0x201` | every cached record of the table is dropped (`FlushRecords`) | unchanged; every summary is flushed | change `0x701` |
| `[T, 05]` `0x401` | the table's cached records, index, record-id list, container references and access data are deleted | every record set of the table is emptied (`PruneRecordSets(KAll)`) | change `0x401` |

"Change" is `DBTableProxy::NotifyDataChange`, which tells the file's owner
(by inference, its windows redraw).

FileMaker Pro downloads nothing when a notification arrives. A purged
record is read again (op92/op90) when it is next needed (inference), and
found sets only grow for added records that follow the record-id list or
shrink for deleted ones. A table-wide `0x201` is what FileMaker Pro itself
sends after Manage Database changed a table's fields (schema-path.md 9). A
server that commits without notifying leaves other clients showing stale
data until they refresh.

---

## 7. Deletion (High)

FileMaker Pro deletes a record by locking it (op60 type 9), committing the
list entry `[T, 05, R] = 03` with op70, and releasing (op65). There is no
dedicated delete opcode for single records; whole-table delete/truncate go
through op86 (query-program.md op 50/51). The server removes `[T, 05, R]`,
removes R from `[T, 0d]` and from every index (5.5), notifies `0x401`, and
leaves `[T, 00]` unchanged (vec:live-op70-commit-delete,
vec:table-states/03-after-delete-04).

---

## 8. Server algorithm (summary)

Per connection keep: the temporary key tree (op14), the lock table entries
of (connection, txn), the op63 registrations (address, mask, client file id
per file), and the queue of outgoing notifications.

- **op71**: check table; lock `[T, 05]` briefly; `last = [T, 00] or 0`;
  `[T, 00] = last + count`; lock `[T, 05, last + i]` (i = 1..count) type 9
  for txn; reply `u32 0` + key `last + 1`.
- **op72**: lock field def; read `[T, 03, 05, F, 0c]` (default "1"); per
  flags store `value` or the incremented text; reply `u32 0` + old value.
- **op14** (flags bit 7): unpack the payload into the connection's temporary
  tree under the destination path; reply the 23-byte empty page.
- **op70**: for each entry `[T, 05, R]`: if another lock user holds R, skip
  it (3.4). New (`01`) or modified (node): take the record from the temporary
  tree, skipping it if its `fc` is not a count (5.4); for each indexed field
  whose value changed, move R between index keys (removing emptied key
  nodes); replace `[T, 05, R]` with the uploaded record; for new, add R to
  `[T, 0d]`. Deleted (`03`): remove R from all indexes and
  from `[T, 0d]`, remove `[T, 05, R]`. Drop R from the temporary tree and
  release the txn's lock on R. Queue `0x101`/`0x201`/`0x401` notifications
  to every other registered connection. Reply the 23-byte empty page.
- **op103**: reply `00000015`.
- **op60**: non-blocking; reply granted (+ `[T, 05, R, fc]` as the mod count
  when asked, `01 00` when it is absent or not a count, 5.4) or `8003` with
  self-conflict and holder names.
- **op62/op65**: release; reply the request header with `idletime` 0.
- **disconnect**: release all locks, drop the temporary tree and
  registrations.

---

## 9. Live probe records (test project, 2026-10-28)

All created by the RE probe in table `80 02` (Contacts) with First Name
"OpenMaker write test", all deleted before finishing:

| id | Last Name | created | deleted |
|----|-----------|---------|---------|
| `04` | Create / Edit | s1 | s2 |
| `05`, `06` | Block one / Block two (`07` reserved, never committed) | s3 | s3 |
| `08` | No lock / Changed without lock | s5 | s5 |
| `09` | Lock release (created twice) | s7, s8 | s7, s8 |
| `0b` | Lock release (index repair) | s12 | s12 |

Final state (vec:table-states, live:s12/after): records `01`-`03` unchanged
byte for byte; `[80 02, 00]` advanced from `0103` to `010b` (ids are never
reused, 5.1); no probe record, rid-list entry or index entry remains. The
table also holds record `0a` ("OpenMaker capture test"), which is not a probe
record: FileMaker Pro on the capture Mac created it at 13:09 (Access.log names
that client) and it was left in place, with its index entries.

One side effect occurred and was repaired: in s7 the probe committed a
modification of `09` under a txn that did not hold its lock (3.4), so
FileMaker Server changed the Last Name index but not the record; the later
delete then left the entry "lock release 2" -> `09` and an empty key node
"lock release" (`[80 02, 01, 02, 00, 01 1330 138e 123d 131e 020a 13da 126b
1330 126b 120f 1410 126b]`). s8 recreated `09` with "Lock release 2" and
deleted it properly (removing the orphaned entry); s12 created `0b` with
"Lock release" and deleted it (removing the empty node). Afterwards
`[80 02, 01, 02]` lists exactly the Last Name keys of records `01`-`03` and
`0a`; only its statistics `[.., 10]` (`5, 3`) differ from an exact count
(5.5).

---

## 10. Test vectors

In `re/work/write/vectors/` (git-ignored; `INDEX.md` lists all). Each `.txt`
has `req` (whole RCTransaction, 20-byte header + body), `rep` (reply result
stack) and `bc` (back-channel) lines in hex.

| vector | covers |
|--------|--------|
| `cap-op71-create-1`, `cap-op71-create-25`, `live-op71-create-1`, `live-op71-create-3`, `live-op71-missing-table` | 3.1 |
| `cap-op72-get-serial`, `live-op72-read-default`, `live-op72-missing-field` | 3.2 |
| `cap-op14-upload-new`, `cap-op14-upload-modify`, `live-op14-upload-new`, `live-op14-upload-modify`, `live-op14-upload-2` | 3.3 |
| `cap-op70-commit-new`, `cap-op70-commit-new-2`, `cap-op70-commit-3new`, `cap-op70-commit-modify`, `live-op70-commit-new`, `live-op70-commit-modify`, `live-op70-commit-delete`, `live-op70-commit-2new`, `live-op70-commit-2delete`, `live-op70-modify-without-lock`, `live-op70-delete-without-lock` | 3.4, 7 |
| `cap-op103-traninfo`, `cap-op103-traninfo-2files`, `live-op103` | 3.5 |
| `cap-op60-lock-modcount0`, `cap-op60-lock-modcount1`, `live-op60-lock-granted`, `live-op60-conflict`, `live-op60-self-conflict`, `live-op60-relock-same-txn`, `live-op60-lock-missing-record`, `live-op60-unexplained-empty-grant` | 4.2 |
| `live-op70-commit-locked-by-other-txn`, `live-op60-after-new-commit` | 3.4 (lock release on commit; the inconsistent FileMaker Server behaviour a server must not copy) |
| `cap-op62-unlock-notify-at-close`, `live-op62-unlock-record` | 4.3 |
| `cap-op65-kill-all-locks`, `live-op65-kill-all-transactions` | 4.4 |
| `cap-op63-request-notification` | 6.1 |
| `live-notify-piggyback-modify`, `live-notify-locate`, `live-notify-call-add2`, `live-notify-call-delete1`, `live-notify-call-delete2` | 6.3, 6.4 |
| `table-states/00..07` (`.slots` = op90 stream of `[80 02]`, `.flat` = `path = value`) | 5 |
| `ridlist_rule.py` | 5.3 (reference encoder, checked against 11,786 captured lists) |

A server implementation can be checked against these the same way the read
path is (`crates/om-protocol/tests/capture_server.rs`): parse each `req`,
apply it to a copy of test project's `[80 02]` subtree from
`table-states/00-baseline.slots`, and compare the reply and the resulting
subtree with the next `table-states` file.

---

## 11. Open questions

1. **Text index keys** for values the server has not seen in an index: the
   full collation table (query-program.md 7.2, open there). A server can
   index text only if it reproduces FileMaker's key builder; until then it
   can reuse keys the client sent in finds or leave a new record out of text
   indexes (finds on that field will miss it). This is the main blocker for a
   fully faithful op70.
2. Word-index breaking rules (`[T, 01, F, 01]`), same source.
3. Repeating fields and container fields in an upload (not observed; the
   chunked-value convention of file-format.md 3.6 presumably applies).
4. Whether FileMaker Pro ever sets op70 flag 8 (`kExclusiveRowAccess`) or
   uploads straight into the file (op14 without bit 7) for records.
5. The exact meaning of the op60 "waiter id" (always `00000001 6ef4xxxx`
   from FileMaker Pro; any value works). The anomalous empty grant (4.2) was a
   malformed probe request.
6. The op91 UploadWithLock payload (never sent by the captured client).
7. Field-level privileges and validation on the server: FileMaker Server
   enforces record-level access (`kNoRecordAccess` 200) in op71/op70; the
   rules come from the security catalog `[23]` and were not exercised.
8. Index entry counts `[T, 01, F, 10]` after deletes (5.5): FileMaker Server
   does not always decrement them; the exact rule is unknown and probably
   irrelevant to clients.
9. What FileMaker Server does with an uploaded record modification count
   that skips counts or is not a count (5.4): whether op70 stores it,
   refuses the record or rewrites the count, and what op60 then returns.
   A probe can tell: lock a probe record, upload it with `fc = 01 05`
   (skipping counts), commit, download it and lock it again; then the same
   with `fc = 02 01` (a length byte that does not match its key).

10. A two-client capture of change notifications (6.4-6.5, [#137](https://github.com/OpenMaker-Dev/openmaker/issues/137)): what
    FileMaker Pro downloads after each code, whether its found sets change as
    6.5 reads from the code, how often FileMaker Server's push pass runs,
    and what FileMaker Server does with a client whose `Notify` calls fail.

**What the maintainer can do to close 3 and 4:** with the capture relay running
(README, "Live observations"), open test project in FileMaker Pro on the Mac,
then (a) add a record, type a value into a container field (insert any small
image) and a repeating field if the layout has one, commit, then delete the
record; (b) duplicate a record (Records > Duplicate Record); (c) run Replace
Field Contents on one field of the found set. Name the capture
`2026-10-2x-s07-writes`.
