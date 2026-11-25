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
| 8 | 2 | format version, 2 (version 1 is read too, 1.2) |
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

### 1.2 The body, version 2

`L` is 130. Version 1 is the same body without its last field, the cut
count (`L` is 122); it is read with a cut count of 0, and the next update
writes version 2.

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
| 122 | 8 | the cut count: how many times the log was cut back over entries the marker covered (section 3.9); it never goes down |

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
  marker index 0 with a zero hash, cut count 0, not in repair), are
  written to
  `state.new`, which is flushed, renamed to `state`, and the store folder
  flushed. So `state` exists only whole. A `state.new` left by a creation
  that did not finish is removed first. A store folder whose `state` is
  missing makes the open fail with an error that says so; creating a new
  one is the caller's decision.
- **A marker advance** - an update that changes only the marker, lowers
  neither its index nor its committed position, and keeps the hash when
  the index is the same - writes the next copy number into the slot that
  does not hold the current copy and flushes the file (`fdatasync`): one
  write of 186 bytes in place and one flush. `state` is rewritten once
  per commit batch (server-storage.md 6.3), so this is never a new file
  and a rename.
- **Any other update** - the term, the vote, the segments, the cut count,
  the repair record and its required end, a marker that goes back - is
  written
  twice before it counts: as the next number into the other slot,
  flushed, then as the number after into the first slot, flushed. What
  such an update confirmed is then in both slots.
- **An update that changes nothing** writes nothing.
- **An open** flushes the store folder, reads the two slots one at a
  time, and takes the valid copy with the highest number (slot 0 between
  equal numbers). It writes that copy back into its own slot, as the very
  bytes it read, and flushes it; then it writes the same values with the
  start count raised by one as the next number into the other slot and
  flushes, and as the number after into the first slot and flushes,
  before it returns. So an open costs three writes, three file flushes
  and a folder flush.
  - The folder flush makes `state`'s own name durable, in case an earlier
    creation stopped between its rename and its folder flush.
  - The write back is there because an earlier process may have stopped
    between a write and its flush, or seen a flush fail, and the system's
    cache can then show a copy the device does not hold
    (server-storage.md 6.12). Only once the copy read is on the device
    does the open write over the other slot.
  - **A format change** never rewrites a copy in place in the new form.
    The write back is the bytes read, so a write cut short there leaves
    the copy as it was, of whatever version; the new form reaches the
    disk only as new copies, through the two writes that follow (next
    number into the other slot, flushed, then the first slot, flushed).
    Version 1 copies become version 2 this way at the first open by this
    code. Every later version keeps this rule.
  - A slot the device fails to read is read again, up to three times in
    all; one that fails every time is passed over like an invalid one,
    and written over before the open returns, so that its copy is lost:
    with a marker advance in it, that advance (1.5).
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

- A copy is at most one slot, 4096 bytes; version 2 uses 186 of them.
- A marker advance is one 186-byte write in place and one flush of the
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
- **The term and vote, the segment run, the cut count, the repair record
  and its required end** are in both slots once their update has
  returned, so losing either slot loses none of them. An update of both
  slots counts from the moment the first slot's flush returns: an open
  takes the higher-numbered copy and writes it to both slots.
- **The marker** goes back by at most one advance: an advance is in one
  slot, and the other holds the copy before it. Recovery re-establishes
  the lost advance's index and hash from the log, whose entries through
  that advance were flushed before it was written, when the log's tail
  is whole (server-storage.md 6.7, step 3). The committed position the
  marker carries is not in the log: on one server it equals the durable
  position, so it comes back with the index; a member of a cluster
  learns it again from the leader (server-storage.md 6.7 step 4, 7.1).
  Until it has, the committed position recorded is not to be trusted as
  high as it was: the log refuses to cut until it is given one again
  (section 3.9).
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
marker in `state` (section 1.2) records which of them are durable, and the
cut count there which batches are still part of the log. All the hashes
are SHA-256. The log owns the store's `state`, and with it the store
folder's lock, for as long as it is open: one store has one log.

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
last batch is zeros, or batches written before a cut (3.9), which carry an
older cut count and are never part of the log again.

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
reserved bytes are zero, its first index is from 1 to the largest index
(3.4) and its size is within bounds. A valid header of a newer version
makes the open fail ("newer format") rather than count as damage. A listed
segment's header must also name this store and the number in the file's
name, and the file must be at least as long as the size; the segments'
first indexes must not go back along the run. Two segments may have the
same first index: a cut back to a segment's first entry leaves it empty
(3.9), and the next one begins where it would have.

