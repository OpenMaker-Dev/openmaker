# FileMaker `.fmp12` on-disk container format (HBAM7)

How a FileMaker Pro 26 database file is laid out on disk, to the depth needed for
a clean-room client to OPEN, READ, round-trip, EDIT, and CREATE valid files. The
same packed-slot buffers travel over the wire (network-protocol.md 5.5, 9), so
this also specifies the record/field encoding carried by `Perform`.

Scope: the container (header, paging, free list), the page/B-tree structure, the
hierarchical key space, the slot/chunk encoding (`HBAMSlot::S_SlotPack` /
`S_SlotUnpack`), integrity, and encryption-at-rest detection. The *semantics* of
individual catalogs (what each field of a table/layout/script record means) stay
with the schema and layout workstreams; this doc maps the key space to them and
stops at the byte format.

Legend: **[O]** observed in shipped files or the binary, **[I]** inference,
**[U]** unknown.

Evidence is cited as: file offsets in the shipped templates; struct field
names/offsets from the FMS 26.0.3 Linux DWARF in
`re/extracted/fms/deb/opt/FileMaker/lib/libDBEngine.so` (read with
`llvm-dwarfdump`); demangled symbol + `nm` address in that `.so` (Ghidra image
base = `nm` address `+ 0x100000`). Decompilations of the cited functions are in
`re/work/fileformat/dec/` (git-ignored). Everything here is cross-checked against
at least two template files and validated by a from-scratch parser,
`tools/fmp12_keytree.py`, that walks the block/key/slot tree of a file with zero
desync.

Primary samples (non-confidential shipped templates, both platform copies
byte-identical): `DynamicTemplates/Contacts/en.fmp12` (573,440 B = 140 pages),
`Tasks/en.fmp12`, `Assets/ja.fmp12`, and `Extensions/<lang>/FMStrs.dls`
(358 pages) - the localized string files use the same HBAM7 container.

---

## 0. One-paragraph summary

An `.fmp12` file is a flat array of 4096-byte **pages**. Page 0 is a fixed
**superblock** (magic `HBAM7`, page size, a version record, the clean-close
flag and file SCN, a file-identity fence). Every other page is a node of a
**B-tree** rooted at page 1 whose leaves hold the data in **key order**, doubly
linked by page number. Each page carries a 20-byte header (`HPgHdr`: SCN high
half, prev/next page, level, free-space, SCN low half) followed by 4076 bytes
of **chunks**. A chunk is a
code byte + optional key segment + optional value; the code byte's bit fields
select key-size and value-size classes, and three top bits push/pop a
**hierarchical key** (a stack of length-prefixed segments). The full key of any
value is the path from the tree root; top-level key segment = catalog (layouts,
scripts, security, fonts, value lists, records, ...). There is **no per-page
checksum** - integrity is structural (link/level/order validation). The exact
chunk stream is both the on-disk page body and the `S_SlotPack` buffer sent over
the network.

---

## 1. File header and global structure

### 1.1 Paging

- The file is an integer number of **4096-byte (`0x1000`) pages** [O, every
  sample is a multiple of 4096]. Page number `p` is at file offset `p * 0x1000`.
  Page 0 is the superblock; pages `1 .. N-1` are B-tree nodes. The page count is
  implicit from the file length (the engine tracks a "paged EOF" through
  `HBFile::GetPagedEOF` / `SetPagedEOF` / `LogPagedEOF`, `nm 0xf55c30 / 0xf55c10 /
  0x153fb40`).
- The page size is not hard-coded: `HBFile::CreateOpen`
  (`nm 0x153e600`) reads `HBEngine::sEngine->mDefaultPageSize` and writes it into
  the file; the usable body per page is `mPageDataMax = pageSize - 0x14`
  (= 4076 for a 4096 page; see 2.1). All shipped files use 4096. [O/I]

### 1.2 Superblock (page 0)

Built by `HBFile::CreateSmallBlockHeader(bool clearInitialBlocks, uint32
pageSize)` (`nm 0x153bbf0`) into a 512-byte buffer, then padded to a full page.
Byte map [O, identical across Contacts/Tasks/Assets except the file-name field]:

| offset | bytes (Contacts/en) | meaning |
|--------|---------------------|---------|
| 0x00-0x03 | `00 01 00 00` | constant |
| 0x04-0x0e | `00 02 00 01 00 05 00 02 00 02 c0` | fixed format-signature (same in every file) |
| **0x0f-0x13** | `48 42 41 4d 37` = **`HBAM7`** | container magic |
| 0x14-0x17 | `00 00 10 00` | **page size**, u32 big-endian (`0x1000`) |
| 0x200-0x227 | `00000000 00000001 001e 000e 00` `HBAM2125JAN11` `c1 02 48 08` `Pro 12.0` `c0 c0` | version block: a u32 0, a u32 1, the record's length (u16 30) and the record, which holds the engine build tag `HBAM2125JAN11` and the format version `Pro 12.0` (the "fmp**12**" format); the same 40 bytes in every file |
| 0x800 | `ac` | **clean-close flag**: `0xAC` = closed cleanly, `0x00` = written since open (or crashed); every shipped file has `ac` [O, `HBFile::WriteCleanCloseAndSCN` / `ReadCleanCloseAndSCN`; hbam-writer.md 5.1] |
| 0x801 | `00` | always 0 [O] |
| 0x802-0x809 | u64 BE | **file SCN** (system change number): Contacts `0x1d66`=7526, Tasks `0x24d0`=9424, FMStrs `0x0149ec`=84460. Equals the largest page SCN (2.1) in all 72 corpus files. (Formerly described here as a u32 "global counter at ~0x806", which is its low half.) [O] |
| 0xc00-0xfff | `EAFI:RC\rR,<name>/\r` ... spaces ... `\rCR:IFAE` | **file-identity fence**: `EAFI:RC`, CR, `R,`, the name the file was created under in UTF-8, `/`, CR, spaces to 0xff7, then CR and `CR:IFAE` ending the page. The name is not the file's current name nor a table's: "Asset Tracker" in Assets (tables Assets, Vendors, ...), "Contact" in Contacts, "資産トラッカー" in Assets/ja, and "Content Management" in the German copy too. Catalog `[38]` holds the same name followed by `/` [O: the 66 starters] |

