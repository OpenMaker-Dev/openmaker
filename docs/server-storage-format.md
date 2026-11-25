# OpenMaker Server store formats

The byte formats of the store engine (`om-store`), whose design is in
[server-storage.md](server-storage.md). All of it is OpenMaker's own.
Every structure carries a format version; until the format is declared
stable it may change between versions of OpenMaker Server
(server-storage.md section 11).

Integers are little-endian.

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
values. Encoding is held to the same bounds, and what decodes encodes
to the same bytes.

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

`om_store::apply::apply` takes the state's roots and an entry, and
returns what changed: the trees whose roots changed with their new roots,
the trees made and dropped, the sessions opened and closed, the
payload (whose notices and reply, with the operation's session and
sequence number, the caller delivers when the entry is published), and
what the unit of work took (5.7). What it costs is 5.8. It reads only the state and the entry:
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
root the caller did not give) is reported apart from these, and leaves
the store as it was too.

### 5.7 One unit of work

The page store's unit of work (`EntryPages`) is one tree's in its
interface. `apply` gives every tree an entry changes (the store's own,
the file's, a file's tree made or dropped) one shared unit: each tree's
commit passes, and the unit is committed once, after the last tree, so
an entry takes effect whole or not at all, and a unit dropped at any
point leaves nothing. Each tree is changed at most once per entry, so no
tree reads a page of the unit before it is committed.

A file's tree is made by writing its empty root (as `tree::create`) and
dropped by releasing its root as `Dropped` in the entry's unit; `apply`
reports both by name, so that the caller tells the page file which trees
exist. It also reports what passed through the unit: numbers allocated,
pages written, and pages released as copied or dropped. A dropped page
stands for everything under it, which the unit does not count.

### 5.8 What one entry costs

What applying one entry reads and holds, at the bounds of 5.2. No
listing reads a value it then deletes, so the memory of one entry does
not grow with the size of the values it deletes.

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
  with one seek per session and no value read, and holds 32 bytes per
  session it closes. It then deletes the server's sessions and locks
  with two edits, whatever their size. The number of sessions a server
  has open is bounded by the server's own connection limits, not here.
- **A file's removal** looks, for each session that holds a lock list
  (for any file), for its list for the file: one lookup per such
  session, reading lists only. It holds one delete edit (under 100
  bytes) per session that has a list for the file, and releases the
  file's tree with one release, whatever its size.
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
