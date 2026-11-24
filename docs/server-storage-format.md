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
can end inside a segment. Separators of index pages are not made: they
move up from a split or down into a join (2.7).

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
   bounds, keys well formed and in order, separators in order, stamps no
   later than the page's own entry, the number of page references equal
   to P (otherwise *malformed*).

The tree then checks that a child has the level and kind its parent
implies and was written by the entry its stamp names, and that an
overflow page holds the bytes its item expects. A page the store cannot
find is *missing*. Nothing is allocated by a size the bytes give before
it is checked against the page size or the bounds, and no read recurses
deeper than the tree's levels.

A read touches only the pages on the way to what it returns: a lookup,
range or walk the pages from the root to the items it returns and those
items' overflow pages; a listing of children the keys of the items it
passes and the children's own values; "changed since" index pages only
(2.8). One exception: comparing with a key or separator longer than the
1,024 bytes its page holds reads its overflow pages when those bytes do
not decide.

**Writing.** An entry applies its edits (2.7) to a working copy of the
pages it reaches, reading the store and changing nothing in it. Then, in
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
leaf or a child entry of an index page. The child of an index page for a
key is the child whose range holds it: the number of separators at or
below the key.

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
the child for the key. Insert the item in its leaf, or replace the item
with that key (a kind change only changes the kind). Then fix the path
bottom-up, each index page fixing the child the primitive went through
(below), then the root rule.

**Fixing a child.** A child that does not fit is split. Otherwise, if it
shrank and is underfull or empty: when its parent has another child, it
is joined with a sibling; when it is empty and its parent's only child,
it is removed (and the parent is then empty); an underfull only child is
left as it is.

**Delete a range** `[lo, hi)`. Before it, note the first and the last key
in the range. Starting at the root, in each index page: the child `i` for
`lo`, and the last child `j` whose range starts below `hi` (the number of
separators below `hi`; the last child when the range has no end).
Children strictly between them are removed whole, unread, with their
separators; child `j` keeps its separator, so child `i`'s range reaches
to it. Then the primitive goes into child `i` if it holds a key in the
range, and into child `j` (when `j > i`) if it does. In a leaf, the keys
in the range are removed. On the way back, child `i` is fixed if the
primitive went into it, then child `j` likewise unless fixing `i` took it
in. Then the root rule. Last, the paths from the root to the leaves for
the first and the last key noted are copied, so that the pages covering
the deleted keys' places carry the entry's stamp (2.8).

**Split.** A node of `n` units that does not fit is split before unit
`s`, `1 <= s < n`, and each side is split again in the same way, left
side first and without a hint, until every part fits:

- the *hint* `h` exists only in a set: in the leaf, the place of the item
  set; in an index page, the place of the first new child when fixing the
  changed child split it, and no hint otherwise; the root takes its own.
  When there is a hint, `1 <= h < n`, the units before `h` take at least
  seven eighths of 16,344 bytes (14,301), and both sides fit, `s` is `h`.
  Items appended in key order therefore fill their pages;
- otherwise `s` is where the two sides' footprints are nearest equal, the
  smallest such `s` on a tie.

The footprints of an index page's sides count each child entry, and the
separators of every child but each side's first; the separator of child
`s` moves up to the parent as the separator of the right side. A leaf's
right side gets a new separator, made from the last key on its left and
its own first key (2.4).

**Join.** A child is joined with its right sibling, or with its left one
when it is the last. Two leaves join by concatenating their items, and
the separator between them goes. Two index pages join with that
separator moved down between their children, unless one of them has no
children, when the other is the result and the separator goes. A joined
node that does not fit is split, without a hint. A joined node that is
empty is joined again with its new sibling, or removed when it is its
parent's only child, so no page below the root is ever empty.

**Root rule.** After each primitive, repeatedly: a root that does not fit
is split, with the primitive's hint, and its parts become the children
of a new root one level up; an index root with no child becomes an empty
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
of the children whose ranges overlap the subtree's range `[k, k')`, those
stamped after `n` either lie wholly inside the range (the answer is yes),
or are at one of the range's two ends, where an index page is looked into
and a leaf counts as a change without being read. It reads at most two
pages per level, and no leaf.

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
