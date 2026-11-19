//! Container data of a hosted file (spec `docs/re/containers.md`), for every
//! host: the reference a container field stores (1.4), the binary-data tree
//! of a library member (2) as calculations read it and as a writer builds it
//! from a calculation's container value, its file name (`FNAM`, 2.4) and its
//! MD5 (2.2).
//!
//! A table's container values live in its library `[T, 1f]`; the record's
//! field holds the member key as text. Hosts read a member's file name and
//! MD5 first ([`MemberInfo`]) and its streams only when a calculation
//! function reads them (`om_calc::Container::library`).

use md5::{Digest, Md5};
use om_calc::eval::StoredContainer;
use om_calc::{Container, LibraryRef, StreamType, Text};
use om_hbam::{serial, OwnedNode};
use std::io::Read;

/// The library key under a table, `[T, 1f]`, and under the file, `[1f]`.
pub const LIBRARY: u8 = 0x1f;

/// Keys of a library (1.2) and of its members (1.3).
pub mod keys {
    pub const HEADER: u8 = 0x01;
    pub const MEMBERS: u8 = 0x05;
    /// Under a member.
    pub const DIRECTORY: u8 = 0x02;
    pub const MD5: u8 = 0x04;
    pub const REF_COUNT: u8 = 0x12;
    pub const PRIVATE_FLAGS: u8 = 0xf1;
}

/// The longest stream value stored inline; longer ones are chunked (1.5).
pub const MAX_INLINE: usize = 1000;

/// The stream holding the data's file name (2.4) and the one naming the
/// main stream.
pub const FILE_NAME: &[u8; 4] = b"FNAM";
const MAIN: &[u8; 4] = b"MAIN";

/// The keys FileMaker Pro asks for when it reads a member's header (6.1
/// step 2): the directory, MD5 and count, the supporting types, the remote
/// types and the private flags; never an embedded data stream.
pub const HEADER_KEYS: [&[u8]; 48] = [
    &[keys::DIRECTORY],
    &[keys::MD5],
    &[keys::REF_COUNT],
    b"DPI_",
    b"ENCR",
    b"FNAM",
    b"GLPH",
    b"MAIN",
    b"REMO",
    b"ROT8",
    b"SIGN",
    b"SIZE",
    b"BAR#",
    b"BART",
    b".sgi",
    b"8bps",
    b"QTIF",
    b"aiff",
    b"au  ",
    b"avi ",
    b"bmpF",
    b"eps ",
    b"epsf",
    b"file",
    b"fldr",
    b"fpIX",
    b"gifF",
    b"jp2 ",
    b"jpeg",
    b"m4a ",
    b"m4v ",
    b"meta",
    b"mov ",
    b"mp3 ",
    b"mpeg",
    b"mpg4",
    b"pdf ",
    b"pict",
    b"pngF",
    b"pntg",
    b"tiff",
    b"tpic",
    b"wav ",
    b"webp",
    b"wma ",
    b"wmv ",
    b"zlib",
    &[keys::PRIVATE_FLAGS],
];

/// Remote types (an embedded type with the case of each letter flipped,
/// 2.3): they hold a reference to an externally stored file (8).
const REMOTE: &[&[u8; 4]] = &[
    b"jpeg", b"pngF", b"gifF", b"bmpF", b"tiff", b"pict", b"QTIF", b"8bps", b"fpIX", b".sgi",
    b"tpic", b"jp2 ", b"pntg", b"heic", b"webp", b"eps ", b"epsf", b"meta", // images
    b"pdf ", b"pgs ", b"key ", b"nmbr", b"ptxt", b"html", b"zip ", // documents
    b"mov ", b"wmv ", b"mpeg", b"mpg4", b"avi ", b"m4v ", b"mp3 ", b"wav ", b"au  ", b"aiff",
    b"m4a ", b"wma ", b"flac", b"moov", // media
    b"file", b"zlib", b"fldr", // files
];

/// Whether `kind` is a remote type (2.3).
pub fn is_remote(kind: &[u8]) -> bool {
    REMOTE.iter().any(|t| t.as_slice() == kind)
}

/// Whether the MD5 leaves `kind` out (2.2): key material, the glyph flag
/// and every remote type.
pub fn excluded_from_md5(kind: &[u8]) -> bool {
    kind == b"ENCR" || kind == b"GLPH" || is_remote(kind)
}