Magic detection: `HBFile::IsHBAMFile(UniversalPath)` (`nm 0x1539a90`) opens the
file through the normal `HBFile::Open` path and reports success by file type
(`mHBType`), i.e. a valid superblock + root page, not just the 5-byte string.
A cheap sniff for a clean implementation is the `HBAM7` bytes at **0x0f** plus the
page size at 0x14; a full validity check parses the root page (see 2.3, 5).
Only the flag and the SCN change when FileMaker writes a file; every other byte
of page 0 stays as created (hbam-writer.md 5.1). So the page 0 of a new file
is these constants, the page size, the fence around its name, and the flag
and SCN: built that way from each starter's fence name, it equals the
starter's page 0 but for the SCN in all 66 files [O:
`crates/om-hbam/tests/templates.rs`, `new_superblock_matches_every_template`].
The fence holds a name of at most 1004 UTF-8 bytes, and a name containing
`/` or a line break would end it early.

### 1.3 Root page

- The tree **root is always page 1**: the engine finds it by that number
  (`HPosInfo::GotoViaIndex`, `SetToIndexRoot`, `HBFile::ScanForFreeList`,
  `IsRootPageSane` `nm 0x153cac0`), and a root split swaps page numbers so the
  new root stays at page 1 (`HPosInfo::SplitRoot`). It is always an index page
  (`PhysLevel` 1..16). FileMaker's own three-level copies keep it at page 1
  [O, hbam-writer.md 4.1, 4.5].
- **`root.NextPage` is the last page number** of the file (page count - 1),
  not a sibling link: all 72 corpus files, and `IsRootPageSane` requires it.
  The engine allocates new pages at `root.NextPage + 1` [O, hbam-writer.md 4.1].
- The **leaf chain head** is the level-0 page with `PrevPage == 0`. It is page 2
  in every shipped file, but FileMaker's compacted three-level copies start the
  leaves at page 4; do not assume page 2 [O].

### 1.4 Free list

- A freed page is marked `PhysLevel = 0xffff` (2.1); nothing else in it
  matters. **There is no on-disk free list**: `HBFile::ScanForFreeList`
  (`0x153f320`, called by `HBFile::ForceOpen` `0x153f040`) scans pages 2..last
  at open and collects the `0xffff` pages in
  an in-memory bit list (`AddFreePageNum` `0x15423b0`, `IsFreePage`
  `0x1542410`); new pages come from the lowest free page, else from the end of
  the file [O].
- Every page must be reachable through the index or free; FileMaker's own
  splits leave free pages behind, and FileMaker Server removes them when it
  verifies an unclean file (hbam-writer.md 4.2).
- All shipped templates are **compacted**: 0 free pages, only levels {0,1}
  present [O, scanned across all six templates + .dls].

---

## 2. Page / block structure

### 2.1 Page header `HPgHdr` (20 bytes)

On-disk page = DWARF `Draco::HPage::CompletePage` (byte_size `0x1000`): member
`mHeader` at `+0x00`, member `mFileData` at `+0x14`. The header is
`Draco::HPgHdr` (byte_size `0x14`), all fields **big-endian** [O, DWARF
`HPgHdr` DIE; confirmed byte-for-byte against samples]:

| off | size | DWARF field | meaning |
|-----|------|-------------|---------|
| 0x00 | 4 | `mLogSeqNum` (`atomic<uint32>`) | **high 32 bits of the page SCN** (not a log sequence number, despite the name); 0 in every file because no file has reached 2^32 changes |
| 0x04 | 4 | `PrevPage` (`HPageNum`=u32) | previous sibling page at this level; 0 = chain head |
| 0x08 | 4 | `NextPage` (`HPageNum`) | next sibling at this level; 0 = chain tail |
| 0x0c | 2 | `PhysLevel` (u16) | B-tree level: **0 = leaf (data)**, `>0` = index; **`0xffff` = free page** |
| 0x0e | 2 | `FreeSpace` (u16) | unused bytes remaining in `mFileData` |
| 0x10 | 4 | `mModCount` (`atomic<uint32>`) | **low 32 bits of the page SCN**: the file SCN at the page's last modification (`HFrame::SetDirty`, `HPgHdr::SetModCount`; hbam-writer.md 5.2). Not checked on open |

Body: `mFileData` is `0x1000 - 0x14 = 4076` bytes. **Used bytes =
`mPageDataMax - FreeSpace`**, where `mPageDataMax = pageSize - 0x14 = 4076`
(unencrypted). Verified: Contacts page 2 `FreeSpace=1181`, used `= 4076-1181 =
2895`, and parsing consumes exactly 2895 bytes [O]. Encrypted files set
`mPageDataMax = pageSize - 0x14 - 0x20 = 4044` (2.4, 6).

My earlier empirical read of five u32 words matches this exactly: word0=LSN,
word1=prev, word2=next, then (level:u16, freespace:u16), then modcount.

### 2.2 Levels and the doubly-linked chains

Pages at each level form a **doubly-linked list in key order** via
`PrevPage`/`NextPage` [O]. A file therefore has one chain per level:

- Contacts/en: level 1 = {page 1} (root/index), level 0 = 138 leaf pages chained
  `2 -> 78 -> 79 -> ... -> 6`. [O]
- FMStrs.dls: level 1 = {page 1}, level 0 = 356 leaves. Still 2 levels. [O]

`PhysLevel` max is `0x10` (16); `VerifyPage` rejects `level > 0x10`.
`FromDiskFmt` builds the slot matrix for key levels 1..16, so a key path has at
most **16 segments** (a push at depth 16 fails outside recovery) [O,
hbam-writer.md 2.3].

### 2.3 Index (level > 0) pages: the key-to-page map

