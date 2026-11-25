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
| 105 | 8 | marker: the committed position: the last entry of this server's log known to be committed, never past the marker's index (3.8) |
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

## 2. Pages and trees

The trees of server-storage.md section 6.4, as `om_store::tree` and
`om_store::page` build them. This is **tree format version 1**: the page
layouts of 2.2 to 2.5 and the shape rules of 2.7. A change to either is a
new version. The test `format_version_1_is_pinned`
(`crates/om-store/tests/trees.rs`) pins the root hash a fixed sequence of
edits gives, so that a change cannot go unnoticed.

### 2.1 Keys and nodes

A tree holds one key tree: nodes addressed by paths of byte strings
(segments). The key of a path is each segment's bytes with `00` written as
`00 ff`, followed by `00 01`. The root, the empty path, has the empty key
and is not stored: it always exists and is always a list.

Plain byte order of keys is the key tree's order
([file-format.md](re/file-format.md) 3.6): a node before its children,
siblings by the bytes of their segments, a shorter segment before a longer
one it begins. A node's key is a prefix of every key below it, and no
segment's encoding contains `00 01`, so the subtree of a node with key `k`
is exactly the keys in `[k, k')`, where `k'` is `k` with its last byte
(`01`) replaced by `02`. A subtree is one contiguous range, in depth-first
order.

A node is a **list** (kind 1, no value), a **value** (kind 2, possibly
empty) or **both** (kind 3: a list that also carries a value). No file
FileMaker wrote has the third kind (server-storage.md section 4); the
store keeps it so that nothing is dropped.

Bounds, checked for every edit before any of an entry is applied (an entry
with an edit beyond them is refused whole, with the edit's number and the
bound):

| Bound | Value |
|---|---|
| encoded path of a node | at most 65,535 bytes |
| value of a node | at most 65,535 bytes |
| segments in a path | at most 64 |
| level of an index page | at most 40 |

The first two are the 64 KiB of server-storage.md section 6.11, taken as
the largest a 16-bit length holds, so a page cannot state more. Pages are
held to the others when read: a key of more than 64 segments, whether its
page holds it whole or only its start, or an index page above level 40,
is malformed (2.6). The depth bound is the store's own: an
edit creates its missing ancestors (2.6), each a prefix of its path, so
without it one 64 KiB path of empty segments would create 32,767 items
totalling about 1 GiB of keys. With it, one edit creates at most 64 items
of at most 64 KiB each.

### 2.2 Pages

Every page is 16,384 bytes:

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | checksum (2.5) |
| 8 | 8 | the page's own number |
| 16 | H | the region: everything the page's hash covers |
| 16 + H | 8 × P | page numbers the region refers to, in order |
| 16 + H + 8P | rest | zero |

The region begins with a 24-byte header:

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | tree format version: 1 |
| 1 | 1 | kind: 1 leaf, 2 index, 3 overflow |
| 2 | 1 | level: 0 for leaves and overflow pages; an index page is one above its children (1 to 40) |
| 3 | 1 | zero |
| 4 | 2 | count: items (leaf), children (index) or data bytes (overflow) |
| 6 | 2 | H, the region's length, this header included |
| 8 | 2 | P, the number of page numbers after the region |
| 10 | 6 | zero |
| 16 | 8 | the index of the entry that wrote the page |

Page numbers are kept out of the region, so that nothing a hash covers
depends on where a server put a page.

### 2.3 Leaf pages

After the header, `count` items in key order, each:

| Size | Field |
|---|---|
| 1 | the node's kind (2.1) |
| 2 | K, the key's length (at least 2) |
| 2 | V, the value's length (0 for a list) |
| K + V | the key, then the value, when 5 + K + V is at most 4,096 (a quarter of a page) |

An item larger than that **spills**: the page holds its first 1,024 bytes
(of the key followed by the value), then the 32-byte hashes of the
overflow pages that hold the rest, `ceil((K + V - 1024) / 16344)` of them.
Their numbers are in the page's pointer array, item by item, in order.

An item's **footprint**, what it takes of a page, is `5 + K + V`, or
`5 + 1024 + 40n` when it spills with `n` overflow pages.

### 2.4 Index pages and overflow pages

An index page holds `count` children (at least one), each:

| Size | Field |
|---|---|
| 32 | the child's hash |
| 8 | the child's stamp: the index of the entry that wrote it |
| 2 | L, the separator's length, for every child but the first |
| L | the separator, when 2 + L is at most 4,096 |

A separator spills by the rule of items: the page holds its first 1,024
bytes and the hashes of its overflow pages. The pointer array holds the
children's numbers first, then the separators' overflow pages, in order.
A child entry's footprint is 48 bytes (hash, stamp and number), plus the
separator's `2 + L` or `2 + 1024 + 40n`.

Child `i` holds the keys from its separator (child 0: from its parent's
lower bound) up to the next child's separator. A separator made by a
leaf split (2.7) is the shortest prefix of the right side's first key `r`
that is above the left side's last key `l`: `r` cut just after the first
byte where the two differ, or, when `l` is a proper prefix of `r`, just
after the byte of `r` that follows `l` (`l` = `7a 00 01`, `r` =
`7a 00 01 61 00 01` gives `7a 00 01 61`). It need not be a whole key, and
can end inside a segment, but as it begins a key it is a whole key or the
start of one, with at most 64 segment ends and none followed by more
bytes after the 64th; a page with any other separator is malformed.
Separators of index pages are not made: they move up from a split or
down into a join (2.7).

An overflow page holds `count` data bytes after its header and refers to
no page. The overflow pages of an item or separator hold the bytes after
its first 1,024, 16,344 to a page, the last one holding the rest.

A page fits when the footprints of its items or children total at most
16,344 bytes (the page less its two headers).

### 2.5 Hash and checksum

A page's **hash** is SHA-256 over its region, bytes `16 .. 16 + H`: its
kind, level and writing entry, its items, or for an index page its
children's hashes, stamps and separators, and the hashes of its overflow
pages. Page numbers, its own included, are outside it. A tree's root hash
therefore covers the whole tree, content and shape, and two stores that
place pages differently have the same root hashes.

A page's **checksum** is the first 8 bytes, as a little-endian integer, of
SHA-256 over the page's hash, then bytes `8 .. 16` (its own number), then
the pointer array. With the rule that every byte after the pointer array
is zero, every byte of the page is covered: the region through the hash,
the numbers directly, the rest by being zero. Damage to a pointer is
found in the page that holds it.

Truncated SHA-256, rather than a CRC implemented in the crate, because:

- the hash is computed on every read and every write anyway, and chaining
  the checksum through it costs one more SHA-256 over at most a few
  kilobytes, where a CRC would be a second pass over all 16 KiB. On the
  development VM (no SHA instructions), SHA-256 of 16 KiB takes 39 µs;
- it is not more code to get right. A CRC fast enough to matter needs
  table-driven or carry-less-multiply code of the crate's own (no
  third-party crate may be added for it);
- 64 bits leave a chance of 2^-64 that a damaged page passes, against
  2^-32 for CRC-32. A CRC's guarantee to find every short burst matters
  little here: the parent's hash finds any change to the region with
  certainty short of a SHA-256 collision, and a nonzero byte in the zero
  fill is always found.

### 2.6 Reading and writing

**Reading.** A page is read by its number and the hash its parent holds
(a root, by the number and hash that name the version). Before any of it
is used, `om_store::page::Page::verify` checks, in order, and names the
page in the error:

1. the headers fit the page (otherwise *damaged*);
2. the checksum matches and the bytes after the pointer array are zero
   (otherwise *damaged*);
3. the page names the number it was read at (otherwise *misplaced*, with
   the number it names);
4. its hash is the one the parent holds (otherwise *stale*: an intact page
   that is not the version the parent names, as a lost write leaves);
5. its content follows the format: lengths within the page and the
   bounds, keys well formed, within 64 segments and in order, separators
   that begin such keys and in order, stamps no later than the page's own
   entry, the number of page references equal to P (otherwise
   *malformed*).

The tree then checks that a child has the level and kind its parent
implies, was written by the entry its stamp names and, if a leaf, is not
empty, and that an overflow page holds the bytes its item expects. A
page the store cannot find is *missing*. Nothing is allocated by a size the bytes give before
it is checked against the page size or the bounds, and no read recurses
deeper than the tree's levels.

A read touches only the pages on the way to what it returns: a lookup,
range or walk the pages from the root to the items it returns and those
items' overflow pages; a listing of children the keys of the items it
passes and the children's own values; "changed since" index pages only
(2.8). Every comparison with a stored key or separator uses the bytes its
page holds first, and reads its overflow pages one at a time only while
the bytes so far are equal to the other key; that is the one way a read
reaches an overflow page it does not return. A reverse listing finds each
child from the last key under it: when the child's own key is longer
than the 1,024 bytes a spilled key keeps in its page, it reads that last
key's overflow pages as far as the child's key goes, and no further.

