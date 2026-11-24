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

## 3. The log

`<store>/log/` (server-storage.md section 6.3), read and written by
`om_store::log`. Every entry is first written here, in batches; the
marker in `state` (section 1.2) records which of them are durable. All the
hashes are SHA-256.

### 3.1 Files

- **Segments** are files named by their number as 16 lowercase hex digits
  (`log/0000000000000001`). The segments in use are the contiguous run
  `state` lists (section 1.2); entries are written only to the last of
  them.
- **A segment's size** is fixed when it is made, 64 MiB by default
  (`Config::segment_size`, from 512 bytes to 1 GiB), and recorded in its
  header; segments made earlier keep theirs when the setting changes. Its
  space is taken when it is made (`File::allocate`), so writes into it
  never need more space.
- **The spare** is the next segment, made ahead of need (3.7): its number
  follows the run, its space is taken and flushed, its name is durable,
  and `state` does not list it. It holds only zeros until it is used.
- **Any other segment file** `state` does not list - a segment retired or
  cut by a process that stopped before removing it, or a spare - is
  removed by the next open. Files whose names are not segment numbers are
  left alone.

A segment holds its header (3.2) and then batches (3.3), one after the
other, each a batch header followed by its entries (3.4). What follows the
last batch is zeros, or bytes a cut left (3.9), which no walk reads (3.5).

### 3.2 A segment's header

At offset 0, 128 bytes:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | `OMLOGSG` followed by a zero byte |
| 8 | 2 | format version, 1 |
| 10 | 6 | zero |
| 16 | 16 | the store's id: the server id in `state` |
| 32 | 8 | the segment's number, as in its name |
| 40 | 8 | the segment's size in bytes |
| 48 | 8 | the index of its first entry (of the next entry, while it holds none) |
| 56 | 8 | the term of the entry before that one, 0 before the first |
| 64 | 32 | the hash of the entry before that one, zeros before the first |
| 96 | 32 | SHA-256 of bytes 0 to 96 |