Index pages use the size classes of section 4 but a different hierarchy
encoding: each index page is a self-contained prefix trie of (separator path,
child page) entries. Bit `0x20` marks an interior slot (a shared segment),
other slots are terminals whose value is the **4-byte big-endian child page
number**, and a code with both top bits set (`0xC0`) marks the **last slot of
its level**; `0x80`/`0x40` never occur. Separators may end in the fence keys
BH (`00 01`) or EH (`00 ff`), written as a zero-length key plus the sentinel
byte. Each separator is the full key of the child's first slot (any key above
the previous child's last key and not above that first key is valid). The
complete grammar, the separator rule and the lookup are in
[hbam-writer.md](hbam-writer.md) section 3; that grammar re-encodes all 72
index pages of the corpus byte for byte [O].

**For a complete read, the index is optional**: walking the level-0 leaf chain
from the head (page 2) via `NextPage` enumerates every key/value in sorted order.
`tools/fmp12_keytree.py` does exactly this and recovers the whole tree with no
desync on all tested files. A writer, however, must keep the index consistent
with the leaves (correct child pointers and separator keys) or the engine's
tree-descent lookups will miss data.

### 2.4 In-memory vs on-disk (`FromDiskFmt`)

`HPage::FromDiskFmt(bool inRecover)` (`nm 0x159d1f0`) converts a raw page into the
in-memory `HPage` (byte_size `0x1060`): the 4096 on-disk image `mPage`
(`CompletePage`), then `mPageDataMax` (u16 at `+0x1000`) and `mSlotMatrix`
(`HSlotMatrix` at `+0x1008`). `mSlotMatrix` is a per-level directory of `HSlot`
entries (`{ushort mChildSlot; ushort mDataOffset;}`, 4 bytes) rebuilt by walking
the chunk stream - it is **not stored on disk**, so a reader reconstructs it and
a writer never serializes it. `ToDiskFmt` is the inverse. A page that is a chain
head with `mFileData[0] == 0` is treated as empty [O, `FromDiskFmt`].

---

## 3. The key space (`HBAbstractKey`, hierarchical "paths")

### 3.1 Key representation

A key is a **sequence of length-prefixed segments** forming a path from the tree
root. In a slot, a segment is `[u8 segLen][segLen bytes]`. The in-memory
`HBAbstractKey` (DWARF: abstract base, concrete `HBMaxKey`) stores
`[u8 totalLen][segment bytes...]` and `S_SlotUnpack` rebuilds it as a push/pop
stack (3.3).

`CmpHbamKey(uchar* k1, uchar* k2)` (`nm 0x159a370`) orders keys: byte 0 is the
total length; with both lengths nonzero it compares `min(len1,len2)` payload bytes
from index 1 and breaks ties by length. Length-0 keys are the fence sentinels:
`00 01` = begin (BH, lowest) and `00` followed by any other byte = end (EH,
highest); FileMaker writes `00 ff`. Fences occur only in index pages (2.3) [O].

### 3.2 Key ↔ path and prefix compression

Within a page the chunk stream descends/ascends a key hierarchy so shared
prefixes are stored once (the `HSlotMatrix` levels). Each page's stream begins by
**re-establishing the full path to its first key** with a run of push chunks, so
every page is self-describing (Contacts page 2 opens `80 20 02 ...` = begin
marker, push segment `02`) [O]. Nesting depth per key is up to 17 segments.

### 3.3 Key reconstruction (`S_SlotUnpack`)

`HBAMSlot::S_SlotUnpack(uchar* netBuffer, uint32 start, uint32 size,
HBAbstractKey& curKey, HBPosition* id)` (`nm 0x1598b00`) streams chunks and
maintains `curKey` as a segment stack: a chunk that opens a level **appends** its
segment; a `0x40` chunk **pops** the last segment; the value of a terminal slot
belongs to the current full path. This is the same routine used on the wire, so
the network record decoder and the file reader share one implementation.

### 3.4 Top-level key map

Top-level segment = catalog/section. The catalog-key constants are
`Draco::K*` objects (type `HBKey<(uchar)'\x02'>` = 2-byte keys) in `.bss`,
initialized at load by `_GLOBAL__sub_I_DBGlobals.cpp` (`nm 0xa848a0`); their
numeric values are not stored in the binary image, so the map below is
**identified from file content** (confidence noted) and cross-referenced with the
constant names (`KLayoutDefs`, `KScriptDefs`, `KThemeDefs`, `KStyleDefs`,
`KFontList`, `KValueLists`, `KTableCatalog`, `KTableAliasCatalog`,
`KRelationCatalog`, `KMetadataDefs`, ...) and layout-model.md 10.1. All values
below are from Contacts/en unless noted; text is XOR-0x5A decoded (3.5).

