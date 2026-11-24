# OpenMaker Server store formats

The byte formats of the store engine (`om-store`), whose design is in
[server-storage.md](server-storage.md). All of it is OpenMaker's own.
Every structure carries a format version; until the format is declared
stable it may change between versions of OpenMaker Server
(server-storage.md section 11).

Integers are little-endian.

## 1. The state file

`<store>/state` (server-storage.md section 6.1), read and written by
`om_store::state`. The file is 8192 bytes: two slots of 4096 bytes, slot 0
at offset 0 and slot 1 at offset 4096. Each slot holds one copy at its
start. The creation writes the rest of each slot as zeros; later writes
cover only the copy, and what follows a copy's hash is not read.

### 1.1 A copy

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | `OMSTATE` followed by a zero byte |
| 8 | 2 | format version, 1 |
| 10 | 2 | zero (reserved) |
| 12 | 4 | body length `L` |
| 16 | 8 | copy number |
| 24 | `L` | body (1.2) |
| 24 + `L` | 32 | SHA-256 of bytes 0 to 24 + `L` |

These fields keep their places in every version, so that a server can
check the hash of a copy of any version. `L` is at most 4040, so that a
copy fits its slot. Bytes after the hash are not read.

A copy is **valid** when it begins with the mark, its length fits, its
hash matches and its body keeps the rules of its version. A copy whose
hash matches and whose version is newer than the server reads makes the
open fail ("newer format"): the server does not pass over it as damaged
and fall back to an older copy.

### 1.2 The body, version 1

`L` is 122.

| Offset | Size | Field |
|---:|---:|---|
| 0 | 16 | this server's id, random, drawn when the store is created |
| 16 | 8 | start count: raised by one at each open, the creation counting as 1 (1.3) |
| 24 | 8 | term (server-storage.md 7.1) |
| 32 | 1 | 1 when the server has voted in this term, otherwise 0 |
| 33 | 16 | the id it voted for, or zeros |
| 49 | 8 | the number of the oldest log segment in use; with none in use, the number the next one gets |
| 57 | 8 | how many log segments are in use |
| 65 | 8 | marker: the index of the last entry of the last flushed batch (section 6.3) |
| 73 | 32 | marker: that entry's hash |
| 105 | 8 | marker: the highest committed index the server has heard of |
| 113 | 1 | 1 while the server is in repair (section 7.5), otherwise 0 |
| 114 | 8 | the repair's required end, or zero |

A flag byte other than 0 or 1, an id or required end that is not zero
where its flag is 0, a reserved field that is not zero, or a segment
number and count whose sum overflows, makes the copy malformed.

**The segments in use** are a contiguous run of numbered files: the log
retires segments from its front and cuts them from its back
(server-storage.md 6.3 and 6.7), so the list is held as its first number
and its length. A copy's size does not grow with the log; the run can be
as long as 64-bit numbers allow, however long a restore point keeps the
log.

### 1.3 Writing and reading

- **Creation.** Both copies, numbered 1 and 2 and holding the same values
  (start count 1, term 0, no vote, segments from 1 with none in use,
  marker index 0 with a zero hash, not in repair), are written to
  `state.new`, which is flushed, renamed to `state`, and the store folder
  flushed. So `state` exists only whole. A `state.new` left by a creation
  that did not finish is removed first. A store folder whose `state` is
  missing makes the open fail with an error that says so; creating a new
  one is the caller's decision.
- **A marker advance** - an update that changes only the marker, lowers
  neither its index nor its committed position, and keeps the hash when
  the index is the same - writes the next copy number into the slot that
  does not hold the current copy and flushes the file (`fdatasync`): one
  write of 178 bytes in place and one flush. `state` is rewritten once
  per commit batch (server-storage.md 6.3), so this is never a new file
  and a rename.
- **Any other update** - the term, the vote, the segments, the repair
  record and its required end, a marker that goes back - is written
  twice before it counts: as the next number into the other slot,
  flushed, then as the number after into the first slot, flushed. What
  such an update confirmed is then in both slots.
