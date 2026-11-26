# File-contained layout images

`Layout::decode_from_tree` hydrates button and graphic images from the original
database's library. No FileMaker application resource or icon is copied into
OpenMaker. Hosted layout downloads include catalog root **31** in addition to
the layout, theme and font catalogs.

The caller-supplied Contacts test database contains eight SVG library members.
Their key paths are `31/5/<library_key>/0x53564720`: the final four-byte key is
the QuadChar `SVG `. Values are original UTF-8 SVG streams, including XML
prologs. Some are ordinary leaf values; the globe at library key 4 has 1,175 bytes
stored in multiple HBAM chunks. `Node::assembled()` reconstructs that stream
before decoding.

This matches DBEngine's `DBLibrary::GetCatalogKeyPath` at `0x18c27ec`, which uses
`KLibrary`, and `DBLibraryMember::AddQuadToDataPos` at `0x18bfaec`, which adds the
requested QuadChar beneath the member. These are static symbol/disassembly
observations from the arm64 image described in [schema-path.md](schema-path.md) 5.7.
The numeric root and SVG path are verified against the real local file rather
than inferred from a symbol name. Other container-library arrangements remain
outside this decoder.

The protobuf's `libraryKey` is the logical library id. Encode it as an
`HBSerialKey` ([query-program.md](query-program.md) 2.6) before looking up
the member: for example, id 200 uses the two-byte segment `80 48`. Ids up
to 127 use a single byte and need no conversion when read as integers.
The QuadChar beneath the member stays four bytes as written.

`Layout.library_images` is keyed by the referenced `library_key`. It contains
only nonzero keys referenced by that layout's decoded button or graphic objects.
Each `LibraryImage` retains the original encoded bytes and a format with MIME
type/extension helpers. SVG originals take precedence over a raster preview.
PNG/JPEG/GIF/BMP/TIFF/WebP fallback recognition uses image signatures rather
than guessing the meaning of an unknown library metadata key. Those fallback
formats are covered by synthetic decoding tests; the current test file verifies
SVG originals. Missing catalogs, missing members and unknown streams leave the
image absent without preventing the rest of the layout from opening.

In the Contacts file, the list's navigation button 7 refers to library key 1,
icon size 16 and display type 1. The details layout's Contacts back button 3 uses
key 3, size 13 and display type 4. The decoder exposes those original protobuf
values; placement/tint are rendering concerns.

Read-only inspection, without exporting any image or record content:

```sh
cargo run -p om-layout --example library_images -- '/path/to/file.fmp12'
OPENMAKER_CONTACTS_TEMPLATE='/path/to/file.fmp12' \
  cargo test -p om-layout --test rich_template -- --ignored
```