| top key | holds | confidence / evidence |
|---------|-------|------------------------|
| 2 | `KStatus`: file metadata & options, format version (`2/3` = "12.0"), the file's locale and formats (`2/0d`, file-metadata.md 2), document preferences and file script triggers (`2/18`, file-metadata.md 3), embedded icons/images (`2/6168/.../PNGf`), page setup and print settings (`2/87`, `2/88`) | High [O] |
| 3 | schema: `3/16` base tables, `3/17` table occurrences (see 3.6) | High [O] |
| 4 | **layouts** - `LayoutDef` / `LayoutObjectStore` / `PartDef` protobuf; `4/5/N/16` = layout names ("Contacts","Contact List","Contact Details") | High [O] (KLayoutDefs) |
| 6 | **themes & styles** - `ThemeStyles`/`ThemeMetadata` protobuf + border-image PNGs; largest catalog (~296 KB) | High [O] (KThemeDefs/KStyleDefs) |
| 16 | custom functions (`KGlobalFunctions`) | High [O, file-metadata.md 1] |
| 17 | scripts & script steps (names "Sort by Name", etc.) | High [O] (KScriptDefs) |
| 23 | security: `23/1` accounts ("Admin"; members `23/1/5/<id>`), `23/2` privilege sets ("[Full Access]","[Data Entry Only]"), `23/3` extended privileges ("fmapp","fmxdbc","fmwebdirect","fmodata","fmxml","fmreauthenticate10") | High [O] |
| 25 | fonts ("arial,sans-serif") | High [O] (KFontList) |
| 26 (`1a`) | the file's own icon image, a binary data tree (`MAIN`, `PNGf`, `SIZE` streams; containers.md 2.3) | Med: [O] the stream tree in every starter file and the test project; [I] that it is the icon, from the constant `KOrigThumbImg` = `1a` (catalogs/hbkey-constants.csv) |
| 31 (`1f`) | the file library: layout graphics, icons and button glyphs, `31/5/<id>/<4cc>` | High [O]: the members' SVG and PNG streams in every starter file (library-images.md); the path from `DBLibrary::GetCatalogKeyPath` with `KLibrary` = `1f` (containers.md 1.1) |
| 32 (`20`) | external data sources, `32/5/<id>`, `10` the name, `82` the file reference | High [O]: a hosted multi-file solution's catalogs (file-metadata.md 1.1); `KFileReferences` = `20`. No starter file has one |
| 33 | value lists ("Phone Type"; `KValueLists`, file-metadata.md 6) | High [O] |
| 38 (`26`) | base directories of external container storage: one in every starter file, named after the file ("Contact/", "Tasks/"); **not** the base-table list, which is `3/16` (3.6) | High [O]: every starter file; `DBBaseDirectoryCatalog` with `KBaseDirectories` = `26` (containers.md 8.1) |
| 64 (`40`) | custom menu sets: an index and one member in every starter file | Med: [O] the catalog in every starter file; [I] what it holds, from the constant `KCustomMenuSets` = `40` (catalogs/hbkey-constants.csv) |
| 65 (`41`) | custom menus, `65/5/<M>/37` the install-when calculation | High [O]: the calculations in the corpus (calc-encoding.md 8); `KCustomMenus` = `41` |
| two- and three-byte serial keys `80 01`... (32769 = `0x8001`, ...) | a base table's data: records, field value indexes and its field catalog `<T>/3` (3.6); the table's key is its member key in `3/16` | High [O]: every starter file's tables (schema-path.md 5.2) |
| `d0 00 01`... (13631489 = `0xD00001`, ...) | a table occurrence's key: its saved found set, sort, current record and last find `<O>/7` | High [O]: the starter files and FileMaker Pro 26's saves (file-metadata.md 11) |

Tables are numbered from the serial key `80 01` and occurrences from
`d0 00 01` (schema-path.md 5.1-5.3; file-metadata.md 1); `om_hbam::design_paths`
takes a top-level serial key with an id from 128 up to 1,065,088
(`d0 00 00`) for a table and one from there on for an occurrence. That
the two never meet is inferred from those first ids: no file with
anywhere near 1,065,088 tables was seen [I].

Sub-key convention within a catalog [O, pattern]: `.../1/...` is typically the
name/ID index (entries keyed by collated name, value = entry id) and `.../5/...`
the entry definitions, keyed by the member id as a serial key (the id
FileMaker shows: `80 0d` is 141, file-metadata.md 1); standard per-entry sub-keys include `16` = name, `64513`
(`0xFC01`)/`64514` (`0xFC02`) = creator/modifier account. Deeper per-catalog
semantics belong to the schema/layout workstreams.

### 3.5 Text obfuscation (not a format layer)

Catalog text (names, strings) is stored **XOR 0x5A per byte** - the same SCSU/XOR
codec used on the wire (network-protocol.md 4.2). Protobuf blobs (layouts,
themes) are stored **raw** (not XOR'd). So a reader XORs character fields but
parses protobuf values directly [O, confirmed: "Admin" = `1b 3e 37 33 34`,
`0x1b^0x5a='A'`; layout protobuf begins with real tag bytes].

### 3.6 Catalog, value, and list-bracket conventions (validated by `om-hbam`)

Established from the 132 shipped templates plus the six FileMaker Server
samples (`re/extracted/fms/deb/opt/FileMaker/FileMaker Server/Data/Databases/.Sample/<lang>_FMServer_Sample`,
the only shipped files with records) and enforced by
`crates/om-hbam/tests/templates.rs` (782,233 chunks, zero desync) [O unless noted]:

- **List brackets (refines 4.2).** `0x80` follows exactly the push that
  creates a node, i.e. it opens that node's child list; the leaf chain starts
  with `0x80` (the root list). A page continuing an open node re-pushes the path
  without `0x80`. `0x40` closes the current list, so the chain's last byte (on
  the tail page) is one extra `0x40` closing the root. Counts over 72 files:
  65,520 new-node pushes all followed by `0x80`, 33,717 page-start re-pushes
  never.
- **Code choice.** Every code byte is the minimal size class for its key and
  value (key 1/2/3 bytes fixed else length-prefixed; value 1/2/4/6/8 fixed,
  else u8- then u16-prefixed; class 0 for no value), so a writer's codes are
  determined by its data.
- **Key order.** Siblings sort by plain byte-lexicographic order of the segment
  (shorter prefix first), consistent with `CmpHbamKey`.
- **Chunked values (refines 4.3).** A value too large for one chunk, and any
  non-ASCII text even when short, is a node with child `0` = header (u32 BE
  total byte length; text adds a u32 character count) and children `1..n` =
  pieces (1000 bytes each when large). Text = XOR 0x5A + SCSU: Japanese
  `4f a5 a3 b5` decodes to "タスク".
- **Schema.** `3/16/5/<table>`: base table, `16` = name; the table id
  (>= 0x8000) is also the table's top-level data key. `3/17/5/<occ>`: table
  occurrence (ids `0xD000xx`), `16` = name, `2` = definition (opaque).
  `<table>/3/5/<field>`: field, `16` = name, `2` = 26-byte options. Options
  byte 0 = kind (1 normal, 2 calculation, 3 summary) and byte 1 = data/result
  type (1 text, 2 number, 3 date, 4 time, 5 timestamp, 6 container) [I, from
  field names across samples].
- **Records.** `<table>/5/<record>/<field>` = the stored value. Observed values
  are masked text, numbers and dates included in their stored text form
  ("2/17/2014", ja "2014/02/17"). Field ids absent from the field catalog (e.g.
  `252`) are engine bookkeeping. Repeating and container values: no sample [U].
- **Layouts.** `4/5/<layout>`: `16` = name ("--" = separator), `2` =
  `LayoutDef`, `3/<part>/2` = `PartDef`, `7` = `LayoutObjectStore` (chunked;
  an empty value for a blank layout).
