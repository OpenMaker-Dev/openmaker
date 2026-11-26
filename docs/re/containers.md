# Container fields - insert, store, show, replace, delete

Status: complete for embedded container storage over the network
(2026-10-28): inserting a picture or file into a record, how FileMaker Server
stores it, how FileMaker Pro reads it back (including server thumbnails),
replacing it and deleting it. External storage is described from the engine
code only (section 8). Open items are in section 12. Each claim carries a
confidence (High/Med/Low) and its evidence:

- `om:<rid>` - an exchange in `re/captures/2026-10-28-s06-omserver-n/conn-001`
  (FileMaker Pro 26 inserting a photo into the Photo field of record `03` of
  test project, against OpenMaker Server, which answered op88 with status 3).
  The image is the maintainer's own test photo; this spec quotes its
  structure, never its bytes.
- `cap:<rid>` - an exchange in `re/captures/2026-10-27-s03-direct/conn-000`
  (FileMaker Pro against FileMaker Server, a hosted solution; layouts only
  are described here).
- `live:<run>/<tag>` - a transaction sent by the container probe
  (`re/work/containers/omc`, scripts `re/work/containers/live/c*.txt`) to the
  lab FileMaker Server 26.0.3, file `test project` (file id `0x165`), table
  `80 02` (Contacts), field `03` (Photo, a container field). Request, reply and
  back-channel bytes are in `re/work/containers/live/<run>/<tag>.{req,rep,bc}`;
  op90 downloads are in `<tag>.slots`. The images are synthetic PNGs made by
  `re/work/containers/mkbd.py`.
- `bin:<name>` - the Linux FileMaker Server engine `libDBEngine.so` 26.0.3
  (DWARF; `nm` addresses) or `libSupport.so`; `mac:<name>` - the Mac client
  engine `DBEngine` (x86_64 slice, `nm` addresses).

Notation as in [write-path.md](write-path.md): transaction integers are
big-endian; `[a, b, c]` is an HBAM key path with segments in hex; record,
table, field and library member ids are `HBSerialKey` segments
(query-program.md 2.6); a **key** field in a transaction is written with
`Push(uchar const*)`: its segments as `[u8 seglen][seg]...` followed by a `u8`
total length (table `80 02` = `02 80 02 03`, field `03` = `01 03 02`). "Push
order" lists fields in the order the sender appends them; the receiver pops
from the end (network-protocol.md 4.2). Text values are SCSU with every byte
XOR `0x5A` (network-protocol.md 4.2). A 4-character stream type such as
`JPEG` is written as its 4 ASCII bytes (`4a 50 45 47`).

---

## 0. Summary for implementers

A container value is not stored in the record. FileMaker keeps the data in a
**library**: a catalog of deduplicated, reference-counted **binary data
members**, one library per table at `[T, 1f]`. The record's field holds only
a short text naming the member. FileMaker Pro uploads the data with op88
before it commits the record, and adjusts reference counts itself with op87;
the server never parses a record to count references.

| step | FileMaker Pro sends | the server does |
|------|---------------------|-----------------|
| insert a picture/file into a field | op88 UploadBinaryData (binary-data tree, table key, field key) | MD5-deduplicate into the table library `[T, 1f]`: reuse a member with the same MD5 (count + 1, status `0x1006`) or create member `M` (count 1, status 0); return the member key |
| commit the record | op14 (record, container field = text `hex(M)` + U+E000), op70, op103, op65 | store the record as for any field (write-path.md); nothing container-specific |
| show it | op90/op92 of the record; op92 of `[T, 1f, 05, M]` with a list of stream keys; op90 of `[T, 1f, 05, M, <stream>]`; op96 DownloadThumbnail | answer from the library like any key; op96: decode the image, scale it into the requested box, return PNG (or JPEG) bytes |
| replace it | op60, op88 (new data), op14, op70 (field id listed), op87 `-1` for the old member, op65 | op87: count - 1; at 0 delete the member |
| clear it / delete the record | op60, op87 `-1` per container value, op70, op65 | as above |
| copy a container value (duplicate record, paste from another record of the same table) | op87 `+1` for the existing member instead of op88 | count + 1 |

**Minimum server for "insert, show, delete a photo" in FileMaker Pro:**
op88 (section 3), op87 (section 5), op96 (section 6.3), plus what the read
and write paths already need (op90, op92, op14, op70, op60, op65, op103).
The library lives in the file like any other keys, so a server that already
serves `[T, ...]` subtrees and commits records needs no new read path. The
one hard part is op96, which needs an image decoder and scaler (6.3).

Why FileMaker Pro showed "Unknown Error: -1" (High): om:122 is op88 (37,776 bytes).
OpenMaker Server answered the 2-byte result `00 03`. op88 uses the shared
upload/download framing, whose client loop rejects any result stack shorter
than 23 bytes with error `0xffff` before reading the status
(network-protocol.md 5.6.6c, "Client algorithm" step 2). FileMaker Pro shows
`0xffff` as "-1".

---

## 1. Where container data lives (High)

### 1.1 Libraries

A library is a `DBLibrary` catalog (bin `DBLibrary::GetCatalogKeyPath`
0xf9c000):

| library | key | used for |
|---------|-----|----------|
| file library | `[1f]` (`KLibrary`) | layout graphics, icons, button glyphs (`[1f, 05, 1..8]` in test project are SVG icons) |
| table library | `[T, 1f]` | every container value of table `T`'s fields |

So the Contacts photos of test project go to `[80 02, 1f]`. A table library
does not exist until the first value is stored; op88 creates it (live:c1:
`[80 02, 1f]` missing before, present after). Once created it is never
removed, even when its last member is deleted (live:c5, c7: an empty header
remains). Both libraries have the same layout:

```
[L, 01]          header (catalog of the library)
[L, 05, M]       member M (one binary data value)
```

### 1.2 Library header `[L, 01]`

| key | value | meaning |
|-----|-------|---------|
| `00` | `[u8 seglen][serial key]` (`01 05` = id 5) | last issued member id; new members take last + 1; ids are never reused (live:c1..c7: 1, 2, 3, 4, 5 although each was deleted) |
| `04` | u32 | number of members |
| `06` | node of value-less leaves `M` | glyph index: members flagged as glyphs (layout icons); empty for container data |
| `07` | node of value-less leaves `M` | member list |
| `08` | node `{ <16-byte MD5>: { M } }` | MD5 index: the member holding data with that MD5 (`DBLibrary::Lookup(CRC)`, used for deduplication) |
| `d8` | 16 bytes | the library's unique id (`KUniqueID`), random, created with the header |
| `fc` | serial key value | modification count (`KModCount`): +2 per new member, +1 per deleted member, unchanged by reference count changes (live:c1 `0102` after the first member, c2 dedup still `0102`, c3 `0105` after one new and one deleted member, c4 `0107` then `0108`, c5 `0109`) |
| `fc01`, `fc02`, `fc03` | text, text, 9 bytes | last modifier stamp of the catalog: session user name ("openmaker"), account name ("admin"), time (format not decoded; every catalog member in the file carries the same three keys) |

### 1.3 Member `[L, 05, M]`

| key | value | written by |
|-----|-------|-----------|
| `02` (`KType`) | stream directory (2.1) | uploader |
| `04` (`KCount`) | 16-byte MD5 of the data (2.2) | uploader |
| `12` (`KRefCount`) | u32 reference count | server |
| `f1` (`KPrivateMemberFlags`) | 8 bytes, `00..01` for glyphs | server (absent for container data) |
| `d8` (`KUniqueID`) | 16 random bytes | server, at creation |
| `fc` (`KModCount`) | serial key value | server, +1 on every count change after creation, including a deduplicated op88 (absent on a new member; live:c2 `0101` after the dedup upload) |
| `fc01..fc03` | as in 1.2 | server |
| `<4cc>` | one key per stream: a leaf, or a chunked node (1.5) | uploader |

Live (live:c1/a2_8002_1f, a member holding a 79-byte PNG):

```
[80 02, 1f, 05, 01]
  02   = 0003 464e414d 00000020 504e4766 0000004f 53495a45 00000004
  04   = 937fa502 97b26388 1ba8bfa7 4099f0da
  12   = 00000001
  FNAM = 00000001 05 33373b3d3f 0014 352a3f34...  ("image", "openmaker-test-1.png")
  PNGf = 89504e47...  (79 bytes)
  SIZE = 0010 0010   (16 x 16)
  d8   = <16 bytes>
  fc01 = "openmaker"  fc02 = "admin"  fc03 = <9 bytes>
```

### 1.4 The record's field value

The container field of the record (`[T, 05, R, F]`) holds **text**: the
member key written as uppercase hexadecimal, one group per key segment, groups
joined by `.`, followed by the character U+E000 (bin `DBData::Save` 0xe9fc40,
the `kDTBinary` branch; `Unicode::NybbleToHex` emits `0-9A-F`). For member
`[01]` the text is `"01"`; member `[80 05]` (id 133) would be
`"8005"`. It is stored and transmitted like any text field, as masked
SCSU. FileMaker Pro encodes U+E000 with a window define, so `"01"` is
the plain SCSU `30 31 1b 68 80` (`SD3` with offset index `0x68` = U+E000,
then `0x80`), masked `6a 6b 41 32 da` (live:c1/a2_rec, the record field `03`;
the same `SD3 68` form is how FileMaker wraps host names in U+E000,
om-scsu `decodes_filemaker_host_sentinels`). Readers must accept any valid
SCSU for it.