**Writing.** An entry applies its edits (2.7) to a working copy of the
pages it reaches, reading the store and changing nothing in it. The copy
keeps items and separators in their pages and compares them as reads do;
it reads a key whole only for the two keys at a leaf split's boundary and
the first and last keys a delete removes. Then, into the entry's unit of
work of the page store (`om_store::page::EntryPages`), which the caller
opens once for the whole entry, it writes every page of the working
copy, children before parents, each stamped with the entry's index, and
releases every page the old version reached and the new one does not
(replaced by a copy, or dropped with everything under it); it returns
the new root and commits nothing. A tree the entry makes starts empty
and is always written, if only as one empty leaf. The unit names the
trees too: the caller gives it each changed tree's new root and the
trees the entry makes, drops and renames, and commits once; the store
holds each tree's name and root and changes them only then. Within one
unit a tree's root is given at most once (made, or given a new root),
and only a page the unit wrote; a tree dropped is one that existed when
the unit began and that the unit gave no root; the old root of a tree
given a new one is released by the unit, either way; and no page the
unit writes refers to a page that is a tree's root once the unit takes
effect (a root it gives, or the root of a tree it leaves alone), since a
page two trees reach would be freed with the one dropped while the other
still reached it (clones will share pages with a share the store
records; until one is recorded, such a reference is refused). The tree
code never writes one: a page it writes refers only to pages of its own
tree below the root, and the root is written last. A root the unit
gives cannot be referred to by a page already committed: it is a page
the unit wrote under a number it allocated, which was free, and a page
is freed only once no version reaches it and what referred to it was
taken down. One unit is open
at a time: a second is refused while one is open, so nothing changes the
trees between a unit's begin and its commit. A unit that breaks any of
this is refused whole, by the page file and by the store in memory
alike. A failure at
any point leaves the store as it was once the unit is dropped, so the
entry can be applied again. An entry that changes an existing tree in
nothing writes nothing and keeps its root. An entry's index must be
above the last entry that changed the tree (the root page's), so stamps
only grow. The page store's guarantees, including what clones need, are
in the documentation of `PageStore` and `EntryPages`.

### 2.7 The shape rules

The shape of a tree, and so its root hash, is a function of the sequence
of edits it was given, entry by entry, and of nothing else: not page
numbers, timing, what is cached, or the order of a hash map. The rules
below fix the shape, which pages an entry copies (so which carry its
stamp), and which overflow pages are kept.

Terms: a node *fits* when its footprint (2.3, 2.4) is at most 16,344
bytes. A node is *underfull* below a quarter of that, 4,086 bytes. A
node *shrank* in a primitive when its footprint after the primitive (and
the fixes below it) is smaller than before. A *unit* is an item of a
leaf or a child entry of an index page; the units of a node have
positions 0 to `n - 1` in key order, and splitting *before* unit `s`
puts units 0 to `s - 1` on the left and `s` to `n - 1` on the right. The
child of an index page for a key is the child whose range holds it: the
number of separators at or below the key.

**Edits.** An entry's edits are applied in order, each as primitives:

- *Put a node* at a path with key `k`: for each proper ancestor,
  shallowest first, a missing one is set as an empty list (kind 1, no
  value), and one that holds a value is changed to hold both (kind 3,
  same value), as `om_hbam::OwnedNode::put` does; then the range
  `[k 00, k')` is deleted (`k 00`: `k` followed by one zero byte; the keys
  strictly under the path); then the node is set, unless the item with key
  `k` already has that kind and value.
- *Delete a subtree* at a path with key `k`: the range `[k, k')` is
  deleted; for the root, every key.

A primitive that would change nothing is skipped and touches no page:
setting an item that already exists with that kind and value, or
deleting a range that holds no key. The shape after a primitive is a
function of the shape before it and the primitive.

**Copies and stamps.** A page is copied into the working copy when a set
or a kind change goes through it (from the root to the leaf), when a
delete goes into it (the root, and each child it goes into), when it is
one side of a join, and when a delete's last step (below) goes through
it. Pages read only to decide something (whether a primitive changes
anything, what a root's only child is) are not copied. At the end of an
entry that changed anything the root is copied if it is not already. The
pages written are the copies, and the new pages splits and joins make,
still in the tree at the end of the entry; each carries the entry's
index.

**Overflow pages.** An item set by a primitive is written anew, and if it
spills its overflow pages are new pages stamped with the entry. Every
other item keeps the overflow pages it has, through copies, splits,
joins and a change of kind. A separator made by a leaf split is new, and
gets new overflow pages if it spills; a separator that moves up or down
keeps its own.

**Set an item.** Descend from the root to the leaf, at each index page to
the child for the key. Insert the item in its leaf where it keeps the
keys in order, or replace the item with that key (a kind change only
changes the kind). Then fix the path bottom-up, each index page fixing
the child the primitive went through (below), then the root rule.

**Fixing a child.** A child that does not fit is split (with its hint, if
it has one). Otherwise, if it shrank and is underfull or empty: when its
parent has another child, it is joined (below); when it is empty and its
parent's only child, it is removed (and the parent is then empty); an
underfull only child is left as it is.

**Delete a range** `[lo, hi)`. Before it, note the smallest and the
largest key in the range. Starting at the root, in each index page: the child `i` for
`lo`, and the last child `j` whose range starts below `hi` (the number of
separators below `hi`; the last child when the range has no end).
Children strictly between them are removed whole, unread, with their
separators; child `j` keeps its separator, so child `i`'s range reaches
to it. Then the primitive goes into child `i` if it holds a key in the
range, and into child `j` (when `j > i`) if it does. In a leaf, the keys
in the range are removed. On the way back, child `i` is fixed if the
primitive went into it, then child `j` (the same page, wherever fixing
`i` moved it) if the primitive went into it, unless fixing `i` joined it
into `i`. Then the root rule. Last, the paths from the root to the leaves
for the two keys noted (each found as in a set) are copied, so that the
pages covering the deleted keys' places carry the entry's stamp (2.8).

**Split.** A node of `n` units that does not fit is split before unit
`s`, `1 <= s < n`, and each side is split again in the same way, left
side first and without a hint, until every part fits:

- the *hint* `h` exists only in a set, and is found for each node on the
  set's path from the leaf up. In the leaf, `h` is the position of the
  item set, after its insertion. In an index page, `h` exists only when
  fixing the child the set went through split that child: then `h` is
  the child's own position plus one, which is, in the index page's
  children after the child is replaced by the parts of its split, the
  position of the second part, however many parts there are. When the
  fix joined the child or left it as it was, the index page has no hint.
  A set goes through one child of each index page, so each has at most
  one hint; the root has its own, found the same way. A delete gives no
  hint. When there is a hint, `1 <= h < n`, units 0 to `h - 1` take at
  least seven eighths of 16,344 bytes (14,301), and both sides fit, `s`
  is `h`. Items appended in key order therefore fill their pages;
- otherwise `s` is where the two sides' footprints are nearest equal, the
  smallest such `s` on a tie.

The footprints of an index page's sides count each child entry, and the
separators of every child but each side's first; the separator of child
`s` moves up to the parent as the separator of the right side. A leaf's
right side gets a new separator, made from the last key on its left and
its own first key (2.4).

*Example* (pinned by the test
`an_index_page_splits_at_the_second_part_of_its_split_child`). After the
empty tree of entry 1, entry 2 puts 21 empty lists at one-segment paths
whose segment is 3,600 bytes of `42` and one byte, 1 to 21 in that order.
An item takes 3,608 bytes, so a leaf holds four: the fifth item splits
the root leaf before position 4 (the four before take 14,432 bytes)
under a new index root, and each later fifth item splits the last leaf
the same way. Each separator is 3,601 bytes, the right side's first key
cut after its last segment byte, so a child entry after the first takes
48 + 2 + 3,601 = 3,651 bytes. The 21st
item splits the fifth leaf (position 4) into parts at positions 4 and 5,
and the index, now of six children and 18,303 bytes, does not fit. Its
hint is 5: positions 0 to 4 take 5 × 48 + 4 × 3,603 = 14,652 bytes, at
least 14,301, and both sides fit, so it splits 5 and 1 under a new root.
At its middle it would have split 3 and 3.

**Join.** A child is joined with its right sibling, or with its left one
when it is the last child. The joined node takes the place of the left
one of the two. Two leaves join into the left one's items followed by
the right one's, and the separator between them goes. Two index pages
join with that separator moved down between their children, unless one
of them has no children, when the other is the result and the separator
goes. A joined node that does not fit is split, without a hint. A joined
node that is empty is joined again by the same rule, or removed when it
is its parent's only child, so no page below the root is ever empty.

**Root rule.** After each primitive, repeatedly: a root that does not fit
is split, with the root's hint in a set, and its parts become the
children of a new root one level up; an index root with no child becomes an empty
leaf; an index root with one child is replaced by that child, whether or
not the entry copied either of them. It stops at a leaf, or at an index
root with more than one child that fits. An empty tree is one empty leaf.

**Why it is deterministic.** Every choice above is a function of the
tree's content (keys, values, kinds, separators, stamps and levels) and
of the primitive. Page numbers are only used to read a page, never to
choose anything; the working copy is kept in vectors in key order; and
the code has no clock, randomness or hash map.

### 2.8 Stamps and "changed since"

Each child entry carries the index of the entry that wrote the child, and
every page written by an entry carries that index, so the stamp of a
child is never below the last entry that changed anything in the child's
range of keys. That holds because a primitive copies every page on its
way to what it changes, every page a split or join makes is new, and a
delete copies the paths to the pages whose ranges grow over the deleted
keys' places (2.7).

`Tree::changed_since(path, n)` answers whether anything at or under
`path` changed after entry `n`. It reads index pages only: at each level,
of the children whose ranges overlap the subtree's range `[k, k')` (from
the child for `k` to the last child whose range starts below `k'`), those
stamped after `n` either lie wholly inside the range (the answer is yes),
or are at one of the range's two ends, where an index page is looked into
and a leaf counts as a change without being read. Whether a child lies
inside is decided by comparing its separators with `k` and `k'`, which
reads a spilled separator's overflow pages only while its bytes are equal
to the key. It reads at most two pages per level, and no leaf.

The answer is never no when something changed. It can be yes when
nothing under the path changed but another path in a leaf at one of the
range's ends did, or when a deletion in such a leaf was of keys outside
the path. For the root path it is exact: yes exactly when an entry after
`n` changed the tree.

### 2.9 Limits

- Pages are not compressed, and leaves keep each key whole (no prefix
  compression), so deep paths repeat their ancestors' segments in every
  key.
- Items are not split across leaves: a large value is one item, with its
  overflow pages.
- An index page with spilled separators can hold as few as about 14
  children, so a tree whose neighbouring keys share prefixes of more than
  about 4 KiB is tall.
- The "changed since" answer has the granularity of the leaves at a
  range's ends (2.8).
- A tree this code did not write can hold an empty leaf below the root,
  which these rules never make. Every read and edit that reaches it
  refuses it; an edit elsewhere in the tree keeps it, and only
  `om_store::tree::check`, which reads every page, finds it.
- Keys longer than 1,024 bytes are compared and listed with reads of
  their overflow pages, as far as the comparison or the child's key goes
  (2.6).

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
most 64 MiB, and an index at most 2^54 - 1. An entry's first 8 bytes are
its index, and the batch mark `OMBATCH\0` read as a little-endian number
is about 2.03 * 10^16, above the bound: so no valid entry begins with the
mark, and a walk never takes an entry for a batch header (3.5, rule 2) or
reads past an entry to rule one out. The bound also leaves the index after
every entry representable. An entry whose fixed fields declare more is not
valid, and nothing is read for its payload: the fixed fields are checked
before anything else is read. An entry written under a larger
`Config::max_payload` than the current one stays readable within the
format's bound. The bound holds wherever an index enters the log: the
append refuses an index past it, a segment header or batch header that
names one is not valid, and an open refuses a start or a marker past it.

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

**Anchored walks.** A walk that begins before the entry the log was
opened from (the start, a checkpoint's entry) reaches it by the framing
alone: an entry's fixed fields (its index must be the one expected, its
length within the batch), a batch header's start and its own hash, the
lengths of entries and batches; not the chain, since nothing before the
start is needed after it. A batch whose last entry comes before the start
is crossed by its header when its entries do not frame or its framing does
not end where the header says: the header, checked by its own hash, says
where the batch ends, the hash of its last entry and so the index the next
batch must name, which is all the walk needs from it (the term of that last
entry it does not say; the next entry's bytes, or the next segment's header,
which must continue from that hash, give it). Inside the start's own batch
the entries are framed one by one, since the start's end must be placed.
The start's entry takes the start's hash and term, whatever its bytes say,
and the entry after it is checked by the chain from that hash: a batch that
begins there names the start's index and hash, which anchors it without
the start's own bytes. On the way, the entries before the start are still
checked by the chain from the segment's header, through each batch header,
as far as it holds; where it stops is noted (an entry that does not frame
in a batch crossed by its header, too), and a read the device fails of an
entry's bytes past its fixed fields is noted the same way rather than
ending the walk. The open, a read and a cut walk so; so does accepting a
loss (6.2), which refuses when the framing breaks before the start where
no header crosses it (a batch's mark or header, an entry in the start's
own batch), so that where the start's successor begins cannot be
established (`LogError::FramingBroken`).

**Reading** (`Log::read`) walks from the segment that holds the first
entry asked for, through the entries before it in that segment (anchored
at the start when the entry asked for comes after it), and returns the
durable entries from it on; a walk that ends before the durable entry is
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
3. Entries up to the firm position are taken one by one: each after the
   start must be whole and chained. A walk that ends before the marker's
   index, or before the start's, is damage. Entries before the start in
   the first segment walked are read through by their framing alone, since
   the log keeps no index of where entries are (3.5, "Anchored walks"):
   the checkpoint covers them, and damage to them does not stop the open
   unless it breaks the framing. They are still checked by the chain from
   that segment's header, and the first place it stops holding, bytes that
   do not check or a read the device fails past an entry's fixed fields,
   is reported (`Opened::before_start`: the segment, the offset, the first
   entry from which it does not hold, and whether the read failed; and
   `Opened::walked_before_start`, the segment so walked): an open
   from a checkpoint before that place could not replay past it. Nothing
   is given up for it. Segments listed wholly before that segment are not
   read at open beyond their headers (step 1); damage in them is found when
   they are read, or by the background check of server-storage.md 6.12
   ("Slow damage"), which is not built.
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
and the folder as if it had made it. A failed flush stops the log. A
segment number whose run `state` could not list (the run's end past the
largest number, 1.2) is refused before anything is made, so that no batch
is written into a segment `state` would then refuse. A crash at any point
leaves an unlisted file, which the next open removes.

### 3.8 A batch flush

`Log::flush_batch` writes the open batch (3.3), every batch carrying the
current cut count:

1. the entries that fit in the current segment, as one batch, with one
   synchronous write after the last batch there;
2. the rest, if any, as one synchronous write at offset 0 of the spare:
   its header (first index: the batch's first entry there; previous hash
   and term: the entry before) followed by the second batch;
3. one `state` update: the marker (the last entry's index and hash, and the
   committed position, which never goes down) and, when step 2 ran, the
   run with the spare added. The marker alone is a marker advance (one
   write and one flush of `state`); with the run, both slots.

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

**The committed position is bounded by the log's own entries.** What a
caller gives is the last entry of this log known to be committed: on a
single server the last entry of the flush; on a cluster the lower of the
leader's committed position and the last entry of this log known to match
the leader's (Raft's rule for a follower's commit index). `flush_batch`
refuses one past the last entry it makes durable, and `set_committed` one
past the durable entry, with nothing written and the log going on. The
bound matters because a cut is refused at or before the committed
position: a follower that recorded its leader's committed position while
its own log held stale entries from before it (never committed, from an
old term) would refuse, for good, the cut that removes them; and a
position past the log's end would protect entries the log does not hold
while letting a cut after it remove what lies between. So every `state`
update keeps the committed position at or before the marker's index, when
the copy it starts from does: a flush and `set_committed` by the bound; a
cut lowers the marker only to the entry before an index past the
committed position; an open, giving up and retiring move the marker
forward or leave it; the first open of a version 1 copy writes its values
unchanged. A copy this log did not write may hold a committed position
past its marker: the open keeps it as it is, nothing repairs it, and the
cut guard protects every entry through it. With the durable entry at 6
and a recorded committed position of 7, `set_committed(6)` confirms and 7
stays; a cut from 7 or before is refused, and a cut from 8 finds nothing
to remove.

### 3.9 A cut

`Log::cut` removes the entries from an index on (server-storage.md 7.1).

**What it refuses**, with nothing changed: every cut until the caller has
given a committed position since the log was opened
(`LogError::CommittedUnconfirmed`), because the one the marker recorded
can be one advance behind (1.5); then an index at or before the committed
position recorded (3.8), at or before the entry the log was opened from,
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
new end carries an older cut count, and appends go on from the new end. A
cut from the entry after the durable one gives up what may follow the
place, as below; it is judged by the same refusals as any cut, and once a
committed position is confirmed it always passes them (the committed
position is at or before the durable entry, 3.8). A cut from an index
further on removes nothing, as on any log that holds nothing there, and
the log still ends unreadable: a cut never removes anything before its
index.

**After a cut of durable entries that ends the log before a place that
failed to read** (whether the place was given up behind a fresh segment,
which the cut drops, or the cut was the first thing done about it), what
is certain is that nothing written before that cut is part of the log
again: it carries an older cut count. The place itself is not settled.
Appends continue in that segment from the cut. When it has room for the
next batch, batches are written over the place in time, with synchronous
writes like every batch; when it has too little, the writer starts the
next segment (3.3), and walks move to it before they reach the old range
(3.5, rule 1). Wherever a later walk still needs bytes at the place, an
open that cannot read them at or before the marker fails
`LogError::Unreadable`, as for any place, and one whose end falls on them
ends unreadable again, and it is given up again.

**Giving up an unreadable end** (`Log::give_up_unreadable_end`, and a cut
from the entry after the durable one of a log that ends at an unreadable
place) discards whatever may follow the place, for good, whatever an open
can read there later: it carries an older cut count. Raising the cut count
alone would not do: the next open
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

Appends then go to the fresh segment, and walks read nothing more of the
segment the log ended in past the last verified entry, until a later cut
of durable entries drops the fresh segment and takes the log back into
that segment (above, "After a cut of durable entries"). The operation is
refused, with nothing written, when the log does not end at
an unreadable place, when the entry the caller names (index and hash) is
not the last one the open verified, and when `state` lists segments after
the one the log ends in. No write of this log leaves that. Every `state`
update keeps the marker's entry in the last listed segment, or just before
its first entry: a batch flush lists a segment only with the marker in it
(3.8); a cut keeps the run to the segment that holds the place it cuts at;
an open moves the marker to the entry it ends at and keeps the run to the
segment that holds that end; retiring never drops the last segment (3.10);
giving up lists one that begins right after the marker. A lost copy of
`state` goes back to an older copy, made under the same rule. So a walk is
in the last listed segment once it has passed the marker, and an open can
end unreadable only there. A disk too full for the segment refuses it as it
refuses an append (`LogError::NoSpace`, 3.7). It needs no committed
position: it removes nothing at or before the durable entry
(server-storage.md 6.3).

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
  nothing to `state`; it stops the log when the call had already written
  to a segment (a batch flush, giving up), since what reached the segment
  is then not recorded and the log's memory no longer matches it. Calls
  that write nothing before their update (`Log::set_committed`, a flush of
  an empty batch, `Log::update_state`, a cut, a refused `Log::prepare`) go
  on after a refusal. A segment number `state` could not list is refused
  by `Log::prepare` before anything is written (3.7); a copy number at its
  limit is refused only at the update.
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

## 4. The page file

`<store>/pages` (server-storage.md sections 6.4 to 6.8), read and
written by `om_store::pagefile`. Its first 16 KiB are four superblock
slots of 4,096 bytes; page `n` (from 1) is the 16,384 bytes at byte
`n * 16384`. The pages of the trees (section 2), the meta streams of
checkpoints (4.3) and free pages share the same numbers.

### 4.1 Versions

What the page file holds is a line of **versions**, each a pair (entry,
step), ordered by entry, then step:

- the changes an entry makes are version `(entry, 0)`: every page a tree
  writes for the entry is born at it;
- the pages a checkpoint writes for itself (its meta stream) are born at
  `(entry, number)`, where `entry` is the last entry it covers and
  `number` the checkpoint's number. A checkpoint **is** version `(entry,
  number)`: its trees, and its own meta stream.

A page leaves the newest version when a tree releases it (or the subtree
it was in) for an entry `E`: it **dies** at `(E, 0)`. Between its birth
and its death every version reaches it; no version at or after its
death does. Two checkpoints at the same entry are told apart by their
steps, and a checkpoint's meta stream is reached by that checkpoint's
version alone (the next checkpoint's version is at or after the meta
stream's death).

### 4.2 A superblock copy

Each copy is written at the start of its slot; the rest of the slot is
not read. Slots 0 and 2 form one pair, slots 1 and 3 the other: a
checkpoint's two copies are in one pair, 8 KiB apart, so that a run of
damaged bytes shorter than 4 KiB reaches at most one copy of each
checkpoint.

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | `OMPAGES` followed by a zero byte |
| 8 | 2 | superblock format version, 1 |
| 10 | 2 | zero |
| 12 | 4 | body length `L`, 96 in version 1 |
| 16 | 8 | the checkpoint's number |
| 24 | `L` | body |
| 24 + `L` | 32 | SHA-256 of bytes 0 to 24 + `L` |

The body, version 1:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | the index of the last entry the checkpoint covers |
| 8 | 8 | that entry's term |
| 16 | 32 | that entry's hash |
| 48 | 1 | the tree format version of its trees (section 2) |
| 49 | 7 | zero |
| 56 | 8 | the number of the first page of its meta stream |
| 64 | 32 | that page's hash (section 2.5) |

A copy is **valid** when the mark, the length and the hash check, the
zero fields are zero, and neither the number nor the meta page is 0. A
valid copy of a newer superblock or tree format version makes the open
fail rather than pass over it. A copy whose first 8 bytes are zero is a
slot never written, or cleared.

A checkpoint's **number** is one more than the highest number of a valid
copy the last open saw, or of the last checkpoint written since. A number
whose copies never reached the disk can be used again; an open overwrites
every other copy that could hold it first (4.6), and a checkpoint is
identified, where it matters, by its number and its meta stream's first
page together (4.4, the predecessor). A valid copy at the highest number
there is (2^64 - 1) makes the open fail before it writes anything: no
checkpoint could follow it.

### 4.3 The meta stream's pages

A checkpoint's meta stream (4.4) is a byte stream kept in pages of the
overflow kind (section 2.4), each stamped with the checkpoint's number:

- **data pages** hold the stream, 16,344 bytes each except the last;
- **directory pages** each hold, as their data, a reference to the next
  directory page (its number and hash, 8 + 32 bytes; all zero for the
  last) and then 1 to 407 references to data pages, in order.

The superblock names the first directory page. Each page is read with
the hash that names it (section 2.5), so every byte of the stream is
checked before it is used; a stream longer than the file's pages, one
that names a page twice, or a directory page of another kind or length,
is malformed, so a stream can never take more memory than the pages it
occupies. The stream is written whole at every checkpoint: a checkpoint
shares no meta page with another.

### 4.4 The meta stream, version 1

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | `OMMETA` followed by two zero bytes |
| 8 | 2 | meta format version, 1 |
| 10 | 6 | zero |
| 16 | 8 | the checkpoint's number |
| 24 | 48 | the entry: index, term (8 each) and hash (32) |
| 72 | 1 | the tree format version |
| 73 | 7 | zero |
| 80 | 8 | the predecessor's number: the newest checkpoint on disk when this one was written; 0 for none |
| 88 | 16 | the predecessor's version: entry and step (8 each); zero for none |
| 104 | 40 | the predecessor's meta stream: its first page's number (8) and hash (32); zero for none. With the number, this identifies the predecessor at open |
| 144 | 8 | `end`: the file's pages, page 0 counted; pages `1..end` exist |

Then, in order:

1. **The trees**: a count (4 bytes), then for each tree in the order of
   its name's bytes, each name once: the name's length (1 byte, 1 to
   255), the name, and the root's page number (8) and hash (32). The page
   file gives names no meaning; the engine names a tree per hosted file
   and one for the rest of the state (server-storage.md 6.2). At most
   65,536 trees: an open reads and checks every root, one 16 KiB page at
   a time, and holds the table (at most 19 MiB) in memory, so a store at
   the limit opens with any cache size.
2. **The restore points listed**: a count (4 bytes), then for each: the
   name's length (1 byte, 1 to 64), the name (UTF-8), the checkpoint's
   number (8), its entry (48), its version (16), and its meta stream's
   first page, number (8) and hash (32). A checkpoint kept as a restore
   point lists itself with page 0 and a zero hash: its own stream is the
   one being read. At most 65,536.
3. **The restore points retiring**: the same layout. These are the
   restore points the predecessor lists and this checkpoint does not,
   told apart by their versions, never by their names (a name dropped
   and kept again names a new restore point, and the old one retires by
   this rule); they are kept while the predecessor is on disk.
4. **The free pages**: a length (8 bytes), which is `ceil(end / 8)`, and
   a bitmap: bit `n % 8` of byte `n / 8` is set when page `n` is free.
   Page 0 and bits past `end` are clear.
5. **The dying pages**: a count (8 bytes), then 81 bytes each, in page
   order:

   | Offset | Size | Field |
   |---:|---:|---|
   | 0 | 8 | the page's number |
   | 8 | 32 | its hash |
   | 40 | 16 | its death: entry and step |
   | 56 | 1 | bit 0: its birth is known; bit 1: it was dropped with what it refers to, which has not been taken down yet; other bits zero |
   | 57 | 16 | its birth, when known; zero otherwise |
   | 73 | 8 | its hold: the number of the first checkpoint whose map records it settled, when an older map on disk records it unsettled; 0 for none (4.5) |

6. **The shared pages**: a count (8 bytes), then 28 bytes each, in page
   order: the page's number (8), how many trees reach it (4, at least 1),
   and the latest version (16) at which a tree stopped reaching it.

The stream ends there. Every count is checked against the bytes left
before anything is allocated for it; a dying page born at or after its
death, a dying or shared page that is free or outside `1..end`, records
out of order, or a root, meta page or restore point outside `1..end` make
the stream malformed. Every page the meta stream occupies must be in use
in its own map, and so must every tree root.

**The map** (items 4 to 6) is the state of the file's pages at the
checkpoint's version. A page that is neither free nor dying is in use: a
page of the checkpoint's trees, of its meta stream, of an older
checkpoint's meta stream that is still dying, or reached by a dying page
that has not been taken down yet. Pages that units of later entries had
taken when the checkpoint was written are recorded free; the map is
rebuilt for them when those entries are applied again (4.7).

### 4.5 The map's rules

- **Reuse.** A dying page becomes free when it is **settled** (its birth
  is known and, if it was dropped, its references taken down), no **kept
  version** lies in `[birth, death)`, and its hold is at most the lowest
  number of a checkpoint on disk. The kept versions are the two
  checkpoints on disk (and the one being written), every restore point a
  superblock on disk lists (a checkpoint's own list and, while its
  predecessor is on disk, the retiring list), and the pins. A pin is of
  the trees at an entry, `(entry, 0)`, and only of a version the page
  file has: the newest (its entry is the last entry whose unit
  committed, or the newest checkpoint's when that is later), a
  checkpoint or restore point on disk, or a version another pin keeps. A
  pin of any other entry is refused: an entry that changed nothing has
  no version of its own, and the page file cannot tell it from an entry
  whose unit has not committed yet, whose commit could free the pages
  such a pin had been handed. A reader of such an entry pins the newest
  version (`PageFile::pin_newest`, which reports the entry it pinned).
  A pin keeps no meta page (birth step above 0), so it takes the roots
  of its version as it is taken and keeps them: from memory for the
  newest version, the newest checkpoint's and a version another pin
  holds; from the meta stream of an older checkpoint or a restore
  point, read while the I/O lock is held, which a reclaim, a checkpoint
  and a restore point's drop hold too, so that none of them frees,
  reuses or retires those pages first. Two pins of one entry are of one
  version and carry the same roots; a pin whose roots cannot be had is
  refused: the version a checkpoint is writing, or a meta stream that
  does not read back (with the page's fault, so a read the device failed
  is told from bytes that do not check) or that breaks the format or the
  open's rule on shared roots. A birth not known yet counts as the earliest.
- **Settling** needs the page's bytes: its birth is its stamp (section
  2.2), and an index page's children carry theirs; a dropped page's
  references are what `Page::references` lists. The bytes are at hand
  for a page not written yet; otherwise they are read, a page at a time,
  by `PageFile::reclaim`, which takes at most a given number of dying
  pages per call, and by a reservation or a checkpoint short of free
  pages. A reclaim reads a dying page from the file, never from the
  cache, so whether its read fails does not depend on what readers
  cached; a read that fails leaves the page dying and unsettled, and
  what it refers to untouched, is reported, and is met again by the next
  pass, which goes on past it to the other pages. (A read the trees make
  while applying an entry may still be served from the cache, and so
  miss a read error a cold read would meet; that makes the apply fail or
  not, never the map different.)
- **Holds.** A map on disk can record a page unsettled, or in use below a
  dropped page it records unsettled. Opening that checkpoint will need
  the page's bytes again, so when the running process settles such a page
  it sets the hold to the next checkpoint's number, and the page is not
  reused until every checkpoint on disk is at least that one: two
  checkpoints later. The pages a held page refers to are held the same
  way when they settle in turn: such a map records them in use below it.
- **Shares** (section 6.8, Rodeh's method). A page reached by more than
  one tree has a count; a page without a record is reached by one. A
  clone raises its root's count. A unit that copies a page whose count is
  above 1 (counting what the unit's earlier releases raise) reads the
  page and raises the count of everything it refers to, since the copy
  refers to it too, and lowers the page's own. A page whose count reaches
  0 dies, at the latest version at which a tree stopped reaching it; if
  it was dropped, what it refers to loses one count each when it is
  taken down, and dies the same way at 0.
- **A function of the changes.** The map after a sequence of calls from a
  checkpoint is a function of the map the checkpoint names and the calls:
  the units committed (in order), the reservations, the checkpoints, the
  pins and the reclaims. It takes nothing from a clock, randomness, the
  order of a hash map, or the page cache: a commit and a reclaim learn
  births and references only from pages not written yet and from the
  pages a reclaim examines (whose bytes are the same whether they were
  cached or read), and pages are taken lowest number first. So applying
  the same entries twice from the same checkpoint, with the same
  checkpoints, pins and reclaims between them, gives the same map, byte
  for byte, and the same page numbers. Recovery applies the entries
  after the checkpoint with the pins gone; the first run's pins and
  reclaims may have differed, and the map recovery rebuilds is then
  another correct one for the same trees.

### 4.6 Writing and reading

- **Creation.** Checkpoint 1, at entry 0 with no trees, is written to
  `pages.new` (its superblock copies in slots 0 and 2, its meta stream in
  pages 1 and 2), which is flushed, renamed to `pages`, and the store
  folder flushed. So `pages` exists only whole. A `pages.new` left by a
  creation that did not finish is removed first.
- **The callers' contract.** One caller applies entries, in order, one
  entry at a time: it reserves the entry, runs the entry's one unit of
  work (every tree's pages and releases, the trees made, dropped and
  renamed, and each changed tree's new root, committed once), and is done
  with the entry before it starts the next; it may reserve later entries
  ahead. Another caller checkpoints and reclaims; readers come at any
  time. The page file refuses what falls outside this, and makes each
  check in the same hold of its lock as the change the check guards:
  1. an entry has one open reservation at a time: a second, while the
     first is open, is refused (one after the first is dropped is not);
  2. an entry has one unit of work, and one unit is open at a time: a
     unit while another is open is refused, and so is one for an entry
     that has committed one (or one before it); a unit dropped, or whose
     commit was refused, may be begun again;
  3. a reservation is checked, its pages promised and it is opened in one
     hold, so a checkpoint sees it open or it sees the checkpoint's entry
     fixed;
  4. a checkpoint at `E` fixes `E` at its first check, in that check's
     hold: it is refused while a reservation for an entry at or before
     `E` is open, and from then until it is installed or fails, a
     reservation, unit or commit at or before `E` is refused; a unit of a
     later entry committed before it plans refuses it. So it covers every
     unit committed at or before `E` and none after. A refusal, not a
     wait: the two callers may be one thread, and a covered unit arriving
     late is a defect of the caller to be reported, not a race to be
     hidden;
  5. every way out of a checkpoint clears what it fixed, an unwind
     included; a checkpoint left by a panic makes the page file refuse
     every later write until it is opened again, since its plan may have
     changed the map in memory;
  6. the page file holds each tree's name and root, and only a unit's
     commit changes them. A root a unit names, for a tree it makes or
     gives a new root, must be a tree's page (not an overflow page) the
     unit wrote, and a tree's root is given at most once in a unit; a
     tree given a new root must have had its old root released by the
     unit (either way), and a page the unit releases that is a tree's
     root must be the old root of a tree it gives a new one; a tree
     dropped is one that existed when the unit began and that it gave no
     root, and the page file releases the root it holds for it; a page
     released that is still in memory must have the hash it is released
     with; no page the unit writes refers to a page that is a tree's root
     once the unit takes effect, beyond the share the map counts for it.
     A unit that breaks any of this is refused whole;
  7. the restore point a checkpoint keeps counts in the floor (4.8) from
     its first check, and a checkpoint that keeps one is refused while
     any reservation is open.

  Since a unit of a later entry committed between a checkpoint's first
  check and its plan refuses the checkpoint (4), and a keep is refused
  while any reservation is open (7), the caller that checkpoints has the
  caller that applies take no new entry for the moment of a checkpoint's
  first check and plan, and holds no reservation open when it keeps a
  restore point; otherwise both refusals repeat for as long as the
  applier does not pause.
- **A checkpoint** of the newest version, at entry `E` (every unit
  committed so far is of `E` or earlier), of the trees as the page file
  holds them: it takes no roots from its caller. Its copies replace the
  older checkpoint's, so the newest must be usable first, or both
  checkpoints left on disk would be refused at open. So before it reads
  or writes anything it refuses a root the reader would refuse: one not
  in use in the newest version (free, dying, or a page of the newest
  checkpoint's meta stream), and one page named by more trees than the
  map counts sharing it. A unit cannot leave either, but a table #719
  wrote from roots its caller gave can (a tree's root that is a page
  inside another tree passes the open, which would have to walk every
  tree to see it, and dies when that tree is dropped and taken down).
  Such a table can also leave a page of the newest checkpoint's meta
  stream dying ("An open", below), which refuses the checkpoint too,
  before the plan would mark that page dying again. Then it reads back from the file every tree's root that is not in
  memory, and checks it, and is refused, naming the tree, when one does
  not read back. The writer keeps every limit the reader enforces
  (trees, restore points, names, numbers), and the meta stream goes
  through the reader's own decoding before any byte of it is written.
  Short of free pages for its meta stream, it reclaims first.
  1. the pages the roots reach that are only in memory are found (a page
     on disk never refers to one in memory); a page in memory of the
     newest version that no root reaches (a unit wrote a page no tree
     names) refuses the checkpoint, with nothing written;
  2. the dying pages in memory that were dropped are taken down (after a
     restart only the disk is left to read them from), the newest
     checkpoint's meta pages die at the new version, and the meta stream
     is laid out in pages taken from the free ones (growing the file if
     it must, drawing on the reserve; 4.8);
  3. the tree pages and the meta pages are written in page order, in
     runs of up to 64 pages, and the file is flushed;
  4. the superblock copy is written into the first slot of the pair the
     newest checkpoint does not hold, and flushed;
  5. then into the second slot of that pair, and flushed. The checkpoint
     that pair held is retired, and so is every restore point only it
     listed.

  A crash before step 4 leaves both checkpoints on disk as they were:
  step 3 wrote only pages that neither reaches nor their maps need
  (4.5). A crash during step 4 leaves the first slot holding the older
  checkpoint's copy, a torn copy, or the new one; the second slot holds
  the older checkpoint's other copy. A crash during step 5 leaves the new
  checkpoint's first copy whole, with its pages flushed before it. A
  write or flush that fails makes the page file refuse every later write
  until it is opened again (section 6.12); a refusal writes nothing.
- **An open** (section 6.7, step 2) flushes the store folder and reads
  the four slots one at a time; a slot the device fails to read is passed
  over. Of the two highest numbers with a valid copy, newest first, it
  takes the first checkpoint whose meta stream reads back, decodes, and
  agrees with its superblock (number, entry, tree format), and whose
  tree roots read back with the hashes it names, are not overflow pages,
  were written at or before its entry, and are in use in its map, no
  root page named by more trees than its map counts sharing it (a clone
  shares its root; a table #719 wrote from roots its caller gave could
  name one page for two trees with no share, and dropping one of them
  would mark the page dying while the other still names it). It does not
  look for a root that is a page inside another tree, which a table #719
  wrote could hold too: that would take a walk of every tree. A unit
  cannot make one (rule 6). In such a table, dropping the tree whose
  root is the inner page frees that page while the other tree still
  reaches it; once the page is taken again, the other tree's reads
  under it fail their hash check (reported as damaged or stale, never
  returned as good), and if the other tree is then dropped too, the map
  takes the page down by its number under the old hash: whatever now
  holds it is marked dying and never freed (its bytes never check), a
  unit releasing it is refused, and a checkpoint is refused when it is a
  root or a page of the newest meta stream. Dropping the outer tree
  first leaves the inner tree's root dying and refuses every later
  checkpoint. A
  device read error or bytes that do not check pass the checkpoint over,
  with the reason; any other failure of a read (the process stopping)
  fails the open. Then, before it returns:
  1. the checkpoint's copy is written into both slots of its pair, each
     flushed: the device may not hold a copy the system's cache showed
     (an earlier process's failed write or flush), and a crash during a
     checkpoint's step 5 leaves an older copy in the second slot;
  2. in the other pair, if a slot holds a valid copy of the predecessor
     its meta stream names, that copy is written into the other slot of
     the pair when that slot does not hold it already, and flushed;
     otherwise each slot that is not blank is cleared (zeros over a
     copy's length), and flushed. Either way a slot that held anything else (a newer
     checkpoint passed over, whose pages the store will now reuse; a
     stale copy; bytes that could not be read) is overwritten before any
     page is: such a checkpoint must never be taken later;
  3. if the file is shorter than the map's `end`, it is extended to it
     and flushed.

  The predecessor is the checkpoint whose number and meta stream the
  chosen one names, and is kept only when a copy of it was found. The
  open frees nothing. Applying the entries after the checkpoint again
  (section 6.7, step 4) then frees what normal running frees (4.8).
- **Reading a page** returns it from memory when no checkpoint has
  written it yet, from the cache when the cache holds that number with
  that hash, and otherwise reads the 16 KiB at its place and checks them
  with `Page::verify` against the hash its parent names. A number 0 or at
  or past `end` is missing; a read that fails, or a file that ends inside
  the page, is reported as unreadable (with the bytes read); bytes that
  do not check are reported as damaged, misplaced, stale or malformed
  (section 2.5). Nothing that does not check is ever returned.

### 4.7 What a crash, a fault and a restart leave

- **The two checkpoints on disk are whole**, and so is every restore
  point a superblock on disk lists: a page is reused only when no kept
  version reaches it (4.5), and a checkpoint's own meta stream is kept by
  its version. When the newest checkpoint's superblock or roots cannot be
  checked, the open takes the one before it, whose pages and whose log
  (section 6.7, "What is kept") are all there.
- **Pins are gone after a restart**; what the superblocks on disk name is
  what is kept. The pages a pin kept are free or dying in the maps on
  disk, and are reused once no kept version reaches them.
- **The map is never applied twice.** The map on disk is the one of its
  checkpoint's version; the changes of later entries are in memory only,
  and are made again, once, when those entries are applied after the
  restart. A page those entries had taken is free in the map on disk,
  and a page they had released is in use there.
- **A page a failed checkpoint wrote** was free in both checkpoints on
  disk and needed by neither map (4.5), so a crash or a failed flush at
  any step costs at most that checkpoint.
- **A device that drops or misplaces a write it reported done** can leave
  a checkpoint whose superblock is valid and whose deeper pages are not
  what it names. Those pages fail their checks when read (section 6.12:
  page damage, found when a page is read), and the open does not go back
  to an older checkpoint for them.

### 4.8 Space

- **The file grows** in steps of at least 256 pages (4 MiB, configured),
  or an eighth of its size when that is more, up to 65,536 pages (1 GiB)
  a step, through `File::allocate`, so that the space is taken when the
  file grows and a later write into it cannot fail for lack of space.
- **Reservations.** Before an entry is appended, the caller reserves the
  pages applying it can take (section 6.5). A reservation succeeds when
  the free pages, less those already promised to other reservations,
  cover it and the **floor**; short of them, it first reclaims (one pass
  over the dying pages at most), then grows the file. The floor is room
  for two checkpoints' meta streams of the largest size the next
  checkpoints can need: the trees that exist now, the restore points of
  the newest checkpoint together with those listed for the next (a
  dropped point is still on disk, so a drop does not lower the floor,
  and a reopen, which lists them again, does not raise it) and one more,
  and a map in which every page of the file is dying (81 bytes a page,
  more than a shared page's record or a free bit). Two, because the next
  checkpoint retires nothing when the other pair is empty, and the one
  after frees the first's meta stream only once it is on disk. From a
  checkpoint's first check until it is installed or fails, the point it
  keeps counts too. Recovery needs this: a reopened page file never
  judges an entry against a higher floor than the one the running page
  file judged that entry's reservation against. A reopen takes a
  checkpoint `C` on disk and judges the entries after `C`'s entry. Each
  such entry's reservation was judged either while `C` was the newest
  checkpoint (`C`'s points are among those counted, and drops since do
  not lower the count), or while `C` was being written, from its first
  check on (its kept point counted then), or before `C`'s first check:
  then the reservation was open at that check, so `C` kept no point (a
  keep is refused while a reservation is open) and lists only points
  counted already. Of the rest of what the running page file knows and a
  reopened one does not, pins and the reclaim cursor do not enter the
  floor, and the map's end, which the running file may have grown past
  the checkpoint's, only raises the running floor; the trees differ only
  by those made and renamed by the entries in between, whose room their
  reservations moved into the floor (below). The cases this does not
  cover are under "Recovery fits". Commits, the taking down of dropped
  subtrees and reclaims never make a map larger than one in which every
  page is dying, so the room stays enough whatever is applied meanwhile;
  it is about 1% of the file. An ordinary entry also leaves the
  **reserve** (256 pages by default), which recovery, a follower's
  entries and the discard of a group may take. Making a tree takes its
  root page and a fixed 4 pages of room for its record in the tables
  (one more record adds at most a data page and a directory page to each
  of the two meta streams), 5 pages to reserve; a rename takes 4. The
  charge is fixed so that an entry takes the same pages each time it is
  applied (`Reservation::taken` counts it). When the file cannot grow
  enough, the reservation is refused with "full" and nothing is
  reserved: the store keeps reading and can checkpoint.
- **Recovery fits, within what is proved here.** Applying the entries
  after a checkpoint again frees exactly what normal running frees: a
  page that leaves the newest version is freed at once when it never
  reached the disk and no kept version reaches it, and the rest when a
  reclaim, a reservation or a checkpoint settles it. When the replay
  takes no checkpoint the first run did not, the versions kept during
  recovery are a subset of those the first run kept at the same point
  (the pins are gone; the checkpoints and restore points are those of
  the superblocks on disk, which the first run kept too), so no page
  recovery holds was free in the first run, and the file the first run
  grew is still there. So, for the caller 4.6 describes, when the store
  is reopened on the newest checkpoint on disk or on the one being
  written, each entry was reserved once (not extended), and the replay
  takes no checkpoint the first run did not take, recovery, reserving
  exactly the pages each entry took (`Reservation::taken`) against a
  floor no higher than the one the first run judged it against (above),
  never needs more free pages than the first run had. Three cases are
  not covered ([#734](https://github.com/OpenMaker-Dev/openmaker/issues/734));
  recovery may take the reserve (256 pages by default) for them, and
  beyond it a replay is refused as full:
  - a reopen that falls back to the older checkpoint because the newest
    cannot be read: the older one can list restore points the newest
    retired, which the running floor no longer counted;
  - an entry that was extended (`Reservation::extend`): the replay
    reserves the entry's whole count at its start, while the first run
    was judged for the extension later, after the entry's own units had
    freed pages;
  - a replay that the budget of pages waiting for a checkpoint forces to
    checkpoint where the first run did not (the first run reserved
    entries ahead, before the pages that fill the budget were written):
    that checkpoint keeps pages the first run freed at once.
- **The configuration** is checked when the page file is created or
  opened: a growth step of 1 to 65,536 pages, a budget of at least one
  page, a reserve of at most 2^32 pages, so that no sum the page file
  makes with them can overflow.
- **Failures latch.** After a failed write or flush, and after an
  allocation that failed other than for a full disk (an allocation can
  write), every later write, growth and checkpoint is refused until the
  page file is opened again. A full disk refuses a growth and leaves the
  page file as it was.
- **What an entry may take.** `pagefile::entry_bound(edits, height)` is
  an upper bound for tree format version 1: a put at depth `d` is at
  most `d` sets and one range delete, a delete one range delete; at each
  level a set copies a page and splits off at most one part, and a range
  delete reaches at most the two pages at its ends, a sibling for each to
  join with, and a part each join splits off; the levels are the tree's
  height plus at most `1 + log2(sets)` new ones; leaf splits and joins add
  the overflow pages of a new separator, and new items theirs. The bound
  is loose, and looser for larger entries: one put of a node at depth 1
  into a tree of height 3 is bounded by 45 pages, one at depth 3 by 82,
  and either writes 3; a thousand puts at depth 3 are bounded by about
  200,000 pages. The exact count is a
  function of the tree and the edits (section 2.7), so a reservation
  reports the pages its units took (`Reservation::taken`): an entry can
  carry it, and recovery and followers reserve exactly that.
  `Reservation::extend` adds to a reservation whose units ran out (the
  apply that ran out changed nothing).
- **Pages waiting for a checkpoint** have a budget (16,384 pages, 256 MiB,
  by default): once as many pages of the newest version wait in memory,
  a reservation is refused with "checkpoint needed" until a checkpoint
  writes them. Pages that left the newest version and stay in memory for
  a pin do not count, and the entries reserved for before the budget was
  reached can take memory past it by what they write, and by what they
  add with `Reservation::extend`, which the budget does not judge (an
  entry that has committed a unit could otherwise not finish: no
  checkpoint can be taken while it is half applied).

### 4.9 Limits and unknowns

- Each checkpoint writes its whole meta stream: the free bitmap (one bit
  per page: 128 KiB for 16 GiB of pages) and every dying and shared page
  (81 and 28 bytes each). A restore point that keeps many pages keeps
  their records in every checkpoint's map while it is listed.
- The trees' roots are a table in the meta stream, not a tree: a
  checkpoint rewrites about 50 bytes and the name per tree, and an open
  reads every root.
- The floor (4.8) keeps about 1% of the file free for checkpoints.
- What an open and a checkpoint hold in memory grows with the trees and
  restore points: at an open, the meta stream and its decoded tables, the
  set of tree names, and two copies of the restore points (the newest
  checkpoint's lists and the next one's); at a checkpoint, a copy of the
  tree table, the set of root numbers, the restore point lists, the
  encoded stream, its decoding for the reader's checks and its pages. A
  tree's record takes 41 bytes and its name (at most 255), a restore
  point's 113 and its name (at most 64): at the limits, with the
  longest names, about 19 MiB of tree table and 11 MiB of restore points
  per list in a stream, somewhat more decoded. Estimated, not measured:
  an open then holds some 120 MiB at its peak and 80 MiB after, and a
  checkpoint some 250 MiB while it plans, beyond the map. With short
  names it is a few MiB: 65,536 trees named in 13 bytes opened in 23 MiB
  of process memory (measured).
- The page file holds no lock: `create` and `open` check that the caller
  holds the store folder's, and the caller keeps it while the page file
  is open.
- The superblock carries no store id, so a `pages` file from another
  store folder is not recognized as foreign.
- A device that damages a page after it is written is found only when
  the page is read; the scrubbing job of section 6.12 is not built.
- Whether a cluster's servers place pages alike does not matter and is
  not checked: maps are each server's own (section 7.5).

## 5. Entries and the state

What a log entry holds, where the state of server-storage.md section 6.2
lives in trees, and what applying an entry does, as `om_store::entry`
and `om_store::apply` implement them. `om_store::model` is the reference
model of server-storage.md section 12 (check 1): the same rules written a
second time over plain maps, sharing with `apply` only the decoding, the
hash and the error types.

### 5.1 An entry

The log (section 3.4) stores an entry's index, term, kind, file and
payload without interpreting them. Here they mean:

| Field | Meaning |
|---|---|
| index | its place in the log, counted from 1 |
| term | the leader's term; never below the previous entry's |
| kind | 1 change, 2 session, 3 file, 4 members, 5 mark, 6 empty. Any other code is refused |
| file | the **id** of the file the entry belongs to, or 0 for none. A file's id is the index of the `file` entry that added it, so ids are never reused, and every server gives a file the same id |
| payload | 5.2 |
| hash | SHA-256 over the previous entry's hash and the entry's bytes before the hash, as section 3.4 writes them: index, term and file (8 bytes each), kind (2), two zero bytes, the payload's length (4), the payload. Before entry 1 the previous hash is 32 zero bytes (`om_store::entry::entry_hash`) |

The file field must fit the kind, or the entry is refused (5.6):

| Kind | File field |
|---|---|
| change | a file the state has |
| session, members, empty | 0 |
| file, added | the entry's own index |
| file, removed or renamed | a file the state has |
| mark | 0, or a file the state has |

The log's format has one 8-byte field for the file; nothing else about a
file is in the entry's header.

### 5.2 Payloads

Integers are little-endian. Byte 0 of every payload except an `empty`
entry's is the **payload format version**, 1; another is refused. A
length is 2 or 4 bytes before the bytes it counts (`len2`, `len4`), a
count is 4 bytes before the items it counts, and a session is 32 bytes:
the server's id (16, the random id of its `state` file, section 1), the
start count (8) and the connection number (8).

**change** (kind 1):

| Field | Bytes |
|---|---|
| version | 1 |
| group | 8: the group the change is part of (server-storage.md 6.9), 0 for none. Reserved: anything else is refused as unsupported |
| op | 1: 0 for none, 1 for an operation, then its session (32) and sequence number (8) |
| reply | `len4`, at most 65,535 bytes; empty when there is no op |
| notices | count, at most 65,536; each `len4`, at most 65,535 bytes |
| lock edits | count, at most 1,048,576; each a tag (1), a session (32), then: tag 1 **set**: key `len2` (at most 16,384), user `len2` (at most 1,024), value `len4` (at most 65,535); tag 2 **remove**: key, user; tag 3 **release the file's locks**: nothing more |
| tree edits | count, at most 1,048,576; each a tag (1), a path, then for tags 2 and 3 a value `len4`. Tags: 1 put a list, 2 put a value, 3 put both, 4 delete. A path is its number of segments (1, at most 64), then each segment `len2` |

A tree edit is held to the tree's bounds (2.1) as it is decoded: at most
64 segments, an encoded path of at most 65,535 bytes, a value of at most
65,535 bytes, and no put at the empty path.

**session** (kind 2): version, then a tag: 1 **opened** and a session; 2
**closed** and a session; 3 **started**, a server's id (16) and its start
count (8).

**file** (kind 3): version, then a tag: 1 **added**, a name (`len2`, 1 to
1,024 bytes), flags (4: bit 0 read-only; any other bit is refused) and
page 0 (`len2`, at most 4,096 bytes, the size of an `.fmp12` page); 2
**removed**; 3 **renamed** and a name. Tag 4, **cloned**, is reserved for
server-storage.md section 6.8 and refused as unsupported.

**members** (kind 4): version, then a tag: 1 **tree format version** and
the version (1). Tag 2, the cluster's members (section 7.6), is reserved
and refused as unsupported.

**mark** (kind 5): version, a purpose (1: 1 restore point, 2 release, 3
comparison) and a name (`len2`, 1 to 1,024 bytes).

**empty** (kind 6): no bytes.

Nothing may follow the last field. Decoding checks every length against
its bound and against the bytes left before it takes anything, never
reserves memory by a count, and does not recurse; malformed bytes are an
error, never a panic. Each item decoded takes at least one byte of the
payload, so the memory a payload decodes to is a fixed multiple of its
size: at most about 28 times for edits that delete the root, about 12
times for paths of empty segments, and close to the payload's size for
values. Encoding is held to the same bounds, and what decodes encodes to
the same bytes. `Payload::size` checks each part's bound and returns the
size the parts would take, which can be past the payload's bound;
`Payload::encode` refuses a payload past it before it allocates
anything.

### 5.3 The state's trees

The state is a set of trees, each a key tree of section 2, named as the
page file keeps them, outside any tree:

| Tree | Name | Holds |
|---|---|---|
| the store's own | `store` | 5.3's table below |
| a file's | `file:` and the id in decimal | exactly the file's key tree, nothing else |

No item of any tree holds a page number, so a root hash depends on
content alone: servers that place pages differently have the same root
hashes at the same entry (server-storage.md 6.4).

The store's own tree, by path. Segments are bytes; numbers in segments
are big-endian, so that rows sort by them, and numbers in values are
little-endian.

| Path | Node |
|---|---|
| `[01]` | value: the last entry applied, its index (8), term (8) and hash (32). Absent before entry 1 |
| `[02]` | value: the tree format version (1). Absent means 1 |
| `[03, id]` | list: a file's row; `id` is 8 bytes |
| `[03, id, 01]` | value: the file's name |
| `[03, id, 02]` | value: its flags (4) |
| `[03, id, 03]` | value: its page 0 |
| `[04, name]` | value: the id (8) of the file with that name. Names are unique |
| `[05, server]` | value: the server's highest start count (8) and the highest connection number opened in that start (8); `server` is its 16-byte id |
| `[06, server, start connection]` | list: an open session, there exactly while it is open. The last segment is the session's start count and connection number, 8 bytes each |
| `[06, server, start connection, 01]` | value: its last completed sequence number (8), 0 for none |
| `[06, server, start connection, 02]` | value: that operation's reply, empty for none |
| `[07, server, start connection, file, lock]` | value: a lock's value. `lock` is the key's length (2, big-endian), the key, then the user |

A file's row is the list `[03, id]`, made by its first child, and so is a
session's. A session's sequence number and reply are leaves under its
list, not its value, so that a server's sessions are listed from their
lists without reading a reply (5.8). The lists above the rows (`[03]`,
`[06, server]`, `[07, server, session, file]` and the like) are made
when their first row is put and kept until the subtree above them is
deleted. So a session's lock list for a file stays,
empty, after its last lock there is removed, until the session closes or
the file is removed: at most one empty list per session and file it
locked in.

Locks are kept under their session first, so that closing a session is
the delete of two subtrees, its list `[06, server, session]` and its
locks `[07, server, session]`, and a server's start the delete of two,
`[06, server]` and `[07, server]`, whatever they hold. The layer above
judges conflicts by key; it builds its lookup by key from a walk of `[07]`
when it starts (`StoreTree::locks`) and keeps it current from the entries
it applies.

With the largest key and user, a lock's path is at most 34,911 bytes
encoded, within the 65,535 of 2.1.

### 5.4 Reserved

- In a change: the group field (5.2).
- In a `file` entry: tag 4, a clone. In a `members` entry: tag 2, the
  members.
- In the store's own tree: under a file's row, segments `04` (its
  container files, server-storage.md 8), `05` (its design history, 9),
  `06` (its kept original image, 6.10) and `07` (the file it was cloned
  from, 6.8); at the top, `08` (the members), `09` (blob reference
  counts, 8) and `0a` (open groups, 6.9).

A build that meets a reserved form in an entry refuses the entry (5.6).

### 5.5 Tree edits

A change carries a file's edits in order; each sees the tree the edits
before it left.

- **Put** a node at a path that is not empty: a **list** (no value), a
  **value**, or **both** (a list with a value). Whatever was at the path,
  and everything under it, is replaced by the node alone. Each missing
  ancestor is made as an empty list; an ancestor that holds a value
  becomes both, keeping its value. A put of the node that is there
  already, with nothing under it, changes nothing.
- **Delete** the node at a path and everything under it, however large,
  as one edit. Its parent stays, an empty list if it was its last child.
  Nothing there changes nothing. The empty path deletes every node; the
  root stays, a list.

These are the rules of `om_hbam::OwnedNode`, the file engine's tree:
`put(path, node)` with a node without children, `remove(path)`, and
clearing the root's children; the test `edits_leave_what_owned_node_holds`
(`crates/om-store/tests/entries.rs`) checks them against it. An
`OwnedNode` maps to a node as its wire form writes it: one with `push`
set or with children is a list, both when it has a value; otherwise it is
a value. Every change the file engine makes to a tree today is carried
by these two edits:

| The file engine (`crates/om-server/src/store.rs`) | Edits |
|---|---|
| `put(path, leaf)`: a record's cell, a last record id, a serial's next value, a modification count | put a value |
| `put(path, subtree)`: a record, the record-id list `[T, 0d]`, a field catalog `[T, 03]`, an index `[T, 01, F, I]`, a container library, a catalog member op13 uploads | put the node, then each node under it in depth-first order (each put makes nothing the next one removes) |
| `remove(path)`: a record, an index, a library, the deferred field list, a catalog member | delete |
| `ensure(path)` alone | put a list at the path when it is missing; put both, with its value, when the path holds a value; nothing otherwise |
| `ensure(path).set_child(leaf)`, `library::store` | put a value below the path (the put makes the ancestors) |
| changes in place through `at_mut`: `modcount::bump` and `library::adjust` set a leaf with `set_child`, and `adjust` removes a member it takes to zero | put a value; delete |
| children dropped in place (`children.retain`, `children = kept`: a record's cells, a table's global repetitions) | delete each one dropped |
| a node replaced in place (`*node = new`), or its children replaced (`entry.children = ...` for an index entry's record list) | delete what is gone and put what is new or changed, or put the node and everything under it |

The file engine refuses a put under a node that holds a value that is not
empty (`om_hbam::writer::holds_value`), but not under a leaf whose value
is empty: that leaf becomes a list that keeps its empty value, which is
both, with an empty value, here, as the put rule above makes it (the
`OwnedNode` test checks this case). A node that is both, whose value
changes while its children stay, takes a put of the node and a put of
each node under it; no path of the file engine found makes that change.

The lock table changes the same way. `LockTable::lock` and `upgrade` are
a **set** (a new lock, or its count or type changed in its value), with a
**remove** first when the lock's identity changes; `unlock` is a set of
the lower count or a remove; the release of a session's locks on one file
(op65, a logout) is a **release the file's locks**; a session's end is a
`session` entry that closes it.

### 5.6 Applying an entry

`om_store::apply::apply` takes the page store, which holds the state's
trees by name with their roots, and an entry, and returns the sessions
opened and closed and the payload (whose notices and reply, with the
operation's session and sequence number, the caller delivers when the
entry is published); the trees it changed are the store's to report
(5.7). What it costs is 5.8. It reads only the state and the entry:
no clock, no randomness, nothing of page numbers, caching or what else
is in memory. Every check below is made from reads of the state before
anything is written. The checks, in this order, the first that fails
being the one reported:

1. The entry is the next: its index is one more than the last applied
   entry's, its term is not below that entry's, and its hash is that
   entry's hash continued (5.1). Otherwise: *not next*, *term behind* or
   *chain*. A repeated, skipped or out-of-order entry is not next.
2. The state's tree format version is one this build knows (only 1; 0 is
   not a version). Otherwise: *unknown format version*.
3. The kind is known and the payload decodes (5.2). Otherwise: *payload*,
   which also covers what is reserved (5.4) and an edit beyond the
   bounds.
4. The file field fits the kind (5.1). Otherwise: *file field*, or *no
   such file* for a nonzero id the state does not have.
5. The kind's own checks, below.

Then, by kind:

- **change.** If it names an operation, its session must be open
  (*session not open*) and its sequence number one more than the
  session's last (*sequence*). Each lock edit, in order, must name an
  open session (*session not open*); a remove must find the lock held,
  counting this entry's earlier lock edits (*no such lock*). Then: the
  lock edits are made under the entry's file, the session's row records
  the sequence number and the reply, and the tree edits (5.5) are applied
  to the file's tree.
- **session.** *Opened* is applied only when the server's row records
  the session's start count and the session's connection number is above
  the highest recorded; then the session's row is made with sequence
  number 0 and the connection number recorded. *Closed* is applied only
  when the session is open; it deletes the session's row and its locks.
  *Started* is applied when the server has no row, whatever the count,
  and otherwise only when the count is above the recorded one; it
  deletes every session and lock of the server, and records the count
  with connection number 0, which means none opened. So connection
  numbers start at 1 (an opening of connection 0 changes nothing), as
  sequence numbers do. An event that is not applied changes nothing
  besides the last entry applied, and is not an error: a delayed or
  repeated entry can never open a session again or close a newer
  start's sessions.
- **file.** *Added*: the name must be free (*name taken*); the row and
  the name are written and the file's tree made, empty, as of this
  entry. *Removed*: the row, the name and every session's locks on the
  file are deleted, and the file's tree is dropped. *Renamed*: the new
  name must be free (*name taken*, also when it is the file's own); the
  row's name and the name rows change.
- **members.** The version must be one this build knows (*unknown format
  version*) and not below the state's (*format version lowered*, which
  cannot arise while 1 is the only version); it is recorded.
- **mark** and **empty** change nothing else. An `empty` entry, a new
  leader's first, would discard the open groups; no build yet has any.

Last, every entry records itself as the last applied.

**Refusals.** An entry that fails any check cannot be right. It is
refused with nothing changed, and the caller stops the store: applying
never guesses and never skips part of an entry. Each check reads only
the state and the entry, so every server, given the same state, refuses
the same entry in the same way: an entry repeated, skipped or out of
order; a broken chain or a term that goes back; an unknown kind,
payload version or tag; a malformed payload or one beyond a bound; a
group, a clone or a members list; a state or a `members` entry of a tree
format version this build does not know, or a lower version; a file
field that does not fit; a file that does not exist; a name taken; an
operation or a lock edit for a session that is not open; a sequence
number that is not the next; a remove of a lock not held. A failure that
is the server's own (a page it cannot read, a page store that fails, a
file's tree the store does not hold) is reported apart from these, and
leaves the store as it was too.

### 5.7 One unit of work

Everything an entry changes goes through the page store's one unit of
work for the entry (`EntryPages`), committed once, so an entry takes
effect whole or not at all, and a unit dropped at any point leaves
nothing. Each tree the entry changes applies its edits into the unit
and hands back its new root (`tree::apply`), which `apply` names in the
unit (`set_root`). A file's tree made is the empty tree written into the
unit (`tree::apply` from nothing) and named with `add_tree`; a file's
tree removed is named with `drop_tree`, and the page store releases the
root it holds for it. The store's own tree is made the same way by entry
1: before it the store holds no tree. Each tree is changed at most once
per entry, so no tree reads a page of the unit before it is committed.
`apply` writes or releases no root on its own account, and the trees it
changed are read from the page store, which holds every tree's name and
root.

### 5.8 What one entry costs

What applying one entry reads and holds, at the bounds of 5.2. A listing
of rows takes them one at a time and holds one row at once per level of
listing, so the scratch space of looking through rows does not grow with
their number, and no listing reads a value it then deletes. What an
entry keeps beyond that, for the edits it makes and what it returns, is
stated for each case below. `Applied::listed` reports the rows the
listings visited and the most held at once.

- **Decoding** holds the payload's items: at most about 28 times the
  payload's size (5.2), and the payload is at most 64 MiB.
- **A change**'s checks read the session's sequence number, the list of
  each session its lock edits name (once per session), and the row of
  each lock it removes that the change itself did not set or release
  (one value of at most 64 KiB at a time). Its lock edits are kept by
  session while it is planned, so a release clears only its own
  session's earlier edits: the work is linear in the lock edits. It
  holds one store-tree edit per lock edit and two for the session's row.
  Its tree edits cost what format section 2 says for each; a delete of
  any size is one range.
- **A server's start** lists the server's open sessions from their lists,
  one at a time, with one seek per session and no value read. It keeps
  the list of the sessions it closes, 32 bytes each, which
  `Applied::closed` returns, and holds one listed row at once besides.
  It then deletes the server's sessions and locks with two edits,
  whatever their size. The number of sessions a server has open is
  bounded by the server's own connection limits, not here.
- **A file's removal** looks through the sessions that hold a lock list
  (for any file) one at a time, a server and then its sessions, holding
  two listed rows at once, and looks up each one's list for the file:
  one lookup per such session, reading lists only. It keeps one delete
  edit (under 100 bytes) for each session that has a list for the file
  and nothing for the others, and releases the file's tree with one
  release, whatever its size.
- **A session's close, an opening, a rename, a mark, an empty or a
  members entry** read and write a fixed number of rows.

### 5.9 Limits

- Groups, clones, the members list, blob reference counts, design
  history, container file lists and the kept original image are not
  built; their entries are refused (5.4).
- A file's flags and page 0 are set when it is added; no entry changes
  them later.
- The bounds of 5.2 (names, replies, lock keys and users, counts) are
  OpenMaker's choices, not measured against what FileMaker clients send.
- An `OwnedNode` leaf without a value (`value: None`, as
  `OwnedNode::default` makes) is held as an empty value. Made an
  ancestor, it becomes both with an empty value here, and a list without
  a value in `OwnedNode`. The file engine makes leaves with a value.
- The decoder holds tree edits to the tree's bounds with the tree's own
  check (`tree::check_edits`), so the model, which shares the decoder,
  does not check those bounds a second time.

## 6. The store

How `om_store::store` joins `state` (section 1), the page file (section
4), the log (section 3) and applying entries (section 5) on one server:
the order of every write of its operations, and what a crash at each step
leaves. Nothing here adds a byte format. The store is driven by its
caller: no thread, timer or background work, so every step below is a
call the simulated disk can stop.

### 6.1 Creating

0. the configuration is checked as an open checks it (6.2, step 0);
1. the store folder's lock is taken; a folder with `state` holds a store
   already, and is refused. A folder without `state` that holds the log's
   folder `log/` (which only an open makes, once `state` exists), or a
   `pages` without the creation's marker, holds a store whose `state` is
   gone: it is refused, and reported as `OpenError::NoState`, never
   created over;
2. the creation's marker, an empty file named `creating`, is made, and
   the store folder flushed, so that its name is durable before anything
   else is made (a creation begun again finds it there);
3. a `pages` left by a creation that did not reach `state` is removed,
   and the page file is created (4.6: checkpoint 1 at entry 0, written to
   `pages.new`, flushed, renamed, the folder flushed);
4. `state` is created (1.3: both copies in `state.new`, flushed, renamed,
   the folder flushed);
5. the marker is removed and the store folder flushed;
6. the store is opened (6.2), which writes the first start's entry.

| A crash | leaves |
|---|---|
| before step 2 is done | at most an empty `creating` and no other file: no store |
| after step 2, before step 4 is done | no `state`, and the marker: what a creation cut short left. An open reports no store (`OpenError::Missing`); a creation begun again removes the `pages` it finds |
| after step 4 | a store, opened as any (6.2); the log is empty. An open removes a marker step 5 did not |

A folder without `state` is so told apart: the marker means a creation
cut short, created over; `log/`, or a `pages` without the marker, means a
store whose `state` is gone, reported for a restore (server-storage.md
6.12, "`state` has no valid copy"); nothing at all means no store.

### 6.2 Opening

0. the configuration checked against what the store must write (the
   log's and the page file's bounds, and this start's entry within the
   largest payload): one that cannot hold it is refused
   (`OpenError::Config`) before anything is opened, so no start is
   counted;
1. the lock, and `state` opened (1.3), which raises the start count in
   both slots; a creation's marker left by step 5 of 6.1 is removed. A
   folder without `state` is looked at under the lock taken again: when
   `state` has appeared (another process created the store meanwhile), it
   is opened; otherwise the folder is told apart as 6.1 says;
2. the page file opened (4.6): the newest checkpoint that reads back, or
   the one before it; its entry is the **start**;
3. the log opened from the start (3.6), checked against the marker;
4. the entries after the start applied again, each once, in order,
   through the durable entry: each is reserved with the reserve allowed
   (`Purpose::Recovery`), applied in its one unit of work, and the
   reservation extended while the entry runs short. The log is not
   retired meanwhile, and the store asks the page file for no reclaim and
   no checkpoint but in one case: a reservation refused because the pages
   waiting for a checkpoint are past their budget (4.8) has the last
   entry applied, which is durable, checkpointed, and the entry reserved
   again. The page file itself frees during the replay what normal
   running frees (4.8), as its reservations do;
5. the version the last entry made (or the checkpoint's, with none) is
   pinned and is the published one;
6. this start's own entry is appended and flushed (6.3): a `session`
   entry that records the start count (server-storage.md 7.3), the first
   write of every start. It is part of the recovery for space: it is
   reserved with the reserve allowed, and when it is refused because the
   pages waiting for a checkpoint are past their budget, or because the
   disk has no room for it (pages past the reserve, or the log's next
   segment), the recovered version, which is durable, is checkpointed
   first (6.6: the log retired and dying pages freed as far as the
   checkpoints and restore points on disk allow), and the entry is
   appended again, once. A disk that still has no room fails the open
   with `OpenError::Start`, whose remedy is to free space; so does a write
   of the entry, or of the marker, that the disk refuses for room, the
   cause kept through the error.

Steps 1 to 3 write what their own opens write. Step 4 writes nothing to
disk but what a forced checkpoint writes. A crash at any step leaves what
the piece's own table says, and the next open does the same work again:
the entries step 4 applied are in memory only until a checkpoint, and are
applied again. A crash in step 6 after the log took the start's entry
can leave that entry durable; the failed open reports it
(`OpenError::Start`), and the next open applies it as any entry.

What the open cannot do is reported, typed, with what the administrator
can do (`OpenError::remedy`): no store; a store whose `state` is gone
(6.1); no valid copy of `state`; no checkpoint that reads back; a log
that does not hold, whole, every entry through its marker; a log that
ends at a place it cannot read; a place at or before the marker it cannot
read; an entry the log holds that cannot be applied (a page that does not
check); no space to apply the log's entries, or for this start's own
entry. Two calls go past two of them, and nothing else does:

- Accepting the loss is two calls. `Store::plan_damage`
  (`Log::plan_damage`, server-storage.md 6.12) shows what would be kept and
  given up, and writes nothing to the log (the store's open steps 0 to 2
  run as in any open): the entry kept, the run listed before and after,
  where the open after the acceptance will end the log, and each segment
  file's fate (kept, lengthened, set aside and why, missing) with the
  entries it held as far as the headers say. `Store::accept_damage` takes
  that plan and does exactly that, or refuses with nothing changed when a
  fresh plan differs (`LogError::PlanChanged`); an administrator who would
  rather keep what a plan gives up restores, or repairs by hand. The plan
  is made from the start, the checkpoint the store opens on. Every listed
  segment the folder holds is opened, and a file shorter than its header
  says is read as it will read once lengthened: its bytes, then zeros to
  its header's size. The rule:
  - **kept**: in the listed segment that holds the entry after the start,
    the start reached by framing alone (3.5, "Anchored walks": a whole
    batch before the start's is crossed by its header; what does not frame
    or check before the start, or a read the device fails of an entry's
    payload or hash there, is reported in the plan and does not count), then the
    chain checked forward from the start's hash through the listed
    segments that follow without a gap, no further than the later of the
    marker and the start; the last entry so reached is `kept`. With it, the
    listed segments before that segment that follow one another without a
    gap. The run is then walked as the open after the acceptance will walk
    it (the open's own walk, writing nothing), and listed only as far as
    that walk goes, so that the open keeps exactly the plan. A kept file
    that is short is lengthened to its header's size with zeros, flushed,
    before `state` changes. Those zeros are bytes like any: where they
    complete a batch whose hashes all check (a file short only by trailing
    zero bytes), the batch is whole, as it was written, and the plan, which
    read the file so, says the open keeps it;
  - **given up**: every other listed segment (one whose own header does
    not check is not looked into), every segment file the run does not
    list, and whatever follows `kept`. Nothing is destroyed: every file
    given up is renamed, in `log/`, to `given-up-<k>.<number>` (`k` the
    next number no name uses, one per acceptance; refused when none is
    left, and never over a file of that name), a name no open removes,
    since an open removes only files named as segments; the kept segment
    whose bytes after `kept` are given up stays in use, and is copied to
    `given-up-<k>.<number>.tail`, a block the device cannot read written as
    zeros and said so. The folder and the copy are flushed before `state`
    changes. A rename is used, not a copy into a folder of its own beside
    `log/`: the file system renames within one folder only, and a file
    given up is often one the device cannot read, which a rename keeps
    whole without reading it. The plan says why of each file: it holds only
    entries up to the start's (only an older checkpoint needs them);
    nothing after the start could be kept, and why (the segment that would
    hold the start's successor is missing, or does not continue from the
    start's entry, or the walk ended cleanly before the start and the
    segment after is missing); it follows a segment that is missing or
    whose header does not check; it holds entries after `kept`; its own
    header does not check; the run does not list it;
  - **refused**, with nothing in the log changed: when the walk cannot say
    where the entry after the start begins (`LogError::FramingBroken`): it
    stops before any entry after the start, at a batch's mark or header,
    at bytes that do not frame in the start's own batch, or past a start
    whose own end its hash does not place (a damaged length would place
    its successor wrong), unless it reached the clean end of its last
    segment, every byte after that in the segment, to its header's size, is
    zero, and the listed segment after it is missing. A segment is made of
    zeros (3.2), so a byte that is not zero there is something written (a
    batch whose mark was damaged still has its header's rest and its
    entries): this refuses too a segment that holds bytes after its end
    from before an earlier cut, when the segment after it is missing. Also
    refused when a listed segment whose header does not check could hold
    that entry; when nothing would change; on a segment of a newer format;
    on a read the device fails of anything the decision depends on (an
    error that is not the device's, such as a stopped process, is the
    call's error, never a report).

  Then one `state` update, written to both slots, lists the kept segments,
  lowers the marker to `kept`, and raises the cut count when anything after
  `kept` is given up; the log is opened as any open opens it, except that
  it writes `state` and removes files only when its walk ends the log where
  the plan says, with the run it says, and no segment file would be
  removed: otherwise it fails having done none of that
  (`LogError::NotAsPlanned`, naming what it would have removed; the
  decision stands, and a new plan that still finds a loss sets those files
  aside, while one that finds none refuses and the next ordinary open
  removes them, as any open removes segment files the run does not list).
  The report
  (`DamageAccepted`) carries the plan and says it all: `kept`, the marker
  before, the cut count, the run before and after, every segment file
  given up with the entries it held as far as the headers say (its first
  from its own header, its last from the next one's; unknown where a
  header is missing or not valid, and for a file the run did not list)
  and where it went, the kept segment whose tail is given up (and the
  places of it that could not be read), the segments lengthened, and the
  names given.

  | A crash | leaves |
  |---|---|
  | after some renames, before `state`'s update | `state` listing segments whose files were renamed: the open reports them missing, and accepting again finds them so and gives them up; their bytes are under the earlier names, which the new plan does not show, nor the entries they held |
  | while copying the tail, before `state`'s update | a partial copy under its new name, the log otherwise as the renames left it |
  | after lengthening a kept file, before `state`'s update | that file at its header's size; nothing else changed |
  | after `state`'s first slot is flushed | the decision, which stands |
- `Store::give_up_unreadable_end` (`Log::give_up_unreadable_end`, 3.9).

Each is done from the moment its `state` update's first slot is flushed,
whether or not the open goes on to succeed. An open that fails after the
decision, an unreadable end the log's open then meets included, says what
was given up (`OpenError::AfterGivingUp`): with
`Recorded::Done` it stands, and the next open starts from it without
reporting it again; with `Recorded::Unknown` the `state` update failed
part way, and the next open shows which (it ends the log where the
decision does, or reports the damage again). When the open passed over a
copy of `state` and the log kept no whole batch after the marker, the
open succeeds and says that the last batch confirmed may be lost
(`Opened::last_batch_may_be_lost`). When the chain of entries before the
checkpoint's entry, in the segment that holds the one after it, stops
checking (3.6, step 3), the open succeeds and says where
(`Opened::before_start`), and whether the checkpoint on disk before the
newest, once the open is done (it may have taken checkpoints of its own),
comes before that place, so that an open falling back to it could not
replay past it (`Opened::fallback_cannot_replay`).

### 6.3 Writing one entry

`Store::append(file, payload)` gives the entry the next index and the
store's term. What a payload says of this server's own starts is the
store's too: a `session` entry that says this server started (the store
writes one at each open, 6.2), or that opens a session of this server in
another start than the current one, is refused from the caller with
nothing changed. Then it:

1. asks the log whether it would take an entry of that size and term
   (`Log::ready_for`, every refusal of an append, the next segment made
   when it would need one): a refusal changes nothing;
2. reserves pages for the entry (`Purpose::Entry`, never the reserve)
   and applies it (`apply::apply`) in its one unit of work, which commits
   at its end; a unit that runs short is dropped, the reservation
   extended (doubling), and the entry applied again. A full disk, a
   reservation refused for a checkpoint, a refused entry or a page that
   cannot be read refuses the entry with nothing changed;
3. pins the newest version (`PageFile::pin_newest`), which is the one
   the unit made: every entry commits a unit, since its unit records it
   as the store tree's last applied entry, and the store checks that the
   pin is of that entry;
4. appends the entry to the log's open batch, in memory, and checks that
   the log hashed it as it was applied.

Steps 1 and 2 write nothing to disk but what a reservation's growth of
the page file writes (4.8, the file's own rules). From step 3 on, a
failure stops the store: the unit's changes are in memory and cannot be
taken back, so every later call is refused, and the store is opened
again.

### 6.4 A flush

`Store::flush` writes the log's open batch and advances the marker (3.8),
with the flush's last entry as the committed position (on one server
every durable entry is committed), then **publishes** every entry the
batch held, in order: the last one's pin becomes the published version,
and each entry's notices and reply are handed back. All of the batch is
published, or none. A failure of the write or of the marker stops the
store.

| A crash | leaves |
|---|---|
| before the batch's write | the entries in memory only: lost, never answered |
| after the write, before the marker | the batch whole after the marker or torn; the next open keeps it whole (3.6) and applies it, or cuts it. Never answered |
| after the marker's flush | the entries durable, answered or not: the next open applies them |

### 6.5 Views

A view is the published entry and its pin, which holds the version's
roots and keeps its pages from reuse while the view lives; views share
the pin of the version they are of. A view holds the page file, not the
store: it lives across later writes, checkpoints, and the store itself.
After the store is opened again, a view taken before is kept only by the
checkpoints on disk (pins are in memory): once later checkpoints retire
those, its pages can be reused, and a read of one fails its hash check
instead of returning another page. A view of a restore point pins the
point's version from its meta stream; one whose meta stream the device
fails to read, or that does not check, is answered as such
(`WriteError::Unreadable`, with the page file's reason), and the store
goes on. A view, and the store, hand out nothing of the page file, the
log or `state`: no read of the working state and no write that would
pass the store's rules (a checkpoint of an entry not yet published, say).

### 6.6 A checkpoint

`Store::checkpoint(keep_as)` is refused while entries are applied past
the published one (flush first): a checkpoint is of the published entry
only. Then:

1. the page file's checkpoint of that entry (4.6), kept as a restore
   point when named;
2. the log retired up to the entry after the oldest checkpoint or
   restore point a superblock on disk names (3.10), never past it. A
   restore point dropped since counts until no superblock on disk lists
   it: an open that falls back to the older checkpoint lists it again;
3. a reclaim of dying pages, bounded.

A crash leaves what 4.6 and 3.10 say; the next open starts from the
newest checkpoint on disk, and the log still holds every entry after the
oldest. A failed write stops the store.

When a reservation is refused because the pages waiting for a checkpoint
are past their budget, the caller flushes, checkpoints, and appends
again: a checkpoint cannot be taken while an entry is applied and not
published.

### 6.7 Limits

- The store does not keep a mark that a start passed over a copy of
  `state` (a design change under review, not in section 1 or
  server-storage.md yet): an open reports the possible loss once, and
  only when it succeeds. `state`'s own open rewrites both slots (1.3), so
  an open that fails after it (the page file's open, the log's, the
  replay, this start's entry) or a crash before it returns leaves two
  whole copies of `state`, and the next open sees nothing to report; so
  does a crash right after an open that reported it. The mark is
  [issue #748](https://github.com/OpenMaker-Dev/openmaker/issues/748).
- On a disk with no room, an open that cannot make room for this start's
  entry by checkpointing (the log retired and dying pages freed as far as
  the checkpoints and restore points on disk allow) fails, and says to
  free space: the log keeps no room of its own for the next start's
  entry, so a disk that filled can need space freed before the store
  opens again.
- Recovery reserves a fixed few pages for each entry and extends them,
  rather than the count each entry took the first time (`Reservation::taken`):
  entries do not carry it. An entry the replay extends may take the
  reserve (4.8).
- The threaded front of the engine (an applying thread, group commit
  across callers, background checkpoints, the limits of server-storage.md
  6.11) is not built: the caller drives every step.
- Accepting a loss (6.2) gives up, set aside and shown in the plan, entries
  it could check in two shapes: a listed segment that is missing (an empty
  segment that giving up an unreadable end made, say) before one whose
  header continues from the last entry kept; and a listed segment whose
  header does not check, one segment or more after the one that holds the
  start's successor, with what follows it, although its entries chain. The
  plan does not look across either.
- An acceptance that fails or crashes after renaming some files and before
  `state`'s update leaves their bytes under the given-up names, but its
  error does not list them, and a new plan, which does not look at
  given-up names, shows those segments missing: neither where they went
  nor the entries they held.