- **Index pages** use the trie grammar of 2.3 (the `0x50` "code" once seen
  on the FMS sample's root was a misparse: a zero-length fence key occupies two
  bytes). The leaf chain remains the complete read path.
- **Root `NextPage`** is the file's last page number in all 72 files (1.3).

### 3.7 Catalog keys no other spec explains

`om_hbam::design_paths` puts a path in a group only where a spec says what
it holds (docs/server-storage.md section 4). This section is the spec for
the keys of catalogs it otherwise covers that no other spec explains:
what the files show, what is known of each, and what is not.

Evidence:
- [O] the 66 FileMaker Pro 26 starter files (six solutions in 11
  languages), read by `crates/om-hbam/tests/design_paths.rs`; counts are
  over all 66 unless said;
- [O] eight copies of starter files that FileMaker Pro 26 saved, after
  one edit each to a field's options or a layout object, or none; none of
  the edits touched these catalogs, and every value below is one the
  starter files have;
- [O] the key constants' numbers ([catalogs/hbkey-constants.csv](catalogs/hbkey-constants.csv)),
  several of which share one number, so a name is [I] where several fit.

No FileMaker binary was read for this section: the meanings rest on the
files and on what other specs record.

| Path (hex) | Observed [O] | What is known | Group |
|---|---|---|---|
| `[11, 5, S, 13]` | u32 0 on all 330 scripts, folders and separators (members without `04`) included, on the 32 scripts of the saved copies, and on the 361 of script-steps.md 1 | the constants with this number are `KCachedCountIgnoreFolders`, `KCompatibility`, `KDragSize` and `KPrefixKey` (also a query program leaf, query-program.md 2.2); which one the script catalog writes, and when it is not 0, is [U] | none |
| `[41, 5, M, 02]` | an 8-byte leaf on all 264 custom menus, the 24 of each Contacts file (no other solution has custom menus), the same 24 values in every language: two u32s. The first differs per menu: `06` `[Format]`, `08` `[Scripts]`, `09` `[Window]`, `12` `[Font]`, `13` `[Spelling]`, `14` `[Size]`, `15` `[Style]`, `16` `[Align Text]`, `18` `[Line Spacing]`, `20` `[Recent]`, `26` `[Show Window]`, `49` `[Hosts]`, `95` `[Find/Replace]`, `9b` `[Import Records]`, `9c` `[Sharing]`, `9d` `[Manage]`, `9f` `[Go to Record]`, `a1` `[Go to Layout]`, `a8` `[Save/Send Records As]`, `ac` `[Go to Request]`, `b5` `[Text Color]`, `c7` `[Saved Finds]`, `c8` `[Send]`, `d1` `[Favorites]`. The second is `78`, but `68` for `[Go to Record]` and `50` for `[Go to Request]` | each menu is named in brackets after one of FileMaker's own menus and holds no items: its keys are `02`, `10`, the install-when calculation `25`, `d8` and the stamp. [I] the first word names the menu it is a copy of; [I] the second holds four bits of when it is shown (by mode or by platform). Where a custom menu item, and the script step it runs, are stored is not observed: no starter menu has an item (calc-encoding.md 8 places item calculations at `[41, 5, M, n, item, k]`, `n` not given) | none |
| `[19, 5, F, 03]` | text on 73 of 139 fonts: `ArialMT` for the family Arial, `HelveticaNeue`, `HelveticaNeue-Bold`, `-Light` and `-Medium` for Helvetica Neue, `.SFNSText` for `.SF NS Text`, `.SFNS-Regular` for `.SF NS`; none on `arial,sans-serif`, a CSS list | the face's PostScript name, beside the family name `10`: the values are those faces' PostScript names [O]; FileMaker's XML names a font's `fontPSAttr` beside `fontFamily` and `fontCodeSet`, and themes name a font by family and code set or PostScript name (layout-model.md 7.3, 8) | Design |
| `[19, 5, F, 02]` | `01 00` on 84 fonts (`.SF NS Text`, `.SF NS`, `arial,sans-serif`), `01 fe` on 55 (Arial and the four Helvetica Neue faces) | [I] a one-byte value as a key string (length, then the byte, as the indexes' `01 0n`); [I] the font's code set, `fontCodeSet`: the calculation constants number font scripts `Roman` 0 to `Other` 10 (calc-functions.md), and what `fe` is, is [U] | none |
| `[4, 5, L, 16]` | `00 00` on Contacts' Contact Details layout in all 11 languages; FileMaker Pro 26 adds it, with `fc`, to a layout it saves in Layout mode (layout-model.md 10.14, 10.17), and did so on the two saved copies whose layout was edited | the constants with this number are `KGuides`, `KRowSetKey` and `KStyleDefs`; a layout's own styles are `KLayoutStyleDefs` `21`, which every layout has (layout-model.md 10.1), so not this. [I] `KGuides`, the layout's guides, here none | none |
| `[17, 2, 1, 07]`, `[17, 3, 1, 07]`, `[19, 1, 07]`, `[21, 1, 07]`, `[40, 1, 07]`, `[41, 1, 07]` | in each of the 396 catalogs (six in each file) the keys are exactly the members' keys `[.., 05, *]`, every leaf is empty, and the list has no value of its own | a member list: it holds nothing the members do not. `KList` is `07`. That FileMaker rebuilds it from the members rather than reading it is [I] | Derived |
| `[21, 1, 06]` | one leaf per value list, all 264: the key is the list's type as two bytes (`00 00` on the 143 custom lists, whose definition `02` starts with type 0; `00 01` on the 121 lists from a field, type 1; file-metadata.md 6.2), then its name's sort key, as in the index's name list `01`, then `00 00` and, where names differ in case or accents, more weights; the value is the list's key string `01 <key>` | the name list by type, as the field catalog's `06` (schema-path.md 5.5). `KTypeList` is `06` | Derived |

A path in no group keeps its object from having a design revision
(docs/server-storage.md section 9): every script (`13`), every custom
menu (`02`), every font (`02`, which all of them hold) and the layouts
with `16` have none.

**Unknowns.** What `[11, 5, S, 13]` holds and when it is not 0; what the
two words of a custom menu's `02` are; where a custom menu item and the
script step it runs are stored, and so whether a custom menu holds step
data (it can: an item runs a step, which can store a credential); what a
font's `02` is, and what `fe` means; what a layout's `16` is. Each needs
FileMaker Pro writing a known change.

Three index keys have the same form and value as keys a spec names, and
take their rule by that inference: `[6, 1, 20]` (`01` in the 11 Contacts
files) and `[40, 1, 20]` (`01` in every file) as the `20` scripts.md
reads as the Manage dialogs' last view order (`KLastViewOrdering` = `20`)
for scripts, layouts and value lists [I]; `[40, 1, 18]` (`01 01`, naming
the one menu set, in every file) as the themes' index `18`, which names a
theme (layout-model.md 7.5) [I]. The themes' member keys `18` and `19`
are named by the constants `KBaseThemeName` and `KBaseThemeVersion`
(layout-model.md 7.5), whose stored values are [U].

The report (`cargo run -p om-hbam --example design_paths -- <file>...`)
lists the keys in no group, and any new ones, by shape.

---

## 4. Slot / chunk encoding (`S_SlotPack` / `S_SlotUnpack`)

This is the byte format of `mFileData` on disk and of the `OctetSeq` packed-slot
buffer over the wire. `HBAMSlot::S_SlotPack` (`nm 0x1597e80`) copies the page's
chunk bytes verbatim into the network buffer
(`memcpy(netBuffer, mFileData + slot.mDataOffset, ...)`), confirming the two are
the same byte stream [O].

### 4.1 Chunk layout

A chunk is: `[code byte] [key bytes] [value bytes]`. Sizes come from the code byte
exactly as `HBAMSlot::SlotUnpackLengths(uchar* buf, uint32 start, uint32*
keylen, uint32* keyoff, uint32* datalen, uint32* dataoff)` (`nm 0x15989b0`)
computes them [O, decompiled]:

- **Key-size class = bits 3-4 of the code** (`(c >> 3) & 3`):
  - 0 -> 1-byte key, key at offset 1
  - 1 -> 2-byte key, key at offset 1
  - 2 -> 3-byte key, key at offset 1
  - 3 -> variable: `keylen = buf[start+1]`, key at offset 2
- **Value-size class = bits 0-2 of the code** (`c & 7`), value right after the key:
  - 0 -> no value
  - 1 -> 1 byte · 2 -> 2 bytes · 3 -> 4 bytes · 4 -> 6 bytes · 5 -> 8 bytes
  - 6 -> 1-byte length prefix, then that many bytes
  - 7 -> 2-byte big-endian length prefix, then that many bytes
- Total chunk size = `dataoff + datalen`.

### 4.2 Hierarchy / flag bits of the code byte

Top three bits control the key hierarchy (from `FromDiskFmt` and `S_SlotUnpack`)
[O]:

- `code == 0x80` exactly: **begin-hierarchy marker** - 1 byte, no key/value;
  opens a page's key tree / separates runs.
- `code == 0x40` exactly: **end-hierarchy (pop)** - 1 byte; removes the last key
  segment from the path.
- bits `0xC0` both set (`0xC0..0xFF`): **last slot of its level** - the low 6
  bits are an ordinary code (`0x20` included); after this slot (and its
  children) the level closes, as if a `0x40` followed. `FromDiskFmt` strips the
  two bits (`byte &= 0x3f`) and remembers the level as finished. This form occurs
  **only in index pages**, which never contain `0x80`/`0x40`; leaf pages use
  only `0x00..0x3F`, `0x80` and `0x40` (corpus census, hbam-writer.md 2.2 and
  3.1). An earlier version of this section called these codes pushes.
- bit `0x20` set (`0x20..0x3F`): the slot **has children** (a push in a leaf
  page, an interior trie node in an index page); it carries a key segment,
  opens a level, and never carries a value.
- `0x00..0x1F`: a plain **leaf value slot** at the current level (key segment +
  optional value), does not open a level.
- Other codes with a top bit set (`0x41..0x7F`, `0x81..0xBF`) do not occur.

Operationally (validated by `tools/fmp12_keytree.py`, zero desync over Mac+Win
templates, a localized variant, and a 358-page `.dls`): maintain a segment stack;
on `0x80` do nothing; on `0x40` pop; on a slot whose code has `0x20` set, read
its key segment and push it; on a `0x00-0x1F` leaf, the key is `stack + this
segment` and the value is the chunk's value field. Index (level > 0) pages are
decoded with the `0xC0` rule above; their terminal values are 4-byte child page
numbers (hbam-writer.md 3.1).

### 4.3 Value typing and large values

- The chunk only carries **raw bytes + a length**; the *type* of a value
  (integer, text, container, protobuf, record id, index key) is determined by its
  **key** (which catalog/sub-key it sits under), not by the chunk. E.g. fixed
  classes 1/2/4/8 bytes are used for counters and ids; class 6/7 length-prefixed
  for text (XOR-0x5A) and protobuf (raw).
- **Values larger than a chunk are split across numbered sub-keys.** A PNG icon
  is stored as `.../PNGf/0` (a size) then `.../PNGf/1`, `/2`, ... each a
  class-7 chunk of 1000 bytes [O]. The engine's `BlobProxy` reassembles them; a
  reader concatenates the numbered segments in order.

---

## 5. Integrity: what a writer must reproduce

**There is no per-page checksum or CRC.** FileMaker validates *structure* only
(`HPosInfo::VerifyAllPages`, `HPage::VerifyPage` `nm 0x15a9180`,
`VerifyIndexPage`, `VerifySlotMatrix`, `HBFile::IsRootPageSane`) [O]. The full
writer specification, including every check FileMaker runs and which fields it
ignores, is [hbam-writer.md](hbam-writer.md); in short, a conforming writer
must:

- keep page 1 as the root index page with `NextPage` = the last page number
  and the file length matching it;
- keep each level's pages doubly linked in key order with the right
  `PhysLevel`, and `FreeSpace = 4076 - used bytes` exactly;
- start every leaf page by re-pushing the open key path (without `0x80`),
  never cut a page between a push and its `0x80`, never put a value on a push,
  and keep keys in order;
- keep every index separator above the previous child's last key and not above
  the child's first key (the first key itself is FileMaker's choice);