### 3.3 A batch

The entries one batch flush writes to one segment. Its header, 136 bytes:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | `OMBATCH` followed by a zero byte |
| 8 | 2 | format version, 1 (the segment's) |
| 10 | 2 | zero |
| 12 | 4 | entry count, at least 1 |
| 16 | 8 | the index of its first entry |
| 24 | 8 | the bytes its entries take |
| 32 | 8 | the cut count `state` held when the batch was written (1.2) |
| 40 | 32 | the hash of the entry before its first, which that entry chains from |
| 72 | 32 | the hash of its last entry |
| 104 | 32 | SHA-256 of bytes 0 to 104 |

A batch header is valid when it begins with the mark, its hash matches,
its version is the segment's, the reserved bytes are zero, the count is not
0, the first index is from 1 to the largest index, the length leaves at
least 64 bytes for each entry, and the batch ends within the segment. A
batch is **whole** when its header is valid, all `count` of its entries
follow it whole and chained (3.4), they fill exactly its length, and the
last one has the hash the header names. The header names the last hash so
that a batch whose write was cut short, or one whose header survived an
overwrite that did not, is never taken for whole, even when every entry
read under it chains. It names the cut count so that a batch written before
a cut is never taken for part of the log after it, even when its entries
chain from entries appended again after the cut, byte for byte (3.9). It
names the entry before its first so that a header that does not continue
the chain is passed over without anything of its entries being read.

**Segment boundaries.** An entry never spans segments. A batch flush
writes the entries that fit in what is left of the current segment as one
batch there, and the rest as a second batch at the start of the next
segment, after its header. So an append's payload is bounded by what an
empty segment holds (`Config::max_payload`, 16 MiB by default; it must
fit in a segment with its headers, and be at most the format's bound,
3.4), and a batch never spans more than two segments: an append that would
make it do so is refused (`LogError::BatchFull`) until the batch is
flushed.

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

**Bounds the format sets, whatever the configuration.** A payload is at
most 64 MiB, and an index at most 2^64 - 2, so that the index after every
entry can be represented. An entry whose fixed fields declare more is not
valid, and nothing is read for its payload: the fixed fields are checked
before anything else is read. An entry written under a larger
`Config::max_payload` than the current one stays readable within the
format's bound. The append refuses an index past the bound.

### 3.5 Walking the log

Opening, reading and cutting all walk the log the same way, from the start
of a listed segment, expecting the index its header names and chaining
from the hash it names. At each place, in this order:

1. **The next segment.** If the next listed segment's header names the
   index expected next, with the hash and term of the entry just read, the
   writer moved to it at this point: the walk continues at its first
   batch, and nothing more of the current segment is read.
2. **Inside a batch** that has entries left: a valid batch header whose
   first index is the one expected, and that names the entry just read
   as the one before it, ends the batch (a cut, 3.9, and the batch written
   after it); otherwise the next entry must be whole and chained, or the
   walk ends there.
3. **Between batches**: a valid batch header whose first index is the one
   expected, and that names the entry just read as the one before it,
   begins a batch; otherwise the walk ends.

The walk itself takes batches of any cut count. Up to the marker that is
right: what the marker covers was written and flushed, and is chained to
the marker's hash, whatever cut count its batch carries. After the marker,
only batches of the current cut count count (3.6).

**What the walk reads.** Each place the walk decides on is read once,
into one buffer, in steps that each go only as far as the decision at hand
needs, and nothing ahead of it (3.11, "Whatever is checked is what is
used"):

- where a batch may begin (rule 3, and rule 2's header): nothing when
  fewer bytes are left in the segment than a header and one entry of no
  payload (200); else the 8-byte mark; only if it is there, the rest of
  the header's start, to byte 72 (version, count, first index, length,
  cut count, the previous hash); only if those name the index expected and
  the entry just read, and the batch fits in the segment, the rest, to
  byte 136, and the header's hash. At an open, a header of another cut
  count after the firm position is settled by its start alone (3.6);
- where an entry is expected (rule 2): its 32 fixed bytes, which also hold
  a header's mark if a header begins there (the rest of the header is then
  read as above, into the same buffer); then, once the fixed fields are
  checked, the rest of the entry, once. Its hash is checked over the bytes
  the fields were decoded from, and every field returned comes from them.
  Where the batch has no room left for an entry, only a header can be
  there, and only its mark is read first.

So a place the device cannot read is met only by a decision that needs it,
and a read error is that decision's: no error in bytes the walk does not
need (a stale payload, the rest of a batch it has judged, the bytes past a
whole batch that a header's mark already rules out) can fail a decision
about the records before them.

**Reading** (`Log::read`) walks from the segment that holds the first
entry asked for, through the entries before it in that segment, and returns
the durable entries from it on; a walk that ends before the durable entry is
damage, reported to the reader. An entry larger than 64 KiB is read into a
buffer of its own, which becomes the payload returned, so a read holds at
most one copy of a payload.

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
   start's, is damage. Entries before the start in the first segment
   walked are read through, since the log keeps no index of where entries
   are; damage to them is reported too.
4. After the firm position, an entry counts only once its whole batch has
   been read, found whole (3.3), and found to carry the cut count `state`
   holds. What its header settles is settled before anything else is
   read: of a batch with an older count, no entry past the firm position
   is read at all. The first batch that is not whole, or carries an older
   count, and everything after it, is **cut**; so is anything after a walk
   that ended.
5. **Where the log ends** after the firm position is decided from what
   must be there, reading only what decides it (3.5, "What the walk
   reads"). After a whole batch, the log ends there when fewer bytes are
   left in the segment than a header and one entry of no payload, or when
   the bytes that would hold the next header's start can be read and do
   not begin a valid header that follows the entry just read and carries
   the current cut count (a wrong mark settles it in 8 bytes). Inside a
   batch, the next entry's own bytes decide, or a header's. Only when the
   bytes a decision needs cannot be read is the end unreadable.
6. After the firm position, a read the device fails ends the log **for
   this open only** (`Opened::unreadable_end`: the segment, the offset of
   the read that failed and the index expected there). Nothing past it is
   cut, dropped or written over, and the cut count stays: the log may
   continue past it, and the caller can tell that from an end the open
   found. The log refuses appends (`LogError::EndUnreadable`) until it is
   opened again, or until a cut (3.9) or giving it up (3.9, "Giving up an
   unreadable end") discards what may follow. An open that can read it
   takes what follows as any whole tail of the current cut count. At or
   before the firm position a read the device fails is
   `LogError::Unreadable`, which the caller tells apart from damage.

On damage the open returns it (`LogError::Damaged`: what, the segment, the
offset and the index expected there) and writes nothing: nothing is cut,
and the marker and the segment run stand. It hands `state` back
(`OpenFailed`), so that the caller can record what it decides
(server-storage.md 6.12): in repair on a cluster, stop on a single server.
It hands `state` back on every other error too, so that the open can be
tried again in the same process; a `state` whose own write failed refuses
every update until it is opened again.

Every batch kept after the firm position is **written again** as the
walk goes, with synchronous writes, from the buffers the walk verified it
in (3.11): its entries as they are read, held up to 1 MiB and written in
runs, and its header last, once the batch is found whole and current
(header and entries in one write when they were held together). Nothing is
read for it a second time. An earlier process may have written the batch
and stopped without a power failure, or seen a flush of it fail, leaving it
whole in the system's cache only: a flush would find nothing to do and
vouch for bytes the device does not hold. A batch that is not kept has its
header never written, and the entries of it already written are the bytes
the walk read there, so the next walk finds it as this one did. Entries at
or before the firm position are not written again: what the marker covers
was written by a synchronous write that returned, and what the start
covers is in a checkpoint, which covers only published entries, published
only once a marker covered them (server-storage.md 6.5 and 6.7); the
marker is behind the start only when a lost copy of `state` took it back
(1.5), after those entries were on the device. Then, in this order:

1. `log/` is made if it is missing, and the store folder flushed, on every
   open: an earlier open may have made `log/` and failed to flush, and a
   later one that finds it must not take its name for durable. The same
   rule holds for every name the log makes: its folder is flushed after
   it, whoever made it and whenever (3.7).
2. One `state` update moves the marker to the last entry kept (its
   committed position unchanged) and drops the listed segments after the
   one the log ends in (none when the log ends at an unreadable place): a
   marker advance in one slot when only the marker moves, both slots
   otherwise.
3. Segment files `state` does not list are removed, and `log/` is flushed,
   on every open.

A crash at any point leaves a log that the next open reads to the same end:
what is written again during the walk is what the walk found, as it found
it; until step 2 the marker is where it was, and the dropped segments are
after the end; step 3 removes only what `state` no longer lists. Nothing
after the end is changed at the open (what was written again of a batch
that is not kept is the bytes the walk read there, and never its header):
a torn batch stays torn, and a batch of an older cut count stays older,
however a later crash settles the system's cache, so nothing there decides
the end differently. The next batch is written where the log ends.

**A tail that comes back is harmless.** A later open keeps what follows
an earlier open's end when it finds it whole, chained and of the current
cut count. Only an unreadable end can give that: a torn batch cannot
become whole (the device holds a subset of what was written), and a batch
of an older count stays older. Such a batch was written by this server
after the marker the earlier open ended at:

- On a single server nothing after the marker was confirmed, unless the
  marker itself went back with a lost copy of `state` (1.5); then the
  batch is one that was confirmed, and getting it back is what is wanted.
  An open that ended at an unreadable place takes no appends, so nothing
  is written over such a batch before an open can read it.
- On a member of a cluster, a log longer than it last reported is an
  ordinary case for the leader: its consistency check keeps the entries
  that match, and has the follower cut those that do not (7.1), which the
  cut count then keeps out for good.

**What appends leave behind.** Appends write at the end the open found.
After an open that ended at an unreadable place they are refused, so they
never write over a batch a lost marker may have confirmed; after a cut
that moved the end before that place (which raises the cut count), after
what may follow it was given up (which lists a fresh segment, 3.9), or
after an open that read past it, they go on. What remains past a new batch
is never read as part of the log unless it truly continues it: a batch
counts after the marker only if its header names the entry just read as
the one before its first (a SHA-256 hash, so an old batch cannot name an
entry written after it) and carries the current cut count, and an entry is
read only inside such a batch. Old bytes inside a new batch's span are
overwritten, or end the walk where they do not chain.

A part of a batch written to a new segment whose `state` update was lost is
not kept: the segment is not listed, so it is not part of the log. Its
entries were never reported durable.

The open returns the committed position the marker recorded as
**unconfirmed** (`Opened::recorded_committed`): a lost or unreadable copy
of `state` can have taken it back one advance (1.5).

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
finishes the file: it opens the file the refused call made, and flushes it
and the folder as if it had made it. A failed flush stops the log. A crash
at any point leaves an unlisted file, which the next open removes.

### 3.8 A batch flush

`Log::flush_batch` writes the open batch (3.3), every batch carrying the
current cut count:

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
returns. A batch that stays in one segment costs one synchronous write and
one write and flush of `state`; one that reaches the next segment costs two
synchronous writes and both slots of `state`, two writes and two flushes.

| A crash | leaves |
|---|---|
| before or during step 1 | the old marker; the batch absent, torn, or (if the write landed whole) whole after the marker, kept by the next open |
| during step 2 | the same, for the part in the current segment; the spare unlisted, removed by the next open |
| during step 3 | the old copy of `state` or the new one: the batch is durable on the device either way, and the next open moves the marker over it if the copy is the old one (a part in the spare is lost with the old copy, 3.6) |
| after step 3 | the batch durable |

Any failure stops the log. An entry the failed call wrote may be found
whole at the next open; the failed call reported nothing durable.

`Log::set_committed` records a higher committed position on its own: a
marker advance in `state`. Giving a committed position, through either
call, confirms it for cuts (3.9).

### 3.9 A cut

`Log::cut` removes the entries from an index on (server-storage.md 7.1).

**What it refuses**, with nothing changed: every cut until the caller has
given a committed position since the log was opened
(`LogError::CommittedUnconfirmed`), because the one the marker recorded
can be one advance behind (1.5); then an index at or before the highest
committed position heard, at or before the entry the log was opened from,
or at or before the one before the oldest entry its last retirement still
needed, which are all committed; and an index before the first entry the
log holds. On a single server nothing cuts; on a cluster the caller learns
the committed position from the current leader before it cuts.

Entries of the open batch are dropped in memory. When the marker covers
entries that are cut:

1. the walk finds the first cut entry: the place the log will continue
   from is its batch's header when it is its batch's first entry, else the
   entry itself;
2. one `state` update, written to both slots, lowers the marker to the
   entry before it, raises the cut count by one and drops the listed
   segments after the one that holds it. **The cut is done from the moment
   the first slot's flush returns**: an open takes that copy (1.3);
3. the files of the dropped segments, and of the spare if segments were
   dropped (its number changed), are removed, and `log/` is flushed, on
   every cut.

Nothing is written to the segments. The batches written before the cut
stay where they were, after the lowered marker, and carry the old cut
count, so no walk takes them for part of the log again (3.6), even when
the cut entries are appended again, byte for byte, and the batch after
them chains from them. Appends continue from the place found in step 1:
the next batch is written over it, and a walk then meets that batch's
header where the cut entry was, which ends the batch the cut entry was in
(3.5 rule 2).

| A crash | leaves |
|---|---|
| before step 2 | the old marker, cut count and run; nothing was cut |
| during step 2, before the first slot's flush returns | the old copy of `state`, or the new one if its write landed: nothing cut, or the cut done |
| after the first slot's flush returns | the cut done: the next open reads the new copy, writes it to both slots, ends the log at the entry before the cut and removes the dropped segments' files. Unless that slot, the only one with the new copy, is lost or cannot be read before that open: the open then takes the old copy, and the cut is undone, as one slot lost costs the update it held alone (1.5) |
| after step 2 | the cut done; dropped files still there are removed by the next open |

**What it returns** (`CutDone`) says what it did: the last entry left, how
many entries of the open batch were dropped, how many durable entries were
cut, the cut count `state` recorded (none when nothing was written), the
unreadable end it discarded what may follow, if the log had one, the fresh
segment it listed, and the segment files it removed. A cut that finds
nothing at or after its index changes nothing and says so.

**A log that ends at an unreadable place** (3.6) has no durable entry past
its end, and takes no appends. A cut at or before its durable entry is the
cut above: the log then ends before the place, the batch that follows the
new end carries an older cut count, and no walk reads as far as the place
again, so appends go on. A cut at any index after the durable entry gives
up what may follow the place, as below.

**Giving up an unreadable end** (`Log::give_up_unreadable_end`, and a cut
past the durable entry of a log that ends at an unreadable place)
discards whatever may follow the place, for good, whatever an open can
read there later. Raising the cut count alone would not do: the next open
would still read the bytes after the last entry to decide whether a batch
begins there, and fail on the same place. So the log continues in a fresh
segment that a walk moves to from the last entry, by rule 1 of 3.5, before
it reads anything more of the segment it ended in:

1. the next segment is made as in 3.7, and its header, naming the entry
   after the last verified one as its first and that entry's hash and term
   as the previous ones, is written with a synchronous write;
2. one `state` update, written to both slots, lists it and raises the cut
   count by one; the marker stays at the last verified entry. **It is
   done from the moment the first slot's flush returns**, as a cut is.

Appends then go to the fresh segment. What lay past the last verified
entry in the segment the log ended in is never read again; a batch there
that an open could read later carries an older cut count too. The
operation is refused, with nothing written, when the log does not end at
an unreadable place, when the entry the caller names (index and hash) is
not the last one the open verified, and when `state` lists segments after
the one the log ends in. No write of this log leaves that: every update
lists a segment only with a marker in it or just before it (3.8, 3.9, this
section), an open drops what follows the segment it ends in, and a lost
copy of `state` goes back to a run and a marker that agree; an open that
ends at an unreadable place has therefore passed the marker in the last
listed segment. A disk too full for the segment refuses it as it refuses
an append (`LogError::NoSpace`, 3.7). It needs no committed position: it
removes nothing at or before the durable entry (server-storage.md 6.3).

| A crash | leaves |
|---|---|
| before step 2 | the old copy of `state`: nothing given up; the fresh segment is unlisted, removed by the next open, which ends at the unreadable place again |
| during step 2, before the first slot's flush returns | the old copy, or the new one if its write landed: nothing given up, or given up |
| after the first slot's flush returns | given up, unless that slot is lost or cannot be read before the next open (1.5) |

### 3.10 Retiring

`Log::retire` is given the oldest entry still needed (the entry after the
oldest kept checkpoint, or older for another server, server-storage.md 6.7
and 7.5). It retires, from the front, every segment whose next segment
begins at or before that entry, never the last:

1. one `state` update, written to both slots, drops them from the run;
2. their files are removed and the folder flushed.

A crash between the steps leaves unlisted files, removed by the next open;
`state` never lists a segment that is gone. No cut reaches back before the
oldest entry needed afterwards.

### 3.11 Faults and bounds

- **What is read back is never made durable by a flush alone.** After a
  flush fails the system can drop the unwritten bytes and still show them
  from its cache; a later flush, by this process or another, finds nothing
  to do and succeeds. So what must become durable is written again by
  this process with a synchronous write, and only that write's success
  counts. Where it applies:
  - the tail an open keeps after the firm position is written again, from
    the buffers the walk verified it in, before the marker moves over it;
    what is at or before the firm position is on the device already (3.6);
  - `state`'s open writes the copy it chose back, as the bytes it read,
    before it writes over the other slot (1.3);
  - every batch and segment header is written with synchronous writes, so
    nothing the log writes waits on a flush;
  - `Log::prepare` may find a file a refused call of this log made; its
    pending writes were never dropped, since a failed flush stops the log,
    and it is flushed after it is finished. An open removes such files, so
    no later process uses one;
  - segment headers are read back at an open and never flushed by it:
    they were written synchronously before their segment was listed;
  - for folders, the same rule is a flush of the folder after every
    change the log makes and at every open, whoever made the change and
    whenever (3.6, 3.7, 3.9, 3.10).
- **Whatever is checked is what is used.** A range of a file is read once
  into memory, and every decision about it (hashes, counts, lengths, the
  fields returned to the caller) and every write-back of it uses that
  buffer; nothing is read a second time to be acted on. A second read can
  return other bytes than the first (a cache that dropped what a failed
  flush left, a device that misreads), and bytes that were not checked are
  not known. Every read of the log and of `state`:
  - the walk (3.5), for an open, a read and a cut: each place once, in
    steps that each read only bytes not read before; a header's fields
    and hash, and an entry's fields, hash and payload, from one buffer.
    `Log::read` returns the fields and payloads of that buffer;
  - the open's write-back of the tail: the walk's buffers (3.6);
  - segment headers at an open: each read once, 128 bytes, checked and
    decoded from them, and not read again: the walk starts after them, from
    the header decoded at the open;
  - a cut's walk to its first cut entry is the walk above; the cut writes
    only `state`. Giving up an unreadable end reads nothing: it uses the
    last entry the open verified;
  - `Log::prepare` reads nothing of a file it finds;
  - `state`'s open reads each slot once (again only after a read that
    failed, which gave no bytes), and decodes the copy it chooses, and
    writes it back, from that buffer (1.3).
- **After a failed write, flush or `state` update** the log refuses every
  call (`LogError::Poisoned`): what reached the device is unknown, the
  failed flush is never retried, and the store stops and opens again
  (server-storage.md 6.12). The refusals of 3.7 are the exceptions: they
  touch only the unlisted spare. A `state` update `state` refuses writes
  nothing and does not stop the log.
- **Everything the log reads is checked before it is used**: headers by
  their hashes, lengths against the batch, the segment and the format's
  bounds (3.4), entries by the chain. No length read from disk makes the
  log hold more than one record in memory, a payload of at most 64 MiB or
  a header, besides what an open holds of a batch it writes again: at most
  1 MiB of its entries and one more entry (3.6).
- **One owner.** The log owns `state` and the store folder's lock from its
  open until it is dropped (`Log::into_state` hands `state` back). Every
  call that writes checks that the lock is still held, so a log of a
  process that has stopped writes nothing. The other fields of `state`
  (the term and vote, the repair record) change through
  `Log::update_state`, which refuses to touch the segment run, the marker
  and the cut count.
- **The open batch** is at most what is left of the current segment and one
  more segment.
- Every listed segment is kept open while the log is.
