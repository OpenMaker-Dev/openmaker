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
at offset 0 and slot 1 at offset 4096. Each slot holds one copy, padded
with zeros to the slot's end.

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
| 16 | 8 | start count: how many times the store has been opened, its creation included |
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
- **An update** writes the next copy number into the slot that does not
  hold the current copy, as one write of the slot in place, and flushes
  the file (`fdatasync`). `state` is rewritten once per commit batch
  (server-storage.md 6.3), so an update is never a new file and a
  rename.
- **An open** flushes the store folder, reads both slots and takes the
  valid copy with the highest number (slot 0 between equal numbers). It
  writes that copy back into its own slot and flushes it, then writes its
  values with the start count raised, as the next number, into the other
  slot, and flushes that before it returns. The write back is there
  because an earlier process may have stopped between a write and its
  flush, or seen a flush fail, and the system's cache can then show a
  copy the device does not hold (server-storage.md 6.12); the folder
  flush, because an earlier creation may have stopped between its rename
  and its folder flush. The start count rises by 1, or by 2 when the
  other copy is not valid: that copy may have been a newer one, written
  by a start whose count would otherwise be given again.
- **No valid copy**: the open fails with an error that names what is
  wrong with each copy. The server then does not start (server-storage.md
  6.12); the caller decides what follows.
- **After a failed write or flush** nothing more is written to the file
  until it is opened again: what reached the device is unknown.

### 1.4 Cost and bounds

- A copy is at most one slot, 4096 bytes; version 1 uses 178 of them.
- An update is one 4096-byte write in place and one flush of the file.
  An open is two such writes, two file flushes and one folder flush.

### 1.5 What the two copies protect against

- **A crash during an update or an open** leaves the copy before it
  whole, so the copy read is the last one confirmed as flushed or a later
  one that was being written. The start count never goes back.
- **A failed or partial write, a failed flush, a full disk**: the same,
  once the store has stopped and been opened again.
- **Damage to the older copy** is passed over.
- **Damage to the newest copy** cannot be told from a write a crash
  interrupted. The older copy is read, and the values go back one update:
  the marker, and the term and vote. The start count still rises past the
  lost copy's, unless the lost copy was itself written by an open that
  found the other copy invalid. `State::opened` reports that the other
  copy was not valid, so that the layers above can act on a fallback
  (for a vote, server-storage.md 7.1).
- **Damage to both copies**: no valid copy.
- **A write the device misplaces or drops after reporting it done**: the
  copy read is one that was written, never a mixture, but it can be older
  than the last one confirmed (server-storage.md 6.12, "What is not
  found").

The simulator's tests (`crates/om-store/src/state/tests.rs`) check each of
these under seeded schedules of crashes and faults.
