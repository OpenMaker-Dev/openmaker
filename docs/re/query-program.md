# op86 QueryProgram - finds, joins, sorts and found sets

Status: in progress (2026-10-28). Sections 0-5 are a first draft; SimpleFind,
index key formats, RID-list algebra and SortList are researched (vectors under
`re/work/query/vectors/`) but not yet written up. Sections are written as they
are pinned;
each claim carries a confidence (High/Med/Low) and its evidence:

- `cap:<rid>` - an exchange in capture `re/captures/2026-10-27-s03-direct/conn-000`
  (FileMaker Pro 26 browsing a hosted solution on FileMaker Server 26).
  Extracted per exchange under `re/work/query/cap03/<rid>-op086/` by
  `re/work/query/tools/capx.py`; `dump86.py` prints a program and its reply.
- `live:<vector>` - a program sent by OpenMaker to the lab server's
  `test project.fmp12`, saved with its reply under `re/work/query/vectors/`.
- `live1:<test>` - a read-only program of the 2026-11-11 run against the
  same file, with the same probe, saved under `re/work/needs-re/146/live1/`
  (section 12 lists the file's records at that time).
- `bin:<name> @<addr>` - the Linux FileMaker Server engine `libDBEngine.so`
  (26.0.3, DWARF). Addresses are `nm` values; Ghidra = `nm + 0x100000`.

This document is the clean-room boundary for op86: an implementer should not
need anything else. Byte layouts use hex; `[a, b, c]` is an HBAM key path
(file-format.md 3) with each segment written as its key bytes.

---

## 0. Summary for implementers

op86 carries a small **program**: an ordered list of instructions plus an
"upload" data area, both encoded as one HBAM slot tree. The server runs the
instructions in order against the hosted file; instructions read and write
numbered **data spots**. Spots under `[2]` are scratch/uploaded, spots under
`[3]` are returned to the client. Results are mostly **RID lists** (sets of
record ids). Index-based matching uses **index keys** (the byte strings
stored in the file's field indexes), which the client derives from find
and relationship values. The Match finder instead carries text operands
and evaluates record text (6.3).

What the official client actually sent while browsing (cap, 96 programs):
95 single-instruction **JoinProjection** programs (portals, related lookups,
value lists based on relationships) and 1 three-instruction program
(SimpleFind -> Projection -> JoinProjection). It did **not** send op86 to
show a layout's initial found set: Show All / the initial found set comes
from the table's record-id list downloaded with op90 (`[table, 13]`, 5.1).

Minimal server for FileMaker Pro to browse and find (section 1 lists the
details):

1. Parse the program (2.1), validate each instruction's access key (2.4).
2. Implement over the file's indexes (section 7): SimpleFind (section 6),
   CombineRidLists / InvertRidList / NodeManipulation (section 8),
   Projection and JoinProjection (section 9), SortList (section 10).
3. Return the `[3]` subtree followed by `u32 N` and the all-zero resume block
   (11.1), with the error slot (11.2) when a program fails.
4. Keep `[table, 13]` (the record-id list) and `[table, 1, field, 0, ...]`
   (value indexes) correct in the hosted file, since the client reads both
   directly with op90.

---

## 1. Transaction framing (High)

Request (client -> server), the Perform `parms` RCTransaction
(network-protocol.md 4.1, 5.6.6a):

| part | bytes | notes |
|------|-------|-------|
| header | 20 | opcode `0x56` (86), taskid 0, transid, **sessionid** (1 after Login), **fileid** (the Open reply file id), idletime |
| payload | N | the program slot tree (section 2) |
| length | 4 | u32 BE = N |
| zero block | 16 | all zero |
| marker | 2 | `ff ff` |
| tail | 1 | `00` |

`maxreturn` (the Perform argument) was `0x10012c` for every op86 in cap.
Evidence: all 96 cap requests parse this way (`dump86.py`).

Reply `parms` (server -> client), a header-less result stack:

| part | bytes | notes |
|------|-------|-------|
| result | N | the program's `[3]` subtree, re-rooted (section 4) |
| length | 4 | u32 BE = N |
| zero block | 17 | all zero in all 96 cap replies |
| status | 2 | u16 BE, 0 = success (popped first by the client) |

The 17-byte block was zero in every observed reply; send zeros (High for
"zeros accepted", Low for meaning).

**Superseded (2026-10-28):** network-protocol.md 5.6.6c, from the server and
client code, reads these 23 bytes as `[u32 N]` plus the resume block (perform,
cursor, depth, copy key): op86 has **no status field**, and the "status" above
is the end of the block. The bytes are identical for a complete reply. Large
found sets page with that block; errors travel only in the slots (4.3).

---

## 2. Program grammar

### 2.1 Top level (High)

The payload is one bracketed slot list (file-format.md 3.6, the same wire form
as an op90 download: `0x80 ... 0x40`, minimal chunk codes, key order) with two
or three top-level keys:

| key | constant (bin) | holds |
|-----|----------------|-------|
| `[1]` | `KInstructions` | the instruction list, one child per instruction |
| `[2]` | `KUpload` | the upload / scratch data spots |
| `[3]` | `KDownload` | the result data spots (absent in requests; filled by the server) |

Key constants are 2-segment-free single bytes; their values come from the
static initializer of the engine globals (bin `_GLOBAL__sub_I_DBGlobals.cpp`
@0xa848a0, stores at 0xa8d98b..0xa8daf7).

### 2.2 Instructions (High)

Each instruction is `[1, i]` where `i` is the instruction number, a
one-segment serial key (2.6) starting at 1 and increasing by 1 in execution
order (cap:912 has `[1,1]`, `[1,2]`, `[1,3]`). Its children are leaves keyed
by single bytes:

| leaf | constant | value | seen in cap |
|------|----------|-------|-------------|
| `00` | `KOpcode` | u8 opcode (2.3) | always |
| `01` | `KSrc` | operand: a spot path (2.5) | always |
| `02` | `KDest` | operand: a spot path | always |
| `03` | `KDelList` = `KJoinPreds` | operand: a spot path (join predicates, 9.2) | JoinProjection |
| `04` | `KFileId` | u32 BE file id, equal to the transaction header's `fileid` | always |
| `05` | `KTableKey` | the target table key in key-string form: `[u8 n][n key bytes]`, e.g. `02 80 01` | always |
| `06` | `KProjList` | operand: a spot path (projection field list) | Projection |
| `07` | `KFileIsGlobalAccess` | 16-byte access key (2.4) | always |
| `08` | `KLookupHigher` | `01 01` next higher, else next lower (8.6) | - |
| `09` | `KFieldKey` | field key segments, e.g. `01 09` (8.4) | - |
| `0a` `0b` `0c` | `KOperand1..3` | op-specific (SortList: 10.1) | - |
| `0d` `0e` | `KResult1..2`; `0d` is also `KMasterList` | `0d`: spot path of a RID list restricting SimpleFind (6.1) | - |
| `10` | `KExcludeRows` | spot path of a RID list (8.4) | - |
| `11` | `KModPath` | | - |
| `12` | `KLocaleType` | | - |
| `13` | `KPrefixKey` | | - |
| `14` | `KRowLimit` | | - |
| `15` | `KRow` | | - |
| `16` | `KRowSetKey` | | - |

Leaves `08`-`16` are named from the engine but were not used by any captured
program; which opcodes read them is in sections 6-10 where known. A `25`
`KFilter` leaf is also read by JoinProjection (9.2).

### 2.3 Opcodes (High for names and dispatch; see section 3 for semantics)

Opcode names are DWARF constants in `DBQueryProgram.h` (`kXxxOp`); the
dispatch is the 51-entry jump table in `DBQueryProgram::Execute`
(bin @0x10b5350, table @0x2e66564, indexed by `opcode - 1`).

| op | name | server handler | in cap |
|----|------|----------------|--------|
| 1 | (version query, inline) | reads its source and answers with the query instruction version | - |
| 3 | (node operation) | `NodeManipulation(op)` | - |
| 4 | `kCopyNodeOp` | `NodeManipulation(op)` | - |
| 5 | `kAndRidlistOp` | `CombineRidLists(op)` | - |
| 6, 7 | (RID-list combinations) | `CombineRidLists(op)` | - |
| 8 | (RID-list inversion) | `InvertRidList` | - |
| 10 | `kSimpleFindOp` | `SimpleFind` | cap:912 |
| 11 | `kProjectionOp` | `Projection` | cap:912 |
| 12 | `kJoinProjectionOp` | `JoinProjection` | 96 programs |
| 13 | (unnamed) | rejected on the host (error `0xffff`) | - |
| 14 | `kSortListOp` | `SortList` | - |
| 15 | `kSummarizeOp` | `GetSumValue` | - |
| 17 | `kLookupRowOp` | `LookupValue` | - |
| 18 | `kExistsInIndexOp` | `ExistsInIndex` | - |
| 19 | `kCreateValueListExOp` | `CreateValueListEx` | - |
| 20 | `kSemanticFindOp` | `SemanticFind` (error 3 unless enabled) | - |
| 40 | `kUploadMultipleOp` | `UploadMultiple` | - |
| 41 | `kNodeExistsOp` | `NodeExists` | - |
| 42 | `kGetKeyOp` | `GetKey` | - |
| 43 | `kGenerateMemberKeyOp` | `GenerateMemberKey` (schema-path.md 3.4) | - |
| 50 | `kDeleteTableOp` | `DeleteTable` | - (write op) |
| 51 | `kTruncateTableOp` | `DeleteTable(truncate)` | - (write op) |
| 2, 9, 16, 21-39, 44-49 | - | error `0xffff` | - |

`kQueryInstructionVersion` = 70 (DWARF constant).

### 2.4 The access key `[1, i, 7]` (High)

Every instruction carries a 16-byte value at leaf `07`:

```
access_key(i) = MD5( BE32(i) || connection_key[16] || BE32(file_id) )
```

- `i` = the instruction number (the decoded serial key of `[1, i]`, 2.6).
- `connection_key` = the 16-byte key from the Connect reply (the same value
  used as the login salt, network-protocol.md 4.3).
- `file_id` = the instruction's `[1, i, 4]` value (= header `fileid`).

Evidence: recomputing this formula reproduces all 98 instruction keys in cap
(96 programs, cap:912 has three instructions with `i` = 1, 2, 3); script in
`re/work/query/` (`access-key check`). The hash is the engine's `CRCContext`
(MD5: initial state `67452301 efcdab89 98badcfe 10325476`, 92-byte context;
bin libSupport `CRCContext::CRCContext` @0xe5dc0) fed by
`DBFileUser::GetGlobalAccessKey(CRC&, RCConnection const&, uint)` (bin
@0xf31470): input order instruction number, connection key, file id. (The file
id comes from the file object; for one file kind - `file[0xf8] & ~1 == 4` - a
different 32-bit field is used. Not observed; Low.)

Server side: `Execute` (bin @0x10b5350, around 0x10b5957) recomputes the key
for each instruction and compares all 16 bytes; on mismatch the program fails
with error `0xffff` and the server raises a server event. It also checks
`[1, i, 4]` against the file. A server must therefore know each connection's
Connect key, and must reject (status `0xffff`) a program whose keys do not
match. The check is guarded by per-file state (bin, path at 0x10b56a8); live
test in 6.x confirms whether it applies to a normal hosted file.


**A missing key is allowed (High, live:out12).** FileMaker Server runs an
instruction that has no `07` leaf (GetKey, NodeExists, SimpleFind and
InvertRidList all did) and answers `ffff` only when a key is present and wrong
(an empty `07` too). FileMaker Pro relies on this: it sends schema checks
(GetKey of `[3, 16, 1, fc]` with NodeExists of `[3, 16]`) without keys
(capture s06-omserver-n).

### 2.5 Operand values: spot paths (High)

Operands (`01`, `02`, `03`, `06`) are key paths written as their raw
segments `[u8 len][bytes]...` with no leading total length:

| operand bytes | path | meaning |
|---------------|------|---------|
| `01 02 01 01` | `[2, 1]` | upload spot 1 |
| `01 03 01 02` | `[3, 2]` | download spot 2 (returned to the client) |
| `01 02 01 03` | `[2, 3]` | upload spot 3 |

Upload spots carry client-supplied inputs (match values, predicates, finders)
or intermediate results; download spots are the outputs the client wants
back. In cap:912 instruction 2 writes `[2, 3]` and instruction 3 reads it, so
upload spots are also scratch registers.

Spot numbering in cap: the client numbers spots in one sequence across both
areas in the order it allocates them (`[2,1]` find data, `[3,2]` find
result, `[2,3]` projection output, `[2,4]` projection list, `[3,5]` join
result, `[2,6]` join predicates). A server only needs to follow the paths.

### 2.6 Serial keys (record ids, instruction numbers) (High)

Record ids, instruction numbers, and other numeric keys are `HBSerialKey`
segments: a prefix-coded unsigned integer, 1 to 9 bytes, whose bytes sort in
numeric order:

| length | first byte | value |
|--------|-----------|-------|
| 1 | `00`-`7f` | the byte |
| 2 | `80`-`bf` | `(BE16 & 0x3fff) + 0x80` |
| 3 | `c0`-`df` | `(BE24 & 0x1fffff) + 0x4080` |
| 4 | `e0`-`ef` | `(BE32 & 0x0fffffff) + 0x204080` |
| n | `n-1` leading 1 bits | `(BE & mask) + offset[n]`, offsets `0x10204080`, `0x810204080`, ... |

Evidence: bin `sSerialBaseValues` = `00 80 c0 e0 f0 f8 fc fe ff`,
`sSerialOffsets` = `0, 0x80, 0x4080, 0x204080, 0x10204080, 0x810204080,
0x40810204080, 0x2040810204080, 0x102040810204080` (read by `Execute` to
decode the instruction number, @0x10b590e). So record key `81 9a` is record
538 and `c4 f1 6c` is 340460. Keys compare as bytes; arithmetic on record ids
(RID-list bitmaps, 5.1) is done on the decoded integers.

Note for `om-hbam`: `uint_segment` / `KeySegment::as_uint` read keys as plain
big-endian integers. That is fine for identity and ordering, but not for
arithmetic; use the decoded value when adding offsets.

---

## 3. Instructions

### 3.1 JoinProjection (op 12) - relationships, portals (High for the cap form)

Finds the rows of the target table (`[1,i,5]`) that match, through one or
more join predicates, the values given for one or more source rows.

Operands:

- `KSrc` (`01`): match values, one subtree per source row:
  `src / <source row key> / <data field key> / <index key>` - each index key
  is a value-less leaf. The source row key is the record id of the row on the
  other side of the relationship (cap:534 uses `c2 2a a1`, the record returned
  by cap:530 in another file; cap:460 uses `01`). One data field key per
  predicate.
- `KDelList` (`03`): the predicate list, `pred / <n>` = one leaf per
  predicate, n = 1, 2, ...; value = `[u8 type][key A][key B]`, each key in
  key-string form `[u8 len][u8 seglen][seg]` (e.g. `02 01 06` = field 6,
  `03 02 82 64` = field 0x8264). A = the **target** table's field (its index
  is searched), B = the **data field key** under each source row in `KSrc`.
  `type` is `DBJoinPred::Type`: 0 `=`, 1 `≠`, 2 `<`, 3 `≤`, 4 `>`, 5 `≥`,
  6 cartesian (`×`), 8 between (bin DWARF `DBJoinPred::Type`; 7 invalid).
- `KDest` (`02`): the RID list of matching target rows (section 5).

Evidence for A/B: cap:546 has predicates `00 [06] [06]` and `00 [0a] [2f]`
with data keyed `[06]` and `[2f]`; cap:742 `00 [01] [01]`, `00 [80 2d] [61]`
with data `[01]`, `[61]`; cap:912 projects field `[01]` of table `80 01` and
joins with `00 [06] [01]` into table `80 02`.

Semantics (confirmed live, operator table and vectors in 9.2): a target row matches when, for every predicate, some index key of
its field A compares to some key under the source row's field B with the
predicate's operator; the result is the union over source rows. An empty
`KSrc` (cap:818, cap:1074) returns an empty list.

Captured examples (all status 0):

| cap | file | target | data | predicates | reply rids |
|-----|------|--------|------|-----------|-----------|
| 460 | 105 | `80 01` | `[1, 1] {80 06 ...}` (a six-digit number) | `00 [1] [1]` | `8180 = 3c000000` -> 538..541 |
| 502 | 141 | `80 06` | `[81 9a, 19] {01 ...}` (a text value's key) | `00 [1] [19]` | `8f dd` |
| 506 | 141 | `80 06` | another text value | same | `90 9d` |
| 510 | 141 | `80 06` | another text value | same | `91 64` |
| 514 | 141 | `80 06` | another text value | same | `91 99` |
| 546 | 285 | `80 05` | `[86 17] {[06] {80 06 ...}, [2f] {80 01 30}}` (cap:460's number) | `00 [6] [6]`, `00 [0a] [2f]` | `87 40` |
| 1034 | 132 | `80 01` | `[c7 7f 6d, 01] {80 06 ...}` (a six-digit number) | `00 [1] [1]` | `c4 19 51` |

### 3.2 SimpleFind (op 10) (Med)

Superseded by section 6 (operands, finder bytes, evaluation, Find mode).
`KSrc` holds one serialized **finder** (7.1) as a single leaf value
(cap:912: `[2, 1, 1] = 02 00 01 03 02 80 01 03 02 81 45 00 01 04 03 01 14 97
06 05 01 14 97 ff fe`); `KDest` receives the RID list of rows of the target
table that satisfy it. In cap:912 the finder is an index range on field
`81 45` of table `80 01`, from text key `01 14 97` (inclusive) to
`01 14 97 ff fe` (exclusive): every value whose index key starts with the
key of the typed text.

### 3.3 Projection (op 11) (Low)

Superseded by 9.1. `KSrc` = a RID list, `KProjList` = a field list (cap:912 `[2, 4] = {01 = 02
00}`), `KDest` = per-row index keys in the `JoinProjection` source form
(`dest / <row> / <field> / <index key>`), consumed by the next instruction.

### 3.4 Others

See sections 8 (RID-list algebra, node operations, ExistsInIndex, NodeExists,
GetKey, LookupValue) and 10 (SortList).

---

## 4. Reply

### 4.1 Success (High)

The result is the program's `[3]` subtree re-rooted so that `[3, n]` appears
as `[n]`: the reply slots are `80 <spot pushes and lists> 40`. cap:912 returns
two spots, `[2]` (the find result) and `[5]` (the join result). A spot that an
instruction wrote but that holds no rows is still present as an empty list
(`80 20 02 80 40 40`, cap:456).

### 4.2 Status codes

Superseded by section 11: op86 has no status field, and errors use the
error slot (11.2). The trailing u16 was 0 in all 96 cap replies. Known non-zero values (bin):
`0xffff` = unknown opcode, opcode not allowed on the host, or access-key
mismatch; 3 = command unavailable (SemanticFind when disabled); 0x12 is set
in one dispatch path (Low). Live tests in 6 extend this.

### 4.3 Error slot (Med)

Confirmed live and specified in 11.2 (the slot is the root leaf `00 = 02 hi lo`).
On a non-zero DBError the server writes an extra value at the `KBegin` key of
the download area: a Pascal-style string `03 02 <hi> <lo>`, i.e. the length 3
then `02` and the error code BE (bin `DBRemoteQueryProgram::PerformOperation`
@0x10b47a0). Its exact wire form is confirmed by a live vector in 6.

---

## 5. RID lists (found sets)

### 5.1 Encoding (High)

A RID list is a node whose children are leaves keyed by record ids (2.6), in
increasing order, in one of two forms:

- **singleton**: key = the record id, no value (`8f dd`).
- **bitmap run**: key = a base id that is a multiple of 32, value = `4k`
  bytes, k BE u32 words; word `j` bit `b` (b = 0 is the least significant
  bit) set means record `base + 32*j + b` is in the set.

Examples: `8180 = 3c000000` = records (0x200 + 26 .. 29) = keys `81 9a` ..
`81 9d` (cap:460, and the client's next four programs use exactly those four
rows as source rows, cap:502-514). `00 = 0000000e` = records 1, 2, 3
(cap:690). `8200 = 80000000 00008000 00100201` = five records (cap:808).
cap:912 mixes 168 singletons and 77 bitmap runs (545 records).

The same encoding stores a table's **record-id list** at `[table, 13]` in the
file (the hosted solution's `[80 01, 0d]` downloaded in cap:442 starts `00 = fffffffe
ffff...`: records 1..; test project Contacts `[80 02, 0d] = {01}`). The
client reads this list with op90 for Show All Records and the initial found
set; no op86 is involved (cap:442-456).

Which form the server chooses: every multi-record run in cap uses a base
multiple of 32 and only extends while records are dense enough; isolated
records are singletons; no singleton shares a 32-block with another singleton
(checked over all cap replies, `ridlist.py`). A reader must accept both forms
in any mix. Writer rule: open (section 13, item 9).

---

## 6. SimpleFind (op 10): one find request

Supersedes 3.2. Live vectors are named `live-<name>` in `re/work/query/vectors/`
(section 12); all ran against test project's Contacts (`80 02`), records
1 "Test Record", 2 "Hello World", 3 "Ima Computer", Company empty in all three.

### 6.1 Operands (High)

| leaf | content |
|------|---------|
| `05` KTableKey | key `[u8 n][n bytes]` of the table searched, e.g. `02 80 02` |
| `01` KSrc | spot whose leaf children `01`, `02`, ... each hold one finder (6.2) |
| `0d` KMasterList (optional) | spot holding a RID list; only those records are candidates |
| `02` KDest | spot that receives the RID list of matching records |

- Several finders under `KSrc` are **ANDed**: a record must satisfy every one
  (live:sf_two_finders_and, PK = rec 1 AND Company empty -> {1};
  live:sf_two_finders_or, PK = rec 1 AND PK = "A" -> {}). This is one Find
  mode request (one row of criteria).
- `0d` restricts the candidates (live:sf_masterlist: range over all PKs,
  restricted to {2} -> {2}). The client uses it to Constrain Found Set
  (bin `DBFindRowSolver::SolveOneTableOfPath` @0x10c10d0 writes `KMasterList`).
- No match is not an error: `KDest` gets an empty list (section 11).

### 6.2 Finder bytes (High)

Written by `DBFinder::Write(SwapOStream&, HBSerialKey<5> const&)` (bin
@0xe73a80); the value of each `KSrc` leaf:

```
u8   kind         FindKind, table below
u8   scope        DBFindScope: 0 word, 1 value, 2 repetition
u8   options      bit0 IncludeBegin, bit1 IncludeEnd, bit2 RelaxedMatch,
                  bit3 IncludeTooLong, bit4 reserved, bit5 Unstored,
                  bit6 ClientOnly, bit7 IsOptional
key  table        [u8 len][key string], e.g. 03 02 80 02 (table 80 02)
key  field        [u8 len][key string], e.g. 02 01 09 (field 9), 03 02 81 45
u16  repetition   1-based, e.g. 00 01
then, by kind:
  0-7, 10-12      key begin, key end: each [u8 len][index key bytes]; 00 = empty
  8  Match        u16 n, then n x u8 regex type; then one operand for each
                  type 0 (text), in order: u8 quoted (0/1), counted text
  9  DateTimeMatch u8, two DBTimePattern blocks, u16 (layout not decoded)
```

**Counted text** (a Match operand; `SwapOStream::Put<TextBuffer>`, Mac
Support 0x1ef012; read by `SwapIStream::Get<TextBuffer>` 0x1ee7f0) (High:
bin, live1):

| part | bytes |
|------|-------|
| count | the number of UTF-16 code units: one byte if below `0x80`; two bytes BE `0x8000 \| n` if below `0x4000`; `c2` and u16 BE if below `0x10000`; else `c4` and u32 BE (`SwapOStream::InternalSCSUCompress`) |
| text | the SCSU bytes of the text, each XOR `0x5A` (as in the file, file-format.md 3.5); the reader expands exactly `count` code units |
| styles | `00`. A reader that finds another byte reads a u32 BE length and skips that many bytes |

"Test" is `04 0e 3f 29 2e 00`, so an unquoted operand "Test" is
`00 04 0e 3f 29 2e 00`. `DBFinder::Read` (Mac DBEngine 0x1a3acd6) reads the
type list, then one operand per type 0; the other types take none. The
earlier probes (out8, out9) failed because none combined the one-byte count,
the masked bytes and the closing `00`.

`FindKind` (DWARF `DBFinder::FindKind`): 0 IndexMatch, 1 NotIndexMatch,
2 IndexRange, 3 IndexScan, 4 RowIdMatch, 5 NotRowIdMatch, 6 RowIdRange,
7 FindDuplicates, 8 Match, 9 DateTimeMatch, 10 FindDupsMinusFirst,
11 IndexFilter, 12 IndexNotFilter. Regex types (`DBRegexType`): 0 text,
1 wildcard, 2 digit, 3 Kleene star, 4 none-or-one, 5 wildcard not matching a
word break, 6 word break. The table key is `[u8 len]` + the `HBAbstractKey`
string (`03 02 80 02` = total 3, segment `[80 02]`); it may also be a table
occurrence id (`04 03 d0 00 02` worked in a sort spec, 10.2).

Example, the only finder in cap (cap:912, field `81 45` of table `80 01`):
`02 00 01 | 03 02 80 01 | 03 02 81 45 | 00 01 | 04 03 01 14 97 | 06 05 01 14
97 ff fe` = IndexRange, word scope, IncludeBegin, begin = text key "x",
end = "x" + `ff fe` (exclusive): every word that starts with "x".

### 6.3 Evaluation (High for kinds 0, 1, 2, 4, 5, 6, 8; Med for 7)

The finder reads the index of `field` in the hosted file (section 7.1):
scope 0 the **word** index `[T, 01, field, 01]`, scope 1 and 2 the **value**
index `[T, 01, field, 00]`. Keys compare as unsigned bytes.

| kind | result | live |
|------|--------|------|
| 0 IndexMatch | records under the index entry whose key = `begin` | sf_v_match_pk {1}; sf_word_pk {1}; sf_value_pk_word {} (a word is not a whole value); sf_v_company_empty {1,2,3} (empty text = key `01`); sf_scope2 {1} |
| 1 NotIndexMatch | all records of the table minus kind 0's result | sf_v_notmatch_pk {2,3} |
| 2 IndexRange | union of entries with `begin <= key <= end`, each bound inclusive only if its option bit is set | sf_v_range_pk `[K, K fffe)` {1}; sf_v_range_all `[01, 01ffff]` {1,2,3}; sf_value_lastname_r "r..." {1}; sf_ts_range 2026-10-28 {2,3} |
| 4 RowIdMatch | the record whose id = `begin` (serial key) | sf_rowid_match {2} |
| 5 NotRowIdMatch | all records minus that one | sf_rowid_not {1,3} |
| 6 RowIdRange | ids between `begin` and `end`, bounds per options | sf_rowid_range `[1,2]` {1,2} |
| 7 FindDuplicates | records whose key is shared with another record | sf_dups_company {} (Med: the only shared key was the empty value, which is not reported) |

"All records of the table" is `[T, 0d]` (7.1). An unindexed field has no
entries, so index kinds find nothing on it; FileMaker uses kind 8 (Match, a
record scan) there (6.4). (`DBFinder::Find`, Mac DBEngine 0x1a37ece, first
has a missing index built, `DBField::BuildIndexIfMissing`, and without one
evaluates kinds 0-2 by reading records, `DoFindKeyMatch`/`DoFindKeyRange`;
whether op 10 on the host takes that path was not traced, Low.)

**IndexScan (3) and `IncludeTooLong` (from bin; the finders observed).**
IndexScan returns the records under every index key that contains `begin`
at any byte offset; `begin` is a key fragment, a key without its leading
`01` (`DBMetaTree::ConvertToIndexKeyFragment`). With option bit 3
`IncludeTooLong` it adds the records under `ff 80` (7.1), whose values
may hold the fragment past the cut (`DBFinder::DoIndexScan`, Mac x86_64
DBEngine @0x1a38fd2, libDBEngine.so nm 0xf6fb10). Among IndexMatch,
IndexRange and IndexScan, only IndexScan reads `ff 80`. Cross-references
to `DBMetaTree::KTooLongEntry` also include `DoIndexFindDuplicates` and
the index builder `DBIndexTree::Parse` (Mac DBEngine); duplicate-find
behavior remains unresolved. IndexMatch and IndexRange on a cut key
return only the records under that key, whatever their options. FileMaker sets `IncludeTooLong` only on the
IndexScan it adds before a Match for a pattern without a leading literal
(options `88`, 6.4). The server's answer to an IndexScan was not run
directly.

**Match, kind 8 (High: bin, live1).** The host reads each candidate
record's value of `field` as text and tests it against the finder's
pattern (`DBFinder::DoMatch` and `FindMatchTester::PerValue`, bin
@0xf70cb0, @0xf6cf90), whether the field is indexed or not: value scope
tests the whole value, word scope each word. The regex types are matched in
order from the start: type 0 matches the next operand at that point
(unquoted with the field's locale, case-insensitive in live1; quoted with
`IsMatchFoldingAll`), type 3 any run of characters, type 1 any one
character, type 2 one digit, type 6 a word break; the value must end where
the pattern ends. Operand bytes are in 6.2. live1 on First Name (field 1,
indexed; the records of section 12):

| test | pattern | result |
|------|---------|--------|
| m_text_test | value, [text "Test"] | {1} |
| m_text_test_q | the same, quoted | {1} |
| m_text_test_lc | value, [text "test"] | {1} |
| m_te_star | value, [text "Te", star] | {1} |
| m_star_est | value, [star, text "est"] | {1, 10} |
| m_text_test_w | word, [text "Test"] | {1, 10, 12} |
| m_text_miss | value, [text "Qqqq"] | {} |
| m_star_only | value, [star] | {} |

The pattern [star] alone matched every record when First Name had no index
(out8 m_star, {1,2,3,10}) and none in live1, with the index built; why is
not known (13). FileMaker Pro does not send it: a criterion of `*` alone
becomes NotIndexMatch on the empty key (6.4).

Not verified: kinds 3, 9, 10, 11, 12.

### 6.4 From Find-mode criteria to finders (client side; Med unless marked)

The server never sees the typed criteria, only finders, so this table is for
building or checking a client. The operator recognition is High
(`DBFindParser::GetOpcode`, bin @0xf5d900, DWARF `DBFindParser::Opcode`); the
finder each one becomes is from the finder constructors in
`DBPredicateBuilder::Process` (bin @0xf5e9d0) and is Med; the reductions
`DBFindRow::Add` makes are below the table, and the rest of the optimisation
pass (`DBFindRow::Optimize`, `DBFindRow::ChooseScope`) was not traced, so
the exact keys are inferred except where observed.

| criterion | parser opcode | finder |
|-----------|---------------|--------|
| plain text `abc` (text field) | none, text | per word: IndexRange, **word** scope, IncludeBegin, `[key(abc), key(abc) ff fe)` - observed cap:912 for `x` (High) |
| plain value (number, date, time, timestamp) | none | value scope (scope = 0 only for text fields, `Process` tail @0xf5ff32); a match or range on that value's key |
| several words `abc def` | word delimiter (space, U+3000, CR) | one finder per word, all ANDed in the request |
| `=abc` | `kEqualOp` (1) | Match (8), word scope on a text field; `DBFindRow::Add` makes it IndexMatch (0) on the word's key (Med) |
| `==abc` | `kDoubleEqualOp` (2) | Match (8); a text field gets repetition scope (2), the whole value (Med) |
| `<v` `<=v` `≤v` | `kLessOp` / `kLessOrEqualOp` (3/4) | IndexRange (2), value scope, open lower bound; IncludeEnd only for `<=` |
| `>v` `>=v` `≥v` | `kGreaterOp` / `kGreaterOrEqualOp` (5/6) | IndexRange (2), value scope, open upper bound; IncludeBegin only for `>=` |
| `a...b` `a..b` `a…b` | `kRangeOp` (7) with left/right args | IndexRange (2), value scope, both bounds included |
| `!` | `kDuplicateOp` (12) | FindDuplicates (7), value scope, whole key range (High) |
| (duplicates minus first) | `kDuplicateMinusOp` (13) | FindDupsMinusFirst (10), value scope (High for the kind) |
| `=` alone | `kFindEmptyOp` (14) | IndexMatch (0), value scope, the empty key (text: `01`, live) |
| `==` alone | `kFindEmptyRepOp` (19) | IndexMatch (0), **repetition** scope (2) |
| `?` | `kFindInvalidOp` (16) | IndexMatch (0), value scope, special key (Low) |
| `*` `@` `#` `"..."` and other text | `kRegularExpr` (18) | Match (8) with regex types 3/1/2 and quoted operands (Med); `*` alone becomes NotIndexMatch (1) on the empty key, value scope (High, `Process`) |
| `~` prefix | sets relaxed | option bit 2 RelaxedMatch |
| `//` (today), date/time patterns | date parser (`DBTimeParser::AddFinders`) | DateTimeMatch (9) or ranges on date keys (Low) |

A find request with no criterion in any field matches all records (not
traced). On a text field the comparison operators and ranges read the
**word** index (scope 0, observed below), not the value index the table
above infers.

**Criteria longer than a key (High, observed).** The finders FileMaker
builds for a criterion whose key does not fit (7.1), traced in FileMaker
Server's engine running Perform Find on the probe file of 7.1 (an English
text field holding `x`×108, `x`×109, `x`×110, `x`×300, `x`×109 + U+0301
and a value with the word `x`×300; a private, uncommitted hook in the
harness printed each `DBFinder` the engine ran). They come from `DBFindRow::Add`
(Mac x86_64 DBEngine @0x1a3ca3a, the same code in FileMaker Pro; from
bin). K is the criterion's key cut to 219 bytes (`01` + 109 weights),
`K+` that key followed by `ff fe` (221 bytes, longer than any key a file
holds); options in hex.

| criterion | finders | records found |
|-----------|---------|---------------|
| `==` + `x`×300 | IndexRange, word scope, options `83` (IsOptional, both bounds), [K, K+]; then Match (8), scope 2 | the `x`×300 value only |
| `==` + `x`×109 | the same with K of `x`×109 | `x`×109 and `x`×109 + U+0301 |
| `"` + `x`×300 + `"` | IndexRange, options `83`, [K, K+]; then Match, scope 0 | the two values holding the word `x`×300 |
| `*` + `x`×120 + `*` | IndexScan (3), word scope, options `88` (IsOptional, IncludeTooLong), `begin` = the 109 weights without `01`; then Match, scope 0 | the two values holding `x`×120 |
| `=` + `x`×300 | IndexMatch, word scope, options `00`, K | every value with a word whose key cuts to K: `x`×109, `x`×110, `x`×300, `x`×109 + U+0301 and the word `x`×300 |
| `x`×300, `x`×300 + `*` | IndexRange, word scope, options `01`, `begin` = `end` = K | as `=` |
| `x`×108 | IndexRange, word scope, options `01`, [K108, K108 `ff fe`) | every word beginning with `x`×108 |
| `<` + `x`×300 | IndexRange, word scope, options `00`, (`01`, K) | words below K: not `x`×109 |
| `>=` + `x`×300 | IndexRange, word scope, options `01`, [K, `fe ff`) | from K up, `x`×109 included |
| `x`×300 `...` `y`×300 | IndexRange, word scope, options `03`, [K, Ky] (Ky: `y`×300 cut) | words from K to Ky, `x`×109 included |

So a criterion longer than a key is sent cut, and `IncludeTooLong` appears
only on the IndexScan pre-filter. `==`, quoted and `*...*` criteria add a
Match that compares the values, and their results are exact; `=`, plain,
`*`-suffixed, `<`, `>=` and range criteria match on the cut key alone, so
FileMaker's own results include or leave out values that agree with the
criterion in their first 109 weights. A plain criterion of exactly 109
weights gets `begin` = `end` = K (no room for `ff fe`), which IndexRange
answers with the records under K. Number and time criteria use value
scope: `-0.5` is IndexMatch, scope 1, `7f ff 9f`; `-1...1` IndexRange,
options `03`, [`7f fe df`, `80 01 20`]; `12:30 PM` on a time field
IndexRange, options `03`, over the whole minute. The table's finder results
come from the engine trace, not from the native client captures below.

**Long criteria sent by FileMaker Pro (High, observed).** FileMaker Pro
26.0.3 sent these op86 requests for Find mode on a text field in a
disposable file hosted by OpenMaker Server on loopback. Each request holds
one instruction. The lengths include the whole op86 request; program
length is the query program within it. `x`×300 is 300 UTF-16 code units.

| criterion | request / program bytes | finders in the program |
|-----------|-------------------------|------------------------|
| `x`×300 | 565 / 522 | IndexRange (2), word scope 0, options `01`, `begin` and `end` both 219 bytes |
| `==` + `x`×300 | 890 / 847 | IndexRange (2), word scope 0, options `83`, `begin` 219 bytes and `end` 221 bytes (`ff fe` suffix); Match (8), scope 2, 300-unit text operand |
| `*` + `x`×300 + `*` | 670 / 627 | IndexScan (3), word scope 0, options `88`, `begin` 218 bytes and `end` one byte (`ff`); Match (8), scope 0, 300-unit text operand |

The plain request's two bounds end in the same cut weights. The Match
finders carry the full criterion text; their operand text is 300 bytes in
this capture. The native client requests establish the wire shapes for
these three criteria. FileMaker Server's replies to them, including its
behavior with and without `IncludeTooLong`, remain unmeasured. The
loopback host returned normal no-match for the plain criterion and an
unsupported-finder error for the other two; those are OpenMaker Server
results, not FileMaker Server evidence. Evidence: private capture
`re/work/issue-316/long-find-loopback/capture/conn-000` and [#126](https://github.com/OpenMaker-Dev/openmaker/issues/126).

**Which Match finders reach the host (Med: bin).** `DBPredicateBuilder::Process`
(Mac DBEngine 0x1a44818) builds a Match finder for the parser opcodes 1
(`=`), 2 (`==`) and 18 (a pattern), with the regex types and operands from
`GetRegExpr`, and sets option bit 2 for `~`. `DBFindRow::Add` (0x1a3ca3a)
then reduces some of them before the program is written
(`DBFindRowSolver::SolveOneTableOfPath` calls `DBFinder::Write`):

- a pattern of no types is dropped;
- outside repetition scope, a pattern that is one unquoted text operand
  becomes IndexMatch (0) on its key, and on a text field one unquoted text
  operand followed by `*` becomes IndexRange (2) from its key to the key
  plus the largest sort weight (IncludeBegin);
- on a number field, a literal followed only by digit wildcards (`#`)
  becomes IndexRange (2), both bounds included, from the literal with a `0`
  for each wildcard to the literal with a `9` for each;
- anything else stays a Match finder. A text field's Match finder that
  starts with a text operand also gets an optional IndexRange finder
  (options `0x83`: IsOptional, IncludeBegin, IncludeEnd) on that literal, to
  narrow the records the host scans.

So FileMaker Pro sends kind 8 for `==` criteria, quoted text, and patterns
with `@`, a leading or inner `*`, `#` on a text field, or more than one
literal. Which exact finders each criterion yields still needs a capture of
Find mode (13).

### 6.5 Programs FileMaker Pro sends for Find (Med)

**Find on a field of the layout's table** (one request): one instruction,
inferred from `DBFindRowSolver::SolveOneTableOfPath` (bin @0x10c10d0) and the
cap form:

```
[01, 01]: 00 = 0a, 01 = [02, 01], 02 = [03, 02], 04 = file id,
          05 = table, 07 = access key  (+ 0d = [02, x] when constraining)
[02, 01]: 01 = finder, 02 = finder, ...     (the request's criteria, ANDed)
reply:    [02] = RID list of the found set
```

live:sf_value_lastname_r is exactly this shape (Last Name begins with "r" ->
{1}). It was built by OpenMaker and accepted by FileMaker Server; it has not
been seen from FileMaker Pro.

**Find on a related field** (observed, cap:912): SimpleFind in the related
table, Projection of the join field's keys, JoinProjection back into the
layout's table (`DBFindRowSolver::SolveFastPath` @0x10bd520):

```
[01, 01] op 0a  src [02,01] (finder)            dest [03,02]  table 80 01
[01, 02] op 0b  src [03,02]  proj [02,04]       dest [02,03]  table 80 01
[01, 03] op 0c  src [02,03]  preds [02,06]      dest [03,05]  table 80 02
[02, 04] = {01 = 02 00}   (field 1 as number)   [02, 06] = {01 = 00 [06] [01]}
reply: [02] = matches in 80 01 (545 records), [05] = found set in 80 02
```

**Several requests, Omit, Constrain, Extend** (Low, not observed): the client
keeps a RID list per request and combines them with ops 6 (OR across
requests), 7 (AND NOT for an Omit request), 5 or `0d` (Constrain) and 6
(Extend), possibly in the same program. A server that implements ops 5-8 and
`0d` covers every combination; the exact instruction sequences FileMaker Pro
emits still need a capture.

### 6.6 Quick Find (client side)

The toolbar's search box in Browse mode and the script step Perform Quick
Find (150) search several fields of the current layout at once. What the
programs on the wire look like is not captured; this section is what a
client builds, from Claris's documentation, the starter solutions and one
observation.

**Fields searched** [Claris: FileMaker Pro help, "Configuring quick find"
and "Performing a quick find in Browse mode"]:

- The fields on the current layout that are included for Quick Find: the
  field object's flag `0x8000` clear (layout-model.md 5.4.1). A field shown
  by several objects is searched unless every one of them is excluded
  ("change the quick find setting for all instances of the field on the
  layout; otherwise, the field will still be included").
- Related fields are searched, with unindexed fields and unstored
  calculations ("searchable, but the search might take longer").
- Merge fields are included by default. The Database Design Report gives a
  text object's merge fields a `quickFind` attribute
  (`<TextObj><FieldList quickFind>`), so the setting exists; where it is
  stored is [U].
- Summary, container and global fields are never searched.
- A layout whose Layout Setup clears **Enable Quick Find** (layout-model.md
  4.2) has no Quick Find; Layout mode then shows every badge gray.

**Matching** [Claris, same pages]: "If you enter more than one word,
FileMaker Pro finds records that contain all of the words", and "the only
operator supported by quick find is the match phrase operator, double
quotation marks". So a record is found when every word, or quoted phrase,
is in at least one searched field (the words may be in different fields).
The FileMaker Pro help for Perform Quick Find gives `Get ( LastError ) =
401` as the test for no match.

[O, [#201](https://github.com/OpenMaker-Dev/openmaker/issues/201), FileMaker Pro 26.0.3 on macOS] One word typed in the search box
of each of the six English starter solutions found the record holding it.
On copies of Contacts, Tasks and Inventory holding Alpha Cedar, Beta Cedar
and Alpha Birch, in that order, with Alpha Birch current, `Alpha` found 2
and kept Alpha Birch current, `Alpha Cedar` found 1 and showed it, and a
word that matches nothing kept the found set and the current record
([#201](https://github.com/OpenMaker-Dev/openmaker/issues/201), recheck at 10:46Z). So the current record stays current when the
new found set holds it, and the first record is shown otherwise [I from
those two cases].

[Reported: The Support Group, "Quick Find, Error Capture, and Omit All"]
A Quick Find that matches nothing, from the toolbar or from Perform Quick
Find, shows "No records match this find criteria." and leaves the found
set as it was, where Perform Find leaves an empty one. Not measured here.

[I] How a word matches a field. Each word is read as a Find mode criterion
(6.4) with every other operator character taken literally, so it matches
as a plain word does: the start of a word in a text field. A number, date,
time or timestamp field takes a word only when its type reads it as a
plain value; the word is no criterion there otherwise (a find with
"Alpha" in a date field must not fail). A quoted phrase is a phrase
criterion on text fields. The found set is unsorted afterwards, as after
a find.

Unknowns:

- What Perform Quick Find does on a layout with Quick Find turned off, and
  whether the search box takes text there (OpenMaker: the box takes no
  text; the step finds nothing, 401).
- Whether objects hidden by a hide condition, on another tab panel or slide
  panel, or in a popover are searched (OpenMaker: every field object of the
  layout).
- Whether merge fields in button labels count (OpenMaker: every resolved
  merge field of the layout counts).
- Which words number, date and time fields take, and the sort order after
  a Quick Find.
- The buttons of the no-match dialog (OpenMaker: OK) and what Quick Find
  does in Find mode.
- Whether Perform Find, Constrain and Extend keep the current record the
  same way (OpenMaker shows the first record of the new found set), and
  which record Quick Find shows when the current one is not found but the
  new found set is sorted.
- `Get ( QuickFindText )` reads the search box (calc-functions.md); when
  FileMaker fills it from the toolbar is not measured.

---

## 7. Field indexes and index keys

### 7.1 Where indexes live (High)

Under each base table `T` (`80 01` and up) in the hosted file:

| path | content |
|------|---------|
| `[T, 05, rid, field]` | the stored record values (file-format.md 3.6) |
| `[T, 0d]` | **every record id** of the table, as one RID list (5.1); `KMasterList` |
| `[T, 01, field, 00, key]` | **value index**: one node per distinct key, whose children are the RID list of the records holding that key |
| `[T, 01, field, 01, key]` | **word index** (text fields): the same, keyed by single words |
| `[T, 01, field, 10]` | 16 bytes, u64 BE + u64 BE: records indexed, distinct value keys |
| `[T, 01, field, 11]` | 16 bytes: the same two counts for the word index |

Evidence: test project `[80 02, 01]` downloaded live with op90
(`re/work/query/live/out3/dl_index/slots.bin`):

- First Name: `[01, 00]` = `{01 12d3 126b 1330 1330 138e}` -> {2} ("Hello"),
  `{01 12ec 135f 120f}` -> {3} ("Ima"), `{01 1433 126b 1410 1433}` -> {1}
  ("Test"); `[01, 10]` = 3 records, 3 keys.
- Company (empty in all three records): `[05, 00, 01]` = `{00 = 0000000e}`,
  i.e. key `01` -> {1, 2, 3}; `[05, 10]` = 3 records, 1 key. The records
  store no value for field 5 at all, yet are indexed under `01`.
- PrimaryKey: `[09, 00]` holds the three whole UUID strings; `[09, 01]` holds
  8 word keys (`[09, 11]` = 8, 8). Word breaking is not a plain split at `-`:
  record 1's "392E36CA-E9D8-45A3-83DD-66A6EFF95ADE" gives "392E36CA",
  "E9D8-45A3-83DD" and "66A6EFF95ADE", and record 3 contributes no "319A"
  word. The word-break rules are open (section 13).
- `[80 02, 0d]` = `{00 = 0000000e}` (records 1, 2, 3); FMS sample Tasks
  `[80 01, 01, 01, 00]` = number keys, `[80 01, 01, 03, 00]` = date keys.

A field with no index has no `[T, 01, field]` node. Index kinds with
`scope` 0 read `[.., 01]`, scopes 1 and 2 read `[.., 00]` (6.3).

The client reads these structures itself (cap: op90 of `[T, 0d]` before every
layout's first display, s06 captures: `[80 02, 0d]` right after the layout
downloads), so a server that changes records must keep them current.

**Indexes are built on demand (High, live:ai_*, out7).** An index finder on a
field without that index returns no records, and FileMaker Server then
builds exactly the index the finder read, with its counts: a word-scope
find on the unindexed CreatedBy left `[80 02, 01, 0a]` = `{01 {admin ->
records}, 11}`; a word find on First Name (value index only) added its
`[01]` word index and `11`; a value find on the unindexed Title added `[00]`
and `10`. A later find answers from the new index. How FileMaker Pro finds
on an unindexed field (a Match scan, or a request that builds the index
first) is open (13).

**Word breaking (High for the cases below).** The word index keys each
word of a value: maximal runs of letters and digits, in which some
characters between two letters or two digits join:

- between digits: `-`, `.`, `,`, `:` and `/` (`392E36CA-E9D8-45A3-83DD-66A6EFF95ADE`
  gives `392E36CA`, `E9D8-45A3-83DD`, `66A6EFF95ADE`; `3.14`, `3,000`,
  `12:30` and `50/50` are one word each);
- between letters, in English: an apostrophe (`'` or U+2019) and `.`
  (`don't`, `l'équipe`, `x.y`, `U.S.A.` gives `U.S.A`). In French an
  apostrophe separates (`l'équipe` gives `l`, `équipe`).

Every other character separates (`e-mail`, `x-1`, `a_b`, `foo@bar.com`
gives `foo`, `bar.com`; `#tag`, `C++`, `AT&T`, `$5`, `5%`), and so does a
change between kanji, hiragana, katakana and Latin letters
(`日本語のテキスト` gives `日本語`, `の`, `テキスト`; `日本語abc123` gives
`日本語`, `abc123`). U+FFFE and an emoji do not separate (`a😀b` is one
word). Evidence: every record of every word-indexed text field in test
project and the en/fr/es/it FMS samples (`crates/om-collate/tests/indexes.rs`),
and the word indexes of the probe file of 7.2 (English, French and
Japanese fields). The full break rules (FileMaker's own word-break
classes, `Unicode::GetWordBreakClass`, Support @0x219760) are open.

**Lines (High, corpus).** The value index keys each line of a text value
(split at CR or LF) on its own and skips empty lines; a value without a
non-empty line is keyed `01`, the empty text.

**Long keys (High).** An index holds at most **219 bytes** of a text key,
the longest key segment a file can hold (hbam-writer.md 2.2): `01` and 218
bytes of weights. A line (value index) or word (word index) whose key does
not fit is listed under the part that fits, and the record is listed once
more under the key **`ff 80`**, which is no value's key: one entry however
many of the record's keys were cut.

The rule is the key builder's (from bin, each point observed below).
`DBMetaTree::MakeSortIndexKey` (libDBEngine.so nm 0x1037e30) gives
`KeyBuilder` a budget of 218 bytes for an index key (200 for a sort key).
`KeyBuilder::AddNextWeights` (Mac x86_64 Support @0x176ede) adds an
element only when its weight fits whole in what is left and fewer than
218 elements have been added, counting elements with a zero primary
(accents) too. A character's elements are added in turn, and the
character fails only when its last element does not fit; the builder
stops at the first failed character, and after the last character it adds
one more element for the end of the text. A key whose builder stopped, or
whose last element did not fit, is flagged: the record goes under `ff 80`.
So:

- The cut falls between whole weights, never inside one. A key of exactly
  219 bytes (109 weights) is whole and not under `ff 80`; with 110
  weights the first 109 are kept and the record is under `ff 80`
  (`x`×109 against `x`×110 and `x`×300; `中`×109 against `中`×110, one
  weight each, 7.2). The two weights of a character without a table entry
  can be separated: `x`×108 + U+1F600 keys `x`×108 then `ffe1`, under
  `ff 80`, while `x`×107 + U+1F600 is whole (219 bytes).
- A value of 218 or more collation elements is flagged although its
  weights fit: `ü`×109 (an element for `u` and one for the diaeresis
  each) keys 109 weights under `ff 80`, `ü`×108 + `u` is whole; `x` + 216
  U+0301 keys `01 1497` whole, `x` + 217 U+0301 keys `01 1497` under
  `ff 80`; `ǘ`×73 (three elements each) keys 73 weights (147 bytes) under
  `ff 80`, `ǘ`×72 is whole.
- An accented letter whose primary weight no longer fits but whose accent
  element does is not flagged: `x`×109 + `é` and `x`×109 + `ü`×5 key
  `x`×109, not under `ff 80`; `x`×109 + `é` + `x` is (the `x` fails).
- Overflow while processing a character handled by a tailoring (7.2)
  does not flag the value: in German, `ä`×60 keys 109 weights not under
  `ff 80`, while `ä`×55 + `x` is under it. This exception applies to the
  character, not every value in that language: a later ordinary character
  can fail and flag the value. In catalog language (4), `y`×300 + a space
  + `x`×300 has value-index entries `01 149c{109} ff80`, although its
  word-index entries are unflagged. Unicode raw handles every code unit
  through its tailoring and is never flagged (`x`×300 keys 109 code units
  alone).
- The padded form (System 1, Unicode standard 3) also needs room for its
  `00 00` and secondary bytes, and is flagged unless they leave part of
  the budget unused: `x`×107 keys 217 bytes whole, `x`×108 keys `01`, 108
  weights and `00 00` (219 bytes) under `ff 80`, `x`×109 keys the 109
  weights alone under `ff 80`. A 219-byte padded key is always flagged.
- Number, date, time and timestamp keys are never flagged; a number key
  keeps at most 200 digits (210 nines and `1E400` keep their first 200).

Evidence: a probe file FileMaker Server's engine wrote with
`tools/fms-script-harness` (contacts-en with each test value copied into
indexed stored calculations, one per index language, and indexed number,
time, timestamp and date fields; every case above, in the value and the
word index; `re/work/needs-re/145/harness`), and the
text-field value and word indexes of the FileMaker-written `.fmp12` files
under `re/` (shipped templates, FMS samples, oracle and hbam-writer
outputs, the MBS plugin example files in `re/work/calc-encoding/corpus-mbs`):
all 18 values there with a key longer than 219 bytes (values of 112 to
828 characters, one of them a 116-character word) are stored as above, and
no segment in any index is longer than 219 bytes. A model of this rule
with the key forms of 7.2 reproduces every key and `ff 80` entry of
158,742 stored values in these files (`re/work/needs-re/145`). The
historical Claris help describes a limit of 100 characters; FileMaker's
files hold 109 weights.

IndexMatch and IndexRange do not read the `ff 80` records; among those
two finders and IndexScan, only IndexScan with `IncludeTooLong` reads
them (6.2, 6.3). Duplicate-find behavior remains unresolved. A find whose
criterion is longer than a key matches on the cut key (6.4).

**Key order (High), and how om-query reads an index (OpenMaker's
implementation).** An index's keys are the children of `[T, 01, field,
kind]`, so the file holds them in key order like every key (file-format.md
3.1: unsigned bytes, a shorter key first on a common prefix), each key
once. om-query relies on that order. Each instruction finds its keys by
binary search and reads the RID lists of only the keys its answer needs:

| instruction | keys it finds |
|-------------|---------------|
| IndexMatch, NotIndexMatch (6.3) | `begin` |
| IndexRange | the run between the bounds |
| FindDuplicates | every key (its answer needs each list's size) |
| ExistsInIndex (8.4) | the client's keys, sorted and looked up in turn from the last position |
| CreateValueListEx (8.7) | the run of keys that start with the prefix |
| JoinProjection (9.2) | for `=`, each source key; for `≠`, `<`, `≤`, `>`, `≥`, one run per source row, since a key is below some source key when it is below the greatest, and so on |
| Projection, SortList | every key (they invert the index) |

The lists are merged into one result a list at a time. With one predicate,
JoinProjection gathers the runs of all rows first and reads each key once;
a cartesian predicate restricts the union to the table's records once.
Answers that follow key order keep it: CreateValueListEx lists values in
key order, and SortList sorts a record held under several keys by its
lowest (13, question 6).

The first time a program reads an index, om-query checks that its keys
increase, one comparison per key. A file whose keys are out of order does
not load (the key tree refuses it), OpenMaker Server builds missing indexes
in key order and changes them in place, so no index it serves fails the
check. An index from another source that does (keys out of order or one
repeated) is read as if sorted, the records of a repeated key merged.
Every lookup then answers as on the sorted index. The entry `ff 80` (long
keys, above) is no value's key, so om-query leaves it out of every index it
reads: no lookup, run, count or full read meets it, and each of its
records is found under its cut key. No value's key starts with `ff`, so in
an index in order it is the last entry, and the index is read one entry
shorter, still in place; anywhere else it is dropped as the index is read
as if sorted. What counts towards a program's work is in 11.4.

### 7.2 Text keys (High for the forms and the weights shown)

`01`, then 16-bit big-endian collation weights (the padded form adds a
secondary level, below). Case is ignored (`T` and `t` both `1433`); the
empty string is `01`. FileMaker builds the key with `Draco::KeyBuilder`
(Support; Mac x86_64 slice `KeyBuilder::MakeKey` @0x1771e8, driven by
`International::MakeKey` @0x1775f2 from `DBMetaTree::MakeSortIndexKey`,
libDBEngine.so nm 0x1037e30; from bin): it reads the value's characters in
order, lets the field's index language *tailor* the current character
(`International::TailorWeights`), and otherwise takes the character's
collation elements from FileMaker's table; each element with a non-zero
primary weight adds that weight. How a long key is cut and flagged is in
7.1 (Long keys).

Evidence for this section (observation unless marked "from bin"): the
value-index keys of every FileMaker-written file under `re/` (the FMS
samples in English, French, Spanish, Italian, German and Japanese, the MBS
plugin examples with English, German and Unicode raw fields, the harness
fixtures) and a probe file FileMaker Server's engine wrote with
`tools/fms-script-harness`: 140 text values, each in a stored calculation
per `Locale::Type` (74 languages; FMUpgradeTool stored the requested 0 and
77 as 21), plus number, time, timestamp and date fields. A model of the
rules below and in 7.3, with FileMaker's cut and long-key flag (7.1),
reproduces the stored keys of all 158,742
values of these files in the untailored languages, German, Japanese
(level-1 kanji), the padded and raw forms and the typed fields
(`re/work/needs-re/145`); the other tailorings are described from the
probe alone. Characters a find reads as operators have no special place
in a key: in a production-like hosted solution's lookup table (5,405
records, English index), every value of five match fields that holds `#`,
`.`, `=` or `?` (2,218 values with `#`, 4,235 with `.` in one field) sits
under the key `om_collate::text_key` gives (observation, 2026-11-17). The probe's keys are in
[catalogs/index-key-vectors.tsv](catalogs/index-key-vectors.tsv).
All weights seen in captures (English index language, test project and the
s03 capture):

| char | weight | char | weight | char | weight | char | weight |
|------|--------|------|--------|------|--------|------|--------|
| `-` | `0223` | `5` | `120a` | `b` | `1225` | `n` | `136d` |
| `0` | `1205` | `6` | `120b` | `c` | `123d` | `o` | `138e` |
| `1` | `1206` | `7` | `120c` | `d` | `1250` | `p` | `13b3` |
| `2` | `1207` | `8` | `120d` | `e` | `126b` | `r` | `13da` |
| `3` | `1208` | `9` | `120e` | `f` | `12a3` | `s` | `1410` |
| `4` | `1209` | `a` | `120f` | `g` | `12b0` | `t` | `1433` |
| | | `h` | `12d3` | `i` | `12ec` | `u` | `1453` |
| | | `k` | `131e` | `l` | `1330` | `w` | `148d` |
| | | `m` | `135f` | `x` | `1497` | `y` | `149c` |

Evidence: live index and projections (7.1, 9.1), cap:502-514 (four text
values), cap:912 ("x").

**The table is DUCET 5.2.0, per code point (High).** Every weight above is
the primary weight of that character in the Unicode Collation Algorithm's
default table of UCA 5.2.0 (`allkeys.txt` 5.2.0 from unicode.org),
unchanged; no other UCA version from 4.1.0 to 9.0.0 matches. A key is `01`
followed by the primary weight of each collation element of the value, in
order, as a big-endian u16: variable elements keep their weight (space
`020a`, `-` `0223`; no "shifted" handling), elements with a zero primary
(combining marks, controls) add nothing, and an expansion adds each of its
primaries. That explains case and accent blindness (`é` = `e`). FileMaker
looks the table up one code point at a time (`Unicode::GetCompSortWeights`,
Support @0x219cc4, from bin), so DUCET's contractions do not apply: Thai
`เก` keys `01 1faa 1f70` in the order written, where DUCET's contraction
would put `ก` first. FileMaker's table differs from DUCET in these points:

- The curly single quotation marks U+2018..U+201B key as `'` (`02ee`) and
  the double ones U+201C..U+201F as `"`, with secondary weight 0x21
  instead of 0x20 (`it’s` = `01 12ec 1433 02ee 1410`, `‘a’` = `01 02ee
  120f 02ee`; the padded form shows the 0x21). Observed for U+2018 and
  U+2019; the others are inferred.
- A CJK compatibility ideograph (U+F900..U+FAFF, U+2F800..U+2FA1F) keys as
  its canonical ideograph (`豈` U+F900 = `01 8c48`), in every form but raw
  (`KeyBuilder::NextDecomposedChar`, Support @0x176b92).
- U+FFFE, U+FFFF and an unpaired low surrogate add nothing (`a` U+FFFE
  `b` = `01 120f 1225`).
- A code point the table does not list gets FileMaker's own weights, not
  UCA's implicit weights (`KeyBuilder::MakeKey`, from bin; each row
  observed):

| code points | weights | example |
|---|---|---|
| CJK unified ideographs U+4E00..U+9FCB | the code point, one weight | `中文` = `01 4e2d 6587` |
| CJK extension A U+3400..U+4DB5 | code point + 0x6FCB | `㐀` = `01 a3cb` |
| Hangul syllables U+AC00..U+D7A3 | code point + 0x19AD | `한국어` = `01 ef09 c71a df61` |
| U+20000..U+2B7FF (CJK extensions B to D) | the high surrogate - 0x16C0 (`c180`..`c1ad`), then the low surrogate | U+20000 = `01 c180 dc00` |
| any other BMP code point | `ffe0`, then the code point | U+0378 = `01 ffe0 0378` |
| any other supplementary code point | `ffe0` + (code point >> 16), then its low 16 bits | U+1F600 = `01 ffe1 f600` |

Supplementary characters DUCET lists key from the table (U+1D400
MATHEMATICAL BOLD CAPITAL A = `01 120f`, U+10000 = `01 3148`).

**Normalization (High).** FileMaker does not normalize a value before
keying it. In untailored non-raw forms, DUCET's elements for a precomposed
letter are those of its decomposition. Measured tailoring examples also
agree: `ä` and `a` + U+0308 are both `01 120f` in English and both
`01 120f 126b` in German; raw keeps the stored code units (`00e4` against
`0061 0308`). This does not establish equivalence for every tailoring:
the Greek-mixed catalog rows give `Ä` primary weight `120f` but
`a` + U+0308 weight `1545`, and `Ñ` weight `136d` but `n` + U+0303
weight `1558`. The remaining tailoring rules stay open. A tailoring
examines the current character together with its following combining
marks, decomposed and upper-cased (`KeyBuilder::NormalizedCurrentChar`,
Support @0x1774a8, from bin); the measured Japanese tailoring keys
halfwidth katakana like the fullwidth ones.

**Index language and form (High).** The field definition's `mIndexLang`
(bytes 4..7, a `Locale::Type`) selects the form. Language 0 means the
file's own language (`[2, 0d]`, file-metadata.md 2):
`DBMetaTree::MakeIndexKey` (libDBEngine.so nm 0x10389b0) resolves 0 through the file's
locale (from bin), and the German and Japanese FMS samples key their
language-0 fields in German and Japanese. (A Projection request that names
language 0 gets no key, 9.1.)

| form | languages | key |
|---|---|---|
| primary | English 21, English international 77 (live:proj_text_l4d), and the tailored languages below | `01` + the primary weight of each element |
| padded | System 1, Unicode standard 3 | the primary form, `00 00`, then for each element with a non-zero secondary weight one byte, the weight minus 0x1F; those bytes are left out when all of them are `01`: `Test` = `01 1433 126b 1410 1433 0000`, `équipe` = `01 126b 13c8 1453 12ec 13b3 126b 0000 01 13 01 01 01 01 01` (`KeyBuilder::AppendHigherLevels`, Support @0x17710e) |
| raw | Unicode raw 2 | `01` + the stored UTF-16BE code units, case kept, a supplementary character as its two surrogates (`Test` = `01 0054 0065 0073 0074`) |

System (1) keyed exactly like Unicode standard (3) in the probe, which
ran FileMaker Server's Linux engine. FileMaker Pro 26's own blank file
keys its PrimaryKey, indexed in Unicode raw, the same way: `01` and the
UUID's UTF-16BE code units (file-format.md 8).

**Tailorings.** Support overrides `TailorWeights` for Arabic, Catalan,
Chinese, Chinese stroke, Croatian, Czech, Danish, Estonian, German, Greek
mixed, Hindi, Hungarian, Icelandic, Japanese, Korean, Latvian, Lithuanian,
Persian, Polish, Romanian, Slovak, Slovenian, Spanish, Spanish
traditional, Swedish, Swedish (FileMaker), Thai, Turkish, Ukrainian,
Vietnamese, Unicode raw and the catalog language (symbol table). A
tailored character usually keys as a base weight plus one, as a weight
after `z` (`14ad`), or as two letters. What the probe shows (primary
keys; only the characters it held, so a language may tailor others):

| language | tailored characters |
|---|---|
| German 25 | `ä ö ü`, and `a`, `o`, `u` + U+0308, as the vowel then `e`: `Äpfel` = `01 120f 126b 13b3 12a3 126b 1330`. German dictionary (26) is untailored (`ä` = `a`) |
| Danish 19, Norwegian 32 | `æ ä` `14ae`, `ø ö` `14af`, `å` `14b0`, `ü` as `y` |
| Swedish 41, Swedish (FileMaker) 42, Finnish 22, Finnish (FileMaker) 23 | `å` `14ae`, `ä` `14af`, `ö ø` `14b0`, `ü` as `y`; `æ` untailored |
| Icelandic 29 | `é ó ú` the vowel + 1 (`126c 138f 1454`), `æ œ` `14e1`, `ö ø` `14e2` |
| Hungarian 28, Turkish 43 | `ö ü` the vowel + 1 (`138f 1454`); Turkish also `ı` `12d4` |
| Czech 18, Croatian 17, Slovenian 38, Slovak 37 | `č š ž` the letter + 1 (`123e 1411 14ae`); Slovak also `ä` `1210` |
| Polish 33 | `ó` `138f`, `ź` `14ae` |
| Spanish 39, Spanish traditional 40 | `ñ` `136e`; `ch` and `ll` untailored in both |
| Estonian 50 | `ä ö ü` `1494 1495 1496`, `š ž` `1430 1432` |
| Latvian 52, Lithuanian 51 | `y` `1304`; `čšž` = `01 124f 1432 14c9` (Latvian), `01 124f 124f 1432 14c9` (Lithuanian) |
| Persian 54 | `ي` U+064A `1951` |
| catalog 4 | `ı` as `i` |
| Vietnamese 56 | a weight `0001` after vowels (`Test` = `01 1433 126b 0001 1410 1433`); the rule is open |
| Thai 57 | Latin letters other weights (`x` `105a`), `0001` after a Thai prevowel (`ไทย` = `01 1fae 0001 1f86 1f91`), leading and trailing spaces dropped; the rule is open |
| Greek mixed 58 | Latin letters key as Greek ones (`Test` = `01 1567 154a 1563 1567`) |
| Chinese 45, Chinese stroke 46 | ideographs by FileMaker's tables, pinyin (`一 中 文` = `a8b2 b4ad 9d6f`) and stroke order (`4e1d 4f0b 4f63`); the tables are not derived |
| Korean 76 | Hangul syllables as jamo weights (`가` = `4fb7 ce8b`), ideographs by a table (`中` = `ab73`) |
| Japanese 31 | below |

The probe's strings key exactly as in English in Catalan 16, Dutch 20,
French 24, Greek 27, Italian 30, Portuguese 34, Romanian 35, Russian 36,
Ukrainian 44, Hebrew 47, Hindi 48, Arabic 49, Serbian 53, Bulgarian 55 and
the Indic languages 59 to 67.

**Japanese (High for kana and level-1 kanji).** For index keys the
Japanese tailoring (`JPN_International::TailorWeights` @0x1b76c0 and
`GetIndexValue` @0x1b7808, from bin) gives kana, the sound marks and
kanji weights of its own; every other character keys from the table
(Latin letters and digits as in English, `ー` `11dd`):

- Hiragana are `40xx` and katakana `44xx`, the low byte the kana's place in
  a gojūon order that gives each syllable slots for its small forms, its
  plain form and its voiced and semi-voiced forms (`ぁ 4000`, `あ 4001`,
  `か 400c`, `き 400e`, `ん 4056`; `ァ 4400`, `カ 440c`, `ン 446a`;
  katakana have extra slots, so `ク` is `4411` but `く` `4010`). Every
  kana's weight is in [catalogs/index-key-vectors.tsv](catalogs/index-key-vectors.tsv)
  (`kana` rows).
- A voiced kana keys as its plain kana then `01f4`, a semi-voiced one then
  `01f8` (`ば` = `4031 01f4`, `パ` = `4438 01f8`); U+3099/U+309B key `01f4`
  and U+309A/U+309C `01f8`. Halfwidth katakana take the fullwidth weights
  (`ﾊﾝｶｸ` = `01 4438 446a 440c 4411`).
- A kanji of JIS X 0208 level 1 (rows 16 to 47) is 0x967E + (row - 1) × 94
  + (cell - 1): `一` (row 16, cell 76) = `9c4b`, `中` = `a33f` (42 of 42
  level-1 kanji in the probe and the Japanese sample). Other kanji have
  table weights whose order is not derived (`豈`, JIS level 2, `b1ed`;
  `丂`, JIS X 0212, `b619`); a kanji outside the table keys as in the base
  form (`㐀` `a3cb`).

A server that only answers from stored indexes needs none of this; one that
builds keys does.

### 7.3 Number, date, time and timestamp keys

Keys compare as unsigned bytes, a shorter key sorting first on a common prefix
(file-format.md 3.1); each form below makes byte order equal value order.

Evidence (observation unless marked): the probe file of 7.2 (59 number,
16 time, 8 timestamp and 5 date values, catalogs/index-key-vectors.tsv),
the FMS samples and the MBS examples (26,898 number values in all these
files, among them `379,5` = `80 03 48 a6` in a German file) and the
captures cited. The key
builders are `DBFixPtKey::InternalSet<FixPt>` (libDBEngine.so nm
0x104bc70) and `DBDateTimeKey::Set` (nm 0x104b2d0), called from
`DBMetaTree::MakeSortIndexKey` (from bin).

**Number (High).** A big-endian u16 head, then one nibble per decimal
digit, integer digits first, then the fraction digits, without leading
integer zeros or trailing fraction zeros:

- zero, `-0` and `0.0`: `80 00`;
- a positive number: head 0x8000 + the number of integer digits, then
  each digit as `digit + 1`, the last byte padded with a `0` nibble;
- a negative number: head 0x7FFF - the number of integer digits, then each
  digit as `14 - digit` (the positive nibble complemented), then an `f`
  nibble, the last byte padded with `f`. The `f` after the digits makes a
  longer magnitude sort first (`-1.25` below `-1.2`).

| value | key | evidence |
|-------|-----|----------|
| 1 | `80 01 20` | FMS sample Tasks::TASK ID (record value 1) |
| 12 | `80 02 23` | sample |
| 250000 | `80 06 36 11 11` | encoder (cap:460 holds a six-digit number of this form) |
| 4000002 | `80 07 51 11 11 30` | cap:822 |
| 1.20 | `80 01 23` | probe |
| 100.10 | `80 03 21 12` | probe |
| 0.5, .5 | `80 00 60` | probe |
| 0.05 | `80 00 16` | probe |
| 0.001 | `80 00 11 20` | probe |
| -1 | `7f fe df` | probe |
| -12 | `7f fd dc ff` | probe |
| -1.5 | `7f fe d9 ff` | probe |
| -1.25 | `7f fe dc 9f` | probe |
| -0.5 | `7f ff 9f` | probe |
| -0.05 | `7f ff e9 ff` | probe |
| -2/3 (stored `-.6666666666666667`) | `7f ff 88 88 88 88 88 88 88 87 ff` | probe |

A key holds at most 200 digits (the nibble loop stops there, from bin);
the head then counts only the digits kept, so 210 nines key as 200 nines
(`80 c8` + 100 `aa`) and `1E400` as `80 c8 21 11 ...`. A number key is
never listed under `ff 80`.

The key is built from the stored text, read the way FileMaker reads a
number (observed): every digit in order, other characters dropped
(`12abc34` = 1234, `1/2` = 12, `$5`, `5%` and `+5` = 5; live:proj_pk keys
"392E36CA-E9D8-45A3-83DD-66A6EFF95ADE" as its 17 digits); the file's
decimal separator, its first occurrence only (`1.5.2` = 1.52), thousands
separators dropped (`1,234.5`, `1 000`); an exponent (`1e3` = 1000,
`12.5e2` = 1250, `1e-5`); a minus sign `-` or U+2212 before the digits, or
parentheses, makes it negative (`(5)` = -5, while `5-` = 5). Fullwidth
digits are stored as ASCII digits; Arabic-Indic digits are not digits.

**Empty and invalid values (High).** In a number, date, time or timestamp
index an empty value is listed under `01`, the key of empty text
(`DBMetaTree::KNullEntry`), and a value with no number in it (`abc`), a
date FileMaker cannot hold (`12/31/9999`, stored `?`) or an invalid
timestamp under `fe ff` (`KInvalidEntry`). Every probe record without a
number value is in its number index under `01`.

**Date (High).** Three bytes `0x800000 | year << 10 | month << 6 | day`
(month and day 1-based), then the time key of midnight `80 00`:

| date | key |
|------|-----|
| 2014-02-17 | `9f 78 91 80 00` |
| 2015-08-02 | `9f 7e 02 80 00` |
| 2015-10-09 | `9f 7e 89 80 00` |
| 0001-01-01 | `80 04 41 80 00` |
| 4000-12-31 | `be 83 1f 80 00` |

Evidence: FMS sample `[80 01, 01, 03, 00]` against each record's Due Date
(11 values); the probe. Years before 1 and the date field's own repetition
handling are open.

**Time (High).** The date part of year 0, `80 00 00`, then the number key
of the seconds since midnight, fraction included (`DBDateTimeKey::Set`
writes `0x800000` for year 0, then the number key of
`DateTime::GetSecsSinceMidnight`; from bin):

| time | key |
|------|-----|
| 21:26:57 | `80 00 00 80 05 88 32 80` |
| 21:26:57.5 | `80 00 00 80 05 88 32 86` |
| 00:00:00 | `80 00 00 80 00` |
| 00:00:00.25 | `80 00 00 80 00 36` |
| 12:00:00.123456 | `80 00 00 80 05 54 31 12 34 56 70` |
| 25:00:00 | `80 00 00 80 05 a1 11 10` |
| -01:00:00 | `80 00 00 7f fb b8 ee ff` |

A find for `12:30 PM` on a time field reads the range [`80 00 00 80 05 56
11 10`, `80 00 00 80 05 56 16 aa aa aa a0`], the whole minute up to
45,059.9999999 s (the finder FileMaker Server's engine built for that
find, traced in the probe).

**Timestamp (High).** The 3-byte date part, then the number key of the
seconds since midnight: 2026-10-27 21:26:57 = `9f aa 9b | 80 05 88 32 80`
(live:proj_ts; the test record's CreationTimestamp), 21:26:57.5 the same
day `9f aa 9b | 80 05 88 32 86`, 0001-01-01 00:00:00.5 = `80 04 41 | 80 00
60`. A timestamp entered as a date alone is stored with midnight and keyed
so; one whose time is invalid (`10/27/2026 -1:00:00`, stored
`10/27/2026`) is listed under the date's key and under `fe ff`. A find for
one day is the range `[date 80 00, date 80 05 5f ff ff]` or any upper
bound above the day's last second (live:sf_ts_range, 2026-10-28 -> {2,
3}).

**Container, boolean, JSON:** not observed.

### 7.4 Keys in requests (High)

The client sends index keys in exactly these forms: finder bounds (6.2),
JoinProjection source values (9.2), Projection outputs (9.1), sort keys (10.3).
A text find bound is often a key followed by `ff fe` or `ff ff` so that an
exclusive or inclusive upper bound covers every key with that prefix (cap:912,
live:sf_value_lastname_r).

---

## 8. RID-list algebra and node operations

Operand values in instruction leaves are key paths written as segments without
a total length (2.5): `01 02 01 01` = `[02, 01]`; a one-segment key such as a
table is `02 80 02` and a field is `01 09`. (Finders, join predicates and sort
specs instead carry keys with a leading total length, 6.2.)

### 8.1 CombineRidLists, ops 5, 6, 7 (High)

`KDest := KDest OP KSrc`, both RID lists (5.1); the result is written back to
`KDest` in the canonical encoding.

| op | OP | live (dest {2,3,5}, src {1,2,3}) |
|----|----|----------------------------------|
| 5 | AND (intersection) | {2,3} - and_lists |
| 6 | OR (union) | {1,2,3,5} - or_lists |
| 7 | AND NOT (dest minus src) | {5} - op7_lists |

Record ids are not checked against the table (record 5 does not exist and
survives). Leaves read: KSrc, KDest (bin `CombineRidLists` @0xfc68e0).

### 8.2 InvertRidList, op 8 (High)

`KDest := [T, 0d] minus KDest`, where `T` is `KTableKey`. A missing `KDest`
counts as empty. live:inv_empty -> {1,2,3}; live:inv_23 (copy {2,3}, then
invert) -> {1}. Reads KDest, KTableKey, KFileId and the file's `[T, 0d]`
(bin `InvertRidList` @0xfc6b70).

### 8.3 NodeManipulation, ops 3 and 4 (High for 4, Med for 3)

- op 4, copy: `KDest :=` a deep copy of the `KSrc` subtree, replacing what was
  there (live:copy_node copies a leaf and a nested node).
- op 3, delete: removes the `KDest` node; `KSrc` is not changed (live:op3_node
  deletes a `[03, 02]` that the request itself supplied; live:op3_move).

Both act on program spots only.

### 8.4 ExistsInIndex, op 18 (High)

Answers "is any of these keys already used by another record", the uniqueness
check for field validation (the client side is `DBQuery::IsExisting`, bin
@0x10afdd0).

| leaf | content |
|------|---------|
| `05` | table, `02 80 02` |
| `09` KFieldKey | field, `01 09` |
| `01` KSrc | spot whose value-less leaves are index keys |
| `10` KExcludeRows (optional) | spot holding a RID list of records to ignore (the record being edited) |
| `02` KDest | result spot |

Result: `[dest, 00] = 01 01` if some key has a record in the field's value
index that is not excluded, else `01 00`. live:exists2_pk true, exists2_miss
false, exists2_two (hit + miss) true, exists2_pk_excl (exclude {1}) false,
exists2_pk_excl3 (exclude {3}) true. The values `01 01` / `01 00` are the
engine's true/false keys (`KTrue` = `[01]`, `KFalse` = `[00]`) as a
length-prefixed value. A field key written `02 01 09` instead fails with error
102 (live:exists_pk).

**FileMaker Pro's own `10` (capture s06-omserver-n).** When the user creates
related records in a portal, FileMaker Pro checks a unique primary key of the
new records with ExistsInIndex on the related table (`80 04`, field `06`) and
lists the records of its own transaction in `10` as leaves with a one-byte
value: `[02, 02] = {01 = 01, 02 = 01}` (records 1 and 2, value `01` = the
commit list's "new" state, write-path.md 3.4). So `10` is a set of records
keyed by id, not necessarily a RID list: a leaf with an empty or one-byte
value names one record (Med for how FileMaker Server reads a longer value; a
RID list's bitmap runs, if sent, are four-byte multiples).

### 8.5 NodeExists (41) and GetKey (42) on the hosted file (High)

Both take `KSrc` as a key path **into the hosted file** (from the root), not a
program spot:

- NodeExists: `[dest, 00] = 01 01` if the path exists, `01 00` if not.
  live:nodeexists_file_02_09 true, nodeexists_file_02_01 false,
  nodeexists_file_rec1 (`[80 02, 05, 01]`) true, nodeexists_file_rec9 false.
- GetKey: `[dest, 00] =` the value stored at the path: `[02, fe]` ->
  `da dc ff d3` (the file's `[2, 254]` stamp), `[80 02, 05, 01, 01]` ->
  `0e 3f 29 2e` (masked "Test"). For a path that is a node the value is an
  opaque 14-byte server position (live:getkey_node); for a missing path the
  spot is empty (live:getkey).

### 8.6 LookupValue, op 17 (High: bin, live1)

Serves a lookup field's "if no exact match, copy next lower/higher value"
(and, inferred from its caller `DBFieldRefArg::Lookup`, the calculation
function `LookupNext`): it finds one record of the target table, and the
client copies the value from that record itself. Server: `DBQueryProgram::LookupValue` and `LocalLookupProjection`
(bin @0x10cdce0, @0x10d25c0; Mac DBEngine 0x1b36a58, 0x1b39076).

| leaf | content |
|------|---------|
| `05` | target table |
| `01` KSrc | source rows, as JoinProjection's `row / field B / key` (9.2) |
| `03` KJoinPreds | join predicates, as JoinProjection's |
| `08` KLookupHigher | `01 01` (KTrue): next higher; any other value or none: next lower |
| `02` KDest | result spot |
| `04`, `07` | file id, access key |

`08` maps to `DBField::LookupDef::LookupType` 2 kLookupNextHigher or 1
kLookupNextLower; types 0 (do not copy) and 3 (use a constant) never reach
the host.

**Algorithm.** The predicates are taken in order, each as in JoinProjection
(9.2), each result ANDed with the earlier ones and filtered by the session's
record access:

1. While some records satisfy every predicate so far, go on with the next
   predicate. If records satisfy them all, no record is chosen: `[dest, 00]
   = 01 00` (record 0).
2. At the first predicate that leaves none: if the source row has no key
   for B, fail with 401. Otherwise take the row's key (with several keys,
   the last one read, Med) and read the value index of the predicate's
   field A in the target table. Next lower starts at the largest key below
   it, next higher at the smallest key above it (`DBIndexCursor::GetByEntryKey`
   with `false`/`true`, then one step when the key itself is present).
3. From there, move down (lower) or up (higher) the index to the first
   entry that holds a record that also satisfied every earlier predicate
   (any record, at the first predicate); the result is that entry's lowest
   record id. The later predicates are not consulted.
4. No such entry: fail with 401 ("no records match").

**Reply.** `[dest, 00]` = the record id as a one-segment key `[u8
len][serial key]`, e.g. `01 02` = record 2, or `01 00` when step 1 found
records. The value is not returned. A failure answers the error slot `00 =
02 0191`, with `[dest]` empty (11.2). live1 (PK, field 9, keys of records
10 < 3 < 1 < 2 < 12 < 13; Company, field 5, empty in all but record 1):

| test | request | reply |
|------|---------|-------|
| lk_pk_lower_A | PK = "A", lower | `01 0d`: record 13, the highest key below "A" |
| lk_pk_higher_A | PK = "A", higher | error 401: no key above |
| lk_pk_higher_low | PK = key `01 11`, higher | `01 0a`: record 10, the lowest key |
| lk_pk_lower_low | PK = key `01 11`, lower | error 401 |
| lk_pk_exact_higher | PK = record 1's key, higher | `01 00`: an exact match names no record |
| lk_company_lower_A | Company = "A", lower | `01 02`: the empty key's lowest record of {2, 3, 10, 12, 13} |
| lk_two_lower | PK = record 1's key and Company = "A", lower | error 401: the empty key holds no record matching PK |
| lk_two_higher | the same, higher | `01 01`: record 1's Company key is above "A" |
| lk_nokey | source row without a key, higher | error 401 |

The October vectors (live:lookup_*) agree: `01 00` wherever a record
matched, `01 02` (then the highest PK) for "A" with no `08`.

**When FileMaker Pro sends it (Med: bin).** `DBQuery::PerformLookup` (Mac
DBEngine 0x1b1c352; called by `DBDependencyList::DoLookup` for lookup
fields and by `DBFieldRefArg::Lookup`) first evaluates the relationship as
for any related record (JoinProjection, 9.2). A related record found is
the exact match; its first record is copied and no op17 is sent. With
none, and the field's option next lower or next higher, it sends one
program of one instruction: op `11`, `08` = `01 00` (lower) or `01 01`
(higher), `05` the target table (from the occurrence's definition), `04`,
`07`, `01` an upload spot holding the current record projected on the
relationship's match fields (`DBQueryProgram::LocalProjection`), `03` an
upload spot with the relationship's predicates (`WriteJoinPredicates`) and
`02` the next download spot. It reads `[dest, 00]` as the record id; error 401 means
nothing is copied. "Do not copy" and a foreign (ODBC) target fail with 401
without asking the host; "use a constant" copies the constant
(`CopyLookupData`). For a target table in a hosted file, a direct
relationship with a single `=` predicate whose target table holds fewer than
25,000 records (the client's count) is looked up by the client itself
(`PerformLocalLookup`, with its own query engine over the records it
reads), so op17 serves larger tables and other relationships. That
FileMaker Pro reads record 0 when op17 answers `01 00` after a race is
inferred from the code and was not traced.

### 8.7 CreateValueListEx, op 19: a field's values (High)

Lists the distinct values of a field from its index. FileMaker Pro sends it
inside op86 for each keystroke in a field formatted with auto-complete (capture
`2026-10-28-s06-omserver-l`, typing "Evil Corp in c" into Company: the
prefix grows by one character per request) and, from bin, to fill a value
list that uses a field's values (below). Server:
`DBQueryProgram::CreateValueListEx` (bin @0x10cbf70; Mac DBEngine
0x1b35a96). Reply layout from FileMaker Server (probes
`re/work/query/live/out10`, `out11`; vectors `vl-*`; live1:vl_*).

| leaf | content |
|------|---------|
| `05` | table: FileMaker Pro names the table occurrence (`03 d0 00 02`); a base table (`02 80 02`) gives the same answer |
| `09` | field, `01 05` |
| `0a` | u8: `01` lists the value index, whole values (`DBQuery::FindValue`); `00` the word index, one entry per word (`DBQuery::FindWord`) (bin). FileMaker Pro sent `01` in every capture; with single-word values `00` gave the same answer |
| `0b`, `0c` | optional, both or neither: a second field's table and field (bin) |
| `13` | the prefix as `[u8 len][index key]`, e.g. `03 01 126b` ("e"); absent = every value |
| `14` | u32, the most values to list (`00000064` = 100), read only when `13` is present: without `13` every value is listed whatever `14` says (bin, Linux disassembly; live1:vl_noprefix_max1) |
| `02` | output spot |

The host first builds the field's index if it is missing
(`DBField::BuildIndexIfMissing`, the value or word index as `0a` asks) and
applies the table's pending changes. Values come from that index: from the
first key, or from the first key not below the prefix, every key in key
order until one does not start with the prefix (`DBValueListData::PrefixMatches`,
by the field's type), except the empty-value key `01`. The output spot holds
two leaves:

- `00`: one entry per value: the record id as a one-segment key string
  `[u8 total][u8 len][serial key]`,
  then the index key as a one-segment key string, then `[u32 len][text]`
  with the record's text for that value, unmasked (plain ASCII for ASCII
  text). With `0b`/`0c` each entry ends with a second `[u32 len][text]`:
  the second field's value in the same record, empty when it cannot be
  read (bin). The record is the first of the key's records whose value
  gives that key, so its lowest record (Med: no key with several records was
  probed). No entries: `00` holds no value.
- `ff`: u64, the number of values listed. When `13` and `14` are present
  and the list stopped at `14` while the next index key also starts with
  the prefix, `ff` is minus that number (two's complement):
  `ffffffffffffffff` for 1 (`vl_first_t_max1`), `fffffffffffffffe` for 2
  (live1:vl_all_max2). A list that reaches `14` with no further key with
  the prefix has `ff` = the number; `14` = 0 lists nothing, `ff` = 0.

**`00` past 1,000 bytes (High: bin, Mac and Linux; observed hosted reply).** The host writes the
entries through a character stream (`HBCharOStream`; its buffer overflows
at 1,000 bytes, bin @0x155ee00, `cmp $0x3e8`). Up to 1,000 bytes `00` is one
value. A longer list makes `00` a node: child `00` = u32 BE total byte
length, children `01`, `02`, ... (serial keys) = pieces of exactly 1,000
bytes, the last the remainder. This is the form of a long value in a file
(hbam-writer.md 2.3). There is no size cut: the host lists every value up to
`14`, and a reply over `0xFFF00` bytes is paged (11.1, observed). A 2026-11-14 live FileMaker Server reply to one direct
op86 / CreateValueListEx query had `[2,00]` as a node: child `00` held total
length 3,772 (u32 BE), and children `01` through `04` held 1,000, 1,000,
1,000 and 772 bytes. The assembled entries held two values (3,507 and 9
text bytes), and `[2,ff]` was 2: it counted entries, not pieces. The host
returned the four pieces in one op86 reply; OpenMaker's client returned no
values for this reply (#464). Earlier probes had `00` as one leaf of at
most 275 bytes [O: private `s464-value-list-20261114/conn-005`].

**A paged field value list (High: observed FileMaker Pro and FileMaker
Server, private capture `s137-hosted-privileges-20261114/conn-000`).** A
pop-up menu backed by an indexed field in a disposable hosted file sent one
op86 program with CreateValueListEx (op19), no prefix `13` and no maximum
`14`. Its Perform `maxreturn` was 1,048,618. The first reply held 1,048,101
slot bytes; five more held 1,048,215 each; the last held 327,032. The six
nonfinal replies had nonzero `perform` and `cursor` handles and depth 3.
FileMaker Pro followed each with op86 whose program payload was empty and
whose request resume block echoed those handles and depth. The last reply
had handles 0 and depth 0. The seven slot streams reassembled to one result
spot `[1]`: its `00` held 6,583,389 entry bytes in 6,583 full 1,000-byte
pieces and a 389-byte final piece, and `ff` was 55,001. Parsing the entries
also counted 55,001, so `ff` counts values, not pieces or pages. GIOP
fragments within a reply must be joined before parsing its Perform result.
FileMaker Pro showed the list from those replies and a second opening in the
same session sent no new op86. This one open brought the whole list without
a 100-value limit.

After the same value list was set to display a second text field, opening
the pop-up sent op19 with `0b` and `0c` naming that field, still without
`13` or `14`. The result took eight pages: 1,048,101 slot bytes, six pages
of 1,048,215, then 107,952 (7,445,343 total). The `[1,00]` entry stream
held 7,408,399 bytes in 7,408 full 1,000-byte pieces and a final 399-byte
piece. `[1,ff]` and the parsed entry count were again 55,001. Each entry
held two length-prefixed texts; the first was `Alpha` and `case-0`, and the
next held the first synthetic value and its matching label. FileMaker Pro
showed both texts in the pop-up [O: same private capture].

The source file held `Alpha`, `alpha`, `ALPHA` and `Álpha` in four records.
The returned list had one entry for those four and displayed `Alpha`, the
first stored form. Thus case and this accent difference shared one index
key in this hosted value list. Width variants were not tested [O: same
capture; other collation rules remain in 7.2].

An unindexed field (Title) answers `00` empty and `ff` 0. The host would
have built an automatic index, so Title's indexing is presumably set to
None (inference).

**One value per index key.** The list holds one entry per key of the value
index, so values that share a key are one value (case and accents, 7.2).
That includes a key cut to 219 bytes (7.1), which stands for every value
that starts with the same 109 weights: the list shows one of them, the
lowest record's (inferred from the rule above; no probe listed a cut key).

**FileMaker Pro's requests (from bin; screens observed below).**
`DBFieldValueListData::Generate` and `GenerateRemote` (Mac DBEngine
0x1d787fa, 0x1d78f94) use op19 for a value list that uses a field's values
when:

- the file is hosted;
- the field's table occurrence exists and its base table is in the same
  file;
- the list does not include only related values (that list is built through
  the relationship, `GenerateRelated`);
- the field is not in an ODBC table.

With a second field, op19 is used only when a flag of the generation context
is set; that flag was not identified. The hosted pop-up configured to display
a second field did use op19 with both operands [O: above]. Otherwise, and when op19 fails with
error 100, the client walks the field's index itself (`GenerateIndexed`; for
a hosted file through the copy of the index it downloads with op90,
inference). The request is one instruction: `00` = `13`, `07`, `04`, `09`,
`0a`, `05` and `02` the next download spot, with `0b`/`0c` for a second
field. It has two shapes:

- **The whole list** (`DBValueListData::SetupValueListData` generates it at
  once unless the list is set up for prefix lookups): no `13` and no `14`,
  so the host lists every value. FileMaker Pro used this shape for the
  Insert From Index dialog in the 2026-11-14 capture.
- **Type-ahead** (`DBValueListIter::FindSubset` and
  `DBValueListData::FindRangeWithPrefix`): `13` = the typed text's key and
  `14` = the caller's maximum (100 in every capture).

The client reads `ff` as a signed count: with a prefix it reads |`ff`|
entries and takes a negative `ff` to mean that more values exist; without a
prefix it reads `ff` entries only when `ff` is positive. It reads `00`
through `HBCharIStream`, which reads a single value or the pieces.

Observed, FileMaker Pro (captures s06-omserver-l and -n, 36 requests):

- every op 19 carries only `00 02 04 05 07 09 0a 13 14`, with `14 = 100`;
- the prefix is `01`, every value, when the field is empty;
- no leaf asks to continue after a cut list, and `13` only matches keys
  that start with the prefix.

**UI paths with a long field value [O: private
`s464-value-list-20261114/conn-006`].** The field's value index held one
existing value and one disposable 3,507-byte value. These are op19
*instructions inside op86 transactions*, not outer RCTransaction opcodes.
Each returned a 3,814-byte slots tree (3,837-byte result) in one network
reply, with the 3,772-byte `00` node above. The first file open had unrelated
op90 schema and layout reads; none of these three actions downloaded the
index with op90.

| UI action | instruction-19 query | request (transaction / GIOP), reply (GIOP) | immediate second use |
|-----------|----------------------|-------------------------------------------|----------------------|
| Open the editable Company field's value-list control | `13=01 01` (all values), `14=100` | 110 / 202 bytes, about 3,874-3,877 bytes | sent the same op86 query again |
| Insert From Index | no `13` or `14` | 100 / 192 bytes, 3,874 bytes | reopening the dialog sent no query |
| Type one character in the field | `13=03 01 12 6b` (prefix), `14=100` | 112 / 204 bytes, about 3,874-3,875 bytes | clearing and typing the same prefix sent the query again |

The immediate Insert From Index reuse does not establish its cache lifetime.
The second-field and paged-list cases above were captured later with a
pop-up menu. After the disposable value was deleted, FileMaker Pro's index
dialog showed only the original value again.

**OpenMaker Server.** It answers as above (#465): `14` only with `13`, no
size cut, `00` as 1,000-byte pieces past 1,000 bytes, and `ff` minus the
count when `14` stopped the list before another key with the prefix. It
reads no `0a`, `0b` or `0c` (OpenMaker's gap, [#238](https://github.com/OpenMaker-Dev/openmaker/issues/238)): every list comes from the
value index, with no second field. A reply that cannot be encoded at all
answers the error slot `02 ffff` (11.2), never the empty list `80 40`. How
OpenMaker's client reads a list is in `docs/value-lists-missing-index.md`;
it reads the pieces (#464) and a negative `ff` as a cut list (#465).

Example (vl_first_t, prefix "t" on First Name): `00 = 02 01 01 | 0a 09 01
1433 126b 1410 1433 | 00000004 "Test"`, `ff = 0000000000000001`.
live1:vl_all_max2 (prefix `01`, `14` = 2, First Name with 6 values): the
first two entries ("Hello", "Ima"), `ff = fffffffffffffffe`.

**Table occurrences.** Instructions may name a table occurrence instead of a
base table. An occurrence's definition `[3, 17, 5, occ, 2]` starts `02 01
00` and then its base table as a key string (`03 02 80 02`), in test project
and every FileMaker Server sample.

---

## 9. Projection and JoinProjection

### 9.1 Projection, op 11 (High for the shape and types 1, 2, 5)

Turns a RID list into the index keys of selected fields, in the form
JoinProjection takes as input. Supersedes 3.3.

| leaf | content |
|------|---------|
| `05` | table of the records |
| `01` KSrc | spot holding a RID list |
| `06` KProjList | spot whose leaves are `field = [u8 data type][u8 index language]` |
| `02` KDest | output spot: `rid / field / key` (value-less leaves) |
| `0a`, `0b` (optional) | a Bloom filter size and bytes that limit the output (read by `Projection`, bin @0xfc83d0; not observed) |

Data type is `DBDataType`: 1 text, 2 number, 3 date, 4 time, 5 timestamp,
6 binary, 7 boolean, 8 JSON. Index language is `Locale::Type` (7.2). Each
record's stored value is converted to the requested type and keyed in that
language; values that do not convert produce nothing, and a record with no key
for any listed field produces no node.

| live | request | output for records {1,2,3} |
|------|---------|-------------------------------|
| proj_text_l15 | `01 = 01 15`, `09 = 01 15` | First Name and PK text keys, English form |
| proj_text_01, proj_text_l03 | language 1 / 3 | text keys plus `00 00` |
| proj_text_l02 | language 2 | `01` + UTF-16BE |
| proj_text | language 0 | nothing |
| proj_pk | `09 = 02 00` | PK digits as number keys (7.3) |
| proj_ts | `0c = 05 00`, `0d = 05 00` | timestamp keys |
| proj_number, proj_date, proj_time | text and timestamps as 2/3/4 | nothing |
| proj_company | Company (empty) as number | `[02] {}` |

cap:912 projects `[02, 04] = {01 = 02 00}` (field 1 as number, language 0)
from the 545 found rows, then joins on it (6.5).

Type 0 keys nothing (live:proj_text_00). FileMaker Server keys the stored
values, not the value index: live:proj_ts keys ModificationTimestamp, which
has no index. When the requested type is the field's own type and the
language gives the index's form, the output equals the value index inverted
(record -> keys) without its empty-value key `01`; om-query answers that case
from the index and reports the others (conversions, unindexed fields, other
languages) as unsupported until it can key stored values.

### 9.2 JoinProjection, op 12: full semantics (High)

Complements 3.1. Operands: `KSrc` = `row / field B / key` (value-less keys
per source row); `KJoinPreds` (`03`) = leaves `01`, `02`, ... each
`[u8 type][key A][key B]` with A and B written with a total length (`02 01
06`, `03 02 82 64`); `KTableKey` = target table; `KDest` = result RID list.

- A is a field of the target table (its value index is searched); B is the
  field id used under each source row.
- A target record matches a source row when **every** predicate holds; a
  predicate holds when some key of the record's field A and some key under
  the row's B satisfy `A_key OP B_key`, keys compared as bytes.
- The result is the **union** over source rows; an empty `KSrc` gives an
  empty list.
- Source row ids are arbitrary keys; the client uses the related record's id
  (cap:534 `c2 2a a1`) or a projected record id (cap:912).

`DBJoinPred::Type` and live results (target PK; source key K = record 1's PK;
PK order is record 3 < 1 < 2):

| type | OP | result | live |
|------|----|--------|------|
| 0 | `=` | {1} | jp_pk_hit |
| 1 | `≠` | {2,3} | jp_ne |
| 2 | `<` (target < source) | {3} | jp_lt |
| 3 | `≤` | {1,3} | jp_le |
| 4 | `>` | {2} | jp_gt |
| 5 | `≥` | {1,2} | jp_ge |
| 6 | `×` cartesian: every target record | {1,2,3} | jp_cart |
| 7 | invalid | - | - |
| 8 | between | not tested | - |

Multi-key and multi-row cases: jp_two_keys (two keys under B, one matching)
{1}; jp_two_rows (one matching row, one not) {1}; jp_two_preds_hit
(PK = K and Company = empty) {1}; jp_two_preds_miss (Company = "A") {};
jp_company_emptykey (Company = `01`) {1,2,3}; jp_pk_miss, jp_empty_src {}.

Not observed: `25` KFilter and `0a` KOperand1, which `JoinProjection` reads
when present (bin @0xfc90c0; by name, a relationship filter and its operand;
section 13).

---

## 10. SortList (op 14): Sort Records

Built by the client in `DBQueryProgramRunner::RemoteSort` (bin @0xfda7b0),
run by `DBQueryProgram::SortList` (bin @0xfca0d0). Not in cap; all evidence
is live (`live-sortA_*`, `live-sortI_*`, `live-sortA_empty_op2`).

### 10.1 Operands (High)

| leaf | content |
|------|---------|
| `05` KTableKey | table, `02 80 02` |
| `01` KSrc | **an inline row set, not a spot path**: `[01, i, 01]` is a node holding either the leaf `ff` (`KAll`: every record of the table) or the node `05` (`KItems`) holding a RID list |
| `0a` KOperand1 | the sort spec (10.2) |
| `0b` KOperand2 | the row set's table, `02 80 02` (a table occurrence `03 d0 00 02` also works); empty -> error 106 |
| `02` KDest | output spot (10.3) |

Examples: live:sortA_last_up uses `[01, 01, 01] = {ff}`; live:sortI_last_up
`[01, 01, 01] = {05 {00 = 0000000e}}`; live:sortI_23_first_down
`{05 {02, 03}}`. A plain spot path in `01` (as for other ops) sorts nothing
(count 0).

### 10.2 Sort spec bytes (High)

`DBSortSpec::Write(SwapOStream&)` (bin @0xf2fbf0); integers big-endian; a
"key" is `[u8 total length][segments]`, `00` when absent:

```
u16  flags              00 00 observed
u16  entry count
per entry (one per sort field, in priority order):
  key  table            03 02 80 02
  key  field            02 01 02
  u16  repetition       00 01
  key  summary table    00     (sort by a summary field: its table)
  key  summary field    00
  u16  summary rep      00 00
  key  value list       00     (sort by value list: the list's key)
  u8   order            0 ascending, 1 descending, 2 by value list, 4 custom
  u32  locale           00000000 = the field's index language
```

Example (live:sortA_last_up, Last Name ascending): `00 00 00 01 03 02 80 02
02 01 02 00 01 00 00 00 00 00 00 00 00 00 00 00`. Order 2 with no value list
sorted ascending (live:sortA_vl). Entry layout from DWARF
`DBSortSpec::Entry` (field spec, summary spec, value-list key, order flags,
locale override, app flags); order values from `Entry::SortOrder` (kUp 0,
kDown 1, kByValueList 2, kCustomOrder 4).

### 10.3 Sort keys (High for text and timestamp)

Each record's sort key for one field:

- ascending text: the record's index key (7.2) plus `00 00`
  ("Computer" = `01 123d 138e 135f 13b3 1453 1433 126b 13da 0000`); empty
  text = `01`;
- descending text: the bitwise NOT of the ascending key, plus `ff ff`
  ("World" = `fe eb72 ec71 ec25 eccf edaf ffff ffff`; empty = `fe ff ff`);
- ascending timestamp: the index key (7.3); descending: its bitwise NOT
  (`6055637ffaaca7ef` = NOT `9faa9c8005535810`);
- calculation fields use their result's key (live:sortA_init_up, "C" =
  `01 123d 0000`).

Numbers, dates and times are expected to follow the timestamp rule (no
suffix); not tested. Records with equal keys form one group.

A field without a value index is still sorted: live:sortA_init_up sorts by
Initial, a stored calculation with no `[80 02, 01, 08]` index node, and
FileMaker Server keys each record's stored value ("C" = `01 123d 0000`,
the 7.2 key of the stored "C"). om-query does the same for unindexed text
fields; an unstored calculation stores nothing and needs the calculation
engine.

### 10.4 Reply: the sort tree (High for one sort field, Med beyond)

`KDest` receives a sort tree that the client loads as its sorted row set
(`DBSortTree`); the display order is the key order of `[01]`, and within a
group the RID-list order. For one sort field, in key order:

| child | content |
|-------|---------|
| `00` | u64 BE: number of records sorted |
| `[01]` | one node per distinct sort key (10.3), whose children are the RID list of the records with that key (single or bitmap, 5.1) |
| `[01 00]` | one leaf per group: key = prefix + sort key, value = `[u8 len]` sort key |
| `[01 01]` | one leaf per group: key = sort key, value = `[u8 len]` (prefix + sort key); then the leaf `ff = 01 ff` |

The prefix is `80 00 00` for an ascending field and `7f ff ff 00` for a
descending one in every vector (its meaning is open; summary-field sorts may
change it). Example, live:sortA_last_up (records 1 Record, 2 World,
3 Computer):

```
00 = 00000000 00000003
[01]    { [01 123d..13da 0000] {03}  [01 13da..1250 0000] {01}  [01 148d..1250 0000] {02} }
[01 00] { 800000 01123d..0000 = 13 01123d..0000   (and the other two) }
[01 01] { 01123d..0000 = 16 800000 01123d..0000   (and the other two)   ff = 01 ff }
```

All three records with one key (live:sortA_company_up):
`[01] { [01] { 00 = 0000000e } }`, `[01 00] { 80000001 = 01 01 }`,
`[01 01] { 01 = 04 80000001, ff = 01 ff }`.

**Second and later sort fields (Med, live:sortA_two):** a level-1 group whose
records still need ordering holds the leaf `sortkey = 01 02` instead of a RID
list, pointing to node `[02]`; `[02]` holds the level-2 groups (sort key ->
RID list) plus the leaf `ff = 01 01 01 01`, and `[02 00]` / `[02 01]` follow
the `[01 00]` / `[01 01]` rules. How several level-1 groups share or split
the level-2 nodes (one node per group, `[03]`, ...?) was not observable with
three records and is open.

---

## 11. Reply trailer, errors and status

Supersedes 4.2 and 4.3; aligned with network-protocol.md 5.6.6c.

### 11.1 Trailer (High)

After the `N` result-slot bytes (4.1): `u32 N`, `u64 perform`, `u64 cursor`,
`i16 depth`, the copy key (empty) and its `u8` length - 23 bytes. A complete
reply has perform 0, cursor 0, depth 0, length 0, which is every cap and
earlier live reply. There is **no status field** for op86. A result larger
than one reply (at most `0xFFF00` slot bytes) is paged with this block exactly
as for op90/op92 (5.6.6c): the server keeps the program's result node and
returns nonzero `perform` and `cursor` handles. The client sends another
op86 with an empty program, and its resume block echoes the handles and
depth from the reply. The client repeats until `cursor` becomes 0. One
observed CreateValueListEx reply took seven pages, with depth 3 on the
first six and 0 on the last (8.7, private capture
`s137-hosted-privileges-20261114/conn-000`).

### 11.2 The error slot (High)

When the program fails, the result slots start with the leaf `00` whose value
is `02` followed by the u16 BE FileMaker error code (the server builds the
3-byte string `03 02 hi lo` in `DBRemoteQueryProgram::PerformOperation`, bin
@0xfb47a0, and stores it at the first key). Spots written before the failure
may follow:

```
80 06 00 03 02 00 66 20 02 80 40 40     [00] = 02 0066, [02] = {}   (live:err_table)
80 06 00 03 02 ff ff 40                 [00] = 02 ffff              (live:jp_badkey)
```

A client checks for a root leaf `00` before reading spots; spots are numbered
from 1, so `00` never names a spot.

| code | meaning (FileMaker) | when | live |
|------|---------------------|------|------|
| `ffff` (-1) | unknown error | unknown or host-forbidden opcode; access-key mismatch | op2_unknown, jp_badkey |
| `0005` | command is invalid | opcode 1 | op1_version |
| `0012` (18) | - | `[01, i, 04]` names a file id this connection has not opened | err_fileid |
| `0066` (102) | field is missing | table or field key not found | err_table, exists_pk |
| `006a` (106) | table is missing | SortList with an empty `0b` | sortA_empty_op2 |
| `0191` (401) | no records match | LookupValue finds no lower or higher record (8.6) | live1:lk_pk_higher_A, lk_nokey |

### 11.3 No match is not an error (High)

A find or join that matches nothing returns a normal reply (no error slot)
with an empty RID list (`20 02 80 40` under the reply root). The "No records
match this find criteria" message (FileMaker error 401) is produced by the
client from the empty list. LookupValue is the exception: when it finds no
lower or higher record it fails with 401 in the error slot (8.6, live1).

### 11.4 Limits on a program (OpenMaker's; FileMaker Server's unmeasured)

How deep, large or long a program FileMaker Server accepts was not
measured. om-query runs every program within the limits below and answers
one that goes past them as it answers a malformed program: the error slot
`02 ffff` (11.2), followed by the spots written before. Each limit is
OpenMaker's choice, set far above what clients send, so that no program can
exhaust the host's stack, memory or time:

| limit | value | rests on |
|-------|-------|----------|
| key path depth | a spot path or file path operand has at most 17 segments, and nothing written into the program's tree goes deeper than 17 levels | the slot reader's 17 levels (file-format.md 3.2): no uploaded program is deeper, and the reply (the tree's `[3]`, re-rooted) stays within the 16 levels a file holds |
| file writes | UploadMultiple writes at most 16 levels deep, and only what the file writer can hold: key segments of 1 to 219 bytes, a page's room for the path (`om_hbam::writer::unwritable`), nothing beneath a node of the file that holds a value | the file writer (hbam-writer.md 2.1-2.3), so the host can save what it keeps |
| instructions | 1,024 | FileMaker Pro's captured programs hold at most 3 (0) |
| program tree | 4,194,304 nodes and 256 MiB of keys and values, uploaded, written and held for the file | a sort of a million records with distinct keys takes about four million nodes (10.4); FileMaker Pro's captured replies are under 3 KB (11.1) |
| work | 16,777,216 nodes and 1 GiB that the instructions read from the tree or the file's indexes, or write into the tree, in total | four times the tree |
| sort fields | 64 | the sort spec's count is a u16 (10.2) |

A CopyNode (8.3) is measured before it is made, so copying the program into
itself stops at the depth or size limit, and a sort tree stops growing at
the size limit. At these limits one program can still take about 600 MiB of
memory (565 MB measured for copies that reach the size limit, release
build) and a few seconds. A result that cannot be encoded as a reply (a
value over 65,535 bytes, 8.7) fails the program with `02 ffff` alone.

Reading the file counts as work too (7.1): each key of an index, once, the
first time the program reads that index (its order is checked); for each
key looked up, the keys a binary search may compare (`log2 n + 1` of `n`);
each index entry read, with its RID list; a node of eight bytes per 64
records each time an instruction combines with the table's record list;
each stored value read; and each node on an UploadMultiple's destination
path in the file, which it reads to refuse a write beneath a value. So the time a program holds the file follows
its own size and its answer, and stops at the work limit whatever the
index's size. Programs that spend the work limit on index reads were
refused after 0.6 to 1.5 s, end to end against a 40,000-key index
(`docs/performance/query-index-lookups.md`).

---

## 12. Test vectors

`re/work/query/vectors/<name>/` (git-ignored; holds per-session access keys):
`program.bin` (payload), `txn.bin` (whole request), `reply.bin` (whole
result stack), `README`; `INDEX.txt` lists all 113. `live-*` ran against
test project (Contacts `80 02`: 1 Test Record, 2 Hello World, 3 Ima
Computer; file id 357/358 per session; access keys valid only for their
session). `cap-*` come from capture s03. Probe tool and scripts:
`re/work/query/omq/` (`probe`), `re/work/query/live/gen_run*.py`.

live1 (2026-11-11, `re/work/needs-re/146/live1/`, run file `run_live1.txt`)
ran with the same probe after other tests had changed the file: Contacts then
held records 1, 2, 3, 10, 12 and 13 (`[80 02, 0d]`); First Name was indexed,
"Test" in record 1 and a longer text containing the word "Test" in 10 (at
its end) and 12; Company was empty in all but record 1. The run also saved
the value indexes of First Name, Company and PK (`dl_*`).

| section | vectors |
|---------|---------|
| 1, 11.1 framing | every vector |
| 2.4 access key | all cap-*; live-jp_badkey (mismatch) |
| 5.1 RID lists | cap-460, cap-912 (168 singles + 77 bitmaps), live-inv_empty |
| 6 SimpleFind | cap-912; live-sf_v_match_pk, sf_word_pk, sf_value_pk_word, sf_v_company_empty, sf_w_company_empty, sf_scope2, sf_v_notmatch_pk, sf_v_range_pk, sf_v_range_all, sf_value_lastname_r, sf_ts_range, sf_rowid_match, sf_rowid_not, sf_rowid_range, sf_dups_company, sf_two_finders_and, sf_two_finders_or, sf_masterlist; Match: live1:m_* (sf_match_first_test and `live/out8`, `out9` used wrong operand framings) |
| 7 indexes and keys | live-proj_* (keys); `re/work/query/live/out3/dl_index`, `dl_master`, `dl_records` (op90 downloads of the index, record list and records) |
| 8.1 combine | live-and_lists, or_lists, op7_lists |
| 8.2 invert | live-inv_empty, inv_23 |
| 8.3 copy/delete | live-copy_node, op3_node, op3_move |
| 8.4 ExistsInIndex | live-exists2_pk, exists2_pk_excl, exists2_pk_excl3, exists2_miss, exists2_two; wrong field-key form: exists_pk, exists_pk_excl, exists_miss |
| 8.5 NodeExists, GetKey | live-nodeexists_file_*, getkey_fe, getkey_rec1_first, getkey_node, getkey; program-spot paths: nodeexists_y, nodeexists_n |
| 8.6 LookupValue | live-lookup_eq_nok8, lookup_eq_miss, lookup_lt_true, lookup_lt_false, lookup_gt_true, lookup_pk, lookup_pk_lt; live1:lk_* |
| 9.1 Projection | cap-912; live-proj_text_l15, proj_text_l4d, proj_text_l03, proj_text_l02, proj_text_01, proj_text, proj_text_00, proj_pk, proj_ts, proj_number, proj_date, proj_time, proj_company |
| 9.2 JoinProjection | cap-460, 502, 546, 550, 742, 818, 1034, 1090, 456 (empty); live-jp_pk_hit, jp_pk_miss, jp_empty_src, jp_company_emptykey, jp_ne, jp_lt, jp_le, jp_gt, jp_ge, jp_cart, jp_two_keys, jp_two_rows, jp_two_preds_hit, jp_two_preds_miss, jp_unindexed (miss) |
| 10 SortList | live-sortA_last_up, sortA_last_down, sortA_first_up, sortA_first_down, sortA_init_up, sortA_ts_up, sortA_ts_down, sortA_pk_up, sortA_company_up, sortA_company_down, sortA_two, sortA_vl, sortI_last_up, sortI_23_first_down |
| 11.2 errors | live-err_table, err_fileid, op2_unknown, op1_version, op1_v, jp_badkey, sortA_empty_op2, exists_pk |

---

## 13. Open questions

1. Text keys (7.2): each tailoring's full set of tailored characters (the
   probe shows only the characters it held), the rules of the Vietnamese,
   Thai and Greek-mixed tailorings, the Chinese, Chinese-stroke and Korean
   tables and the Japanese weights of kanji outside JIS X 0208 level 1;
   whether System (1) follows the machine's language; language 77, which
   FMUpgradeTool would not create. The full word-break rules (7.1). The
   cut of long keys in every form (7.1) and `IncludeTooLong` (6.3) are
   solved; three FileMaker Pro long-criterion wire requests are captured
   (6.4), but its finders were not replayed against FileMaker Server with
   and without `IncludeTooLong`, and duplicate-find
   behavior with cut keys remains unresolved (6.3, 7.1).
2. How FileMaker reads a stored number written in another locale's format
   beyond the decimal and thousands separators (7.3).
3. The instruction sequences FileMaker Pro sends for several requests, Omit,
   Constrain and Extend (6.5), and for a find on an unindexed field (7.1); a
   capture of Find mode against test project would settle them (OpenMaker
   Server records every connection).
4. Final finders for `=`, `==`, `*`, `@`, `#`, `?`, `//` after the client's
   optimisation pass (6.4): `DBFindRow::Add` is read, `ChooseScope` and
   `DBFindRow::Optimize` are not. The `==` and `*...*` long-criterion
   captures hold Match finders; other optimized forms remain unmeasured.
   Why the pattern [star] alone matched every record without an
   index (out8) and none with one (live1, 6.3).
5. Finder kinds 3 (IndexScan), 9 (DateTimeMatch), 10, 11, 12 (6.3).
6. The `80 00 00` / `7f ff ff 00` prefix in sort trees and the layout of
   multi-group, multi-field sorts (10.4). OpenMaker numbers further level
   nodes depth first (`[02]`, `[03]`, ... in the order groups are visited),
   which matches live:sortA_two but is unconfirmed beyond one group; a value
   such as a record held under several value-index keys (multi-line text,
   repetitions) is sorted by its lowest key, also unconfirmed.
7. JoinProjection's `25` KFilter / `0a` operands, and the between type (9.2).
8. Ops 15 GetSumValue, 20 and 51 (2.3). Ops 40, 43 and 50 are in
   schema-path.md 3.4. FileMaker Pro's own op17 has not been captured
   (8.6: a lookup on a relationship of more than one predicate, or into a
   table of 25,000 records or more, with no exact match).
9. Which writer rule produces singles versus bitmaps in RID lists (5.1); any
   valid mix is accepted by a reader.
10. FileMaker Server's own limits on a program's depth, size, number of
    instructions and sort fields (11.4).
11. CreateValueListEx (8.7): the cache lifetime of Insert From Index, a
    successful reconnect's effect on the pop-up cache, the exact generation
    flag that lets a list with a second field use op19, a list restricted to
    related values, and width variants in the field's collation. The editable
    Company control in the smaller capture sent `13=01 01` and `14=100`
    on open, while the later pop-up omitted both and fetched every value;
    whether control kind, list size or another condition chose those shapes
    was not measured. The earlier control's Inspector style was not recorded.
