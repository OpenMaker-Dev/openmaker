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
the first and last keys a delete removes. Then, in
one entry of the page store (`om_store::page::EntryPages`), it writes
every page of the working copy, children before parents, each stamped
with the entry's index, releases every page the old version reached and
the new one does not (replaced by a copy, or dropped with everything
under it), and commits. A failure at any point leaves the store as it
was, so the entry can be applied again. An entry that changes nothing
writes nothing and keeps the root. An entry's index must be above the
last entry that changed the tree (the root page's), so stamps only grow.
The page store's guarantees, including what clones need, are in the
documentation of `PageStore` and `EntryPages`.

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
  the trees at an entry, `(entry, 0)`: it keeps no meta page (birth step
  above 0). A birth not known yet counts as the earliest.
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
  entry at a time: it reserves the entry, runs its units (each one
  tree's changes, or the whole entry's: several trees' pages and
  releases with the entry's changes to the set of trees, committed
  once), and is done with the entry before it starts the next; it may
  reserve later entries ahead and runs their units in their turn.
  Another caller checkpoints and reclaims; readers come at any time. The
  page file refuses what falls outside this, and makes each check in the
  same hold of its lock as the change the check guards:
  1. an entry has one open reservation at a time: a second, while the
     first is open, is refused (one after the first is dropped is not);
  2. units of one entry are open at a time: a unit of another entry
     while one is open is refused, as is one of an entry before the last
     one committed;
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
  6. the page file counts the changes to the set of trees committed; a
     unit that changes the set (makes, drops or renames a tree) or drops
     a page is refused whole at commit if the count moved since the unit
     began, so overlapping changes by units of one entry give one result
     whatever order they commit in, or the later is refused. A released
     page still in memory must have the hash the unit released it with
     (a byte-identical page passes: a hash covers a page's content, not
     its number);
  7. the restore point a checkpoint keeps counts in the floor (4.8) from
     its first check, and a checkpoint that keeps one is refused while
     any reservation is open;
  8. a checkpoint checks every root it is given as an open will (a tree's
     page, not an overflow page, written at or before `E`), whether the
     root is in memory or on disk.

  The page file is specified and tested for the caller that applies one
  entry at a time, normally through one unit; several units of one entry
  are allowed, and what is claimed for them is exactly what these rules
  state. Since a unit of a later entry committed between a checkpoint's
  first check and its plan refuses the checkpoint (4), and a keep is
  refused while any reservation is open (7), the caller that checkpoints
  has the caller that applies take no new entry for the moment of a
  checkpoint's first check and plan, and holds no reservation open when
  it keeps a restore point; otherwise both refusals repeat for as long as
  the applier does not pause.
- **A checkpoint** of the newest version, at entry `E` (every unit
  committed so far is of `E` or earlier), given every tree's root: the
  trees given must be exactly the trees that exist, those of the newest
  checkpoint and those made, dropped and renamed since by units (a
  change to the set of trees is part of a unit of its entry, checked and
  committed with it). The writer keeps every limit the reader enforces
  (trees, restore points, names, numbers), and the meta stream goes
  through the reader's own decoding before any byte of it is written.
  Short of free pages for its meta stream, it reclaims first.
  1. the pages the roots reach that are only in memory are found (a page
     on disk never refers to one in memory); a page in memory of the
     newest version that no root reaches, a root that left the newest
     version, or a tree of the newest checkpoint that is neither given
     nor dropped refuses the checkpoint, with nothing written;
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
  were written at or before its entry, and are in use in its map. A
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