/// The MD5 of streams in directory order (2.2): each stream's type and
/// bytes, the excluded types left out.
pub fn md5<'a>(streams: impl IntoIterator<Item = (&'a [u8], &'a [u8])>) -> [u8; 16] {
    let mut hash = Md5::new();
    for (kind, bytes) in streams {
        if !excluded_from_md5(kind) {
            hash.update(kind);
            hash.update(bytes);
        }
    }
    hash.finalize().into()
}

/// The member a container field's value names: its text is the member key
/// as uppercase hexadecimal, then U+E000 (1.4). Only one-segment keys, the
/// form of a table library's members.
pub fn reference(text: &str) -> Option<Vec<u8>> {
    let hex = text.strip_suffix('\u{e000}')?;
    if hex.is_empty() || hex.len() % 2 != 0 || hex.contains('.') {
        return None;
    }
    let key: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect::<Option<_>>()?;
    serial::decode(&key).is_some().then_some(key)
}

/// The text a container field stores for `member` (1.4).
pub fn reference_text(member: &[u8]) -> String {
    let mut text: String = member.iter().map(|b| format!("{b:02X}")).collect();
    text.push('\u{e000}');
    text
}

/// The name of an `FNAM` path list (2.4): `u32 n`, then per path `[u8
/// len][type][u16 len][name]`, both masked SCSU. Embedded data has one
/// path whose name is the file name; for a list of several the first
/// path's name is taken.
pub fn file_name(bytes: &[u8]) -> Option<String> {
    first_path(bytes).map(|(_, name)| name)
}

/// The type and name of an `FNAM`'s first path.
pub fn first_path(bytes: &[u8]) -> Option<(String, String)> {
    let (count, rest) = bytes.split_first_chunk::<4>()?;
    if u32::from_be_bytes(*count) == 0 {
        return None;
    }
    let (&type_len, rest) = rest.split_first()?;
    let kind = om_scsu::decode_scsu(rest.get(..usize::from(type_len))?).ok()?;
    let rest = rest.get(usize::from(type_len)..)?;
    let (len, rest) = rest.split_first_chunk::<2>()?;
    let name = rest.get(..usize::from(u16::from_be_bytes(*len)))?;
    Some((kind, om_scsu::decode_scsu(name).ok()?))
}

/// The picture stream types a client draws, in the order it prefers them
/// when a member holds several: `PNGf` before `JPEG` (6.2, observed), then
/// the types FileMaker stores pictures under that have no preview beside
/// them (OpenMaker's order). A preview FileMaker stores beside a PDF, HEIC
/// or WebP picture (`PNGf` or `JPEG`) is chosen before the original.
pub const PICTURE_TYPES: [&[u8; 4]; 6] = [b"PNGf", b"JPEG", b"GIFf", b"BMPf", b"TIFF", b"WEBP"];

/// The stream of container data a client shows as a picture (6.2): the
/// first of [`PICTURE_TYPES`] it holds, with its bytes; `None` for data
/// with none of them, such as a file or media.
pub fn picture_stream(container: &Container) -> Option<(StreamType, &[u8])> {
    PICTURE_TYPES.iter().find_map(|kind| {
        container
            .streams
            .iter()
            .find(|(stream, bytes)| stream.0 == **kind && !bytes.is_empty())
            .map(|(stream, bytes)| (*stream, bytes.as_slice()))
    })
}

/// An `FNAM` of one path (2.4): `kind` (`image` for pictures, `file` for
/// other files) and the file name.
pub fn fnam(kind: &str, name: &str) -> Vec<u8> {
    let (kind, name) = (om_scsu::encode_scsu(kind), om_scsu::encode_scsu(name));
    let mut out = 1u32.to_be_bytes().to_vec();
    out.push(kind.len().min(255) as u8);
    out.extend(&kind[..kind.len().min(255)]);
    out.extend((name.len().min(0xffff) as u16).to_be_bytes());
    out.extend(&name[..name.len().min(0xffff)]);
    out
}

