# Server thumbnails

FileMaker Pro asks the host for a container's picture scaled to the box it
shows it in (op96 DownloadThumbnail, `docs/re/containers.md` 6.3).
OpenMaker Server (`crates/om-server/src/thumbnail.rs`) does the following:

1. **Pick the stream.** It takes the member's image stream, the first of
   `PNGf`, `JPEG`, `GIFf`, `BMPf`, `TIFF` and `WEBP`.
2. **Decode it** with the `image` crate.
3. **Scale it** to exactly the box, even when that stretches it, with a
   Catmull-Rom filter. The result is pixel for pixel what
   `imageops::resize` gives, but it is computed one output row at a time.
4. **Encode it**: JPEG at quality 85 for a JPEG source, PNG otherwise.
5. **Cache it and serve it in chunks.** Each file keeps its 64 most recent
   thumbnails, up to 64 MiB of them.

The answer is always unencrypted (return flags 0).

## Memory

The image comes from a client (op88), so a small file can claim a large
image, or a large table, and it is untrusted. What a request may allocate
is therefore worked out before anything is decoded, in two steps, and
reserved from one budget of 1 GiB shared by every request in progress.

1. **Reading the header** (`header_room`). The figure comes from the data
   alone, after the claims a decoder would reserve memory for have been
   checked against the data's length.
2. **Rendering** (`plan`). The figure comes from the header: the decoded
   image and the decoder's own buffers, then the source and the scaled
   image, then the scaled image and its encoding (the source is freed by
   then). A render keeps the header's share while it decodes.

Each step waits for room before it starts, and holds neither the file's
store nor the thumbnail cache while it waits. A step whose figure is above
the budget is refused, and the client gets `THUMBNAIL_MISSING`, as for data
that is not an image. A request waiting for room holds only the stream's
bytes.

Reading a header is bounded as follows. "Measured" means a hostile file
built for it was decoded and its heap counted. The worst file of each kind
is compared with what main took before this change.

| Format | How the header read is bounded | Evidence |
|---|---|---|
| TIFF | The tiff crate reserves room for a tag's values from the count the tag claims, before reading them, up to 256 MiB per tag. The server reads the first directory itself and refuses a TIFF in which a tag the decoder reads claims values its data does not hold. The tags it reads are listed in `TIFF_READ_TAGS`; others, such as ICC, XMP and orientation, are read only on request, which rendering never makes. Reading them is counted at 40 bytes a value. | Measured. A 122-byte TIFF claiming 8,000,000 strip entries took 244.1 MiB before and is now refused before any decoder runs. 1,000,000 strip entries actually present in 9 MB of data take 45.8 MiB. |
| JPEG | Up to 3 times the data: a copy of it, its metadata segments (each at most 64 KiB and present in the data), and extended XMP joined into one more copy. | Measured: 19.7 MB of XMP, extended XMP and ICC segments take 31.4 MiB. |
| PNG | The decoder is given a limit of 3 times the text, ICC and EXIF chunks it keeps, plus 1 MiB, counted from the chunk headers. An ICC profile is decompressed up to what is left of that limit; a larger one is dropped, and rendering never uses it. The vector it grows by doubling takes up to 3 times that again. | Measured. 204 KB of ICC that inflates to 200 MB took 25.0 MiB before and 3.4 MiB now. A 10 MB text chunk takes 26.0 MiB. |
| GIF | XMP and ICC extensions are kept as they arrive, in vectors that grow by doubling: up to 3 times the data. | Measured: 10.5 MB of XMP takes 24.0 MiB. |
| WebP | Only the known chunk kinds are indexed, one entry each, and only the 6-byte ANIM chunk is read. | Measured: 200,000 unknown chunks take nothing measurable. |
| BMP | Fixed headers and a palette of at most 256 colours. | Measured: a claim of 2^31 palette colours is refused by the decoder, nothing allocated. |

On top of each figure, 1 MiB is allowed for a decoder's state, tables and a
row.

## Limits

- **The figures are tied to library versions.** They are read from the
  sources of the versions in `Cargo.lock`:

  | Crate | Version |
  |---|---|
  | image | 0.25.10 |
  | png | 0.18.1 |
  | fdeflate | 0.3.7 |
  | tiff | 0.11.3 |
  | zune-jpeg | 0.5.15 |
  | image-webp | 0.2.4 |
  | gif | 0.14.2 |

  An upgrade of any of them must be checked against `header_room`,
  `plan`, `tiff_tags`, `png_limit` and `jpeg_layout`.
- **Not counted in the budget:** the stream's bytes, which each request
  copies out of the library before it waits.
- **Time is not bounded.** A render takes as long as it takes; the slowest
  measured is about 2.5 s.
- **Some images get no thumbnail** (`THUMBNAIL_MISSING`):
  - TIFFs a tag of which, among those the decoder reads, points past the
    data. The tiff crate could not read them either.
  - Palette (indexed) TIFFs, which the `image` crate does not decode
    ([#141](https://github.com/OpenMaker-Dev/openmaker/issues/141)).
  - Floating-point images, which the PNG encoder cannot write.
  - Renders planned above the budget, for example a progressive JPEG above
    about 87 megapixels.
- **Not compared with FileMaker.** What FileMaker Server does with these
  files is unmeasured.