In the file the cell has the form of text that is not one byte per
character (hbam-writer.md 2.3), even though it is short: a node whose
child `00` is `00 00 00 05 00 00 00 03` (5 bytes, 3 UTF-16 units) and
whose child `01` is `6a 6b 41 32 da`. FileMaker Pro 26.0.3 stored `"01"`
that way when it inserted a picture into a local Contacts starter's Photo
field. The same five bytes as one leaf pass Check Consistency, but Pro
shows an unknown-file icon and the reference text instead of the
picture. Changing only that cell to the node form made Pro show the
picture (observed, #911). Ordinary short text that is not ASCII
(`Ångström 東京`) stored as one leaf was shown correctly in the same
check, so a leaf is not refused in general.

The member key is relative to the table library of the record's table; the
library is not named in the value.

A container field with no value has no leaf in the record (like an empty
text field). Container fields have no value index: `[T, 01, F]` does not
exist for them (test project Photo, all states), and FileMaker Pro offers no
indexing for container fields, so op70 never touches an index for them (Med:
the index-update rule of write-path.md 5.5 applies only to fields whose
`[T, 01, F]` node exists).

### 1.5 Stream values inline or chunked

A stream value of at most 1000 bytes is one leaf `[.., <4cc>] = bytes`. A
longer one is a node: leaf `00` = u32 total length, then the data in pieces of
1000 bytes (the last one shorter) under keys 1, 2, 3, ... written as serial
keys (`01`..`7f`, then `80 00` = 128, `80 01`, ...) (bin
`HBPosition::Put(HBAbstractKey const&, BlobProxy&, bool, unsigned)` 0x1556fe0:
inline when the size is below `0x3e9` and chunks are not forced). This is the
general chunked-value rule of file-format.md 3.6, observed here on
`PNGf`/`JPEG` streams (om:122, live:c3, live:c4 with 1154 pieces) and
confirmed by every captured leaf: none is longer than 1000 bytes.

```
+PNGf
  00 = 00001b74          (7028 bytes)
  01 = <1000 bytes>
  ...
  07 = <1000 bytes>
  08 = <28 bytes>
```

---

## 2. Binary data: the stream tree (High unless noted)

A container value (`DBBinaryData`) is a set of **streams** keyed by a
4-character type. The same tree is what op88 uploads, what a library member
stores, and what FileMaker Pro reads back. `DBBinaryData::SaveToPosition`
(bin 0xd8b6e0) writes it; `DBBinaryData::Load(HBPosition&, bool)` (bin
0xd8a430) reads it.

```
{ 02 = directory, 04 = MD5, <4cc> = stream, <4cc> = stream, ... }
```

Siblings are in byte order of their keys (`02`, `04`, then the 4cc keys,
uppercase before lowercase), as for every slot list.

### 2.1 `02` - stream directory (`KType`)

`u16 n`, then `n` entries `[4cc][u32 size]` in 4cc byte order, one per stored
stream. om:122: `0004 | DPI_ 00000004 | FNAM 00000021 | JPEG 0000922e | SIZE
00000004` (four streams, a 37,422-byte JPEG). A reader creates one stream per
entry and loads its bytes from the key of the same name. A remote stream
type (2.3) is also listed and loaded this way, and then resolved through its
embedded type (bin `Load` 0xd8a430). `SaveToPosition` leaves remote types out
of `n` in some save modes; how that combines with external storage was not
observed (Low).

### 2.2 `04` - MD5 of the data (`KCount`, used as `CRC`)

16 bytes: MD5 over, for each stream in directory order whose type is not
excluded, the 4 type bytes followed by the stream bytes (bin
`DBBinaryData::GetCRC` 0xd88a30; `QuadChar::IsExcludedFromCRC`
libSupport 0x231700). Excluded: `ENCR`, `GLPH` and every remote type (2.3).
Verified: om:122 (`DPI_`+`FNAM`+`JPEG`+`SIZE` hashes to the uploaded
`b717bc7f...`, and no other combination does) and every probe upload (the
server accepted and indexed the MD5s computed by `mkbd.py` with this rule).
The file name in `FNAM` is part of the hash, so the same picture inserted
under two names is stored twice.

The server keys deduplication on this value (3.4) and FileMaker Pro uses it
as the cache identity of the data; a writer must compute it exactly.

### 2.3 Stream types

From the initializer of `QuadChar::kType_*` in libSupport (0xc6800 onwards)
and the type predicates there. **Embedded** types hold the data;
**remote** types are the same 4cc with the case of each letter flipped and
hold a small reference to an externally stored file instead (section 8);
`QuadChar::GetEmbeddedQuad` maps remote to embedded.

Data types (embedded / remote):

| kind | types |
|------|-------|
| images | `JPEG`/`jpeg`, `PNGf`/`pngF`, `GIFf`/`gifF`, `BMPf`/`bmpF`, `TIFF`/`tiff`, `PICT`/`pict`, `qtif`/`QTIF` (QuickTime image), `8BPS`/`8bps` (Photoshop), `FPix`/`fpIX` (FlashPix), `.SGI`/`.sgi`, `TPIC`/`tpic` (Targa), `JP2 `/`jp2 `, `PNTG`/`pntg` (MacPaint), `HEIC`/`heic`, `WEBP`/`webp`, `EPS `/`eps `, `EPSF`/`epsf`, `META`/`meta`, `DIB `, `EMF+`, `SVG ` (layout glyphs) |
| documents | `PDF `/`pdf `, `PGS `/`pgs ` (Pages), `KEY `/`key `, `NMBR`/`nmbr`, `PTXT`/`ptxt` (plain text), `HTML`/`html`, `ZIP `/`zip ` |
| media | `MOV `/`mov `, `WMV `/`wmv `, `MPEG`/`mpeg`, `MPG4`/`mpg4`, `AVI `/`avi `, `M4V `/`m4v `, `MP3 `/`mp3 `, `WAV `/`wav `, `AU  `/`au  `, `AIFF`/`aiff`, `M4A `/`m4a `, `WMA `/`wma `, `FLAC`/`flac`, `MPG3`, `MooV`/`moov`, `snd ` |
| any file | `FILE`/`file` (stored as is), `ZLIB`/`zlib` (stored zlib-compressed), `FLDR`/`fldr`, `FORK` (Mac resource fork) |

FileMaker Pro and Go 26.1.1 insert AVIF images (`.avif`) into container
fields (Claris release notes, FileMaker Pro 26.1.1; libavif 1.4.2). The
Mac binaries add the `.avif` extension, `image/avif` and an "AV1 Image File
Format" file type [O: strings]; which stream type such an image is stored
under (an `AVIF` 4cc, or `FILE`) is [U] until a 26.1.1 insertion is read.

Supporting (metadata) types, `QuadChar::IsSupportingType`: `SIZE`, `DPI_`,
`ROT8`, `FRM#`, `FORK`, `FNAM`, `SIGN`, `REMO`, `ENCR`, `GLPH`. Others seen:
`MAIN`/`main` (names the main stream), `BAR#`/`BART` (barcode text and type),
`sgnt` (signature JSON), `METO`, `EMBO`, `XMLO`, `XML2`, `XMLD`, `AOID`,
`TEMP`, `STDO`, `API `, `Ukno`.

FileMaker Pro picks the **master** type (the viewable data stream) from the
types present; for an inserted JPEG the tree holds `JPEG` plus `DPI_`, `FNAM`
and `SIZE` (om:122). A file inserted with Insert File would hold `FILE` (or
`ZLIB` when compressed) plus `FNAM` (Med: from the type catalog; not
captured). Layout graphics in the file library hold `SVG ` or `PNGf` plus
`FNAM` and `GLPH` (test project `[1f, 05, 1..8]`), some also `JPEG` beside
`PNGf` (cap:570, a layout image of the hosted solution).

### 2.4 Supporting stream formats

| type | format | evidence |
|------|--------|----------|
| `FNAM` | `u32 n` (1), then `n` entries `[u8 len][type text][u16 len][name text]`, both texts masked SCSU; type `image` for pictures. For embedded data the name is the file name only. This is FileMaker's container path list ("image:name"; other types such as `file`, `imagemac`, `filewin` name references) | om:122 (`00000001 05 "image" 0015 <21 chars>`), test project `[1f, 05, x]`, live:c1; bin `GenerateFNAM` 0xd91a70 (serialises a path list) |
| `SIZE` | `u16 width, u16 height` in pixels | om:122 `0280 0280`, live:c1 `0010 0010` |
| `DPI_` | `u16 horizontal, u16 vertical` dots per inch | om:122 `012c 012c` (300 dpi from the photo's metadata). FileMaker Pro writes it when the image carries a resolution |
| `GLPH` | 1 byte, `01` = the member is a glyph (monochrome layout icon) | test project `[1f, 05, x]` |
| `MAIN` | 4 bytes: the type of the main stream (`PNGf`) | test project `[1a]` and `[02, 18 18, x]` (the file's own icons) |
| `REMO` | external storage header: `u32` flags (little-endian), 4 bytes naming the base directory, and for open storage the relative path (8.2) | bin `GenerateREMO` 0xd7d240 |
| `ENCR` | 32 bytes of key material for secure external storage (8.2), excluded from the MD5 | bin `GenerateENCR` 0xd7f6a0 |
| `ROT8`, `FRM#`, `SIGN`, `BAR#`, `BART` | orientation, frame count, signature time stamp, barcode text and type; formats not decoded (Low) | names only |

Data streams hold the file bytes unchanged (`JPEG` = the JFIF file, `PNGf` =
the PNG file).

---

## 3. op88 UploadBinaryData (`RCUploadBinaryData`) (High)

Uploads one binary-data tree and stores it in the library of the target
table. FileMaker Pro sends it from `DBData::Save` / `DBBinaryData::Save`
(mac 0x18700cc) when a container value is set in a hosted file, before the
record is committed and independently of the record's locks. om:122 was sent
while record `03` was locked for editing (om:120 op60) and before any op14.

### 3.1 Request

op88 uses the shared upload framing (network-protocol.md 5.6.6a/5.6.6c) with
the binary-data tree as payload. Push order (bin `RCUploadBinaryData::
CallPushAddition` mac 0x1d2af7e, `PerformPopAddition` mac 0x1d2afda):

```
[ payload, N ][ u32 N ][ u64 perform ][ u64 cursor ][ i16 depth -1 ][ copy key ][ u8 copy-key length ]
[ table key ][ field key ][ text: SCSU bytes ][ u32 text length ][ 16 bytes, zero ]
```

| field | value | meaning |
|-------|-------|---------|
| payload | bracketed slot list, section 2 | the binary data |
| table key | `02 80 02 03` | the table whose library receives the data |
| field key | `01 03 02` (field `03`) | the container field; it selects the field's storage options (embedded or external, section 8). `KUnknown` means "no field" |
| text | empty (`00000000`) | `mSolvedPath`: the evaluated external-storage path when the field's storage path is a calculation; empty otherwise (Med) |
| 16 bytes | zero | reserved; the client always sends zeros and the server ignores them (Med: popped into an unused member) |

`maxreturn` was `0x100244` (om:122) and the header carries the open file's
`sessionid`/`fileid` (network-protocol.md 4.1). The server sets the
transaction's target key to `[table, 03, 05, field]` (the field definition).
The upload takes no record lock: it worked while the uploading client held
record `03` (om:120/122) and in live runs with no record lock at all
(live:c4, c6, c7). The server serialises library changes with an internal
lock on the library name and briefly locks the member while counting
(`DBLibKeyLockHolder`, `HostAdjustReferenceCount`).

om:122, the tail after the payload (37,706 bytes):

```
0000934a | 0000000000000000 0000000000000000 | ffff | 00 | 02800203 | 010302 | 00000000 | 00 x16
```

live:c1/a_up1-p0 (a 185-byte payload, file id `0x165`):

```
58 00 0000 00001005 00000001 00000165 00000000
80 06 02 1a 0003464e414d00000020504e47660000004f53495a4500000004 06 04 10 937fa502...
... 40 | 000000b9 | 00 x16 | ffff | 00 | 02800203 | 010302 | 00000000 | 00 x16
```

### 3.2 Reply

The shared trailer followed by the op88 reply addition. Push order (bin
`PerformPushAddition` mac 0x1d2ba00; client `CallPopAddition` mac 0x1d2ba94):

```
[ slots N = 0 ][ u32 0 ][ u64 perform ][ u64 cursor ][ i16 depth ][ copy key ][ u8 copy-key length ]
[ error text: SCSU bytes ][ u32 text length ][ member key ][ u16 status ]
```

| status | meaning | member key |
|--------|---------|-----------|
| `0x0000` | stored as a new member | the new member, e.g. `01 01 02` = `[01]` |
| `0x1006` (4102, `kWarn_LibraryObjectExists`) | a member with the same MD5 already exists; its count was incremented | the existing member |
| `0xffff` | intermediate part of a multi-part upload (3.3) | empty (`00`) |
| `100` (`kFileMissing`) | the file id is not open on this connection | empty |
| other | storing failed; the error text may explain | empty |

FileMaker Pro treats `0` and `0x1006` alike: it takes the member key and
writes the field text of 1.4. A nonempty error text is attached to the error
the client reports.

live:c1/a_up1-p0 reply (32 bytes):
`00000000 0000000000000000 0000000000000000 ffff 00 00000000 0101 02 0000`.
live:c2/a_dup-p0, the same payload again:
`... ffff 00 00000000 0101 02 1006`.

### 3.3 Multi-part upload (High)

A payload larger than `0xFFF00` bytes is sent in parts of whole chunks
exactly as for any upload (network-protocol.md 5.6.6c "MultiUpload"): every
part carries the same op88 addition; the first part has perform 0 and cursor
0; each later part echoes perform, cursor and copy key from the previous
reply, with depth -1. Intermediate replies carry no member key and status
`0xffff` (the value the server initialises it to; the operation has not run
yet). live:c4 (a 1,158,773-byte payload, a 1,153,018-byte PNG in 1154
pieces, split after 1,048,189 bytes):

```
part 0 reply: 00000000 | 00007757f0103660 | 00007757f01038c0 | ffff | 04 504e4766 05 | 00000000 | 00 | ffff
part 1 req tail: ... | 00007757f0103660 00007757f01038c0 | ffff | 04 504e4766 05 | 02800203 010302 00000000 00 x16
part 1 reply: 00000000 | 00 x16 | ffff | 00 | 00000000 | 0103 02 | 0000
```

The copy key `[PNGf]` is where the server stopped unpacking: inside the
chunked `PNGf` node. The read-back (live:c4/a_png3, a paged op90 of 1,158,668
slot bytes) reassembled the original PNG byte for byte.

FileMaker Pro has a second path for data above 1 MiB (`0x100000`): when an
engine option is set and the server connection reports type 3, it uploads the
streams over FileMaker Server's HTTPS file-transfer web service
(`CallWebServiceUploadBinaryData`, `WebServiceUploadStreams`) and the server
loads them from the upload folder (`PerformWebServiceLoadStreams`) before
storing (Low: from code; not observed, and FileMaker Pro falls back to op88
when the option is off). A server that never advertises the web service
receives large uploads as multi-part op88.

### 3.4 What the server does (High)

`RCUploadBinaryData::PerformOperation` (bin 0x1109f80) with
`DBBinaryData::SaveToLibrary` (bin 0xd7a9d0), after the whole payload has
arrived:

1. Find the open file by the header's file id; if none, status 100.
2. Load the uploaded tree as binary data (2); recompute nothing.
3. If the field's storage is external, write the streams to files and
   replace them by remote streams (section 8). Not the case for test project
   Photo.
4. Lock the library name, then look the MD5 (`04`) up in the library's MD5
   index `[L, 01, 08]`.
5. **Found:** increment that member's count by 1 (as op87, section 5) and
   answer its key with status `0x1006`. The uploaded tree is discarded.
6. **Not found:** create member `M` = last id + 1 (`[L, 01, 00]`), write the
   uploaded keys (`02`, `04` and every stream) under `[L, 05, M]`, set
   `12` = 1, `d8` = a new random 16-byte id and the `fc01..fc03` stamps; add
   `M` to the member list `[L, 01, 07]`, to the MD5 index
   `[L, 01, 08, <md5>, M]` and, for glyphs, to `[L, 01, 06]`; store the new
   last id and count in `[L, 01, 00]` and `[L, 01, 04]`; add 2 to
   `[L, 01, fc]` (1.2).
   Create the library header first if the library does not exist. Answer
   `M` with status 0.
7. Reply with N 0, both handles 0, depth -1, empty copy key, an empty error
   text, the member key and the status.

The new member's count of 1 is the reference held by the value the client is
about to commit (section 5). A member uploaded and never committed stays in
the library with count 1 unless the client releases it (FileMaker Pro does on
Revert Record, 5.2).

---

## 4. The rest of the write sequence

### 4.1 New record with a picture (High for the server side)

```
op71  HostCreateRecord   table 80 02, count 1, txn T       -> record R
op88  UploadBinaryData   tree, table 80 02, field 03        -> member M, status 0
op14  Upload             R with field 03 = "<hex M>" (all stored fields)
op70  HostCommit         [80 02, 05, R] = 01 (new), txn T
op103 TransactInfo       -> 00000015
op65  KillAllLocks       txn T
```

This is live:c1 (record `0e`, member `01`), the order FileMaker Pro uses for
records (write-path.md 1.1) with op88 inserted when the user puts the picture
into the field; om:98-122 shows FileMaker Pro sending op88 in the middle of
an edit, before op14. op14 and op70 are exactly those of write-path.md 3.3
and 3.4: the container field is one more text leaf of the uploaded record
(live:c1/a_op14, field `03` = `6a6b4132da`). The commit list names the record
only (new) or the changed field ids (modified), as for any field.

What changes in the file, compared with a text field:

| key | change |
|-----|--------|
| `[T, 05, R, F]` | the member text (1.4), written by op70 |
| `[T, 1f, ...]` | written by op88 (3.4), not by op70 |
| `[T, 01, F]` | none (no index for container fields, 1.4) |
| `[T, 0d]`, `[T, 00]` | as for any new record (write-path.md 5.1, 5.3) |
| member count `[T, 1f, 05, M, 12]` | unchanged by op70: stays 1 after the commit (live:c1/a2_8002_1f). The server's commit does not look at container values; `HostTableCommit::CommitTable` (bin 0x11be170) reads the library only for an optional debug change log |

No thumbnail, preview or extra stream is stored for an embedded picture; the
member holds exactly what was uploaded plus the server's `12`, `d8`, `fc*`
keys (live:c1, c3, c4). Thumbnails are computed on request (6.3).

### 4.2 Existing record

Editing a record and putting a picture into its container field is op60 (lock
the record), op88, op14 (the whole record), op70 with the record node listing
field `03` (and the other changed fields, such as the modification
timestamp), op65 (live:c3 for the replace case, 7.1). om:114-122 is the start
of this sequence from FileMaker Pro (op60 on record `03`, then op88).

---

## 5. Reference counting

### 5.1 Model (High for the server, Med for the client rules)

`[L, 05, M, 12]` counts the holders of member `M`. The server changes it in
exactly two places: op88 (+1 on the new or deduplicated member, 3.4) and op87
(any delta, 5.3). When it reaches 0 the server deletes the member (5.3).
Everything else is decided by the client: FileMaker Pro counts one reference
per stored record value and one per uncommitted copy in an edit buffer, and
sends op87 when a holder goes away. The server never derives counts from
records (4.1), so a server must not try to: it would double count.

What FileMaker Pro sends (mac `DBRecordCursor::PutData` 0x192a544,
`CommitRow` 0x1924ba6, `RevertRow` 0x19250c2, `DBCursor::CopyRowData`
0x1920df8, `DBCursor::DecrementLibraryRef` 0x1921b24, `DBData::Save`; each
`DBLibraryMember::AdjustReferenceCount` on a hosted file becomes one op87):

| user action | references |
|-------------|------------|
| put new data into a field | op88 (+1 on the new value) |
| put a value that already lives in this table's library (paste from another record of the table, duplicate record) | op87 +1 on that member instead of op88 (`DBData::Save`: same file and table) |
| replace a value that was itself set in the same uncommitted edit | op87 -1 on the replaced value right away |
| commit a modified record | op87 -1 on each container value of the record as it was stored before (the edit buffer's references become the stored record's) |
| revert the record | op87 -1 on each container value of the edit buffer |
| delete the record | op87 -1 on each container value of the stored record |
| duplicate a record | op87 +1 on each container value copied (`CopyRowData`) |

OpenMaker's bounded recovery policy (inferred, not observed native recovery):
an ordinary same-row/table stored copy retains the source cell/version and
immutable native library/member fingerprint before op87 +1. The record-data
submission mark does not authorize retry of this separate library acquisition.
A lost increment ACK retains its intent and bytes without release authority;
a known increment owns only one extra reference, never the original stored
reference. Native FileMaker behavior after such a lost ACK remains unmeasured.

Whether FileMaker Pro also takes a reference for unchanged container values
when it copies a stored record into its edit buffer (and releases it at
commit, a net zero) was not captured (Low). It does not matter to a server,
which only applies the deltas it receives.

### 5.2 Orphans

A member whose holders all forgot to release it (a client that crashed
between op88 and op70, or an uploaded value that was reverted by a client
that did not send op87) keeps a positive count and stays in the file. Nothing
on the server collects it during normal operation; FileMaker's Recover
recounts references (`DBLibraryRecoverer`, Low). A server can leave them.

### 5.3 op87 AdjustReferenceCount (`RCAdjustReferenceCount`) (High)

Request body, push order (mac `RCAdjustReferenceCount::Call` 0x1ac4b74):

| field | type | value |
|-------|------|-------|
| member path | key | the member's absolute key path: `[T, 1f, 05, M]` for a table library, `[1f, 05, M]` for the file library |
| delta | i32 | +1, -1, ...; 0 reads the count |

Reply: `u32` the member's count after the change. The client treats
`ffffffff` as failure.

```
live:c3/a_op87_m1   req body 02 8002 01 1f 01 05 01 01 09 | ffffffff   rep 00000001
live:c3/a_op87_0    req body 02 8002 01 1f 01 05 01 01 09 | 00000000   rep 00000001
live:c3/a_op87_old  (count was 1)                                     rep 00000000  -> member 01 deleted
```

Server (bin `RCAdjustReferenceCount::Perform` 0xf9a820,
`DBLibraryMember::HostAdjustReferenceCount` 0xf99820):

1. Pop the delta, then the key. If the first segment is a table id
   (first byte `0x80..0xCF`) the library is `[that table, 1f]`, otherwise the
   file library `[1f]`; the member key is the segment after `1f, 05`.
2. Lock the member, read `12` (missing = 0), add the delta, clamping at 0
   (a negative delta larger than the count gives 0), write `12` back, and
   increment the member's modification count `fc`. Whether a delta of 0
   (a read) also bumps the member's `fc` was not observed (Low); OpenMaker
   Server writes nothing for it.
3. If the count is now 0, delete the member: remove `[L, 05, M]`, its entry
   in `[L, 01, 07]`, its MD5 index entry `[L, 01, 08, <md5>]` (the whole node),
   its glyph entry, decrement `[L, 01, 04]`, add 1 to `[L, 01, fc]`; for external
   storage also delete its files. `[L, 01, 00]` (last id) is unchanged.
4. Reply the new count.

live:c3 after deleting member `01` and keeping `02`: `[80 02, 1f, 01]` =
`{00 = 0102, 04 = 00000001, 07 = {02}, 08 = {a26b09ea...: {02}}, fc = 0105}`
(member list, MD5 index and count updated, last id kept).

**A server must validate the member path.** FileMaker Server does not: a
truncated op87 body (4 bytes, no key) made it treat the empty key as the
member, so it wrote the count to the file's root key `[12]` and the
modification count to root `[fc]` (section 10). Had a delta brought that
count to 0, the "member" to delete would have been the root. Answer
`ffffffff` and change nothing unless the path is `[T, 1f, 05, M]` or
`[1f, 05, M]` naming an existing member.

---

## 6. Reading containers

### 6.1 What FileMaker Pro does to show a container (Med)

1. The record arrives as usual (op92 of `[T, 05]` with a record-id list, or
   op90 of `[T, 05, R]`); field `F` holds the member text (1.4). The client
   parses the member key and builds a `DBBinaryData` bound to (file, table,
   member) (mac `DBData::Load`, `DBBinaryData::Load(DBFileUser const&,
   HBSerialKey const&, HBAbstractKey const&, HBPosition&)` 0x18790c6).
2. **Header fetch:** one op92 DownloadList of the member node
   `[T, 1f, 05, M]` with flag 1 and a fixed list of 48 keys: `02`, `04`,
   `12`, the supporting types `DPI_ ENCR FNAM GLPH MAIN REMO ROT8 SIGN SIZE
   BAR# BART`, the remote types `.sgi 8bps QTIF aiff au   avi  bmpF eps  epsf
   file fldr fpIX gifF jp2  jpeg m4a  m4v  meta mov  mp3  mpeg mpg4 pdf  pict
   pngF pntg tiff tpic wav  webp wma  wmv  zlib`, and `f1`. The server returns
   the requested children that exist (network-protocol.md 5.6.6b): the small
   leaves, never the embedded data streams, which are not in the list.
3. **Data fetch:** one op90 DownloadWithLock of `[T, 1f, 05, M, <type>]` for
   the stream it needs. An inline stream (up to 1000 bytes) comes back as a
   terminal value (node state 1); a chunked stream as the node's slot stream
   `80 03 00 <u32 total> 07 01 03e8 <1000 bytes> ... 40` (node state 2), paged
   above `0xFFF00` bytes like any op90 (live:c4: 1,158,668 slot bytes, paged).
   The client concatenates pieces `01`, `02`, ... (1.5).
4. **Thumbnail:** for display at a size other than the data's own, a client
   connected to FileMaker Server asks for a scaled image with op96 (6.3)
   instead of downloading the data (mac `DBBinaryData::GetExactThumbnail`
   0x1888236: op96 when the file is hosted and the value is a library member
   without a `sgnt` stream).

Steps 2 and 3 are observed for the file library: cap:552-570 and
om-e:52-70 (op92 of `[1f, 05, x]` with this exact list and flag 1, then op90
`[1f, 05, x, "SVG "]` or `[1f, 05, x, "PNGf"]`), and live for the table library
(live:c1/a_hdr returns `02 04 12 FNAM SIZE`; live:c1/a_png, live:c3/a_png2,
live:c4/a_png3). That FileMaker Pro uses the same two steps for a record's
container value is from the code (the same `DBLibraryMember` read path with
the table library), not from a capture (Med). Step 4 is from the code; the
server side of op96 is live-verified (6.3).

For a server: steps 1-3 need nothing beyond serving the library keys from
the file, which a key-tree server already does. op92's flag 1 locks the
member node for the duration of the list (network-protocol.md 5.6.6b).

### 6.2 Which stream the client fetches (Med)

The client fetches the viewable master stream: the image type for pictures
(`JPEG`, `PNGf`, ...), `PDF ` for PDFs, and for other files nothing until the
user exports or opens the file (`FILE`/`ZLIB` then). When a member holds
both `JPEG` and `PNGf`, FileMaker Pro fetched `PNGf` (cap:570/572, a layout
graphic). The selection lives in `DBBinaryData::GetViewableType` /
`DetermineTypeToThumbnail` (not read further).

### 6.3 op96 DownloadThumbnail (`RCDownloadThumbnail`) (High: live)

Returns the member's image scaled to fit a box, generated by the server.
Request body, push order (mac `RCDownloadThumbnail::Call` 0x1d2fcc6, server
`Perform` bin 0x1113510):

| field | type | value |
|-------|------|-------|
| reserved | 4 x i32 | 0 |
| height | u32 | requested box height, pixels |
| width | u32 | requested box width, pixels |
| member key | key | `[M]`, e.g. `01 04 02` (relative to the library) |
| table key | key | `02 80 02 03` |
| flags | u8 | bit 1 (`02`), bit 2 (`04`) and bit 3 (`08`) set by the client from three booleans; `08` = return the thumbnail decrypted (`requestDecryptedThumbnail`) |
| offset | u64 | byte offset of the chunk wanted, 0 first |
| max chunk | u32 | `000fffe6` from FileMaker Pro |

`maxreturn` is `0x100000`.

Reply, push order:

```
[ u32 return flags ][ thumbnail bytes, n ][ u32 n ][ u32 height ][ u32 width ][ u64 total length ][ u16 status ]
```

| status | meaning |
|--------|---------|
| 0 | last (or only) chunk |
| `0x10` (16, `kRetryError`) | a chunk, more follow: the client asks again with offset + n (it loops up to 219 times) |
| `0x0a` (10, `kDataMissing`) | no such member; the reply is the status only (live:c7/g_thumb_missing `000a`) |

Return flags bit 0 = the bytes are encrypted (the client then decrypts with
the container's key, `thumbnailType` 2) (Med). live:c6 and c7, a 48 x 48
PNG member:

| request | reply |
|---------|-------|
| 24 x 24, flags `00` or `02` | status 0, total 2192, 24 x 24, return flags 1, 2192 bytes that are not a plain image (encrypted, per the flag) |
| 256 x 256, flags `08` | status 0, total 138,293, 256 x 256, return flags 0, a 256 x 256 PNG |
| height 32, width 64, flags `08` | a 64 x 32 PNG (6588 bytes) |
| same, max chunk 1000 | status `0x10`, total 6588, n 996 (max chunk minus 4), the first 996 bytes |

So FileMaker Server scales the image to exactly the requested box (upscaling
included) and encodes it as PNG for a PNG source; a JPEG source gets a JPEG
thumbnail (mac `GetExactThumbnail`: thumbnail type JPEG when the master type
is JPEG, otherwise PNG; Med). A server that cannot encrypt can always answer
with return flags 0 and plain image bytes: the client records the flag and
only decrypts when it is set (Med). Whether FileMaker Pro asks with `08`
was not captured (Low).

**For OpenMaker Server:** decode the master image stream (JPEG, PNG, GIF,
BMP, TIFF, HEIC, WEBP as available), scale it to width x height, encode PNG
(or JPEG for JPEG sources), cache it per (member, box), and serve it in chunks
of at most `max chunk - 4` bytes. Answer `0x0a` for a missing member and for
non-image data. What FileMaker Pro displays when op96 fails for an existing
member (does it fall back to op90 of the stream and scale locally?) was not
observed (Low); implementing op96 avoids the question.

### 6.4 op95 DownloadFile (`RCDownloadFile`) (Med, from code)

Streams an **externally stored** file (section 8) in chunks; not used for
embedded data. Request push order (mac `RCDownloadFile::Call` 0x1d2e9d6,
called from `RCDownloadIStream::StreamBuffer::underflow`; the parameter
names are the DWARF names of bin `RCDownloadFile::Call` 0x1110d00):

```
[ field key ][ table key ][ text md5 ][ text fileName ][ text relativePath ]
[ u32 baseID ][ u64 offset ][ u32 max chunk, 000ffffa ]
```

| field | value |
|-------|-------|
| field key, table key | the container field and its table, as in op88 |
| md5 | the remote stream reference's 16-byte checksum (8.3) as hexadecimal text (`GetRemoteStreamCRC`) |
| fileName | the file name of the reference (8.3) |
| relativePath | the folder below the base directory (8.4), its separators normalised before sending |
| baseID | the base directory id from `REMO` (8.2) |
| offset | byte offset of the chunk wanted |

Reply: `[bytes][u32 n][u16 status]`; status `0x50de` (20702) and `0x50df`
(20703) mark the end of the stream (the client maps them to 0 and 16).
The server (bin `RCDownloadFile::Perform` 0x1111440) finds the open file,
looks up the base directory by id and takes its absolute directory
(`DBBaseDirectoryEntry::GetAbsoluteDirectory`), and reads the chunk through
its disk cache, keyed by the directory, relative path, file name and MD5
(`DiskCache::GetObject`); it also accepts a path in its file transfer area
(`UniversalPath::IsInFileBox`). The status values for a missing or
mismatching file were not traced (Low).

### 6.5 op97, op98, op99 (Low, from code)

| op | request push order | reply | use |
|----|--------------------|-------|-----|
| 97 VerifyContainer | `[table key][member key][u8 flag]` | `u16` status | check that an external file exists and matches |
| 98 GetContainerURL | `[table key][member key][text]` | `u16` status, text URL | a streaming URL for interactive (web viewer, media) containers |
| 99 TransferContainer | `[text solved path][key][key][...]`, see `Call(DBFieldSpec, HBSerialKey<9>, unistring)` | counts and status | move a field's data between embedded and external storage on the host |

None of them is needed for embedded pictures.

### 6.6 Optimize for: images or interactive content (High: FileMaker Pro saves, 2026-11-19)

A container field object's Inspector setting "Optimize for" (Images, or
Interactive content) and an interactive container's "Start playback
automatically" are two bits of the object's graphic format (data-format.md
"Graphic format"), the big-endian `u16` at bytes 210-211 of its 213-byte
`KFormat` record (attribute `13` of the object).

**Observed** [O]: FileMaker Pro 26.0.3 saved a copy of the English
Content Management starter four ways (#200, git-ignored captures). The
object is the Content Details layout's File Container field object, object
23, 325 x 215.

| Save | Bytes 209-212 | Graphic format |
| --- | --- | --- |
| the starter, untouched | `00 00 07 00` | `0x0007` |
| Optimize for: Images | `00 00 07 00` | `0x0007` |
| Optimize for: Interactive content | `00 01 07 00` | `0x0107` |
| the same, and Start playback automatically | `00 03 07 00` | `0x0307` |

- Between consecutive saves only byte 210 of that record changed.
- The object's other attributes (`3`, `6`, `7`, `16` and its UUIDs `216`
  and `217`) kept their keys and bytes.
- No other field object's graphic format changed in the file.
- The layout's other differences between the saves are FileMaker Pro
  rewriting the layout, which the Walk lane also saw in an unrelated file
  saved without entering Layout mode [O, relayed].

So:

| Bit | Setting |
| --- | --- |
| `0x0100` | Optimize for: Interactive content (clear: Images) |
| `0x0200` | Start playback automatically |

**Inferred** [I]:
- The scaling and alignment bits keep their meaning on an interactive
  container: the saves kept `0x07` under both.
- "Start playback automatically" is offered only with Interactive
  content, so `0x0200` without `0x0100` is not expected.
- None of the six English starters sets `0x0100`: their field objects
  store 5 or 7 (data-format.md).

**Unknown** [U]:
- Any other interactive option, and where it is stored.
- How FileMaker Pro draws an interactive container's PDF (its controls and
  scrolling) and plays its media: no screenshots of either were taken.

OpenMaker reads the bits as `om_layout::GraphicFormat::interactive` and
`::autoplay`. Desktop: an interactive container shows a PDF as its PDF
view (desktop-client.md), and its audio and video start by themselves only
with autoplay (desktop-client.md, #1338).

---

## 7. Replacing, clearing and deleting

### 7.1 Replace (High, live:c3)

```
op60  Lock               [80 02, 05, 0e], type 9, txn T        -> granted, mod count 01 00
op88  UploadBinaryData   new tree                               -> member 02, status 0
op14  Upload             the whole record, field 03 = "02", fc = 01 01
op70  HostCommit         { 80 02: { 05: { 0e: { 03, 0d } } } }, txn T
op87  AdjustRefCount     [80 02, 1f, 05, 01], -1                 -> 0 (member 01 deleted)
op65  KillAllLocks       txn T
```

After it: record `0e` field `03` = `6a684132da` ("02" + U+E000), member `02`
with count 1 and a chunked `PNGf`, member `01` gone from `[L, 05]`, `07`
and `08` (live:c3/a3_8002_1f). The order of op87 relative to op70 is the
probe's; FileMaker Pro releases the old value while committing the row (mac
`CommitRow`), and the server does not care about the order.

### 7.2 Clear the field (Med)

Clearing a container (Edit > Clear, or setting it to empty) uploads the record
without the field's leaf, commits it with the field id listed, and releases
the old member with op87 -1. The record then has no leaf for `F` (the server
replaces the whole record, write-path.md 5.2). Not run live; it is the
replace sequence without op88.

### 7.3 Delete the record (High, live:c5)

```
op60  Lock               [80 02, 05, 0e], type 9, txn T  -> granted, mod count 01 01
op87  AdjustRefCount     [80 02, 1f, 05, 02], -1         -> 0 (member 02 deleted)
op70  HostCommit         [80 02, 05, 0e] = 03, txn T
op65  KillAllLocks       txn T
```

Afterwards the table equals its state before the record existed (live:c5:
`[80 02]` identical to the baseline of live:c1 except `[80 02, 00]`, the last
record id, and the now empty library header `[80 02, 1f, 01]` with count 0
and last member id 3).

A server that deletes records on its own (op86 DeleteTable/TruncateTable,
query-program.md) would also have to release their container references;
whether FileMaker Server does was not examined (12).

---

## 8. External storage (Med: engine code; Low where marked; not run live)

Everything in this section is read from the engine code (bin, nm
addresses, unless marked) except the starter files' base directory
catalog and the lab server's folder names; no file with an externally
stored field was available, so none of it is observed.

### 8.1 Field options and base directories

A container field stores **embedded** (everything above) unless its
definition selects external storage ("Store container data externally"
in the field's storage options). The choice is three bits of the field
definition's `mOptionSet` (schema-path.md 5.5; DWARF enum
`DBField::Options`):

| bit | name | meaning |
|-----|------|---------|
| `0x1000` | `kRemoteContainer` | store container data externally |
| `0x2000` | `kOpenContainer` | open storage; clear means secure storage |
| `0x4000000` | `kSecureLessFolder` | secure storage with fewer folders |

`0x4000` (`kNeedLibraryUpdate`) and `0x8000` (`kSkipLibraryUpdate`) are
named in the same enum. FileMaker Pro 26.1.1 stored `0x6000` after turning
off external Open storage and deferring the transfer. The existing image
stayed external. New Record and text-only changes to new and existing
rows succeeded. The container definition and values stayed byte-identical.
Completing the transfer changed the word to `0x2000` and moved the image
back into the file.
[Open storage comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1060)

FileMaker Pro 26.1.1 stored `0x4000` on a 24-repetition Container after
turning off external Secure storage (`0x1000`) and deferring the transfer
to internal storage. New Record and edits of a separate Text field
succeeded with error 0. After reopening, the field definition, repetition
count, and stored image reference in repetition 2 were unchanged.
Inferred: `0x4000` marks a pending library transfer and `0x2000` retains
the Open selector. `0x8000`, hosted writes and changes to the container
in this state are unmeasured.
[Secure storage comparison](https://github.com/OpenMaker-Dev/openmaker/issues/1083)

The other settings are
members of the field definition `[T, 03, 05, F]` (bin
`DBBinaryData::GenerateREMO` 0xd7d240, `RemoteHeader::ConstructFlags`
0xd97900):

| key | meaning |
|-----|---------|
| `[T, 03, 05, F, 26]` (`KBaseDirectories`) | 4 bytes naming the base directory, copied into `REMO` (8.2); absent means none (`REMO` then holds `ff ff ff ff`). Readers use the first of the four bytes as a signed id (`RemoteHeader::GetBaseID` 0xd8e2f0), so -1 is none; how the field's value is laid out was not observed (Low) |
| `[T, 03, 05, F, 27]` (`KStorageFormula`) | open storage's relative path, a calculation: the client evaluates it per record and sends the result as op88's text field (3.1) |

test project's Photo field has neither key and neither option bit:
embedded (and live:c1 stored the data in the library).

**Base directories** (File > Manage > Containers) are the catalog
`[26]` (top-level 38, `KBaseDirectories`; `DBBaseDirectoryCatalog`,
`DBBaseDirectoryEntry`): header `[26, 01]` with `00` the last issued id as
a key (`04 00 00 00 00`) and `04` the count; entries `[26, 05, <4-byte
id>]` with `10` the path text and `f0` flags, bit 0 = relative to the
file's folder (`GetRelative`), plus `d8`, `d9` and the stamps. Every
starter file has one entry, id `00 00 00 00`, named after the file
(`Tasks/` in Tasks) with `f0` = `01` [O: read-only dump]. How FileMaker
turns a relative entry into a folder was not fully traced (Low). On
FileMaker Server the default base directory of a hosted file is
`<folder of the file>/RC_Data_FMS/<file name>/`: the lab server has
`Data/Databases/RC_Data_FMS/test project/` (empty) and
`Data/Databases/Containers/RC_Data_FMS/<file>/` for the files in that
folder (empty: those solutions embed their containers). Observed
2026-10-28 by listing directory names only.

The thumbnail settings of Manage Containers are in the file options
record (file-metadata.md 3.1): `mThumbnailOptions` bit 0 generates
thumbnails, bit 1 keeps them permanently (clear: temporary), and
`mThumbnailCacheSizeInMB` (bin `DBBaseDirectoryCatalog::
GetGenerateThumbnails` 0xd71fb0, `GetThumbnailsAreTemporary` 0xd71fe0).

### 8.2 What changes in the member

op88 is the same request. External storage requires a field configured
for external storage (8.1) and a value that can be stored remotely. Naming
a field in op88 alone is not sufficient: embedded uploads also name
fields. The server's `SaveToLibrary` (bin 0xd7a9d0) loads the named field's
definition; the exact engine test of `kRemoteContainer` was not traced.
A value with data in an `XMLO`, `XML2`, `snd ` or `EMBO` stream (embedded
objects and sounds) stays embedded (`IsDataRemotable` 0xd7c740). Before storing the
member it (`GenerateRemoteStreams` 0xd7f980):

1. writes each stream whose type can be remote (`QuadChar::
   IsRemotableType`) to a file (8.4) and replaces it in the member by its
   **remote** type (2.3, case flipped: `JPEG` -> `jpeg`), whose value is
   the reference of 8.3;
2. adds `REMO` (`GenerateREMO`):

   | offset | size | content |
   |--------|------|---------|
   | 0 | 4 | flags, a little-endian `u32`: bits 0-2 the encryption type, 0 for open storage; for secure storage the file's encryption-at-rest type (the first byte of `HBFile::mEncryptionInfo`, masked to 3 bits), or 1 for a file without encryption at rest. Bit 3 is `kSecureLessFolder`; readers treat bits 3-10 equal to exactly 1 as "fewer folders" (`RemoteHeader::IsSecureWithLessFolder` 0xd87ac0) |
   | 4 | 4 | the field's `KBaseDirectories` value as stored, or `ff ff ff ff` |
   | 8 | n | open storage only: the evaluated relative path (op88's text) in UTF-8, NUL-terminated (`RemoteHeader::GetSolvedCalc` 0xd97840) |

   For secure storage `REMO` is 8 bytes. The flags are written from
   memory, so they are little-endian on every platform FileMaker runs on
   [I: the code writes host order];
3. adds `ENCR` for secure storage: 32 bytes of key material per value,
   from a generator seeded with the clock (`GenerateENCR` 0xd7f6a0, not a
   cryptographic random source). The stream encryption key is taken from
   it (`GetEncryptionKey` 0xd8b3c0); a value without `ENCR` uses its own
   MD5 `04` instead (`GetEncrKeyData` 0xd8b2c0). The cipher was not traced
   (Low).

The library member, its count and the record's field text work exactly as
for embedded data; deleting the member at count 0 also deletes the files
(`DeleteRemoteFiles`). Remote streams and `ENCR`/`GLPH` are excluded from the
MD5 (2.2); whether the stored `04` is the client's MD5 of the original data
or recomputed after the streams become remote was not examined (Low).

### 8.3 The remote stream reference

The value of a remote type (`RemoteStream`; read by
`GetRemoteStreamSize` 0xd8aa30, `GetRemoteStreamCRC` 0xd88ba0 and
`GetRemoteStreamFileName` 0xd88970; written by `GenerateRemoteStreams`):

| offset | size | content |
|--------|------|---------|
| 0 | 4 | the stream's size, a little-endian `u32` |
| 4 | 4 | zero |
| 8 | 16 | MD5 of the stored file's data, computed as FileMaker writes it (whether over the plain or the encrypted bytes for secure storage was not established, Low); op95 sends it as hexadecimal text |
| 24 | n | the file name in UTF-8, NUL-terminated; absent when there is none (readers read a name only when the reference is longer than 24 bytes) |

For open storage the name is the stored file's name. For secure storage
FileMaker drops the first four characters of the generated name before
storing it (Low: the name's composition is not fully traced, 8.4).

### 8.4 The folder tree

A stored file is `<base directory>/<relative path><file name>`
(`RemoteHeader::GetAbsolutePath` 0xd86f20, `GetRelativePath` 0xd87190):

- **Open storage:** the relative path is `REMO`'s evaluated path text
  (the field's `KStorageFormula`), and the file keeps a readable name from
  the inserted file (Low: the cleanup of characters a file name cannot
  hold, `UniversalPath::CleanupFileName`, was not traced).
- **Secure storage:** the relative path starts with `Secure/`. The rest
  comes from a string `S` of 32 hexadecimal digits, the MD5 of the
  value's checksum `04` followed by the stream type as UTF-16 text
  (`GenerateSecureString` 0xd9ab00), so the location follows from the
  data and its type, not from chance. By default the folders are
  `S[0:2]/S[2:4]/S[4:12]/S[12:20]/S[20:28]/` and `S[28:32]` begins the
  file name (`GenerateSecureName` 0xd87b30); with fewer folders the split
  is shallower. Both are partly traced (Low).

### 8.5 Reading external data and thumbnails

Clients read external data with op95 (6.4) and thumbnails with op96
(6.3). FileMaker Server generates the thumbnails into a disk cache: the
host and client thumbnail folders are `DBFile` members
(`mHostThumbnailPath`, `mClientThumbnailPath`), the cache path components
`/Thumbs/` and `/LocalThumbs/` are strings of the engine, and a thumbnail
file's name starts with `<width>x<height>_` and ends with the image
type's extension, with characters such as `&`, `<` and `>` replaced (bin
`DBBinaryData::GenerateThumbnailFileName` 0xd9b810) (Low: the folder
layout was not observed). Whether thumbnails are kept or temporary
follows 8.1's settings. op99 moves data between embedded and external
storage when a field's storage option changes.

An implementer can postpone external storage: answering op88 for an external
field by storing the data embedded is invisible to FileMaker Pro except in
the file's storage report and the folder on disk (Med).

---

## 9. Server algorithm (summary)

State: the per-table libraries in the file (no per-connection state beyond
the upload framing of network-protocol.md 5.6.6c).

- **op88**: unpack the payload (multi-part as for op14/op92); validate it
  as a binary-data tree (`02` directory consistent with the streams, `04` 16
  bytes; recompute the MD5 by 2.2 and reject a mismatch, which FileMaker
  Server does not do); library `L = [T, 1f]` (create the header `[L, 01]`
  with `00` none, `04` 0, empty `06`/`07`/`08` nodes, a random `d8` if
  missing); if `[L, 01, 08, md5]` names member `M`: count + 1, reply `M`,
  status `0x1006`; else `M` = last id + 1, store the tree under `[L, 05, M]`
  plus `12` = 1 and `d8`, update `00`, `04`, `07`, `08`, `fc` (+2), reply `M`,
  status 0. Reply framing: 3.2.
- **op87**: validate the path (5.3); count = max(0, count + delta); at 0
  delete the member and its index entries; reply the count (`ffffffff` on
  error).
- **op90/op92 on `[T, 1f, ...]`**: as for any key (no special case).
- **op96**: 6.3.
- **op14/op70**: unchanged; the container field is a text leaf. Do not adjust
  counts on commit or delete.
- **op95/97/98/99**: answer "not supported" until external storage is
  implemented; FileMaker Pro only sends them for external data (Med).
- **Stamps**: `fc01..fc03` are bookkeeping that FileMaker Pro does not read
  when it shows a container (the header fetch list of 6.1 does not name
  them). A server that cannot produce the `fc03` time format can leave all
  three out; OpenMaker Server does (Med).

---

## 10. Live probe records and changes on the lab server (2026-10-28)

Runs c1-c7, 15:12-15:25 CDT, one login each, all against `test project`
(file id `0x165`), table Contacts `80 02`, field Photo `03`. No other hosted
file was opened. No server configuration was changed.

| object | created | removed |
|--------|---------|---------|
| record `0e` (First Name "OpenMaker container test", Last Name "Photo") | c1 | c5 |
| member `01` (16 x 16 PNG; count 1, 2 after the dedup upload, then 1, 0) | c1 | c3 |
| member `02` (48 x 48 PNG, chunked) | c3 | c5 |
| member `03` (640 x 600 PNG, 1.15 MB, multi-part upload) | c4 | c4 |
| members `04`, `05` (48 x 48 PNG, op96 tests, no record) | c6, c7 | c6, c7 |

Final state, checked by downloading `[80 02]` before c1 and after c5/c7:
records `01`-`03`, `0a`, `0c`, `0d` unchanged (`[80 02, 0d]` =
`0000340e` before and after); `[80 02, 00]` advanced from `010d` to `010e`
(ids are never reused, write-path.md 5.1); the Contacts library header
`[80 02, 1f, 01]` remains with count 0 and last member id 5 (FileMaker's own
structure, which FileMaker Pro leaves the same way after inserting and
removing a picture); the file library `[1f, 01]` is byte-identical. The
file grew from 573,440 to 2,170,880 bytes: the pages freed by the 1.15 MB
test stay in the file as free pages until it is compacted.

**One side effect, not repaired:** in c2 the probe's `tx` command sent only
the first word of three op87 bodies (`02800201`, 4 bytes, no key). FileMaker
Server read them as delta `0x02800201` with an empty member key and wrote
the count and modification count of that "member" at the **root** of the
file: `[12]` = `07800603` (three times the delta) and `[fc]` = `01 03`. No
other key changed (all leaves compared). The keys are not read by FileMaker
for anything known, and a copy of the file taken from disk at 15:19 passed
`FMDeveloperTool --checkConsistency` (529 blocks, 0 bad). They cannot be
removed online (op13 only writes; a further op87 bringing the count to 0
would make the server delete the "member", i.e. the root). Offline repair,
for a moment when nobody has test project open: close it on the server, run
`re/work/containers/omc/target/release/fixroot <copy> <fixed> 12 fc` (loads
the key tree with om-hbam, removes exactly those two root leaves, rebuilds the
file with `om_hbam::writer::rebuild`), check `<fixed>` with
`FMDeveloperTool --checkConsistency`, put it in place and open it. Tried on
the 15:19 copy: the leaf sets differ in exactly `[12]` and `[fc]` and the
result passes the consistency check (102 blocks, 0 bad, compacted). Not
applied, because FileMaker Pro (the capture Mac) and other OpenMaker
sessions were connected at the time.

Temporary copies made on the lab server for the consistency checks
(`/tmp/omc`) were deleted.

---

## 11. Test vectors

`re/work/containers/` (git-ignored; `INDEX.md` there lists every file):

| path | covers |
|------|--------|
| `capn/00122-op088/txn.bin`, `result.bin` | om:122, FileMaker Pro's op88 and OpenMaker Server's short reply (contains the maintainer's photo: do not copy) |
| `live/c1/a_up1-p0.{req,rep}` | 3.1, 3.2 new member |
| `live/c2/a_dup-p0.{req,rep}` | 3.2 dedup, status `0x1006` |
| `live/c4/a_up3-p0..p1.{req,rep}` | 3.3 multi-part |
| `live/c1/a_op14.req`, `a_op70.req`, `a2_rec.slots` | 4.1 record with a container value |
| `live/c1/a2_8002_1f.slots`, `live/c3/a3_8002_1f.slots`, `live/c5/e9_8002_1f.slots` | 1.2, 1.3, 5.3 library states |
| `live/c3/a_op87_*.{req,rep}`, `live/c5/e_op87.*` | 5.3 |
| `live/c1/a_hdr.{req,rep}`, `a_png.lastrep`, `live/c3/a_png2.slots`, `live/c4/a_png3.slots` | 6.1 header fetch and stream reads |
| `live/c6/f_thumb*.{req,rep}`, `live/c7/g_thumb*.{req,rep}` | 6.3 |
| `live/c3/*` (op60, op14, op70 node list), `live/c5/*` | 7.1, 7.3 |
| `live/c2/a_op87_m1.*`, `live/c2c/c_12.lastrep`, `c_fc.lastrep` | 10 (the malformed op87 and the root keys it wrote) |
| `data/img*.payload`, `mkbd.py` | binary-data trees for synthetic PNGs (directory, MD5, chunking, split into parts) |

Each `.req` is a whole RCTransaction (20-byte header and body), `.rep` the
reply result stack, `.bc` the back channel. A server test can replay c1-c7 in
order against a copy of test project's `[80 02]` from `live/c1/a0_8002.slots`
and compare every reply and the final `[80 02]` with `live/c5/e9_8002.slots`
(member keys, ids and the `d8`/`fc*` values differ by design).

---

## 12. Open questions

1. **A capture of FileMaker Pro doing this against FileMaker Server**: insert
   a picture, view it, replace it, delete the record (and Duplicate Record,
   Insert File, Revert Record). It would confirm the client side of 5.1 (when
   op87 is sent, its order relative to op70), 6.1 step 4 (whether and with
   which flags op96 is used to show a record's picture) and 6.2. With the
   capture relay running (README, "Live observations"), open test project on
   the Mac and do these steps on a new record; name it
   `2026-10-2x-s07-containers`.
2. What FileMaker Pro shows when op96 fails or is unsupported (6.3).
3. Thumbnail encryption (op96 return flag 1): key and cipher.
4. External storage (section 8): the option bits, `REMO`, `ENCR`, the
   remote stream reference and op95 are specified from the code only.
   Not settled: the layout of a field's `KBaseDirectories` value, how a
   relative base directory resolves on FileMaker Pro, the secure file
   name and the fewer-folders split, open storage's file name cleanup, the
   cipher, and the thumbnail folders. A field with external storage in a
   scratch copy, filled by FileMaker Pro, settles them ("External storage:
   still open").
5. `ROT8`, `FRM#`, `SIGN`, `BAR#`/`BART` stream formats and the 9-byte
   `fc03` time stamp.
6. Whether FileMaker Server releases container references when records are
   deleted by op86 DeleteTable/TruncateTable or by a server-side script.
   Partly settled (2026-10-29): a script's Set Field that clears a value
   releases its member, deleted at 0 (script oracle s34); DeleteTable
   takes the whole library with `[T]`. Truncate Table needs a table the
   harness's patch builder cannot name yet (106 without one).
7. The HTTPS file-transfer path for uploads above 1 MiB (3.3): when the
   client uses it and what the server must advertise.

---

## Implementation notes (OpenMaker Server scripts)

2026-10-29 (`om-server` `library`, `store`, `writes`, `scripts::host`).
Server scripts work on the hosted file's libraries directly; FileMaker
Server's own behavior for them (12.6) is not observed.

- Reading. A calculation in a server script reads a container field as the
  file name of the member of its table's library that the record's text
  names (1.4: the hexadecimal key, then U+E000; the name `FNAM` holds, 2.4,
  the first path's for a list of several) and keeps the member as its
  reference (`om_calc::Container::library`: table, member key, MD5). The
  streams are read only when a function reads data (the container analysis
  functions, the encoders and crypt functions, the embedding functions and
  `Length`; `om_calc::functions::reads_container_data`): the directory's
  streams but `FNAM` (2.1), the main one first (the type `MAIN` names, else
  the first that is not supporting data, 2.3), chunked values assembled
  (1.5). Two members compare by their MD5 (2.2 covers data and name). A
  text that names no member stays that text without data (as text a
  script set into the field does, below). ExecuteSQL reads only the name
  (execute-sql.md 8.1). The shared code is `om_data::containers`.
- `VerifyContainer` answers `?` for embedded data, also for a field, before
  and after its commit (script oracle s34-containers-setfield); it answers
  only for externally stored data, from the host (`StoredContainer::
  external`).
- Set Field into a container (script oracle s34-containers-setfield,
  -types, -extensions; FileMaker Server's engine on contacts-en, the
  library read from the run copy):
  - Container data is stored as a member. Data a calculation decoded (one
    `FILE` stream and a file name) is typed by the name's extension, any
    case: `png` `PNGf`, `jpg`/`jpeg`/`jpe` `JPEG`, `gif` `GIFf`, each with
    `SIZE` (u16 width, height) when the picture's header reads, whatever
    the bytes (a PNG named `x.jpg` is stored as `JPEG`), and `DPI_` when it
    has a resolution; `bmp` `BMPf`, `tif`/`tiff` `TIFF`, `webp` `WEBP`,
    `heic` `HEIC` only when the bytes are a picture, then with `SIZE` and a
    `JPEG` preview (`WEBP` also `MAIN`); `pdf` `PDF ` (with a `PNGf`
    preview, `MAIN` and `SIZE` when FileMaker renders it); `mov`/`qt`
    `MOV `, `mp4` `MPG4`, `m4v` `M4V `, `avi` `AVI `, `mpg`/`mpeg` `MPEG`,
    `wmv` `WMV `, `wma` `WMA `, `mp3` `MP3 `, `m4a` `M4A `, `wav` `WAV `,
    `aif`/`aiff` `AIFF`, `au` `AU  `; anything else (`heif`, `svg`, `psd`,
    `jp2`, `tga`, `pict`, `flac`, `3gp`, `ogg`, `txt`, no extension) `FILE`.
    The `FNAM` path type is `image` for pictures (and a rendered PDF),
    `movie` for sound and movies, `file` otherwise.
  - The same data twice is one member with two references; another
    record's value of the same table one more reference to its member;
    a cleared value releases its member (deleted at 0).
  - Text, numbers and dates are stored as their text in the field, no
    member: `"hello"` reads `hello`, `Length` 5, `Base64Encode`
    `aGVsbG8=`; `Date ( 10 ; 29 ; 2026 )` is `10/29/2026`.
  - OpenMaker (`om_calc::functions::stored_streams`,
    `om_data::containers::tree`) builds the same trees, without
    FileMaker's previews (it has no renderer for them; the MD5 is its
    own trees'). FileMaker's engine stores the data when Set Field runs
    and releases a value set twice in one edit at once (the member's `fc`
    counts both changes); OpenMaker Server holds the data in the session
    (the cell holds a token, U+E001, a number, U+E000, never written) and
    stores it in the library with the record's commit, in the same saved
    commit, so a revert, a reverted transaction or a failed save stores
    nothing. Counts are the same once committed; the member's `fc` may
    differ.
- Deleting. A record a script deletes releases every container value it
  stored, as FileMaker Pro's op87 -1 does for a client's delete (7.3);
  generally each commit of a script's record changes the counts by the
  difference between the members the stored record named and those the
  new one names (a copy made by Duplicate Record +1, 5.1; a deleted record
  -1 each), in the same commit as the record, under the store's hold. A
  member that reaches 0 is deleted with its header entries (5.3). The
  commit's journal keeps what the change touches (the counts, the whole
  member and the header when it is deleted or stored), so a save that
  fails puts every count, stored and deleted member back with the
  records. Inside a transaction the release happens when Commit
  Transaction writes the deletion; a reverted transaction releases
  nothing. op70 commits of clients are unchanged: FileMaker Pro sends its
  own op87 (9).
- op86 DeleteTable removes `[T]` with its library `[T, 1f]`: every member
  goes at once, no reference is left to release. OpenMaker Server drops
  the library's cached op96 thumbnails with it. Externally stored files of
  a deleted table (schema-path.md 3.4) are not deleted: external storage
  is not implemented (below).
- Export Field Contents in a server script writes the field's main data
  stream (or its text as UTF-16LE with a byte order mark) beneath the Documents or the
  session's temporary folder, through their handles like Export Records;
  800 elsewhere. An empty field writes nothing. (The script harness cannot
  run step 132: fmsased handles it.)

## Implementation notes (desktop client)

Native FileMaker Pro 26.0.3 storage probes (2026-11-01, disposable Contacts
copy, synthetic 39-byte UTF-8 text file) confirm uncompressed Insert File,
commit and Export Field Contents preserve the file's exact bytes. Text
export of `hé` is `FF FE 68 00 E9 00` (UTF-16LE with BOM). Embedded
VerifyContainer returns `?`; a JSONNumber conversion to 0 is not a raw
verification result.

A stored `ZLIB` main stream contains gzip data. Readers expand it once
for calculations, logical file size and export, while retaining the
original stream for storage MD5, internal size and copies. The measured
39-byte file has a 60-byte compressed stream, storage MD5
`274803A1970D4A12FE94A7F2FD07E9E4` and internal size 107. Both stream
buffers count towards the cache budgets; expanded files over 1 GiB or
incomplete/invalid gzip data are unreadable. This supports reading native
compressed members; Insert File's compression options remain unimplemented.

2026-10-29 (`om_data::host`, `om-provider` `library`, `containers`).

- Reading follows 6.1: a calculation reading a container field has the
  provider download the member's `04` and `FNAM` (two op90; FileMaker
  Pro's op92 header list would also do) and gets the file name; the
  member's data (one op90 of `[T, 1f, 05, M]`) only when a function reads
  it (`CalcSource::library_member`, `library_data`; `CalcNeed::Member`,
  `MemberData` for clients that evaluate later). Members are immutable
  within a library, but dropping the library restarts its member ids. The
  provider revalidates headers at page/calculation-request boundaries and
  before a copy, retaining bodies only when the fresh MD5 matches. Cache
  keys include the owning connection and native table; calculation values
  carry that connection identity, the client table id and the MD5. Data
  stays within 128 MiB (64 MiB in a client's `CalcData`, with a single
  response/large member allowed to exceed that budget). Completed GUI
  calculation results survive replies containing only missing library data,
  so body eviction does not repeatedly fetch already-completed objects.
  A pending evaluation retains the bodies it already read while later
  executed branches fetch their own data. Its active working set, like an
  oversized single response, can exceed the soft cache budget until the
  evaluation completes; this prevents sequential branches from evicting
  each other forever. Record/context changes still invalidate results.
- Writing follows 5.1: Set Field (and Insert Calculated Result) into a
  container field uploads the tree with op88 (in parts of whole chunks
  above 0xFFF00 bytes, 3.3), or takes op87 +1 for a
  member of the same table's library, and puts the reference text into
  the record; text stays text. After a successful commit the client sends
  op87 -1 for the stored value of each changed container cell; after a
  successful delete for each value of the deleted record; Revert Record
  releases the references the edit took, a second Set Field in one edit
  the first one's; Duplicate Record counts each copied value +1. Script
  transactions release at Commit Transaction. A failed release only
  leaves a count too high (an orphan, 5.2); a release never happens
  before the commit that drops the value succeeded.
- Insert File (131), Insert Picture (56), Insert PDF (158) and Insert
  Audio/Video (159) (2026-10-30, `om-gui` `scripting/host/containers.rs`):
  the first path of the step's list naming a file, read with the data
  file steps' path rules; Insert File reads `file` paths, Insert Picture
  and Insert PDF `image` paths, Insert Audio/Video `movie` paths, each
  family with its platform forms (Claris "Creating file paths"). The
  file is typed by its name as Set Field types a decoded file of that
  name (s34) and goes through the write path above, or into Insert File's
  variable. FileMaker Pro's own answers are not observed (FileMaker
  Server has no handler for these steps, so the harness cannot run
  them); OpenMaker's choices: errors 100 (no path names a file), 826
  (invalid path), 102 (no active field), 413 (not a container field),
  734 (a file Insert Picture cannot show as a picture), 2 (a file over
  1 GiB). Not done: "Store only a reference" (how a reference is stored
  is not specified, 12), which stops the script as unsupported, and
  Insert File's display and compression options (their slots are not
  decoded, script-steps.md 9: always "content when possible", never
  compressed). A step without a path asks for the file (see "Desktop
  menus and file dialogs" below).
- A related container (a field of a portal row's occurrence, one
  relationship away; #568): a click in the row selects it, and Insert
  Picture or File from the menu, or a step naming the field, store the
  file in the related table's library (op88 for that table) and put the
  reference into the row's record: a saved row's, or in the creation row
  a new related record's. It is committed with the window's record, as
  Set Field writes a related field, and Revert Record releases what it
  took. The window shows the picture before the save. Export Field
  Contents from the menu writes a selected row's container. What FileMaker Pro
  does when a file is inserted into a creation row is unmeasured; the
  record is created as Set Field there creates one (script-runtime.md
  13.24).
- Export Field Contents (132): the field's main data stream, or its text
  as UTF-16 little-endian with a byte order mark (Claris: "a UTF-16
  format text file"; the encoding form is OpenMaker's choice), to the
  first path Export Records could write to, folders made only with
  "Create folders"; 800 when none can be written. OpenMaker Server writes
  text the same way. Without a path it asks for the output file (below).
- Insert from URL (`om-url`, shared with the server): see the notes on
  Insert from URL below.
- Stored and auto-enter calculations and validations of a write read
  container data as they run (2026-10-30, `om-provider` `containers`,
  `om_data::rules::ContainerReader`): the header of a member when a
  calculation reads the field naming it, its data only when a function
  reads its streams, in the branch the calculation takes, each once per
  write and through the connection's cache; a member the record names
  but no rule reads is never downloaded, so it can neither cost a
  download nor block the write. The rules run without the connection: a
  read not downloaded yet answers 1224 (counted as pending, so loops over
  it stop at once), the first such read of a run is downloaded and the
  rules run again, and a run that lacked a read is discarded even when it
  computed a value (`IsValid` and `EvaluationError` catch the 1224). Only
  the first is downloaded because the rest of that run saw `?` and may
  have taken a branch the complete data does not. A download that fails
  fails the write; data the library lacks, or external data it cannot
  read, is 1225 to the function that reads it, which refuses the write.
  `GetContainerAttribute ( field ; "filename" )` (any spacing or case of
  the name) answers from the header's file name on every host, like
  `GetAsText`, without loading the streams; an unnamed member and every
  other attribute still load them.
- Browse (2026-11-13, `om-gui` `document/container.rs`): a field object
  showing container data uses the header read above. A member whose
  `FNAM` path type is not `image` (a file, sound, a movie) shows a
  document icon above its file name, and its data is not downloaded,
  except a `file` path whose name ends in `.pdf`: its `PDF ` stream's
  first page is drawn (`hayro` in the PDF worker process, #566, as
  `GetThumbnail` draws it), a point of
  the page to a point of the layout, like a picture. Otherwise the client downloads the member (op90,
  not op96: OpenMaker scales locally), takes the first stream of `PNGf`,
  `JPEG`, `GIFf`, `BMPf`, `TIFF`, `WEBP` (6.2; a PDF's or HEIC picture's
  stored preview comes before its original) and draws it scaled by the
  object's graphic format (data-format.md). Data a calculation made is
  shown the same way from its streams. Decoding happens off the painting
  thread with at most 16,384 pixels a side and 256 MiB per picture; data
  that does not decode leaves the field empty, and a picture type it does
  not decode (HEIC, PICT, EPS, ...) without a preview shows as a file.
  Whether FileMaker Pro uses op96 for a record's picture (6.1 step 4) was
  not captured.

## Implementation notes (Insert from URL into containers)

2026-10-30 (`om-url`; script oracle s34-containers-http and -http-types:
FileMaker Server's engine fetching from a loopback HTTP server in the
harness container, and s34-containers-url).

- Into a container field the response is container data: named by
  `Content-Disposition`'s `filename` (quoted or not) when the response has
  one, else by the URL's last path segment, percent-decoded (`a%20b.png`
  is `a b.png`), without the query (`red.png?x=1` is `red.png`); a
  fragment stays in the name (`red.png#frag`; it is not sent). The data is
  typed by that last path segment, never by the disposition's name or
  `Content-Type`, as Set Field types a decoded file of that name (`x.gif`
  holding a PNG is `GIFf`; a disposition naming `x.gif` over `red.png` is
  `PNGf`; `text/plain` changes nothing), and named under a `file` path
  whatever its type (`FNAM` type `file`, unlike Set Field's `image` and
  `movie`). Without "Select entire contents" the container is replaced. An
  empty response is 1631.
- Into a text field the body is UTF-8 text with or without a charset; a
  PNG body there is 507. A variable gets text, or container data with
  `--FM-return-container-variable` (Claris).
- `file:` URLs: on FileMaker Server's engine any path outside the
  Documents and temporary folders is 100 (the harness has neither). Both
  OpenMaker hosts read them: the server only beneath its Documents and
  the session's temporary folders, opened through their handles like an
  import (`scripts/host/files.rs`); the desktop as its data file steps
  read files.
- OpenMaker Server's script sessions now run Insert from URL at all
  (before: 3), with the desktop's client (HTTP and HTTPS, the cURL
  options it supports, 256 MB body, timeouts, ending by the script's time
  limit); the script's abort ends the step at any point of the transfer
  (script-runtime.md, "Implementation notes (desktop client)").

## Implementation notes (calculations and bulk deletes)

2026-10-30 (script oracle s34-containers-calc, s34-containers-release,
-release-each, -release-partial, -release-truncate).

- Stored and auto-enter calculations see a container field's data on
  FileMaker Server's engine, before and after the commit: decoded `a.txt`
  ("hello") gives `GetContainerAttribute ( Photo ; "filename" )` `a.txt`,
  `Length` 5, `Base64Encode` `aGVsbG8=`; text a script put into the field
  is text (no file name, `Length` 5); a cleared value empties them, and an
  auto-entered value that replaces itself goes too (its "do not evaluate
  if all referenced fields are empty" leaves it empty, not unchanged).
  OpenMaker's record rules read containers through a `ContainerReader`
  (`om_data::rules`) on every host: server script sessions (the hosted
  file's libraries and the data a step holds for the commit), the
  server's recompute after a schema change (the table's library, set
  aside while the records are rewritten), and the desktop (above, reading
  only what the rules read). Before,
  a table with such a calculation refused every script write. Both hosts
  read text in a container field as text.
- Delete All Records over every record of a table and Truncate Table
  remove the table's whole library `[T, 1f]`, header included; Delete All
  Records over part of the table releases each deleted value (5.3), and
  records deleted one at a time leave the empty header (as op87, 7.3).
  OpenMaker Server does the same in the bulk deletion's saved commit and
  forgets the library's cached thumbnails (member ids start again).
  Inside a transaction the deletion releases values as usual (FileMaker's
  behavior there is not observed). Truncate Table's step loses its table
  in FMUpgradeTool's import; its battery runs a file `om-server`'s
  `step_table_probe` example makes.

## External storage: still open

The readers are in place (2026-10-30): `om_data::containers::read_with`
takes a host's `ExternalFiles`, turns a remote stream into its embedded
type with the file's bytes and marks the data external (`VerifyContainer`
answers whether every file was read); OpenMaker Server opens such files
only beneath `RC_Data_FMS/<file name>` beside the hosted file (8.1), a
name at a time without following links (`files::open_container_file`).
What is missing is the step between them. The remote stream reference,
`REMO`, the option bits and op95 are now specified from the code (8.1-8.4,
6.4), but no sample file stores a field externally, so the layouts are
not observed and parts of the folder tree are only partly traced (12.4);
until then neither host finds the file a member names, and external data
reads as missing. The desktop would read the files with op95 (6.4). The
Save as XML grammar for the setting is not public, so the script harness
cannot generate such a field either. The probe that confirms it needs
FileMaker Pro (on the capture Mac, in the isolated copy):

1. Copy `test project.fmp12` to a local scratch folder; in Manage
   Database, give Contacts::Photo "Store container data externally",
   open storage, default base directory; add a second field Photo2 with
   secure storage.
2. Insert the same two files into both fields of a new record: a small
   PNG (`openmaker-ext.png`) and a text file (`openmaker-ext.txt`).
3. Close the file. Read `[80 02, 1f, 05, *]` and `[80 02, 03, 05, 03]` and
   `[.., 05, <Photo2>]` with `om-hbam`'s `dump_keys`, and list the
   `Files/<file name>/` folder tree beside the file (names and sizes,
   `sha256sum` of each).
4. Then FMDeveloperTool `--saveAsXML` of the copy: its Storage element for
   the two fields gives the harness the XML to build external fields, so
   the rest (what FileMaker Server's engine writes, op95, VerifyContainer
   1/0) can be probed on the development machine.
5. In the same session, for the desktop's Insert steps (which FileMaker
   Server cannot run): in a script with error capture on, Insert Picture
   [Store only a reference] of the PNG into a third record's Photo (then
   dump that member: how a reference is stored), Insert Picture of the
   text file and Insert File into a text field (`Get ( LastError )` of
   each), Insert File of the PNG with "Display: Icon" and with "Compress
   when possible" (dump both members: stream types and `FNAM`), and
   Export Field Contents of a text field holding "hé" (the file's bytes:
   UTF-16 byte order and byte order mark).

Reading must then go through descriptor-relative opens beneath the hosted
file's container folder (`RC_Data_FMS/<file name>/`), as the server's
`scripts/host/files.rs` does, never a path built from stored data.


### Desktop review safeguards (2026-10-30)

The script host's text clipboard does not carry an owned container value.
Copy, Cut and Paste targeting container fields therefore return error 3
without changing data or reference counts. Replace Field Contents and plain
text/last-visited/index insertion into a Browse-mode container field are
also refused until they can preserve typed values and take one reference
per target. Clear and typed Set Field/Insert remain supported; Set Field
By Name routes container values through that same counted path. These are
temporary OpenMaker restrictions, not measured FileMaker error choices.

Local file URLs and Insert steps open nonblocking on Unix, verify that the
opened descriptor is a regular file, and read at most their limit plus one
byte before rejecting an oversized result. This prevents device/FIFO reads
and keeps the limit effective when a regular file grows after metadata was
read. A missing/unreadable/nonregular file is 100; the existing per-step
oversize errors are preserved.

External streams that cannot be read intact are refused with OpenMaker's
unsupported-operation error 1225 when a calculation reads their data,
Export Field Contents writes them, or a copy needs to upload them into a
different library. Export refuses before creating or truncating its target.
Header-only text reads still show the stored name and VerifyContainer
still reports 0. This is an explicit unsupported case, not native error
parity or support for external-storage references.

### Desktop container ownership checks (2026-10-30)

Duplicate Record copies populated, stored, normal, non-global container
fields; globals and container-result calculations are not sent as values.
A duplicate with container fields requires the calculation schema. Each
copied member takes a reference, and a coded create refusal releases those
references. A transport or verification failure can leave the write's
outcome unknown, so those references are retained rather than risking
stored data; recount/recovery after such a failure is still outstanding.
A later found-set refresh failure does not undo a successful create.

Before taking a reference or uploading data, the provider checks the fresh
record plan and refuses global/calculated container targets with 201.
A positive reference adjustment for a missing member fails with 3.

Setting a container to its stored value leaves no draft and releases the
unused newly acquired reference at once. A failed draft assignment also
releases what it acquired. Replacing a prior draft still releases that
draft's reference. Transaction acquisitions are counted separately:
rollback and a failure before op70 submission release every acquisition;
a verified successful transaction releases superseded/unused acquisitions
and keeps those its changed cells store. After op70 submission a transport
or verification failure can have stored the data, so acquisitions remain
for recovery. This intentionally permits orphans when the outcome is unknown.

A container draft that returns to its stored value keeps its acquired
reference until commit or revert. After a successful commit the desktop
releases that extra reference even though the field did not enter the
changed set. A failed commit keeps the draft and its reference. Literal
strings shaped like library references are refused with error 3; they did
not acquire ownership and cannot be used to copy or restore container data.
Use a typed container value instead. This is an OpenMaker restriction, not
a measured native error choice.


### Owning-file desktop integration (2026-10-31)

Related container headers and bodies follow `ExternalFiles::with_session`
through `NativeSource::with_table`, using the owning file's native table id.
A client-only imported table id never goes on the wire. The owning connection
also identifies cached headers/bodies and typed values: identically numbered
members in root and related files remain separate, and values from a closed
connection cannot be counted or copied through a reopened connection.
A copy into a writable root container reads the source on its owning session,
checks its original MD5/connection identity, and uploads all its stored streams
and its FNAM path type. Same-library copies validate identity using metadata
only before op87 +1. A replaced/missing source returns OpenMaker error 1225
before taking a reference or uploading replacement data. Unsupported related
writes, related layouts and related value lists keep their existing refusals.

The op60/op88 and reference-count facts above do not establish native
reconnect behavior or an entry-order requirement across files. OpenMaker's
never-submitted recovery across existing owning files and its bounded mixed
new/existing-row attachment workflow are its own policy, covered by synthetic
hosts only (see `docs/recovery.md`). Neither establishes native recovery parity.

The regression fixtures are synthetic in-process loopback hosts, including
two files with identical native table/record/field/member ids and deliberately
different names, hashes and streams. They establish these supported OpenMaker
paths, not native FileMaker Pro compatibility. The native probes listed above
remain a separate readiness gate; external-storage decoding, reference-only
and dialog insertion, display/compression options and text-export encoding
remain as documented. No native app or customer file is needed by these tests.


### Desktop menus and file dialogs (2026-11-02)

Claris documents that Insert File without a source file shows a dialog in
which the user selects a file, that its data goes into the target or the
active container field, and that Export Field Contents without an output
file asks the user for a file name and location ("Insert File script
step", "Export Field Contents script step"). The desktop does both. On
macOS the Insert commands' file comes from the system's open panel
(`NSOpenPanel`, shown modeless while the window waits on the step's
prompt, titled with the command and saying the field the file goes into;
`om-gui` `file_panel`), and on Linux from the desktop's file chooser,
asked through xdg-desktop-portal and titled with the command. On Windows,
on Linux without a file chooser portal, in the GUI tests, and for Export
Field Contents, an in-app dialog takes a path (absolute, or relative to
the Documents folder). Neither offers "Store only a reference
to the file", which FileMaker Pro's panel has: how FileMaker stores a
reference is unmeasured ("External storage: still open", step 5), so such
a step still stops as unsupported. That FileMaker Pro shows the panel
modeless rather than as a sheet, and its exact title and message, are
unmeasured [I].

[Claris's container commands](https://help.claris.com/en/pro-help/content/data-in-container-fields.html)
describe PDF and Audio/Video insertion for interactive containers.
[Claris's formatting help](https://help.claris.com/en/pro-help/content/formats-container-fields.html)
excludes fields directly in portal rows from interactive formatting and
allows fields inside a portal's popover. OpenMaker's interactive
containers are available outside portal rows.

- In Browse, clicking a container selects the field for Insert and Export
  and shows a focus ring. Leaving the field, layout, record or mode ends
  the selection. A refused field exit keeps focus on the previous field.
  See [Browse mode](../desktop-client.md#browse-mode) for picture and PDF
  display. Audio and video play on the current record in Form or List view
  outside portal rows, in a `web-viewer` build with a native player for
  that window (WebKitGTK on Linux/X11 and WKWebView on macOS).
- Insert > Picture..., Audio/Video..., PDF... and File... run script steps
  56, 159, 158 and 131 without a source path on the selected field. Insert
  requires full menus, a writable selection whose field object allows
  entry in Browse mode (Edit, not View Only or Select Only, which Claris
  documents as preventing changes through the object), and no running
  script. PDF and Audio/Video additionally require a field optimized for
  interactive content outside portal rows. The same checks are made again
  when the command runs. Edit > Export Field Contents... runs Export Field
  Contents (132).
- Insert PDF and Insert Audio/Video type the source by name using the
  same rules as FileMaker Server's Set Field (oracle s34). Insert PDF also
  requires a PDF header (`%PDF-` within the first 1024 bytes); Insert
  Audio/Video takes what the Browse player plays (a `movie` path, or a
  file named as audio or video, such as an `.aac` stored as a file). A
  file of the wrong type yields error 700 before any edit preparation or
  upload. The stored representation and error for these commands in
  FileMaker Pro remain unmeasured [I, OpenMaker's choice].
- Before showing the dialog, Insert checks that the selected stored
  container field can be written in Browse on the current connection.
  Export reads the field's data first and refuses external data that
  cannot be read intact (1225) before showing a dialog or writing output.
- The dialog's answer applies only to what it was opened for: the same
  file session, window and window UUID, mode, layout, record, active
  field and object, and field definition (the calculation schema it was
  resolved in; Manage Database in another window reads it again). The
  worker serves other windows while the dialog waits, and a peer file can
  have identical layout, record and field ids. Cancel (or Escape) is
  error 1; a changed context is error 3 with the message that nothing
  was changed; a lost connection or a closed file cancels. Neither
  uploads data, changes a record or a reference count, nor creates or
  changes an output file.
- Inserted data takes the same path as Set Field into a container field
  (op88/op87, record rules, transaction ownership); the record keeps the
  file as an unsaved change, together with the user's own typing, until
  it is saved or reverted. The menu commands run as one-step scripts, as
  Print and Export Records do, so the insertion fires no OnObjectModify
  (not measured against FileMaker Pro).
- Export offers the container's file name (its last path component; the
  field's name with `.txt` for text). An existing file is replaced only
  after the user confirms; whether or not a dialog was shown, the data is
  written to a temporary file beside the destination and renamed over it
  only when complete, so a failure or Cancel leaves an existing file as
  it was. Folders are created only with "Create folders".
- Not measured against FileMaker Pro: its dialog texts and buttons, its
  answer when the field or record changes while its dialog is open, and
  the selection's behavior in List and Table view (Table view cells are
  not selectable). Imported and related-file container writes keep their
  refusals.

Closing a popover ends its container-field selection; commands cannot keep
acting on the hidden field. Opening a menu alone keeps the selection.