/// What a host reads of a member before its streams: the file name its
/// `FNAM` holds (empty without one), that path's type and the MD5 of its
/// data (`04`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberInfo {
    /// Owning desktop connection, when read over a session.
    pub owner: Option<[u8; 16]>,
    pub name: String,
    pub md5: [u8; 16],
    /// The type of the `FNAM` path (2.4): `image` for pictures and a PDF
    /// FileMaker rendered, `movie` for sound and movies, `file` for other
    /// files; empty without an `FNAM`.
    pub path_type: String,
}

impl MemberInfo {
    /// A member's header (its keys of [`HEADER_KEYS`] or the whole member);
    /// `None` without a 16-byte MD5.
    pub fn read(member: &OwnedNode) -> Option<MemberInfo> {
        let md5 = member
            .child(&[keys::MD5])
            .and_then(|n| n.value.as_deref())
            .and_then(|v| v.try_into().ok())?;
        let (path_type, name) = member
            .child(FILE_NAME)
            .and_then(OwnedNode::assembled)
            .and_then(|bytes| first_path(&bytes))
            .unwrap_or_default();
        Some(MemberInfo {
            name,
            md5,
            owner: None,
            path_type,
        })
    }

    /// The value of a container field naming this member of `table`'s
    /// library: its file name, its data left in the library.
    pub fn value(&self, table: u64, member: &[u8]) -> Container {
        Container {
            text: Text::plain(self.name.as_str()),
            library: Some(std::sync::Arc::new(LibraryRef {
                source: None,
                owner: self.owner,
                table,
                member: member.to_vec(),
                md5: self.md5,
            })),
            ..Container::default()
        }
    }
}

/// A member's data as calculations see it (`DBBinaryData`, 2): the file
/// name its `FNAM` holds as the text, every other stream in directory
/// order with the main one first (the type `MAIN` names, else the first
/// data stream), chunked values assembled (1.5), and whether the streams
/// match the stored MD5 (2.2) with their directory sizes, which
/// `VerifyContainer` answers. `None` when the member has no readable
/// directory. A member stored externally (a remote stream, 8) reads
/// without that data: [`read_with`] has the host fetch it.
pub fn read(member: &OwnedNode) -> Option<StoredContainer> {
    read_with(member, &NoExternalFiles)
}

/// Where a host reads externally stored container data (containers.md 8):
/// the file a remote stream refers to, beneath the hosted file's container
/// folder.
pub trait ExternalFiles {
    /// The bytes of the file remote stream `kind` (an embedded type with
    /// the case of each letter flipped, 2.3) of `member` refers to with
    /// `reference`; `None` when it cannot be read.
    fn read(&self, member: &OwnedNode, kind: [u8; 4], reference: &[u8]) -> Option<Vec<u8>>;
}

/// A host that reads no externally stored data.
pub struct NoExternalFiles;

impl ExternalFiles for NoExternalFiles {
    fn read(&self, _member: &OwnedNode, _kind: [u8; 4], _reference: &[u8]) -> Option<Vec<u8>> {
        None
    }
}

/// The embedded type of remote type `kind` (2.3: each letter's case
/// flipped back).
pub fn embedded_type(kind: [u8; 4]) -> [u8; 4] {
    kind.map(|b| if b.is_ascii_alphabetic() { b ^ 0x20 } else { b })
}

/// One complete gzip member, as a native ZLIB stream stores it. Do not
/// return partial output for a bad checksum, truncated body or trailing
/// data. The limit applies to the logical file, not its compressed size.
fn expand_gzip(bytes: &[u8], limit: u64) -> Option<std::sync::Arc<Vec<u8>>> {
    let mut decoder = flate2::bufread::GzDecoder::new(bytes);
    let mut expanded = Vec::new();
    decoder
        .by_ref()
        .take(limit + 1)
        .read_to_end(&mut expanded)
        .ok()?;
    if expanded.len() as u64 > limit || !decoder.into_inner().is_empty() {
        return None;
    }
    Some(std::sync::Arc::new(expanded))
}