- mark every unused page `PhysLevel = 0xffff`, so each page is either indexed
  or free;
- maintain the superblock's clean-close flag (`0x800`) and SCN (`0x802`), and
  stamp written pages with increasing SCNs (`mLogSeqNum`:`mModCount`). FileMaker
  does not check the SCNs; an unclean flag makes it verify the file on open.

The superblock signature, magic, page size, version record and identity fence
never change after creation. The redo log (`HBLogFile`, `HBLogRecord`,
`ApplyRedoLog`) is FileMaker Server's own page-image log, separate from the
base file and not needed to open or write it.

What **round-trips trivially** (copy through unchanged): protobuf blobs (layouts,
themes - and FileMaker preserves unknown protobuf fields, README F25), the
chunk/value bytes, and any catalog a writer does not touch. What must be
**rebuilt on edit**: the affected pages' slot stream + `FreeSpace`, the leaf/index
links if a page splits or merges, and the index separators.

---

## 6. Encryption at rest (EAR)

- **The shipped templates and `.dls` files are NOT encrypted** [O]: text is plain
  XOR-0x5A and protobuf is readable; `PhysLevel`/links parse directly.
- EAR is carried by `HBEncryptionInfo` (DWARF, byte_size `0x88`), passed to
  `HBFile::CreateOpen(uint* permissions, HBEncryptionInfo* encryptionInfo)`.
  Members [O, DWARF]: `mEncryptionType` (`atomic<int8>`, **0 = none**),
  `mIterationType` (`EncryptionIterationType`), `mpEncryptionKeyBlock` /
  `mpEncryptionKeyBlock2` (`HBEncryptionKeyBlock*`), `mEncryptionSalt`
  (`UUniqueID`), `mEncryptionChecksumBlock` (`HBEncryptionChecksumBlock`),
  `mEncryptionHint` (`unistring`), `mSharedID` (`unistring`).
