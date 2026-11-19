# Writing HBAM7 (`.fmp12`) files

How to modify the HBAM B-tree of an `.fmp12` file so that FileMaker Pro and
FileMaker Server open the result cleanly: the leaf stream and its pagination,
the index (level > 0) pages, page allocation, splits and growth, the free
pages, the superblock and page bookkeeping, crash handling, and the checks
FileMaker runs. It extends [file-format.md](file-format.md) (container, page
header, chunk codes, key space), which a reader of this document should know.
It covers the storage layer only: which keys and values a record commit or a
schema edit must write is in [write-path.md](write-path.md) and the catalog
specs.

Confidence: **High** = read in the engine and confirmed on files that
FileMaker's own tools accepted; **Med** = one of the two; **Low** = inference.
Evidence cites demangled symbols with their `nm` address in the FileMaker
Server 26.0.3 `libDBEngine.so` (Ghidra address = `nm` + `0x100000`), the 72
sample files of file-format.md 3.6 (66 templates + 6 FMS samples; "the
corpus"), and the runs in section 9. The prototype writer that produced the
test files is in `re/work/hbam-writer/` (git-ignored, Python, not product
code).

---

## 0. Summary: the simplest writer FileMaker accepts

A writer that follows these rules produces files that FileMaker's consistency
check passes and FileMaker Server hosts (section 9). Everything not listed is
free for the writer to choose. (High)

1. **Pages.** 4096-byte pages, page 0 the superblock, page 1 always the root.
   The file is exactly `(root.NextPage + 1) * 4096` bytes: the root's
   `NextPage` field holds the **last page number**, not a sibling.
2. **Leaf level.** The leaf pages are one bracketed chunk stream (file-format.md
   3.6) cut into pages. Each page starts by re-pushing, without `0x80`, the key
   path still open at the end of the previous page; a page is never cut
   between a push and its `0x80`; `FreeSpace = 4076 - used bytes`; leaves are
   doubly linked in key order. Any fill level and any page order are accepted.
3. **Index levels.** Every index page is a self-contained prefix trie of
   (separator path, child page) entries, using only codes `0x00..0x3F` with
   the `0xC0` bits marking the last slot of each level (no `0x80`/`0x40`).
   Each separator lies in (last key of the previous child, first key of this
   child]; writing exactly the child's first key is always right. Index pages
   of one level are doubly linked in key order; levels count down by one to
   the leaves.
4. **Free pages.** A page that is no longer used gets `PhysLevel = 0xFFFF`.
   Every page other than page 0 is either reachable through the index or
   free. New pages come from free pages or from the end of the file (bump
   `root.NextPage`).
5. **Bookkeeping.** Stamp every page you write with a fresh 64-bit SCN (high
   half in `mLogSeqNum`, low half in `mModCount`); on close write the largest
   SCN into the superblock at `0x802` and set the clean-close byte `0x800` to
   `0xAC`. Clear `0x800` to `0x00` before the first write of a session.
6. **Crash safety.** FileMaker needs no log: an unclean file is verified on the
   next open. For atomic multi-page commits use an OpenMaker journal (6.3).

The shortest correct implementation for local files (M7) is a **bulk
rebuild** (section 7.2): re-paginate the whole edited key stream and rebuild
the index bottom-up. A server needs the **incremental** policy (section 7.1):
rewrite the touched leaves, split or free pages, and patch the index along the
path.

---

## 1. Model and terms

- **Leaf stream.** The level-0 pages, in chain order, carry one chunk stream.
  Its items are `begin` (`0x80`), `pop` (`0x40`), `push s` (a key segment that
  opens a node) and `leaf s v` (a key segment with an optional value). The
  stream starts with `begin` (the root list) and ends with one `pop` that
  closes it. Every `push` that creates a node is followed by `begin`; the
  node's children follow in key order; a `pop` closes it. (High, corpus)
- **Open path** at a position: the segments of the pushes not yet closed by a
  pop.
- **Fences.** Two zero-length keys compare below and above every real
  segment: **BH** (begin hierarchy, bytes `00 01`) and **EH** (end
  hierarchy, `00 FF`). `CmpHbamKey` (`0x159a370`) and `HPage::FindSubkey`
  (`0x159f960`) treat a zero-length key whose sentinel byte is `01` as BH and
  any other sentinel as EH; FileMaker writes `FF`. (High)
- **Key order.** Paths compare segment by segment; two segments compare by
  their bytes over the shorter length, then the shorter one first; BH is below
  and EH above every segment. Siblings in the stream follow this order. (High)
- **First key of a page** (the engine's `FirstLeaf`): after the re-pushed
  prefix, the page's first item determines it:

  | first item after the re-push | first key |
  |---|---|
  | `leaf s` | open path + `s` |
  | `push s` (then `begin`) | open path + `s` + BH |
  | `begin` (only the first leaf page) | BH |
  | `pop` | open path + EH |

  (High: 8,047 of the corpus's 8,055 separators equal it exactly; section 3.2)

---

## 2. Leaf pages

### 2.1 Pagination (High)

A leaf page body is `repush(open path at the cut) + items`, where `repush` is
one push chunk per open segment (key class by length, value class 0, **no**
`0x80` after it). Corpus: all 8,055 page boundaries re-push exactly the open
path; the prototype re-encodes all 8,055 leaf pages byte for byte from this
rule.

Rules for where a page may end:

- Never between a `push` and its `begin`: the pair is one unit. (Corpus: no
  page ends with a push; 228 end with `begin`, 554 with `pop`, 7,345 with a
  leaf.) A cut there makes the next page look like it creates the node again:
  FileMaker's check reports a bad block (mutation n6), although om-hbam's
  reader accepts it.
- Anywhere else: after a `begin` (the node's list continues on the next page,
  which re-pushes the node without `0x80`), between siblings, before or after
  a `pop`, or with the next page starting with a `pop` (Inventory/sv page 4).
- A page holds at least one item after its re-push. Do not write empty leaf
  pages; free them.
- Pages never close the open path at their end; only the stream's own pops
  close nodes.

Header: `PhysLevel = 0`, `PrevPage`/`NextPage` = neighbours in chain order (0 at
the ends), `FreeSpace = 4076 - body length`. Bytes after the body (slack) are
ignored; FileMaker leaves stale bytes there and the checker accepts any
content (mutation m15). Zero-filling slack is cleanest.

Fill: any. FileMaker packs compacted files to within a few bytes; its
incremental splits leave pages about 3/4 full (4.3); the prototype's
half-full and pathologically uneven pages all pass (9.2).

### 2.2 Chunk encoding in leaf pages

- Allowed codes: `0x00..0x1F` (leaf), `0x20..0x3F` (push), exactly `0x80` and
  `0x40`. Never a code with both top bits set (those are index-page codes,
  3.1), never `0x41..0x7F` or `0x81..0xBF`. Corpus: 210,457 leaf and 99,237
  push chunks, 65,592 of each marker, nothing else. (High)
- **A push never carries a value** (value class 0). `VerifySlotMatrix`
  rejects a slot with both data and children ("Can't have both data and
  children"); the corpus has no valued push in leaf or index pages. A key is
  either a value leaf or a list. (High)
- A leaf with value class 0 is a key with no value (distinct from an empty
  list `push s, begin, pop`). Both occur. (High)
- Key segments are 1 to 219 bytes: the length byte of a variable-length key
  must be below `0xDC` ("Key too large", `HPage::VerifySlotMatrix` `0x15aa070`;
  a 219-byte segment passed, 9.2). A length byte of 0 means a fence and only
  occurs in index pages. (High)
- Size classes: FileMaker always writes the smallest code for the key and
  value lengths (file-format.md 3.6). A non-minimal encoding (every key
  variable-length, every value length-prefixed) passed the consistency check
  (w10); write minimal codes anyway so output matches FileMaker byte for
  byte. (High for minimal, Med that non-minimal is safe beyond the checker)
- A chunk must fit inside the page body ("Data too large" means a chunk runs
  past the end). (High)

### 2.3 Values (High for the rule, from the corpus)

- A value of up to **1000 bytes** is stored inline in one leaf chunk. No
  inline value in the corpus is longer than 1000 bytes.
- A longer value becomes a node: child `0` = u32 BE total length, children
  `1..n` = pieces of exactly 1000 bytes, the last one the remainder (1,556
  nodes in the corpus, every one consistent). Piece ids are serial keys
  (query-program.md 2.6): `01`..`7f`, then `80 00` for piece 128, `80 01`,
  ... (correction: a FileMaker Server-stored 1,154-piece value keys piece
  128 as `80 00` and piece 1154 as `84 02`, containers.md 1.5; no corpus
  value had more than 127 pieces, so the two readings agreed there).
- Text whose characters are not all ASCII uses the same node form with an
  8-byte child `0` (u32 byte length, u32 character count) even when short
  (2,610 nodes in the corpus, all single-piece). Which values are text and
  how text is encoded (XOR 0x5A + SCSU) is file-format.md 3.5 and 3.6.
- Key depth: at most **16 segments**. `HPage::FromDiskFmt` builds the slot
  matrix for levels 1..16 and fails a push at level 16 or deeper outside
  recovery, so a leaf may sit at depth 16 and a push at depth 15. A 16-deep
  path passed (9.2). (High; om-hbam's `MAX_DEPTH = 17` is one too lenient for
  a writer.)

### 2.4 Editing the stream (High: prototype runs, 9.2)

Edits operate on the logical stream; pagination follows.

- **Replace a value**: replace the leaf item (an inline value can turn into a
  chunked node and back; replace the whole subtree).
- **Insert a key** under an existing node: insert `leaf s v`, or
  `push s, begin, children..., pop` for a list, at the position that keeps
  the siblings in key order. The position may be the end of a page whose
  open path is the parent, or the start of the next page after its re-push.
- **Delete a key**: remove the leaf, or the whole span from its `push` to the
  matching `pop`. A span may cross pages; pages it empties are freed (4.2),
  and pages after it inside the span change their open path.
- **Re-paginate** every touched page with the open path recomputed from the
  last untouched page before it: if the content fits, rewrite the page in
  place; if it overflows, split it into as many pages as needed (new pages
  linked after it); if it is empty, unlink and free it. Pages outside the
  touched range keep their bytes: an edit that inserts or removes whole
  subtrees leaves the open path at the end of the range unchanged.
- After any change to a page's first item, update its separator (3.2); after
  adding or removing a page, add or remove its index entry.

---

## 3. Index pages (level > 0)

### 3.1 Grammar (High)

An index page holds a sorted list of entries (separator path, child page
number), stored as a prefix trie. Every index page is self-contained: it
starts at the top of the key space (level 1) with full paths and needs no
re-push, and it closes every level it opens. Decoder: `HPage::FromDiskFmt`
(`0x159d1f0`), the same walk as for leaves.

- Codes are `0x00..0x3F` plus the flag pair `0xC0`. Size classes are those of
  file-format.md 4.1, read after stripping the flag.
- Bit `0x20` set: an **interior** slot. Its key segment is shared by every
  separator below it; its children follow at the next level. Value class 0.
- Bit `0x20` clear: a **terminal** slot. Its value is the child page number,
  u32 big-endian, always value class 3 (4 bytes). The separator is the
  segments of its interior ancestors plus its own key.
- Both top bits set (`0xC0`): this slot is the **last** at its level. After
  the subtree of the last slot, the walk returns to the parent level, and on
  from there while the parent was itself flagged last. The page ends when the
  last level-1 slot is done; the body must end exactly there.
- No `0x80` or `0x40` chunks ("BH/EH node in the index",
  `HPage::VerifySlotMatrix` `0x15aa070`). Corpus: 5,117 terminal and 2,938
  interior unflagged codes, 3,010 and 4,259 flagged, nothing else.
- A **fence** segment is written with key class 3, a length byte of 0, and the
  sentinel byte: `00 01` = BH, `00 FF` = EH. The 0-length key occupies two
  bytes (`HPage::GetBufferSize` `0x159c5d0`, `HPage::GetChildPage`
  `0x15ad970`). In the corpus only index pages have fences: 542 BH, 6 EH.
- Sharing is maximal: consecutive separators with the same segment at a depth
  sit under one interior slot, and an interior slot may have a single child.

Encoding (what the prototype does, byte-identical to all 72 index pages of the
corpus): for the entries sharing the current prefix, group them by their next
segment in order; for each group emit a terminal slot if the group is one
entry ending at this depth, otherwise an interior slot followed by the
group's encoding one level deeper; set `0xC0` on the last group of each list.
No separator is a prefix of another (separators end in a leaf key, a fence,
or a key below a node), so a group never needs both forms.

Example, the first bytes of the Contacts/en root (page 1):

```text
1b 00 01 00000002      terminal BH                        -> page 2
20 02                  interior 02
28 18 18                 interior 18 18
20 01                      interior 01
f8 04 504e4766               interior "PNGf", last in its list
03 03 0000004e                 terminal 03                 -> page 78
03 07 0000004f                 terminal 07                 -> page 79
c3 0b 00000050                 terminal 0b, last           -> page 80
20 02                      interior 02  (back under 18 18) ...
```

### 3.2 Separators (High)

Let child `i` of an index level have first key `F(i)` (section 1; for an
index child, its first entry's separator) and let `L(i-1)` be the last key on
the child before it in the same level (for a leaf, the full path of its last
slot, `pop` slots counting as open path + EH; for an index page, its last
separator). The entry for child `i` must satisfy

```text
L(i-1)  <  separator(i)  <=  F(i)
```

- `HPage::VerifyIndexPage` (`0x15aadd0`) compares each entry with the child's
  first leaf: equal is fine; greater is an error ("doesn't match first leaf of
  data page"); smaller is fine only if it is above the previous page's last
  leaf. `HPosInfo::VerifyAllPages` (`0x15b4d10`) also checks that looking up a
  page's first and last entries through the index leads to that page
  ("Index does not lead to first/last entry of page #").
- FileMaker writes **exactly the first key** whenever it creates an entry
  (8,047 of 8,055 corpus entries; every entry of FileMaker's own compacted
  copies, 9.3). The 8 others are stale entries left after the first keys of
  a page were deleted; they sit strictly between the two bounds, and those
  files pass the check (9.2). A writer should always write the exact first
  key; it never has to be shortened.
- Mutations: a separator equal to a key on the previous page (m13) or above
  the child's first key (m14) makes the file damaged.
- The first entry of the first page of every index level is `BH`, because the
  stream begins with `begin`.

### 3.3 Lookup (High)

`HPosInfo::GotoViaIndex` (`0x15bd400`) starts at page 1 and calls
`HPosInfo::DescendLevel` (`0x15b0b80`) until it reaches a level-0 page. On an
index page the key is walked down the trie one segment at a time with a binary
search among siblings (`HPage::FindSubkey` `0x159f960`); where the segment is
not found, the engine takes the last terminal at or before that point (the
rightmost terminal under the previous sibling, else the previous terminal, else
the leftmost one). It then reads the child page number and repeats on the
child. In B-tree terms: follow the last entry whose separator is `<=` the key.
That is why separators must be ordered and bounded as in 3.2.

### 3.4 Structural rules (High unless marked)

- `PhysLevel` of an index page is its height above the leaves; children of a
  level-`L` page have level `L-1` (`VerifyPage` "Prev page %u at level %u
  instead of level %u").
- Every child is referenced exactly once in its level ("duplicates an index
  entry"), never a free page ("pointing to free page") or a missing one.
- Index pages of one level other than the root are doubly linked in key order
  through `PrevPage`/`NextPage`, ends 0 (mutation n1: a broken link at level 1
  makes the file damaged). The root is alone at its level: `PrevPage = 0`,
  `NextPage` = last page number (4.1).
- `FreeSpace = 4076 - body length`, as for leaves.
- An index page holds at least one entry. A root with a single entry is valid:
  a new file's root has exactly one (`BH -> 2`, 4.6), and `SplitRoot` leaves
  one entry before splitting the child (4.3, 4.5).
- Index pages may sit at any page numbers. FileMaker's compacted copies put
  them first (pages 2 and 3, leaves from 4); the prototype's bulk rebuild puts
  them after the leaves (9.2). Both pass.

---

## 4. Pages: root, allocation, free pages, splits, growth

### 4.1 Root and file length (High)

- The root is **always page 1**. `GotoViaIndex`, `HPosInfo::SetToIndexRoot`
  (`0x15b0ac0`), `HBFile::ScanForFreeList` (`0x153f320`) and
  `HBFile::IsRootPageSane` (`0x153cac0`) pin page 1 by number; nothing stores
  a root page number.
- The root is an index page: `IsRootPageSane` requires `1 <= PhysLevel <= 16`.
  A single-page tree is therefore never written; the smallest tree is a root
  over one leaf (4.6).
- **`root.NextPage` = the last page number** (`page count - 1`). All 72 corpus
  files have it, and `IsRootPageSane` requires `file pages == root.NextPage +
  1` and `root.PrevPage <` that count. A root `NextPage` one short (m02), or a
  file with an extra page the root does not cover (m08, m10), is damaged.
  `HCacheMgr::GetNewPage` (`0x15699c0`) allocates at the end by reading the
  root's `NextPage`, using the next number and incrementing it, and
  `ScanForFreeList` rewrites the field from the file length when it differs.
- The file length must be exactly that page count; there is no other size or
  page-count field (page 0 is unchanged by every FileMaker write except the
  SCN, 5.1).

### 4.2 Free pages (High)

- A free page is any page with `PhysLevel = 0xFFFF`. Nothing else in it
  matters: a free page with garbage links and body passes (n5). FileMaker
  clears it with `HPage::Empty` (`0x159eae0`: LSN, links and level zeroed,
  `FreeSpace = 4076`) and then sets the level (`HCacheMgr::FreePage`
  `0x156a390`); write that header for tidiness.
- **There is no on-disk free list.** `HBFile::ScanForFreeList`, called by
  `HBFile::ForceOpen` (`0x153f040`), reads every page from 2 to the last at
  open and puts each `0xFFFF` page into an in-memory bit list
  (`HBFile::AddFreePageNum` `0x15423b0`); `GetNewPage` takes the lowest free
  page first and only then grows the file.
- Every page from 1 to the last must be either reachable through the index or
  free: `VerifyAllPages` checks "indexedCount plus freeCount" against the page
  count. An orphan page (not linked, not free) makes the file damaged (n2).
- Free pages may sit anywhere, including at the end (m09 with `root.NextPage`
  covering them). A writer may reuse any free page in any order, or shrink the
  file by dropping trailing free pages and lowering `root.NextPage`.
- FileMaker's own edits leave free pages behind (4.3). FileMaker Server
  removes them when it verifies an unclean file: it moved pages from the end
  into the holes, fixed the neighbours' links and the index entries, truncated
  the file and lowered `root.NextPage` (9.4).

### 4.3 How FileMaker splits, shares and merges (High; for reference only)

A writer does not have to copy this policy; it shows what shapes FileMaker
itself produces.

- **Overflow** (`HPosInfo::MakeRoom` `0x15bd690`): when an insert needs more
  room than the page has, the engine first tries the left sibling: if that
  page has at least half a page free and the two pages together have room, it
  moves the page's leading entries to the left page (`HPage::ShareToLeft`
  `0x15a8310`) and re-indexes the page's new first key. Otherwise it splits.
- **Split** (`HPage::SplitPage` `0x15a6ba0`): the split point is about 3/4 of
  the page's used bytes (`HPage::FindSplitPoint` `0x15a4310`), so the left part
  keeps about 3/4. Both halves are built in fresh pages; the left half then
  takes over the original page number (the page swap) and keeps its
  neighbours, the right half goes in a new page linked after it, and the
  scratch page left over is freed (so every split leaves one free page). The
  right page's first key is inserted into the parent
  (`HPosInfo::InsertIndex` `0x15b3ad0`), which may split in turn. While
  compacting, leaves are cut exactly at the insertion point instead, filling
  pages completely.
- **Merge** (`HPosInfo::MergePage` `0x15b0fe0`): two neighbours are merged when
  their free space together exceeds 4,890 bytes (so the merged page keeps
  more than about 800 bytes free), and the emptied page is deleted from the
  index and freed.
- **Root split** (`HPosInfo::SplitRoot` `0x15c0570`): a new page receives the
  root's level + 1 and its links, and one entry for the root's first key
  pointing at the new page's number; then the two pages swap numbers, so the
  new root is page 1 and the old root's content moves to the new page (links
  cleared, as it is alone at its level until split). The old root then splits
  normally, adding its second entry to the new root.

### 4.4 What FileMaker accepts (High: 9.2)

All of the following passed `--checkConsistency` and FileMaker read every key
of them (`--copyCompress`, 9.3):

- three-level trees grown by the prototype (root at level 2 over 5 to 125
  level-1 pages);
- leaf pages filled completely, half full (w07, w09), and left uneven by
  repeated greedy splits (pages holding one or two entries, w04);
- level-1 index pages holding only a few entries each (w04: 125 index pages
  for 1,152 leaves);
- leaf and index pages in any page-number order, new pages appended at the
  end or placed in freed holes, freed pages in the middle and at the end;
- the leaf head anywhere (FileMaker's copies start the leaves at page 4).

So a writer may use the simplest policy that keeps 4.1 to 4.2 and sections 2
and 3: split only on overflow into as many pages as needed, free a page only
when it is empty, never merge, and grow the root by `SplitRoot` when it
overflows. Splitting at about half (or 3/4, like FileMaker) rather than
greedily keeps later inserts from splitting again at once; it is a
performance choice, not a format rule.

### 4.5 Adding a level (High)

When the root overflows: allocate a page `N`, move all the root's entries
into it with the root's old level, link `N` alone at that level (`PrevPage =
NextPage = 0`), and give the root (still page 1) `PhysLevel + 1` and a single
entry `(first key of N, N)`; then split `N` like any index page, which adds
the second root entry. The root's first entry is always `BH`. This is what the
prototype does (w03..w06) and matches the engine's result.

The height is bounded by the 16 levels of `PhysLevel` (at most 16 for the root;
`VerifyPage` rejects `PhysLevel > 16`).

### 4.6 A new, empty tree (High)

`HPosInfo::CreateFile` (`0x15ad2b0`) makes: page 1 = root, `PhysLevel 1`,
`PrevPage 0`, `NextPage 2`, one entry `BH -> 2` (bytes `db 00 01 00000002`);
page 2 = leaf, body `80 40` (the empty root list). The superblock is built by
`HBFile::CreateSmallBlockHeader` (file-format.md 1.2). A usable FileMaker
database also needs its catalogs (key `2` file status, schema, security),
which are outside this document; copying the page-0 image of an existing file
and building the tree from a template's key stream is the practical route.

---

## 5. Bookkeeping

### 5.1 Superblock fields a writer maintains (High)

| offset | size | meaning |
|---|---|---|
| `0x800` | 1 | clean-close flag: `0xAC` = closed cleanly, `0x00` = open with writes (or crashed) |
| `0x801` | 1 | `0x00` |
| `0x802` | 8 | file **SCN** (system change number), u64 big-endian |

- Written by `HBFile::WriteCleanCloseAndSCN` (`0x153bed0`): with "clean" it
  stores `0xAC` and the SCN, otherwise `0x00` (and leaves the SCN bytes).
  `HBFile::ReadCleanCloseAndSCN` (`0x153bff0`) reads them at open and raises
  the in-memory SCN to the stored one when the flag is `0xAC`. An older
  format's flag `0xA1` with `0x801 = 0xA8` is also read as clean
  (`HBFile::ConvertCleanClose` `0x153c960` upgrades it).
- The "global counter at ~0x806" of earlier file-format.md versions is the
  low half of this SCN.
  Every corpus file has `AC 00` and an SCN equal to its largest page SCN.
- No other byte of page 0 changes when FileMaker writes a file: its compacted
  copies, its verified-and-compacted hosted files and the test project's
  live copy differ from the Contacts template's page 0 only at `0x808..0x809`.
  There is no page count, root pointer or free-list head in page 0.

### 5.2 Page SCN: `mLogSeqNum` and `mModCount` (High)

Every time the engine dirties a page (`HFrame::SetDirty` `0x1595960`) it
increments the file SCN and stamps the page with the new value: low 32 bits in
`mModCount`, high 32 bits in `mLogSeqNum` (`HPgHdr::SetModCount` `0x15ac920`
and `GetModCount` `0x15a1e60` split and join the 64-bit value). So
`mLogSeqNum` is **not** a redo-log sequence number; it is 0 in every file
because none has reached 2^32 changes.

None of these values is checked when a file is opened or verified: pages with
`mModCount` 0 or `0x7FFFFFFF` (m04, m05), a nonzero `mLogSeqNum` (m07), a
superblock SCN of 0 or below the pages' (m06, n3, n4) all pass;
`VerifyAllPages` recomputes the maximum page SCN ("reset maxSCN"). Still,
keep FileMaker's invariant: stamp each page you write with `++SCN`, starting
from the superblock value, and store the final SCN at close, so that SCNs
only grow (the server's persistent cache and incremental backups compare
them; Low, inferred from the `HSCacheMgr`/`HFrame::GetSCN` users).

### 5.3 Database-level modification counts (High for the split, Med for details)

The counters the network protocol compares are not page fields. op89
`RCCompareModCounts` gathers them with `DBFileUser::GatherModCounts`
(`0xf41b30`) and `DBCatalog::GatherModCounts` (`0xe6eb10`): each catalog
member's count is an ordinary key (`KModCount`, read by
`DBCatalogCommon::GetModCount` `0xe593b0`), field data has `KDataModCount`
(`DBField::GetDataModCount` `0xeef7e0`), and records carry field `fc`
(write-path.md 5.4). Whoever edits those catalog entries or records writes
these keys like any other value; the HBAM writer does not derive them. Their
numeric keys and bump rules belong to the catalog workstream.

### 5.4 Other things FileMaker writes on its own (Med)

- A clean hosted open and close by FileMaker Server changed no byte of the
  file (9.4).
- After verifying an unclean file, FileMaker Server also rewrote the file
  status keys `[2, 8]` and `[2, 9]` (an entry count went from 1 to 2 and an
  entry decoding to "Server 26.0.3" was added; presumably the history of
  versions that opened or checked the file). This is catalog content, not
  storage bookkeeping.

---

## 6. Crash safety

### 6.1 What FileMaker does (High)

- The engine clears the flag (`0x800 = 0x00`) when it writes the first dirty
  page of a session (`HCacheMgr::WritePageToTempBuf` `0x1568260` calls
  `WriteCleanCloseAndSCN(false)`), and `HBFile::Close` (`0x153a6b0`), after
  flushing every page, writes `0xAC` and the SCN if the session wrote
  anything. A file with unflushed or flushed-but-unclosed writes is therefore
  marked unclean on disk, and a crash leaves it so; a session that only reads
  never touches page 0 (9.4). FileMaker Server's `HBFile::Pause`/`Resume`
  (backups) also set and clear the flag.
- `HBFile::Open` runs `HBFile::Verify` (`0x153e980`, which calls
  `VerifyAllPages`) when a file was not closed cleanly (or a verify option is
  set); the branch is skipped for one permission mode, which looks like
  read-only access (Med for that case). If verification passes, the file
  opens; otherwise the open fails with error 805, "File is damaged, use
  Recover". A clean file
  is opened without any check, so a writer must never set `0xAC` on a tree
  that is not consistent.
- FileMaker Server logs the check (event 194 "was not closed properly;
  performing consistency check on N blocks...", then 633 "consistency check
  succeeded") and, as observed, also compacts the free pages (4.2) and
  rewrites `[2, 8]`, `[2, 9]` (5.4) before opening the file normally (9.4).
- FileMaker Server keeps a separate redo log (`HBLogFile`, `HBLogRecord`;
  `HBFile::ApplyRedoLog` `0x153f770` writes a logged page image back into the
  database). It is not part of the `.fmp12`, no file needs it to open, and
  OpenMaker does not need to write one.

### 6.2 What a writer needs (High for the requirement, design is OpenMaker's)

A commit that touches several pages (a split rewrites the left page, writes the
new right page, changes the next page's `PrevPage`, the parent index page,
possibly its ancestors, and the root's `NextPage`) is not atomic on disk, and
no write order makes it so, because parents and siblings are updated in place.
Rules:

1. Before the first page write of a session, set `0x800 = 0x00` and flush
   (fsync) page 0. Then a crash is always detected by FileMaker.
2. For atomic commits, OpenMaker keeps its own **rollback journal** next to
   the file (for example `<file>.omjournal`): the original image of every
   existing page about to be overwritten plus the original file length,
   flushed before the database pages are written; the database is flushed;
   then the journal is deleted. On open, an existing journal means an
   unfinished commit: copy the saved pages back and truncate to the saved
   length. FileMaker never sees the journal; if FileMaker opens a crashed file
   whose journal was not applied, it verifies the file (rule 1) and either
   opens it or asks for Recover.
3. At a clean close (or a quiescent checkpoint with nothing in flight), flush
   the database, write the SCN and `0x800 = 0xAC`, and flush page 0.
4. A bulk rebuild (7.2) writes a complete new file and renames it over the old
   one, which is atomic without a journal.

---

## 7. Writer policies

### 7.1 Incremental (om-server commits, large files)

Proven by the prototype (`proto/writer.py`, runs w02..w06):

1. Load the root, and lazily the index pages, into memory; keep a map from
   each page to its parent (or find the parent by descending from the root
   with the page's first key, 3.3).
2. For each edited key, descend to its leaf (3.3) and apply the edit to the
   leaf's item list (2.4). Collect the touched leaves.
3. Recompute the open path across the touched range, then re-paginate each
   touched leaf from right to left: rewrite in place if it fits; on overflow
   split into new pages linked after it (allocate free pages first, else
   append and bump `root.NextPage`); free it if empty.
4. Propagate to the parents: insert an entry for each new page right after
   its left neighbour's entry, delete the entry of each freed page, and set the
   separator of every page whose first key changed (and, if that entry was
   first in its index page, the separator of that index page in its parent,
   and so on up). Split an overflowing index page (left part keeps its number,
   right part goes to a new page linked after it, its first entry inserted
   into the grandparent); free an index page that lost its last entry and
   remove its entry; grow the tree with 4.5 when the root overflows.
5. Recompute the `PrevPage`/`NextPage` links of the pages that changed
   neighbours at every level, set `FreeSpace`, stamp SCNs, and write the dirty
   pages under the journal (6.2).

### 7.2 Bulk rebuild (M7 save, compaction, file creation)

Proven by the prototype (`rebuild()`, runs w08, w09):

1. Produce the whole leaf stream from the key tree (section 1).
2. Cut it into pages 2, 3, ... (2.1), filling each page as far as wanted.
3. Build the index bottom-up: the entries of level 1 are (first key, page) of
   every leaf; pack them into index pages; the entries of the next level are
   (first entry, page) of every index page; repeat until one page is left. That
   page is the root: page 1, `PhysLevel` = number of index levels.
4. Number the other index pages after the leaves (or before them, like
   FileMaker), link every level, set `root.NextPage` to the last page, stamp
   SCNs, write page 0 with `0xAC` and the final SCN.

FileMaker's `--copyCompress` output has exactly this shape (9.3).

---

## 8. What FileMaker checks (a test checklist)

FileMaker runs these checks on an unclean open and in `FMDeveloperTool
--checkConsistency`. The wording in quotes is the engine's
own diagnostic text (`HPosInfo::VerifyAllPages` `0x15b4d10`, `HPage::VerifyPage`
`0x15a9180`, `HPage::VerifyIndexPage` `0x15aadd0`,
`HPage::VerifySlotMatrix` `0x15aa070`, `HBFile::IsRootPageSane` `0x153cac0`).
A writer's test suite should assert all of them; the prototype's checks in
`re/work/hbam-writer/proto/` do. (High)

**File level**

- Page 1 can be read and is not free ("ROOT page could not be pinned / is
  marked free"); its level is 1..16; `root.NextPage + 1` equals the page count
  of the file ("Last page # doesn't match EOF page #").
- Every page is either reached through the index or free ("indexedCount plus
  freeCount ... doesn't match page count"); a page in use is not on the free
  list and every `0xFFFF` page is ("Page is incorrectly on the free list",
  "Page # not present in free list").
- For every page, looking up its first and its last entry through the index
  leads to that page ("Index does not lead to first/last entry of page #").
- The links of each level are consistent ("Next/Prev link mismatch at lvl").

**Each page** (`VerifyPage`)

- `PhysLevel` is at most 16 and not `0xFFFF` for a used page.
- `PrevPage`/`NextPage` point to existing pages that are not free, are not the
  page itself, are not shared with another page, are at the same level, and
  point back ("Prev page %u not doubly linked, its next is %u").
- Continuity with the next page at the same level: the next page re-pushes
  exactly the path still open, without re-opening it ("Lack of BH/EH
  continuity with next page", "Lack of key continuity with next page").

**Inside a page** (`VerifySlotMatrix`)

- No empty level; correct begin/end transitions ("Bad BH/EH transition"); no
  slot with both data and children; each child list belongs to one parent and
  child positions increase; no chunk running off the page ("Data offset
  points off the page", "Data too large"); key segment length below 220 ("Key
  too large"); no `0x80`/`0x40` in an index page; every non-last level has
  children and the lowest has none; at least one leaf ("No first leaf on
  page"); and **`FreeSpace` exactly `4076` minus the bytes walked** ("FreeSpace
  of %hu is incorrect, should be %hu").
- "Keys out of order" exists but did not fire when two sibling leaves inside
  one page were swapped (m11): the in-page order check is not part of the
  normal verification. Lookups still depend on the order, so a writer must
  keep it and its own tests must check it.

**Index entries** (`VerifyIndexPage`)

- Each child page exists and is not free; no child is referenced twice; each
  separator is within the bounds of 3.2.

**Not checked** (mutations that passed): the clean-close flag (m01), the
superblock SCN (m06, n3, n4), page `mModCount`/`mLogSeqNum` (m04, m05, m07),
slack bytes after a page body (m15), the content and links of free pages
(n5), in-page key order (m11), minimal chunk codes (w10).

### 8.1 Mutation results (FMDeveloperTool 26.0.3 `--checkConsistency`)

Single changes to a copy of Contacts/en (`m*`) or of the prototype's w06
(`n*`); "damaged" = error 805, "File is damaged, use Recover".

| file | change | result |
|---|---|---|
| m01 | `0x800` = 0 (unclean) | passes |
| m02 | `root.NextPage` one short | damaged (0 bad blocks) |
| m03 | a leaf's `FreeSpace` off by one | damaged, 1 bad block |
| m04, m05 | a leaf's `mModCount` 0 / `0x7FFFFFFF` | passes |
| m06 | superblock SCN 0 | passes |
| m07 | a leaf's `mLogSeqNum` 5 | passes |
| m08 | an all-zero page appended (root covers it) | damaged, 1 bad block |
| m09 | a free page appended, `root.NextPage` raised | passes |
| m10 | a free page appended, root not raised | damaged |
| m11 | two sibling leaves swapped inside a page | passes (not detected) |
| m12 | a leaf's `NextPage` skips one page | damaged, 2 bad blocks |
| m13 | a separator below the previous page's last key | damaged, 2 bad blocks |
| m14 | a separator above the child's first key | damaged, 2 bad blocks |
| m15 | garbage in a leaf's slack | passes |
| n1 | a level-1 index page's `NextPage` zeroed | damaged, 1 bad block |
| n2 | a free page turned into an unlinked empty leaf (orphan) | damaged, 1 bad block |
| n3 | unclean flag and SCN 0 | passes |
| n4 | clean flag, SCN below the pages' SCNs | passes |
| n5 | a free page with garbage body and links | passes |
| n6 | a page cut between a push and its `0x80` | damaged, 1 bad block |

---

## 9. Verification performed (2026-10-28)

### 9.1 Method

- **Corpus round trip.** The prototype parses and re-encodes every leaf page
  (8,055) and every index page (72) of the 72 corpus files from the rules in
  sections 2 and 3: all byte-identical.
- **Prototype writer** (`re/work/hbam-writer/proto/writer.py`,
  `scen1.py`, `scen2.py`): starts from a copy of Contacts/en (or of a
  password-protected copy of the test project for hosting) and edits keys
  under a private top-level key `4F 4D` ("OM"), which sorts between catalog
  `41` and the first table `80 02`, so the splits land in the middle of the
  key space.
- **om-hbam**: every output is read with the om-hbam crate (a throwaway
  binary, `re/work/hbam-writer/omcheck`): leaf chain, all slots, key tree,
  catalog and records.
- **FileMaker**: `FMDeveloperTool 26.0.3.309 --checkConsistency` and
  `--copyCompress` on copies in a scratch directory on the lab server;
  hosted opens with `fmsadmin` (9.4).

### 9.2 Prototype outputs (all: om-hbam reads them, `--checkConsistency` "0 bad blocks")

| file | what it does | pages | levels (count) | free |
|---|---|---|---|---|
| w01 | no logical change, root rewritten with a new SCN | 140 | 0:138 1:1 | 0 |
| w02 | rename a layout (value grows in place) | 140 | 0:138 1:1 | 0 |
| w03 | insert 7,000 values of 300-500 bytes as one subtree: multi-way leaf split, root overflow, third level | 880 | 0:873 1:5 2:1 | 0 |
| w04 | w03 + 300 single inserts between existing keys, 10 chunked values of 5-25 KB, a non-ASCII text node, a 16-segment path, a 219-byte key | 1,279 | 0:1152 1:125 2:1 | 0 |
| w05 | w04 + 1,500 single deletes: emptied leaves and index pages freed | 1,279 | 0:999 1:98 2:1 | 180 |
| w06 | w05 + 500 new values elsewhere and 50 inline values replaced by chunked ones: freed pages reused | 1,279 | 0:1083 1:98 2:1 | 96 |
| w07 | w03's data with every split at half a page | 1,705 | 0:1698 1:5 2:1 | 0 |
| w08 | bulk rebuild of w06 (7.2), index pages after the leaves | 835 | 0:831 1:2 2:1 | 0 |
| w09 | bulk rebuild of w03 at half fill | 1,769 | 0:1760 1:7 2:1 | 0 |
| w10 | one Contacts leaf re-encoded with non-minimal codes | 140 | 0:138 1:1 | 0 |

The same w03..w07 edits on
the password-protected base produced files of 881..1,706 pages with the same
shapes.

### 9.3 FileMaker rewrites the files and keeps every key

`--copyCompress` (account "Admin", empty password) read w03 and w06 through
FileMaker's engine and wrote compacted copies. A digest over every key and
value under `4F 4D` is identical in FileMaker's copies and the prototype's
files (7,002 and 6,690 items). FileMaker's copies are themselves three-level
trees (root level 2 with two entries, level-1 pages 2 and 3 linked to each
other, leaves from page 4 in order, pages packed to within a few bytes, every
separator the exact first key, superblock SCN = largest page SCN), the
FileMaker-written multi-level samples that file-format.md lacked.

### 9.4 Hosted open by FileMaker Server

On the lab FileMaker Server (26.0.3), opened by ID with `fmsadmin`:

- Contacts-based copies (account without password): refused with event 1017
  "cannot be opened because it is insecure" (the server requires
  password-protected files). The unclean one was still verified and compacted
  first (event 194, file shrank by exactly its 96 free pages, all keys kept).
- Test-project-based w06 (clean): event 184 "Opened database", status
  Normal; closed with no byte of the file changed.
- Test-project-based w06 with `0x800 = 0`: event 194 "was not closed
  properly; performing consistency check on 1279 blocks...", event 633
  "consistency check succeeded", event 184, status Normal. After close: 1,184
  pages (the 96 free pages removed by moving pages from the end into the
  holes, links and index entries updated, `root.NextPage` lowered), flag `AC`
  and a new SCN, `[2, 8]`/`[2, 9]` updated, every other key unchanged.

### 9.5 Changes made on the lab server, all reverted

- Created `/tmp/openmaker-wtest/` (template and prototype copies, FileMaker's
  output) and removed it after the runs.
- Copied four files named `openmaker-wtest-*.fmp12` into
  `Data/Databases/`, opened two of them (the other two were refused), closed
  them by ID, and deleted all four. FileMaker Server had created an empty
  `Data/Databases/RC_Data_FMS/openmaker-wtest-*` folder for each; removed.
  `fmsadmin list files` shows none of them; no other hosted file was opened,
  closed or written.
- Left in place (not revertible): the Event.log lines of these opens and
  closes.

---

## 10. Open questions

1. FileMaker Pro (desktop) on an unclean local file: it uses the same
   `HBFile::Open` path, so it should verify and either open or report error
   805, but this was not run on the Mac. (Med)
2. Whether anything in FileMaker Server (persistent cache, progressive backup)
   misbehaves when page SCNs go backwards; the checker ignores them. Keep them
   increasing. (Low)
3. Text values over 1000 bytes with the 8-byte text header: no sample; the
   1000-byte piece rule of binary values is assumed. (Low)
4. Which flag enables `VerifySlotMatrix`'s in-page key-order check (it did not
   fire for m11). Irrelevant to a correct writer. (Low)
5. Non-minimal chunk codes passed the checker (w10); whether every engine
   reader accepts them was not exercised beyond `--checkConsistency`. Write
   minimal codes. (Med)
6. The numeric keys of `KModCount`/`KDataModCount` and when FileMaker bumps
   them (5.3): catalog workstream.
7. Encrypted (EAR) files: same tree with a 4,044-byte page body and an
   encrypted page image (file-format.md 6); not investigated.