/// [`read`], with externally stored data read through `external`: a
/// remote stream becomes its embedded type's stream with the file's bytes,
/// and the data counts as external (`VerifyContainer` then answers whether
/// every file was read). A file `external` cannot read leaves the data
/// without that stream, not intact; the reference bytes are never data.
pub fn read_with(member: &OwnedNode, external: &dyn ExternalFiles) -> Option<StoredContainer> {
    let directory = member.child(&[keys::DIRECTORY])?.value.as_deref()?;
    let (count, entries) = directory.split_first_chunk::<2>()?;
    let count = usize::from(u16::from_be_bytes(*count));
    let mut intact = entries.len() == count * 8;
    let mut hash = Md5::new();
    let mut name = String::new();
    let mut path_type = None;
    let mut remote = false;
    let mut streams = Vec::with_capacity(count);
    for entry in entries.chunks_exact(8).take(count) {
        let (kind, size) = entry.split_at(4);
        let kind: [u8; 4] = kind.try_into().expect("4 bytes");
        let size = u32::from_be_bytes(size.try_into().expect("4 bytes")) as usize;
        let Some(bytes) = member.child(&kind).and_then(OwnedNode::assembled) else {
            intact = false;
            continue;
        };
        intact &= bytes.len() == size;
        if is_remote(&kind) {
            remote = true;
            match external.read(member, kind, &bytes) {
                Some(data) => streams.push((StreamType(embedded_type(kind)), data)),
                None => intact = false,
            }
            continue;
        }
        if !excluded_from_md5(&kind) {
            hash.update(kind);
            hash.update(&bytes);
        }
        if &kind == FILE_NAME {
            if let Some((kind, file)) = first_path(&bytes) {
                path_type = Container::known_path_type(&kind);
                name = file;
            }
        } else {
            streams.push((StreamType(kind), bytes.into_owned()));
        }
    }
    let stored = member.child(&[keys::MD5]).and_then(|n| n.value.as_deref());
    intact &= stored == Some(hash.finalize().as_slice());
    let main = streams
        .iter()
        .find(|(kind, _)| &kind.0 == MAIN)
        .and_then(|(_, named)| <[u8; 4]>::try_from(named.as_slice()).ok())
        .and_then(|named| streams.iter().position(|(kind, _)| kind.0 == named))
        .or_else(|| streams.iter().position(|(kind, _)| !kind.is_supporting()));
    if let Some(main) = main {
        let stream = streams.remove(main);
        streams.insert(0, stream);
    }
    // FileMaker's ZLIB stream contains a gzip member. Keep its original
    // bytes for copying, MD5 and internal size, but expose the original
    // file to calculations and exports. A corrupt or incomplete stream
    // is unreadable, rather than successful compressed-byte output.
    let decompressed = match streams.first() {
        Some((StreamType::ZLIB, bytes)) => {
            // Match the desktop Insert File size limit. The cache budgets
            // count this buffer as well as its stored representation.
            const MAX_EXPANDED: u64 = 1 << 30;
            Some(expand_gzip(bytes, MAX_EXPANDED)?)
        }
        _ => None,
    };
    Some(StoredContainer {
        data: Container {
            text: Text::plain(name),
            streams,
            decompressed,
            // Stored data keeps its type; one without a path type reads
            // as typed by its stored streams too.
            path_type: path_type.or(Some("file")),
            ..Container::default()
        },
        intact,
        external: remote,
    })
}

/// A stream as the file stores it: inline up to 1000 bytes, otherwise child
/// `00` = the u32 length and 1000-byte pieces under serial keys 1, 2, ...
/// (1.5).
pub fn stream_node(kind: &[u8], bytes: &[u8]) -> OwnedNode {
    if bytes.len() <= MAX_INLINE {
        return OwnedNode::leaf(kind, bytes);
    }
    let mut children = vec![OwnedNode::leaf(vec![0], (bytes.len() as u32).to_be_bytes())];
    children.extend(
        bytes
            .chunks(MAX_INLINE)
            .enumerate()
            .map(|(i, piece)| OwnedNode::leaf(serial::encode(i as u64 + 1), piece)),
    );
    OwnedNode::list(kind, children)
}