A header is **valid** when it begins with the mark, its hash matches, its
reserved bytes are zero, its first index is not 0 and its size is within
bounds. A valid header of a newer version makes the open fail ("newer
format") rather than count as damage. A listed segment's header must also
name this store and the number in the file's name, and the file must be at
least as long as the size; the segments' first indexes must not go back
along the run. Two segments may have the same first index: a cut back to a
segment's first entry leaves it empty (3.9), and the next one begins where
it would have.

### 3.3 A batch

The entries one batch flush writes to one segment. Its header, 96 bytes:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | `OMBATCH` followed by a zero byte |
| 8 | 2 | format version, 1 (the segment's) |
| 10 | 2 | zero |
| 12 | 4 | entry count, at least 1 |
| 16 | 8 | the index of its first entry |
| 24 | 8 | the bytes its entries take |
| 32 | 32 | the hash of its last entry |
| 64 | 32 | SHA-256 of bytes 0 to 64 |

A batch header is valid when it begins with the mark, its hash matches,
its version is the segment's, the reserved bytes are zero, the count is not
0, the length leaves at least 64 bytes for each entry, and the batch ends
within the segment. A batch is **whole** when its header is valid, all
`count` of its entries follow it whole and chained (3.4), they fill exactly
its length, and the last one has the hash the header names. The header
names the last hash so that a batch whose write was cut short, or one whose
header survived an overwrite that did not, is never taken for whole, even
when every entry read under it chains.

**Segment boundaries.** An entry never spans segments. A batch flush
writes the entries that fit in what is left of the current segment as one
batch there, and the rest as a second batch at the start of the next
segment, after its header. So the largest payload is bounded by what an
empty segment holds (`Config::max_payload`, 16 MiB by default; it must
fit in a segment with its headers), and a batch never spans more than two
segments: an append that would make it do so is refused
(`LogError::BatchFull`) until the batch is flushed.

### 3.4 An entry

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | index, counted from 1 |
| 8 | 8 | term |
| 16 | 8 | the file it belongs to (0: none); not interpreted by the log |
| 24 | 2 | kind (server-storage.md 6.3); not interpreted by the log |
| 26 | 2 | zero |
| 28 | 4 | payload length `P` |
| 32 | `P` | payload |
| 32 + `P` | 32 | SHA-256 of the previous entry's hash followed by bytes 0 to 32 + `P` |

The previous entry's hash of entry 1 is 32 zero bytes. An entry is
**whole and chained** when its index is the one expected, its reserved
bytes are zero, it fits in its batch and segment, and its hash matches.
Terms do not go down along the log (the append refuses a lower one).

### 3.5 Walking the log

Opening, reading and cutting all walk the log the same way, from the start
of a listed segment, expecting the index its header names and chaining
from the hash it names. At each place, in this order:

1. **The next segment.** If the next listed segment's header names the
   index expected next, with the hash and term of the entry just read, the
   writer moved to it at this point: the walk continues at its first
   batch, and nothing more of the current segment is read.
2. **Inside a batch** that has entries left: a valid batch header whose
   first index is the one expected ends the batch (a cut, 3.9, and the
   batch written after it); otherwise the next entry must be whole and
   chained, or the walk ends there.
3. **Between batches**: a valid batch header whose first index is the one
   expected begins a batch; otherwise the walk ends.

Stale bytes - a batch a cut left behind, a batch from before a cut - never
continue the walk: their first entry chains from an entry that is no
longer in the log, or was zeroed (3.9).

**Reading** (`Log::read`) walks from the segment that holds the first
entry asked for, through the entries before it in that segment, and returns
the durable entries from it on; a walk that ends before the durable entry is
damage, reported to the reader.

### 3.6 Opening

`Log::open` (server-storage.md 6.7 step 3) is given `state` and the entry
the checkpoint it starts from covers (index, term and hash; index 0 for the
start of the log). The **firm** position is the later of that entry and
the marker.

1. Every segment `state` lists is opened and its header checked (3.2).
   One missing, or with no valid header, is damage, whatever the others
   hold.
2. The walk starts at the last segment whose first index is at or before
   the entry after the start. A log whose first segment begins later has a
   gap, which is damage. The entry at the start's index, and the entry at
   the marker's index, wherever the walk meets them (a segment header's
   previous hash counts), must have the hashes those name, or the log is
   damaged.
3. Entries up to the firm position are taken one by one: each must be whole
   and chained. A walk that ends before the marker's index, or before the
   start's, is damage.
   Entries before the start in the first segment walked are read through,
   since the log keeps no index of where entries are; damage to them is
   reported too.
4. After the firm position, an entry counts only once its whole batch has
   been read and found whole (3.3). The first batch that is not whole, and
   everything after it, is **cut**; so is anything after a walk that ended.

On damage the open returns it (`LogError::Damaged`: what, the segment, the
offset and the index expected there) and writes nothing: nothing is cut,
and the marker and the segment run stand. What follows is the caller's
decision (server-storage.md 6.12): in repair on a cluster, stop on a single
server. A read error is `LogError::Unreadable`, which the caller tells
apart from damage.

Otherwise the open makes what it found durable, in this order:

1. Every segment holding entries after the marker is flushed. An earlier
   process may have written them and stopped without a power failure,
   leaving them in the system's cache only.
2. Where the log now ends, up to 160 bytes are zeroed when they are not
   zeros already (a synchronous write): a batch header's worth and an
   entry's fixed part and hash after it. What was cut cannot be read there
   again, however a later crash settles the bytes after it, even under the
   header of a batch written there later whose entries the crash lost.
3. One `state` update moves the marker to the last entry kept (its
   committed position unchanged) and drops the listed segments after the
   one the log ends in: a marker advance in one slot when only the marker
   moves, both slots when the run changes.
4. Segment files `state` does not list are removed, and the folder flushed.

A crash at any point leaves a log that the next open reads to the same end:
step 1 only makes durable what the walk found whole; step 2 only touches
bytes after the end; until step 3 the marker is where it was, and the
dropped segments are after the end; step 4 removes only what `state` no
longer lists.

A part of a batch written to a new segment whose `state` update was lost is
not kept: the segment is not listed, so it is not part of the log. Its
entries were never reported durable.

### 3.7 Making the next segment

`Log::prepare`, called by the caller ahead of need, and by an append that
needs the next segment and finds it missing:

1. create the file (or open the one a refused attempt left), take its space
   (`File::allocate`), flush it;
2. flush the folder, so that its name is durable before `state` can list
   it;
3. open it for synchronous writes.

A disk too full for it refuses the append (`LogError::NoSpace`) before
anything of the entry is kept, and so does an allocation that fails part
way; neither stops the log, since nothing listed changed, and a later call
finishes the file. A failed flush stops the log. A crash at any point
leaves an unlisted file, which the next open removes.

### 3.8 A batch flush

`Log::flush_batch` writes the open batch (3.3):

1. the entries that fit in the current segment, as one batch, with one
   synchronous write after the last batch there;
2. the rest, if any, as one synchronous write at offset 0 of the spare:
   its header (first index: the batch's first entry there; previous hash
   and term: the entry before) followed by the second batch;
3. one `state` update: the marker (the last entry's index and hash, and the
   highest committed position heard, which never goes down) and, when step
   2 ran, the run with the spare added. The marker alone is a marker
   advance (one write and one flush of `state`); with the run, both slots.

A synchronous write is on the device when it returns (`O_DSYNC` on Linux;
elsewhere each write is followed by `sync_data`), so that it is the log's
flush: steps 1 and 2 are durable before step 3 begins, and a failed write
is reported by the write itself. The entries become durable when step 3
returns.

| A crash | leaves |
|---|---|
| before or during step 1 | the old marker; the batch absent, torn, or (if the write landed whole) whole after the marker, kept by the next open |
| during step 2 | the same, for the part in the current segment; the spare unlisted, removed by the next open |
| during step 3 | the old copy of `state` or the new one: the batch is durable on the device either way, and the next open moves the marker over it if the copy is the old one (a part in the spare is lost with the old copy, 3.6) |
| after step 3 | the batch durable |

Any failure stops the log. An entry the failed call wrote may be found
whole at the next open; the failed call reported nothing durable.

`Log::set_committed` records a higher committed position on its own: a
marker advance in `state`.

### 3.9 A cut

`Log::cut` removes the entries from an index on (server-storage.md 7.1). It
refuses an index at or before the highest committed position heard; at or
before the entry the log was opened from, or the one before the oldest
entry its last retirement still needed, which are committed too (the
committed position the marker records can go back one advance with a lost
copy of `state`, 1.5; a checkpoint cannot); and before the first entry the
log holds. Entries of the open batch are dropped in memory. When the marker
covers entries that are cut:

1. the walk finds the first cut entry: the record to destroy is its batch's
   header when it is its batch's first entry, else the entry itself;
2. one `state` update, written to both slots because the marker goes back,
   lowers the marker to the entry before it and drops the listed segments
   after the one that holds it;
3. the first 160 bytes from that record (fewer at a segment's end) are
   zeroed with a synchronous write, as an open zeroes where it cuts (3.6);
4. the files of the dropped segments, and of the spare if segments were
   dropped (its number changed), are removed, and the folder flushed.

Appends continue from the zeroed place: the next batch is written over it.
A walk then meets that batch's header where the cut entry was, which ends
the batch the cut entry was in (3.5 rule 2).

| A crash | leaves |
|---|---|
| before step 2 ends | the old marker and run; nothing was cut |
| after step 2, before step 3 | the lower marker, and the cut entries still whole after it in the segment that held the first of them: the next open keeps those as a whole tail, as if the cut had stopped there. Nothing the marker covers is missing |
| after step 3 | the log ends at the entry before the cut; the dropped segments' files, if still there, are unlisted and removed by the next open |

The marker is lowered first so that no `state` copy that can be read ever
covers a zeroed entry; zeroing before it would let a crash leave the old
marker over a destroyed entry, which the next open reports as damage. The
zeroing is what makes the cut hold: without it a later crash could bring
back cut entries after the lower marker, and the next open would keep
them.

### 3.10 Retiring

`Log::retire` is given the oldest entry still needed (the entry after the
oldest kept checkpoint, or older for another server, server-storage.md 6.7
and 7.5). It retires, from the front, every segment whose next segment
begins at or before that entry, never the last:

1. one `state` update, written to both slots, drops them from the run;
2. their files are removed and the folder flushed.

A crash between the steps leaves unlisted files, removed by the next open;
`state` never lists a segment that is gone.

### 3.11 Faults and bounds

- **After a failed write, flush or `state` update** the log refuses every
  call (`LogError::Poisoned`): what reached the device is unknown, the
  failed flush is never retried, and the store stops and opens again
  (server-storage.md 6.12). The refusals of 3.7 are the exceptions: they
  touch only the unlisted spare.
- **Everything the log reads is checked before it is used**: headers by
  their hashes, lengths against the batch and the segment, entries by the
  chain. A segment's size is at most 1 GiB, so no length read from disk
  makes the log hold more than one segment's bytes in memory.
- **The marker's place in `state`** is checked on every call that writes:
  `state` must be the one the log was opened with, its lock held, and its
  segment run and marker as the log last wrote them.
- **The open batch** is at most what is left of the current segment and one
  more segment.
- Every listed segment is kept open while the log is.