- **An update that changes nothing** writes nothing.
- **An open** flushes the store folder, reads the two slots one at a
  time, and takes the valid copy with the highest number (slot 0 between
  equal numbers). It writes that copy back into its own slot and flushes
  it; then it writes the same values with the start count raised by one
  as the next number into the other slot and flushes, and as the number
  after into the first slot and flushes, before it returns. So an open
  costs three writes, three file flushes and a folder flush.
  - The folder flush makes `state`'s own name durable, in case an earlier
    creation stopped between its rename and its folder flush.
  - The write back is there because an earlier process may have stopped
    between a write and its flush, or seen a flush fail, and the system's
    cache can then show a copy the device does not hold
    (server-storage.md 6.12). Only once the copy read is on the device
    does the open write over the other slot.
  - A slot the device fails to read is passed over like an invalid one.
  - An open cut short may have written a raised start count that no start
    used; the next open raises from it. So start counts can skip values,
    and are never issued twice.
- **No copy**: when neither slot gives a valid copy, the open fails. If
  both were read and neither holds a valid copy, the error says the server
  does not know what it confirmed; if one could not be read, the error
  says the disk failed to read it. The server does not start either way
  (server-storage.md 6.12); the caller decides what follows.
- **After a failed write or flush** nothing more is written to the file
  until it is opened again: what reached the device is unknown.
- **One writer.** What writes `state` holds the store folder's lock for
  as long as it can write, and refuses a lock that the store folder's
  `Fs` did not give out or that is no longer held.

### 1.4 Cost and bounds

- A copy is at most one slot, 4096 bytes; version 1 uses 178 of them.
- A marker advance is one 178-byte write in place and one flush of the
  file. Any other update is two such writes and two flushes. An open is
  three writes, three file flushes and one folder flush. Before it, the
  server makes sure of the store folder itself (`Fs::create_root`): it
  makes the folder if it is missing, inside a folder above that must
  exist, and flushes that folder above at every start, one more folder
  flush, so that a store folder an interrupted first start made is
  durable too.

### 1.5 What the two copies protect against

Crashes, failed or partial writes, failed flushes and a full disk put at
most one slot at risk at a time: the state code writes a slot only while
the other holds a copy that it, or the update before, flushed itself.
Against those, and against the device losing or damaging one slot at
any time:

- **No start count is issued twice**: the raised count is in both slots
  before an open returns.
- **The term and vote, the segment run, the repair record and its
  required end** are in both slots once their update has returned, so
  losing either slot loses none of them.
- **The marker** goes back by at most one advance: an advance is in one
  slot, and the other holds the copy before it. Recovery re-establishes
  the lost advance's index and hash from the log, whose entries through
  that advance were flushed before it was written, when the log's tail
  is whole (server-storage.md 6.7, step 3). The committed position the
  marker carries is not in the log: on one server it equals the durable
  position, so it comes back with the index; a member of a cluster
  learns it again from the leader (server-storage.md 6.7 step 4, 7.1).
- **Damage to both slots, or one slot lost together with the other
  failing to read**: no copy, and the open fails.

What is not covered:

- **One slot lost and the log's tail damaged at once**: the marker goes
  back one advance and the log no longer holds the entries of that
  advance whole, so the batch it confirmed is lost without a sign. This
  is a double fault; a single one of them is found.
- **One slot changed on the device while a write to the other is cut
  short**: this can leave neither slot with a valid copy, and then the
  server does not start (server-storage.md 6.12); a cut that leaves the
  older copy in the other slot whole does not. This is a double fault
  too; a crash alone, or damage alone, leaves a valid copy.
- **A write the device misplaces or drops after reporting it done**: the
  copy read is one that was written, never a mixture, but it can be older
  than the last one confirmed, and the guarantees above do not hold
  (server-storage.md 6.12, "What is not found").

The seeded tests (`crates/om-store/src/state/tests.rs`) check each of
these: every open against the copies each slot can hold, and every
confirmed value against what the open returns.