- When encryption is on, `CreateOpen` reduces the per-page body:
  `mPageDataMax = pageSize - 0x14 - 0x20`, i.e. **32 bytes per page are reserved**
  (IV/MAC) and the page body shrinks to 4044 bytes [O, decompiled]. This is the
  detection signal at the page level, alongside `mEncryptionType != 0` in the
  file's stored encryption info.
- **Do not attempt to break EAR.** A clean client should detect it
  (`mEncryptionType`/reserved-tail) and require the password to derive the key via
  the engine's KDF; decryption design is out of scope here.

---

## 7. Parser

`tools/fmp12_keytree.py` (read-only, run `python3 -I`) is the from-scratch
validator for this spec. It:

- pages the file, reads each `HPgHdr`, finds the leaf-chain head,
- walks the level-0 chain and decodes every chunk with the section-4 rules,
- reconstructs the hierarchical key as a push/pop segment stack, and
- prints `key -> value` with length + raw/ASCII/XOR-0x5A preview.

Supporting tools: `tools/fmp12_blocks.py` (raw page-header dump),
`tools/fmp12_analyze.py` (superblock regions + chain tracing),
`tools/dwarf_struct.py` (struct layouts from DWARF). It parses Contacts (138
leaves), Tasks, Assets/ja, and FMStrs.dls (356 leaves) with **zero byte desync**,
which is the empirical proof of the chunk format.

---

## 8. A new file

What a blank file FileMaker Pro 26.0.3 makes (File > Create New, Blank)
holds, compared key by key with what OpenMaker writes for one
(`om_data::new_file`; docs/new-files.md) [O: three blank files made in Pro
on macOS, 2026-11-13, named "Alpha", "Beta Gamma" and "Alpha Name", the
last with one field added; they differ only in the names]. Every member
and catalog index carries a UUID `d8` and the stamp `fc01`-`fc03` of its
creation (schema-path.md 8.1).

| Key | Pro's blank file | OpenMaker |
|---|---|---|
| page 0 | 1.2: the constants, the page size and the fence around the creation name | the same |
| `2` | `03` "26.0" (the starters, made by older versions, hold "12.0"), `05` 1208, `0d` the locale, `18` the document preferences with options `0x8403` (default fields on, file-metadata.md 3.2) and layout 1 as "Switch to layout" (unchecked), `fe` `mInternalCreation`, the open/close history `06`/`08`/`09`, `5d`, and page setup `85`/`86`/`87`/`88` (macOS print records) | the same but for the history, `5d` and page setup, which Pro writes itself at open and close [O: Pro opened OpenMaker's files without them and kept them through its edits] |
| `3` | one base table `80 01` named as the file, its occurrence `d0 00 01` of the same name, no relationship catalog `fb`; the occurrences' index holds an empty `08` and the dependency entries of the five default fields | the same, made by Manage Database's own planner; the dependency entries come out identical |
| `80 01` | five **default fields** (below), with tags `da`; **one record**, `01`, holding what the fields enter for it; the record list `0d`, the last issued record `00` = `01 01`, and PrimaryKey's value index `[80 01, 01, 01, 00]` with its counts `10` = 1 record, 1 key | the same fields and record (the values entered by OpenMaker's record rules); PrimaryKey indexed in Unicode raw, its record's key in the raw form (below) |
| `4` | one layout named as the file: a 110-point top part and a body, a stored table view `0d`, view flags `0x0000000f10f30801` (Form, List and Table enabled, Form first, layout-model.md 4.2), theme `01 01`; its index has `20` = `04` | one body part (layout-model.md 10.6), a stored table view written by OpenMaker (layout-model.md 10.14), the same flags, theme and `20` |
| `6` | Apex Blue, with a preview image `0e`; the index has `18` = `01 01`, `20` = `01` | OpenMaker's own theme (below), no preview, the same `18` and `20` |
| `11` | the script catalog: an index with only `20` = `04`, and an empty member list | the same |
| `17` | [Guest] (inactive) and Admin (no password) as in local-files.md 2; File Options log in as Admin, with `18` = "4" ("Show sign-in fields even when OAuth or AD FS is enabled", 3.1); the three built-in sets (session-identity.md 2.7); **eleven** extended privileges, the starters' ten and `fmplugin` (`0b`, with a description `03`); the file access catalog requiring authorization (`f0` = 1) with two entries for the file itself (types 1 and 0, account Admin, the file's name; type 0 with a 32-byte `0d`) | the same but: `fmplugin` without a description, and the file access catalog with no entries and `f0` = 0, since what the entries' `0d` hashes is not known [U] |
| `19` | the fonts of Apex Blue: four faces of Helvetica Neue and Arial | the one font `00 00` "arial,sans-serif", `02` = `01 00`, that every starter has; OpenMaker's theme names no font |
| `21` | the value lists: an index with only `20` = `01` | the same |
| `40` | one member with only its UUID and stamp; an index listing it under the empty name, with `18` = `01 01` and `20` = `01` (as in every starter; what it holds is [U]) | the same |
| `41` | the 24 standard menus (`[Format]`, `[Scripts]`, `[Window]`, ...) | an index only, as the starters have it |
| `d0 00 01` | `07`: the window's found set and record (F75), written at close | none |