/// The binary-data tree of a calculation's container value as a container
/// field stores it, as FileMaker Pro uploads it with op88 (2, 3.1): the
/// streams FileMaker stores for it (`om_calc::functions::stored_streams`:
/// decoded data typed by its file name's extension, with `SIZE` and `DPI_`
/// for a picture), the name as `FNAM`, the directory `02` in type order and
/// the MD5 `04`. `None` for a value without data (a bare reference text).
/// Returns the tree's keys and its MD5.
pub fn tree(container: &Container) -> Option<(Vec<OwnedNode>, [u8; 16])> {
    let (streams, path) = om_calc::functions::stored_streams(container)?;
    let mut streams: Vec<(Vec<u8>, Vec<u8>)> = streams
        .into_iter()
        .filter(|(kind, _)| &kind.0 != FILE_NAME)
        .map(|(kind, bytes)| (kind.0.to_vec(), bytes))
        .collect();
    if !container.text.is_empty() {
        streams.push((FILE_NAME.to_vec(), fnam(path, container.text.as_str())));
    }
    streams.sort_by(|a, b| a.0.cmp(&b.0));
    streams.dedup_by(|a, b| a.0 == b.0);
    let mut directory = (streams.len() as u16).to_be_bytes().to_vec();
    for (kind, bytes) in &streams {
        directory.extend(kind);
        directory.extend((bytes.len() as u32).to_be_bytes());
    }
    let md5 = md5(streams.iter().map(|(k, b)| (k.as_slice(), b.as_slice())));
    let mut keys = vec![
        OwnedNode::leaf(vec![keys::DIRECTORY], directory),
        OwnedNode::leaf(vec![keys::MD5], md5.to_vec()),
    ];
    keys.extend(streams.iter().map(|(k, b)| stream_node(k, b)));
    keys.sort_by(|a, b| a.segment.cmp(&b.segment));
    Some((keys, md5))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic text file inserted by FileMaker Pro 26.0.3 with
    /// "Compress when possible". The ZLIB stream holds a gzip member.
    #[test]
    fn compressed_file_exposes_logical_bytes_and_preserves_storage_attributes() {
        use om_calc::eval::{self, Context, NullHost};
        use om_calc::{Date, Locale, Value};

        let compressed = include_bytes!("../tests/fixtures/native-compressed-file.gz");
        let value = Container {
            text: Text::plain("openmaker-oracle.txt"),
            streams: vec![(StreamType(*b"ZLIB"), compressed.to_vec())],
            path_type: Some("file"),
            ..Container::default()
        };
        let (nodes, digest) = tree(&value).unwrap();
        let stored = read(&OwnedNode::list(vec![1], nodes)).unwrap();
        assert!(stored.intact && !stored.external);
        assert_eq!(
            stored.data.data(),
            "OpenMaker native container oracle\r\nhé\n".as_bytes()
        );
        assert_eq!(stored.data.streams, value.streams);
        assert_eq!(stored.data.retained_data_size(), 60 + 39);
        let cloned = stored.data.clone();
        assert!(std::sync::Arc::ptr_eq(
            cloned.decompressed.as_ref().unwrap(),
            stored.data.decompressed.as_ref().unwrap(),
        ));
        assert_eq!(tree(&stored.data).unwrap().1, digest);

        let mut host = NullHost::new(Locale::en_us(), Date::from_ymd(2026, 11, 1).unwrap());
        host.locals.set("$c", 1, Value::Container(stored.data));
        for (expression, expected) in [
            ("Length($c)", "39"),
            ("GetContainerAttribute($c;\"fileSize\")", "39"),
            (
                "GetContainerAttribute($c;\"MD5\")",
                "274803A1970D4A12FE94A7F2FD07E9E4",
            ),
            ("GetContainerAttribute($c;\"internalSize\")", "107"),
            (
                "Base64Encode($c)",
                "T3Blbk1ha2VyIG5hdGl2ZSBjb250YWluZXIgb3JhY2xlDQpow6kK\r\n",
            ),
            (
                "TextDecode($c;\"utf-8\")",
                "OpenMaker native container oracle\rhé\r",
            ),
        ] {
            let result = eval::evaluate_text(expression, &mut host, &Context::default());
            assert_eq!(result.code(), 0, "{expression}");
            assert_eq!(
                eval::display(&result.value, &host),
                expected,
                "{expression}"
            );
        }
    }

    #[test]
    fn compressed_bodies_require_complete_valid_data_within_the_limit() {
        let compressed = include_bytes!("../tests/fixtures/native-compressed-file.gz");
        assert_eq!(expand_gzip(compressed, 39).unwrap().len(), 39);
        assert!(expand_gzip(compressed, 38).is_none());
        for end in 0..compressed.len() {
            assert!(expand_gzip(&compressed[..end], 39).is_none(), "{end}");
        }
        let mut corrupt = compressed.to_vec();
        corrupt[compressed.len() - 8] ^= 1; // gzip's payload CRC
        assert!(expand_gzip(&corrupt, 39).is_none());
        let mut trailing = compressed.to_vec();
        trailing.push(0);
        assert!(expand_gzip(&trailing, 39).is_none());
        let value = Container {
            text: Text::plain("bad.txt"),
            streams: vec![(StreamType(*b"ZLIB"), corrupt)],
            path_type: Some("file"),
            ..Container::default()
        };
        let (nodes, _) = tree(&value).unwrap();
        assert!(read(&OwnedNode::list(vec![1], nodes)).is_none());
    }

    #[test]
    fn only_the_selected_compressed_main_stream_supplies_logical_bytes() {
        let compressed = include_bytes!("../tests/fixtures/native-compressed-file.gz");
        let value = Container {
            text: Text::plain("two.bin"),
            streams: vec![
                (StreamType::FILE, b"selected".to_vec()),
                (StreamType(*b"MAIN"), b"FILE".to_vec()),
                (StreamType(*b"ZLIB"), compressed.to_vec()),
            ],
            path_type: Some("file"),
            ..Container::default()
        };
        let member = |c: &Container| OwnedNode::list(vec![1], tree(c).unwrap().0);
        let ordinary = read(&member(&value)).unwrap();
        assert_eq!(ordinary.data.data(), b"selected");
        assert!(ordinary.data.decompressed.is_none());
        let mut value = value;
        value.streams[1].1 = b"ZLIB".to_vec();
        let compressed = read(&member(&value)).unwrap();
        assert_eq!(
            compressed.data.data(),
            "OpenMaker native container oracle\r\nhé\n".as_bytes()
        );
        assert_eq!(&compressed.data.main_stream().unwrap().0 .0, b"ZLIB");
    }

    /// Externally stored data (8): the remote stream refers to a file the
    /// host reads; its reference bytes are never data.
    #[test]
    fn remote_streams_read_through_the_host() {
        struct Files(Option<Vec<u8>>);
        impl ExternalFiles for Files {
            fn read(&self, _: &OwnedNode, kind: [u8; 4], reference: &[u8]) -> Option<Vec<u8>> {
                assert_eq!(&kind, b"pngF");
                assert_eq!(reference, b"ref");
                self.0.clone()
            }
        }
        let fnam = fnam("image", "a.png");
        let mut directory = 2u16.to_be_bytes().to_vec();
        for (kind, len) in [(b"FNAM", fnam.len()), (b"pngF", 3)] {
            directory.extend(kind);
            directory.extend((len as u32).to_be_bytes());
        }
        // Remote types are left out of the MD5 (2.2).
        let md5 = md5([(FILE_NAME.as_slice(), fnam.as_slice())]);
        let member = OwnedNode::list(
            vec![1],
            vec![
                OwnedNode::leaf(vec![keys::DIRECTORY], directory),
                OwnedNode::leaf(vec![keys::MD5], md5.to_vec()),
                OwnedNode::leaf(FILE_NAME.to_vec(), fnam),
                OwnedNode::leaf(b"pngF".to_vec(), b"ref".to_vec()),
            ],
        );
        let missing = read(&member).unwrap();
        assert!(missing.external && !missing.intact);
        assert!(missing.data.streams.is_empty());
        let found = read_with(&member, &Files(Some(b"\x89PNG".to_vec()))).unwrap();
        assert!(found.external && found.intact);
        assert_eq!(&found.data.streams[0].0 .0, b"PNGf");
        assert_eq!(found.data.data(), b"\x89PNG");
        assert!(!read_with(&member, &Files(None)).unwrap().intact);
        assert_eq!(embedded_type(*b"jp2 "), *b"JP2 ");
    }

    #[test]
    fn references_name_one_serial_member() {
        assert_eq!(reference("01\u{e000}"), Some(vec![0x01]));
        assert_eq!(reference("8005\u{e000}"), Some(vec![0x80, 0x05]));
        assert_eq!(reference("01"), None);
        assert_eq!(reference("1\u{e000}"), None);
        assert_eq!(reference("photo.png"), None);
        // `80` alone is not a serial key.
        assert_eq!(reference("80\u{e000}"), None);
        assert_eq!(reference_text(&[0x80, 0x05]), "8005\u{e000}");
        assert_eq!(reference(&reference_text(&[0x2a])), Some(vec![0x2a]));
    }

    /// A valid 3 x 2 PNG.
    fn png() -> Vec<u8> {
        const PNG: &str = "89504e470d0a1a0a0000000d49484452000000030000000208020000001216f14d\
                           0000001049444154789c63f8cfc000410c70160041d205fb87f0b94800000000\
                           49454e44ae426082";
        let hex: String = PNG.split_whitespace().collect();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn trees_round_trip_through_a_member() {
        // Stored data keeps its streams; long ones are chunked.
        let long = [0x89, b'P', b'N', b'G'].repeat(600);
        let value = Container {
            text: Text::plain("Ångström.png"),
            streams: vec![
                (StreamType(*b"PNGf"), long.clone()),
                (StreamType(*b"SIZE"), vec![0, 16, 0, 16]),
            ],
            ..Container::default()
        };
        let (keys, md5) = tree(&value).unwrap();
        let member = OwnedNode::list(vec![1], keys);
        assert_eq!(member.child(b"PNGf").unwrap().children.len(), 4);
        let info = MemberInfo::read(&member).unwrap();
        assert_eq!(info.name, "Ångström.png");
        assert_eq!(info.md5, md5);
        let stored = read(&member).unwrap();
        assert!(stored.intact);
        assert_eq!(stored.data.text.as_str(), "Ångström.png");
        assert_eq!(stored.data.streams, value.streams);
        // The value names the member and compares by its MD5.
        let named = info.value(0x8002, &[1]);
        assert!(named.is_unloaded());
        assert_eq!(named.library.as_ref().unwrap().md5, md5);
        assert_eq!(tree(&Container::default()), None);
    }

    /// Decoded data is stored as FileMaker stores it (script oracle s34):
    /// typed by the name's extension, a picture with its size.
    #[test]
    fn decoded_data_is_typed_by_its_extension() {
        let member = |name: &str, data: &[u8]| {
            let (keys, _) = tree(&Container::file(name, data.to_vec())).unwrap();
            OwnedNode::list(vec![1], keys)
        };
        let directory = |m: &OwnedNode| m.child(&[keys::DIRECTORY]).unwrap().value.clone().unwrap();
        let fnam_type = |m: &OwnedNode| {
            let f = m.child(FILE_NAME).unwrap().value.clone().unwrap();
            String::from_utf8(
                om_scsu::decode_scsu(&f[5..5 + usize::from(f[4])])
                    .unwrap()
                    .into(),
            )
            .unwrap()
        };
        // s34-containers-extensions x.png: FNAM, PNGf, SIZE 3 x 2.
        let picture = member("x.PNG", &png());
        assert_eq!(&directory(&picture)[..2], [0, 3]);
        assert_eq!(
            picture.child(b"SIZE").unwrap().value.as_deref(),
            Some(&[0, 3, 0, 2][..])
        );
        assert!(picture.child(b"PNGf").is_some());
        assert_eq!(fnam_type(&picture), "image");
        // A picture's bytes under another name are a file (red.txt), and
        // other bytes under a picture's name are that picture (fake.png).
        let txt = member("red.txt", &png());
        assert!(txt.child(b"FILE").is_some());
        assert_eq!(fnam_type(&txt), "file");
        let fake = member("fake.png", b"hello");
        assert!(fake.child(b"PNGf").is_some() && fake.child(b"SIZE").is_none());
        // Sound and movies (a.mp3: MP3 , x.mp4: MPG4).
        let mp3 = member("a.mp3", b"hello");
        assert!(mp3.child(b"MP3 ").is_some());
        assert_eq!(fnam_type(&mp3), "movie");
        assert!(member("x.mp4", b"hello").child(b"MPG4").is_some());
        assert_eq!(fnam_type(&member("x.pdf", b"hello")), "file");
        assert!(member("noext", b"hello").child(b"FILE").is_some());
    }
}