Pro's blank file has no library `[31]` and no `[38]`, which the starters
have.

**The default fields** (Pro's Manage Database lists them as PrimaryKey,
CreationTimestamp, CreatedBy, ModificationTimestamp, ModifiedBy), as their
definitions decode (`om_data::field_options`):

| Field | Type | Options |
|---|---|---|
| `01` PrimaryKey | Text | auto-enter calculation `Get( UUID )` (stored with that spacing), replacing an existing value; prohibit modification; validation during data entry, no override, not empty, unique; indexing minimal, language 2 (Unicode, raw); tags `#^FMI* #_FMI_0 ` |
| `02` CreationTimestamp | Timestamp | auto-enter creation timestamp; prohibit modification; not empty, four-digit year, no override; tags `#_FMI_0 ` |
| `03` CreatedBy | Text | auto-enter creation account name; prohibit modification; not empty, no override; tags `#_FMI_0 ` |
| `04` ModificationTimestamp | Timestamp | as CreationTimestamp, on modification |
| `05` ModifiedBy | Text | as CreatedBy, on modification |

Each has a comment `03`; none has a next serial value `0c`. The
calculation's context `04` is stored as `01 00`. Pro's PrimaryKey index
holds its record's key as `01` and the UUID's UTF-16BE code units
(observed), the raw form of query-program.md 7.2. OpenMaker writes the same
definitions, PrimaryKey's index language Unicode raw (2) included (#777),
and keys its record's PrimaryKey the same way; its planner stores the
calculation's context as the occurrence (`03 d0 00 01`), as for any
auto-entered calculation it plans, and the comments are its own.

**The first record.** The Browse window of a new file shows one record,
made with the file: PrimaryKey a version 4 UUID in capitals, both
timestamps the time of creation as text in the file's format ("11/13/2026
12:37:53 PM"), both accounts "Admin". In Pro's files the modification time
is later than the creation time, by the time Manage Database was open.

**Themes.** A theme FileMaker installs has its own id
(`com.filemaker.theme.apex_blue`); one made in a file has
`com.filemaker.theme.custom.<UUID>` (capitals, `_` for `-`) and the group
`custom` (Meetings). `0f` is 1 on a desktop theme (Enlightened), 2 on a
touch theme. FileMaker Pro 26 opens a file whose only theme is of the
second kind with styles of OpenMaker's own (OpenMaker Plain), names it in
Layout mode and the Inspector, and keeps it through its own edits [O:
#593, 2026-11-13]. Every theme FileMaker ships gives the text component
(`3`) of the objects that show text a geometry: an edit box's text
`position` 3 (absolute) with insets `117`-`120` and margins, a text
object's text `height` 100%, a button's `width` and `height` 100%, each
with `box-sizing` 1 [O: Apex Blue in the blank files, the starters'
themes]. A theme without it made Pro draw an edit box's border but neither
its value nor its label, nor those of a field Pro added itself, while
OpenMaker drew them [O: #593, 2026-11-14]; with it, see #593 for the
check of the next files.

**`mInternalCreation`** is a count of seconds: in Pro's three files it
grows with their creation times exactly (83 and 35 seconds apart). Its
epoch is about 36.5 days before 1904-01-01: the values exceed the seconds
since 1904 of the files' creation (local time) by 3,155,296, and the
starters' exceed the times of their own first stamps by about as much
[O; the epoch's meaning is U]. OpenMaker writes the seconds since 1904.
Any value works as long as every checksum that hashes it uses the same
one.

---

## 9. Open questions

1. Exact numeric values of the `Draco::K*` catalog keys (they live in `.bss`, set
   by a runtime initializer). Content-based identities in 3.4 are solid for the
   major catalogs; a full table would come from executing/tracing the initializer
   or from the schema worker. The catalogs 26, 31, 32, 38, 64 and 65 and the
   occurrence keys are identified in 3.4, 26 and 64 by their constants' values
   [I]; the keys 3.7 lists are [U].
2. ~~The full index-page chunk grammar~~ Resolved (2026-10-28): prefix trie
   with `0xC0` last-of-level flags and fence keys; byte-exact on all 72 corpus
   index pages (2.3, hbam-writer.md 3).
3. ~~Page split/merge policy, root relocation~~ Resolved: the root is always
   page 1; FileMaker accepts any fill, page order and number of levels that
   keep the structural rules (hbam-writer.md 4). Three-level trees were built,
   checked and rewritten by FileMaker.
4. ~~Superblock counter and `mModCount` enforcement~~ Resolved: the counter is
   the u64 SCN at `0x802`, page SCNs are `mLogSeqNum`:`mModCount`; none of them
   is checked on open (hbam-writer.md 5, 8.1).
5. ~~Redo log~~ Resolved as not needed: FileMaker marks a written file unclean
   (`0x800 = 0`) and verifies it on the next open; the redo log is FileMaker
   Server's own sidecar (hbam-writer.md 6). Its record format was not studied.
6. EAR key derivation and page cipher (intentionally not investigated).
