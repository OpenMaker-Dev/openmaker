//! A new PDF made of pages of others: the documents the MBS plug-in
//! subset's DynaPDF commands assemble (`om_scripts::runtime::dynapdf`,
//! docs/plugins.md). Read in the PDF worker process only (`pdf_worker`),
//! as every PDF is.
//!
//! Each page of the plan is copied with what it refers to: its content,
//! resources, fonts, images, annotations (but see below for references to
//! the catalog, the page tree and pages not copied, which a whole copy
//! refuses). Streams keep their
//! encoded bytes and filters (decrypted, if the source was encrypted with
//! an empty password); numbers, strings and names are written as read. An
//! object two pages share is copied once. A page's inherited attributes
//! (Resources, MediaBox, CropBox, Rotate) are written on the page. A
//! reference to a page that is not copied, to the page tree or to the
//! document's catalog becomes `null`, so no copy drags in the rest of its
//! document; one to a copied page names that page in the new document.
//! The document has no outlines, structure tree or interactive form: what
//! needs them is copied as the objects the pages refer to. Nor does it keep
//! a source's optional content configuration, without which hidden content
//! may show, so a source that has one is refused
//! ([`optional_content_kept`]).
//!
//! A page can be changed as it is copied ([`Planned`]): its boxes set, and
//! a page of one of the sources drawn over it (text the DynaPDF commands
//! wrote), as a form XObject after the page's own content, which is
//! wrapped in `q` ... `Q` so its graphics state cannot leak into it. The
//! page gets resources of its own then, the original entries and the form
//! under a name none of its XObjects has, so no shared resources
//! dictionary changes and no name the page's content uses is taken. A
//! turned page (`/Rotate`) is not drawn over (refused).
//!
//! A dictionary entry whose value is `null`, or a reference to an object
//! that does not exist, is absent (PDF 1.7, 7.3.7 and 7.3.10): a page with
//! `/Resources null` inherits its parent's.

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::ops::Range;
use std::sync::Arc;

use hayro::hayro_interpret::hayro_syntax::object::{
    Array, Dict, MaybeRef, Name, Number, Object, ObjectIdentifier, Rect,
};
use hayro::hayro_interpret::hayro_syntax::page::Rotation;
use hayro::hayro_interpret::hayro_syntax::Pdf;

use super::pdf_form::{self, Field, Highlight, Link};

/// The most source documents and planned pages one request holds.
pub const MAX_SOURCES: usize = 64;
pub const MAX_PAGES: usize = 100_000;
/// The most objects one new document holds, and how deeply direct values
/// may nest in one object.
const MAX_OBJECTS: u32 = 2_000_000;
const MAX_DEPTH: usize = 64;
/// How many `/Parent` levels an inherited page attribute is looked up in.
const MAX_PARENTS: usize = 64;
/// The page attributes a page inherits from its ancestors.
const INHERITED: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// The four bytes of the binary-marker comment (PDF 1.7, 7.5.2) after the
/// header of every PDF OpenMaker writes: its reports and the documents this
/// assembler makes. Appending copies only a document that carries it
/// ([`document_kept`]): one OpenMaker wrote, whose every part it knows.
pub const MARKER: &[u8; 4] = b"\xcf\xcd\xcb\xb4";

/// How many of a PDF's pages [`info`] describes.
pub const INFO_PAGES: usize = 10_000;

/// What [`info`] tells of a page: whether it is turned (`/Rotate`), which
/// [`assemble`] does not draw over, and its boxes by [`BoxKind`], each
/// lower-left x and y, upper-right x and y, as written on the page or (the
/// media and crop boxes) inherited, `None` where it has none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageGeometry {
    pub turned: bool,
    pub boxes: [Option<[f64; 4]>; 5],
}

/// A PDF's page count, whether [`assemble`] keeps how its pages look
/// ([`optional_content_kept`]), with `whole` whether it keeps everything
/// else the document holds ([`document_kept`]), and the geometry of its
/// first [`INFO_PAGES`] pages; `None` for data that does not read.
pub(super) fn info(
    data: Vec<u8>,
    whole: bool,
) -> Option<(u32, bool, Option<bool>, Vec<PageGeometry>)> {
    let pdf = Pdf::new(data).ok()?;
    let pages = pdf.pages();
    let count = u32::try_from(pages.len()).ok()?;
    let mut tree = Inherited::default();
    let geometry = pages
        .iter()
        .take(INFO_PAGES)
        .map(|page| {
            let [media, crop] = tree.boxes(&pdf, page.raw());
            let own = |kind| own_box(page.raw(), kind).flatten();
            PageGeometry {
                turned: !matches!(page.rotation(), Rotation::None),
                boxes: [
                    media,
                    crop,
                    own(BoxKind::Trim),
                    own(BoxKind::Bleed),
                    own(BoxKind::Art),
                ],
            }
        })
        .collect();
    Some((
        count,
        optional_content_kept(&pdf),
        whole.then(|| document_kept(&pdf)),
        geometry,
    ))
}

/// Check only the selected source pages. The index prefix is bounded
/// before allocation; move the PDF buffer into the parser without a
/// second full-size copy. A non-array Annots value is refused.
pub(super) fn annotation_free(mut data: Vec<u8>, count: u32) -> Option<bool> {
    if count as usize > MAX_PAGES {
        return None;
    }
    let len = (count as usize).checked_mul(4)?;
    let pages = data
        .get(..len)?
        .chunks_exact(4)
        .map(|b| u32::from_be_bytes(b.try_into().expect("four-byte index")))
        .collect::<Vec<_>>();
    data.drain(..len);
    let pdf = Pdf::new(data).ok()?;
    for index in pages {
        let page = pdf.pages().get(index as usize)?;
        match (
            page.raw().get::<Array<'_>>(b"Annots"),
            effective(page.raw(), b"Annots"),
        ) {
            (Some(items), _) if items.raw_iter().next().is_some() => return Some(false),
            (Some(_), _) | (None, None) => {}
            _ => return None,
        }
    }
    Some(true)
}

/// [`info`]'s geometry as bytes: for each page a `u8` of flags (bit `k`
/// for a box of kind `k`, bit 7 for a turned page), then each box it has,
/// four `f64`s, big-endian.
pub(super) fn encode_geometry(pages: &[PageGeometry], out: &mut Vec<u8>) {
    for page in pages {
        let mut flags = u8::from(page.turned) << 7;
        for (kind, rect) in page.boxes.iter().enumerate() {
            if rect.is_some() {
                flags |= 1 << kind;
            }
        }
        out.push(flags);
        for value in page.boxes.iter().flatten().flatten() {
            out.extend(value.to_be_bytes());
        }
    }
}

/// [`encode_geometry`]'s bytes read back, exactly `pages` of them; `None`
/// for any other length, unknown flag, or number that is not finite.
pub(super) fn decode_geometry(mut bytes: &[u8], pages: usize) -> Option<Vec<PageGeometry>> {
    let mut out = Vec::with_capacity(pages.min(INFO_PAGES));
    for _ in 0..pages {
        let (&flags, rest) = bytes.split_first()?;
        bytes = rest;
        if flags & 0b0110_0000 != 0 {
            return None;
        }
        let mut page = PageGeometry {
            turned: flags & 0x80 != 0,
            ..PageGeometry::default()
        };
        for (kind, rect) in page.boxes.iter_mut().enumerate() {
            if flags & (1 << kind) == 0 {
                continue;
            }
            let mut values = [0.0; 4];
            for value in &mut values {
                let (number, rest) = bytes.split_at_checked(8)?;
                bytes = rest;
                *value = f64::from_be_bytes(number.try_into().ok()?);
            }
            if values.iter().any(|v| !v.is_finite()) {
                return None;
            }
            *rect = Some(values);
        }
        out.push(page);
    }
    bytes.is_empty().then_some(out)
}

/// A dictionary's own box of `kind`, its corners ordered lower-left then
/// upper-right as PDF readers take them (PDF 1.7, 7.9.5), read at full
/// precision: `None` when it has none, `Some(None)` for one that is not
/// exactly four finite numbers.
fn own_box(dict: &Dict<'_>, kind: BoxKind) -> Option<Option<[f64; 4]>> {
    effective(dict, kind.key())?;
    let read = || {
        let array = dict.get::<Array<'_>>(kind.key())?;
        // Exactly four entries, counted before they are resolved: the
        // resolving iterator ends at one that does not resolve.
        if array.raw_iter().take(5).count() != 4 {
            return None;
        }
        let mut items = array.iter::<Object<'_>>();
        let mut values = [0.0; 4];
        for value in &mut values {
            *value = match items.next()? {
                Object::Number(n) if n.as_f64().is_finite() => n.as_f64(),
                _ => return None,
            };
        }
        let [x0, y0, x1, y1] = values;
        Some([x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)])
    };
    Some(read())
}

/// The page-tree nodes read so far, so that a node is read once however
/// many pages it has: reading it again for each page would cost its long
/// `/Kids` array each time. The dictionaries are kept, not the boxes read
/// from them, and only nodes that read, as in [`Copier::parent`]: a failed
/// lookup may repair the cross-reference table, after which a reference
/// that failed (a parent, or a box kept in an object of its own) can
/// resolve.
#[derive(Default)]
struct Inherited<'a>(HashMap<ObjectIdentifier, Dict<'a>>);

impl<'a> Inherited<'a> {
    /// A page's media and crop boxes (PDF 1.7, 7.7.3.4): its own, or the
    /// nearest of its [`MAX_PARENTS`] nearest ancestors', counted from the
    /// page whatever was read before. A parent is an indirect reference
    /// (7.7.3.3).
    fn boxes(&mut self, pdf: &'a Pdf, page: &Dict<'a>) -> [Option<[f64; 4]>; 2] {
        let own = |dict: &Dict<'_>| [own_box(dict, BoxKind::Media), own_box(dict, BoxKind::Crop)];
        let mut found = own(page);
        let mut node = page.clone();
        for _ in 0..MAX_PARENTS {
            if found.iter().all(Option::is_some) {
                break;
            }
            let Some(MaybeRef::Ref(reference)) = node.get_raw::<Object<'_>>(b"Parent") else {
                break;
            };
            let id = ObjectIdentifier::from(reference);
            node = match self.0.get(&id) {
                Some(dict) => dict.clone(),
                None => {
                    let Some(dict) = pdf.xref().get::<Dict<'a>>(id) else {
                        break;
                    };
                    self.0.insert(id, dict.clone());
                    dict
                }
            };
            for (found, own) in found.iter_mut().zip(own(&node)) {
                if found.is_none() {
                    *found = own;
                }
            }
        }
        found.map(Option::flatten)
    }
}

/// Whether a document's pages look the same in a new document, which
/// carries no optional content configuration (PDF 1.7, 8.11.4): only when
/// the document has none. Without it a reader ignores the content's
/// optional content markers, so content its configuration hides shows: a
/// group turned off, or content a membership dictionary hides while its
/// groups are on (`/P /AnyOff`), whatever the configuration says.
pub(super) fn optional_content_kept(pdf: &Pdf) -> bool {
    pdf.xref()
        .get::<Dict<'_>>(pdf.xref().root_id())
        .is_none_or(|catalog| !catalog.contains_key(b"OCProperties"))
}

/// Whether a new document of all of a document's pages ([`assemble`]) keeps
/// all that it holds:
/// - its catalog names nothing past its type and page tree (no outlines,
///   interactive form, structure tree, names, page labels, metadata
///   stream, open action, page mode or viewer preferences, which the page
///   copy leaves behind);
/// - its trailer names nothing the new trailer leaves out
///   ([`trailer_kept`]: no document information, file identifiers or
///   encryption);
/// - hayro reads no document information from it either;
/// - its page tree reads the same down its `/Kids` and up each page's
///   `/Parent` links, within the [`MAX_PARENTS`] ancestors the copy reads
///   inherited attributes from ([`tree_kept`]).
///
/// - and first of all, it is a PDF 1.7 OpenMaker wrote ([`MARKER`]): a
///   PDF another program wrote, or rewrote, can hold what reading it and
///   writing it again would change (how a reader repairs it, comments
///   inside its references, how its strings are spelled), which no check
///   here can rule out.
///
/// A document OpenMaker's reports or this assembler wrote, without
/// annotations, keeps all; appending to a PDF refuses one that does not
/// rather than drop what it holds.
pub(super) fn document_kept(pdf: &Pdf) -> bool {
    let mut header = b"%PDF-1.7\n%".to_vec();
    header.extend(MARKER);
    header.push(b'\n');
    if !pdf.data().as_ref().starts_with(&header) {
        return false;
    }
    let Some(catalog) = pdf.xref().get::<Dict<'_>>(pdf.xref().root_id()) else {
        return false;
    };
    catalog
        .keys()
        .all(|key| matches!(key.as_ref(), b"Type" | b"Pages"))
        && trailer_kept(pdf.data().as_ref())
        && *pdf.metadata() == hayro::hayro_interpret::hayro_syntax::metadata::Metadata::default()
        && tree_kept(pdf)
}

/// Whether the trailer of the cross-reference table the file's last
/// `startxref` names holds only `/Size` and `/Root`, the entries the new
/// document's trailer has (PDF 1.7, 7.5.5; an update's trailer repeats
/// every entry, 7.5.6). The table is read entry by entry up to its
/// `trailer` keyword, so no comment or other text stands in for it, and
/// the trailer's keys are read as written, so an escaped name
/// (`/Encr#79pt`) counts as another key. Anything else refuses: document
/// information (`/Info`), file identifiers (`/ID`), encryption
/// (`/Encrypt`), earlier revisions (`/Prev`), a hybrid file's
/// cross-reference stream (`/XRefStm`), and a file whose cross-references
/// are a stream, or whose table or trailer does not read.
fn trailer_kept(data: &[u8]) -> bool {
    table(data).is_some_and(|table| table.kept)
}

/// A file's one cross-reference table, as [`trailer_kept`] reads it.
struct Table {
    /// Whether its trailer holds only `/Size` and `/Root`, and the file
    /// has only this one revision.
    kept: bool,
    /// The objects it lists in use (`n`), at most [`MAX_OBJECTS`].
    in_use: Vec<ObjectIdentifier>,
}

/// The file's cross-reference table and trailer, read entry by entry from
/// where `startxref` names; `None` when they do not read. The file must
/// spell `startxref` and `trailer` once each: a file with more (another
/// revision, or a comment naming an older table) is not kept, as which
/// revision a reader takes is then not this table's alone.
fn table(data: &[u8]) -> Option<Table> {
    const STARTXREF: &[u8] = b"startxref";
    let count = |word: &[u8]| data.windows(word.len()).filter(|w| *w == word).count();
    let single = count(STARTXREF) == 1 && count(b"trailer") == 1;
    let end = data
        .windows(STARTXREF.len())
        .rposition(|w| w == STARTXREF)?;
    let mut at = end + STARTXREF.len();
    let offset = integer(token(data, &mut at)?)?;
    // Only what comes before `startxref` is the table and its trailer.
    let data = data.get(..end)?;
    let mut at = usize::try_from(offset).ok()?;
    if token(data, &mut at)? != b"xref" {
        return Some(Table {
            kept: false,
            in_use: Vec::new(),
        });
    }
    let mut in_use = Vec::new();
    loop {
        let word = token(data, &mut at)?;
        if word == b"trailer" {
            break;
        }
        let first = integer(word)?;
        let count = integer(token(data, &mut at)?)?;
        // Each entry is an offset, a generation and `n` or `f`; the data
        // ends the loop whatever the count says.
        for number in first.. {
            if number >= first.saturating_add(count) {
                break;
            }
            integer(token(data, &mut at)?)?;
            let generation = integer(token(data, &mut at)?)?;
            match token(data, &mut at)? {
                b"n" => {
                    if in_use.len() >= MAX_OBJECTS as usize {
                        return None;
                    }
                    in_use.push(ObjectIdentifier::new(
                        i32::try_from(number).ok()?,
                        i32::try_from(generation).ok()?,
                    ));
                }
                b"f" => {}
                _ => return None,
            }
        }
    }
    let mut root = false;
    let kept = dictionary(data, &mut at, 0, &mut |key| {
        root |= key == b"Root";
        matches!(key, b"Size" | b"Root")
    })?;
    Some(Table {
        kept: kept && root && single,
        in_use,
    })
}

/// The non-negative integer `word` spells.
fn integer(word: &[u8]) -> Option<u64> {
    if word.is_empty() || !word.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(word).ok()?.parse().ok()
}

/// PDF white space (PDF 1.7, 7.2.2) and comments from `at`.
fn skip_space(data: &[u8], at: &mut usize) {
    while let Some(&byte) = data.get(*at) {
        match byte {
            b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' => *at += 1,
            b'%' => {
                while data.get(*at).is_some_and(|&b| b != b'\n' && b != b'\r') {
                    *at += 1;
                }
            }
            _ => return,
        }
    }
}

/// Whether `byte` ends a name, number or keyword (white space or a
/// delimiter, PDF 1.7, 7.2.2).
fn ends_token(byte: u8) -> bool {
    matches!(
        byte,
        b'\0'
            | b'\t'
            | b'\n'
            | b'\x0c'
            | b'\r'
            | b' '
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

/// The name, number or keyword at `at` (after white space), as written;
/// `None` at the end of the data or a delimiter.
fn token<'d>(data: &'d [u8], at: &mut usize) -> Option<&'d [u8]> {
    skip_space(data, at);
    let start = *at;
    while data.get(*at).is_some_and(|&b| !ends_token(b)) {
        *at += 1;
    }
    data.get(start..*at).filter(|word| !word.is_empty())
}

/// Reads the dictionary at `at` (after white space), giving `key` each of
/// its keys as written and skipping each value ([`skip_value`]): `false`
/// at the first key `key` refuses, `None` when it does not read or nests
/// deeper than [`MAX_DEPTH`]. Nothing read is kept.
fn dictionary(
    data: &[u8],
    at: &mut usize,
    depth: usize,
    key: &mut dyn FnMut(&[u8]) -> bool,
) -> Option<bool> {
    skip_space(data, at);
    if depth > MAX_DEPTH || !data.get(*at..)?.starts_with(b"<<") {
        return None;
    }
    *at += 2;
    loop {
        skip_space(data, at);
        let rest = data.get(*at..)?;
        if rest.starts_with(b">>") {
            *at += 2;
            return Some(true);
        }
        if !rest.starts_with(b"/") {
            return None;
        }
        *at += 1;
        // An empty name (`/` alone) is a key too.
        let start = *at;
        while data.get(*at).is_some_and(|&b| !ends_token(b)) {
            *at += 1;
        }
        if !key(data.get(start..*at)?) {
            return Some(false);
        }
        skip_value(data, at, depth + 1)?;
    }
}

/// Moves `at` past the object written there (after white space): a
/// dictionary, array, string, name, or a number, keyword or reference
/// (`12 0 R`).
fn skip_value(data: &[u8], at: &mut usize, depth: usize) -> Option<()> {
    skip_space(data, at);
    if depth > MAX_DEPTH {
        return None;
    }
    match *data.get(*at)? {
        b'<' if data.get(*at + 1) == Some(&b'<') => {
            dictionary(data, at, depth, &mut |_| true).map(drop)
        }
        b'<' => {
            *at += data.get(*at..)?.iter().position(|&b| b == b'>')? + 1;
            Some(())
        }
        b'[' => {
            *at += 1;
            loop {
                skip_space(data, at);
                if *data.get(*at)? == b']' {
                    *at += 1;
                    return Some(());
                }
                skip_value(data, at, depth + 1)?;
            }
        }
        b'(' => {
            let mut open = 0usize;
            loop {
                match *data.get(*at)? {
                    b'\\' => *at += 1,
                    b'(' => open += 1,
                    b')' => {
                        open = open.checked_sub(1)?;
                        if open == 0 {
                            *at += 1;
                            return Some(());
                        }
                    }
                    _ => {}
                }
                *at += 1;
            }
        }
        b'/' => {
            *at += 1;
            while data.get(*at).is_some_and(|&b| !ends_token(b)) {
                *at += 1;
            }
            Some(())
        }
        _ => {
            let first = token(data, at)?;
            // A reference: two integers and `R`.
            if integer(first).is_some() {
                let mut ahead = *at;
                if token(data, &mut ahead).is_some_and(|g| integer(g).is_some())
                    && token(data, &mut ahead) == Some(b"R")
                {
                    *at = ahead;
                }
            }
            Some(())
        }
    }
}

/// Whether the page tree read down its `/Kids` is the one read up its
/// `/Parent` links, within reach of the copy ([`page_tree`]).
fn tree_kept(pdf: &Pdf) -> bool {
    page_tree(pdf).is_some()
}

/// The page tree's nodes (not its pages), when the tree read down its
/// `/Kids` is the one read up its `/Parent` links, within reach of the copy
/// and wholly understood by it:
/// - from the catalog's `/Pages`, each node and page is reached once, by
///   reference, names as its `/Parent` the node whose `/Kids` lists it (the
///   root none), is a `Pages` node or a `Page`, and is at most
///   [`MAX_PARENTS`] levels down; and these are all the pages the document
///   reads as. Readers find a page's inherited attributes down the tree,
///   the copy up its parents (`Copier::inherited`): only where both agree
///   does it keep them;
/// - an inheritable attribute, on a node or a page, is of its type (a
///   dictionary of resources, a rectangle, an integer) or `null`: readers
///   pass over one that is not, which the copy would write on the page;
/// - a node holds nothing but its tree (not even what its pages inherit,
///   which the copy writes on each page and so would drop where every page
///   has its own): the new document has a page tree of its own, so
///   anything else a node held would be left behind;
/// - a page has no annotations, whose strings a copy may not spell as
///   they were.
///
/// `None` otherwise.
fn page_tree(pdf: &Pdf) -> Option<std::collections::HashSet<ObjectIdentifier>> {
    let xref = pdf.xref();
    let catalog = xref.get::<Dict<'_>>(xref.root_id())?;
    let Some(MaybeRef::Ref(root)) = catalog.get_raw::<Object<'_>>(b"Pages") else {
        return None;
    };
    let mut seen = std::collections::HashSet::new();
    let mut nodes = std::collections::HashSet::new();
    let mut pages = 0usize;
    let mut stack = vec![(ObjectIdentifier::from(root), None, 0usize)];
    while let Some((id, parent, depth)) = stack.pop() {
        if depth > MAX_PARENTS || !seen.insert(id) {
            return None;
        }
        let node = xref.get::<Dict<'_>>(id)?;
        let linked = match (parent, node.get_raw::<Object<'_>>(b"Parent")) {
            (None, None) => true,
            (Some(parent), Some(MaybeRef::Ref(reference))) => {
                ObjectIdentifier::from(reference) == parent
            }
            _ => false,
        };
        let typed = INHERITED
            .iter()
            .all(|key| match node.get::<Object<'_>>(key) {
                None | Some(Object::Null(_)) => true,
                Some(_) => match *key {
                    b"Resources" => node.get::<Dict<'_>>(key).is_some(),
                    b"Rotate" => node.get::<i32>(key).is_some(),
                    _ => node.get::<Rect>(key).is_some(),
                },
            });
        if !linked || !typed {
            return None;
        }
        match node.get::<Name<'_>>(b"Type").as_deref() {
            Some(b"Pages") => {
                let understood = node
                    .keys()
                    .all(|key| matches!(key.as_ref(), b"Type" | b"Kids" | b"Count" | b"Parent"));
                if !understood {
                    return None;
                }
                let kids = node.get::<Array<'_>>(b"Kids")?;
                for kid in kids.raw_iter() {
                    let MaybeRef::Ref(kid) = kid else {
                        return None;
                    };
                    stack.push((ObjectIdentifier::from(kid), Some(id), depth + 1));
                }
                nodes.insert(id);
            }
            Some(b"Page") if !node.contains_key(b"Annots") => pages += 1,
            _ => return None,
        }
    }
    (pages == pdf.pages().len()).then_some(nodes)
}

/// A page box a page may set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxKind {
    Media = 0,
    Crop = 1,
    Trim = 2,
    Bleed = 3,
    Art = 4,
}

impl BoxKind {
    pub const ALL: [BoxKind; 5] = [
        BoxKind::Media,
        BoxKind::Crop,
        BoxKind::Trim,
        BoxKind::Bleed,
        BoxKind::Art,
    ];

    fn key(self) -> &'static [u8] {
        match self {
            BoxKind::Media => b"MediaBox",
            BoxKind::Crop => b"CropBox",
            BoxKind::Trim => b"TrimBox",
            BoxKind::Bleed => b"BleedBox",
            BoxKind::Art => b"ArtBox",
        }
    }
}

/// One page of the new document: page `page` of source `source` (indexes
/// from 0), with `boxes` set on it (lower-left x and y, upper-right x and
/// y), and `overlay`'s page of its source, if any, drawn over it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Planned {
    pub source: u32,
    pub page: u32,
    pub overlay: Option<(u32, u32)>,
    pub boxes: Vec<(BoxKind, [f64; 4])>,
}

impl From<(u32, u32)> for Planned {
    fn from((source, page): (u32, u32)) -> Self {
        Planned {
            source,
            page,
            ..Planned::default()
        }
    }
}

/// The request [`assemble`] reads: the sources, each a `u64` length and
/// its bytes, after their `u32` count; then the plan, a `u32` count of
/// pages, each its source and page (`u32`s, indexes from 0), its overlay's
/// source and page (`u32`s, the source all ones for none), and a `u8` count
/// of boxes, each a `u8` kind and four `f64`s; then the form fields
/// ([`pdf_form::encode`]). Big-endian.
pub fn bundle(
    sources: &[&[u8]],
    plan: &[Planned],
    fields: &[Field],
    actions: &[String],
    links: &[Link],
    highlights: &[Highlight],
    max: u64,
) -> Option<Vec<u8>> {
    // The exact size, counted before the sources are copied: no request
    // past `max` is built.
    let mut form = Vec::new();
    pdf_form::encode(fields, actions, links, highlights, &mut form);
    let size = (sources.iter().map(|s| s.len() as u64 + 8))
        .chain(
            plan.iter()
                .map(|page| 17 + 33 * page.boxes.len().min(BoxKind::ALL.len()) as u64),
        )
        .fold(8 + form.len() as u64, u64::saturating_add);
    if size > max {
        return None;
    }
    let mut out = Vec::with_capacity(usize::try_from(size).ok()?);
    out.extend((sources.len() as u32).to_be_bytes());
    for source in sources {
        out.extend((source.len() as u64).to_be_bytes());
        out.extend(*source);
    }
    out.extend((plan.len() as u32).to_be_bytes());
    for page in plan {
        out.extend(page.source.to_be_bytes());
        out.extend(page.page.to_be_bytes());
        let (overlay, overlay_page) = page.overlay.unwrap_or((u32::MAX, 0));
        out.extend(overlay.to_be_bytes());
        out.extend(overlay_page.to_be_bytes());
        out.push(page.boxes.len().min(BoxKind::ALL.len()) as u8);
        for (kind, values) in page.boxes.iter().take(BoxKind::ALL.len()) {
            out.push(*kind as u8);
            for value in values {
                out.extend(value.to_be_bytes());
            }
        }
    }
    out.extend(form);
    Some(out)
}

/// A part of the request's buffer, read as one source without a copy.
struct Part {
    all: Arc<Vec<u8>>,
    range: Range<usize>,
}

impl AsRef<[u8]> for Part {
    fn as_ref(&self) -> &[u8] {
        &self.all[self.range.clone()]
    }
}

/// A [`bundle`]'s sources, plan, fields, actions and links.
type Request = (
    Vec<Part>,
    Vec<Planned>,
    Vec<Field>,
    Vec<String>,
    Vec<Link>,
    Vec<Highlight>,
);

/// The sources and the plan of a [`bundle`]; `None` for one that is
/// malformed or past the limits.
fn unbundle(data: Vec<u8>) -> Option<Request> {
    let all = Arc::new(data);
    let mut at = 0usize;
    let mut take = |n: usize| -> Option<Range<usize>> {
        let end = at.checked_add(n).filter(|end| *end <= all.len())?;
        let range = at..end;
        at = end;
        Some(range)
    };
    let u32_at = |range: Range<usize>| Some(u32::from_be_bytes(all[range].try_into().ok()?));
    let count = u32_at(take(4)?)? as usize;
    if count > MAX_SOURCES {
        return None;
    }
    let mut parts = Vec::with_capacity(count);
    for _ in 0..count {
        let len = u64::from_be_bytes(all[take(8)?].try_into().ok()?);
        let range = take(usize::try_from(len).ok()?)?;
        parts.push(Part {
            all: Arc::clone(&all),
            range,
        });
    }
    let pages = u32_at(take(4)?)? as usize;
    if pages > MAX_PAGES {
        return None;
    }
    let mut plan = Vec::with_capacity(pages);
    for _ in 0..pages {
        let source = u32_at(take(4)?)?;
        let page = u32_at(take(4)?)?;
        let overlay = u32_at(take(4)?)?;
        let overlay_page = u32_at(take(4)?)?;
        let count = all[take(1)?][0] as usize;
        if count > BoxKind::ALL.len() {
            return None;
        }
        let mut boxes = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = *BoxKind::ALL.get(all[take(1)?][0] as usize)?;
            let mut values = [0.0; 4];
            for value in &mut values {
                *value = f64::from_be_bytes(all[take(8)?].try_into().ok()?);
            }
            if values.iter().any(|v| !v.is_finite()) {
                return None;
            }
            boxes.push((kind, values));
        }
        plan.push(Planned {
            source,
            page,
            overlay: (overlay != u32::MAX).then_some((overlay, overlay_page)),
            boxes,
        });
    }
    let (fields, actions, links, highlights) = pdf_form::decode(&all[at..])?;
    Some((parts, plan, fields, actions, links, highlights))
}

/// The new PDF a [`bundle`] asks for; `None` when a source does not read,
/// the plan names a page no source has, or the copy passes the limits.
/// With `whole`, also `None` rather than write `null` for a reference to
/// an object that exists (the catalog, a page-tree node or a page not
/// copied): appending to a PDF keeps all of it or nothing.
pub(super) fn assemble(data: Vec<u8>, whole: bool) -> Option<Vec<u8>> {
    let (parts, plan, fields, actions, links, highlights) = unbundle(data)?;
    let pdfs = parts
        .into_iter()
        .map(|part| Pdf::new(Arc::new(part)).ok())
        .collect::<Option<Vec<_>>>()?;
    // A whole copy is of documents whose every part it keeps: checked here
    // as `info` checks it, and again, object by object, once the pages are
    // copied (`Copier::accounted`).
    if whole && !pdfs.iter().all(document_kept) {
        return None;
    }
    for planned in &plan {
        let pdf = pdfs.get(planned.source as usize)?;
        let page = pdf.pages().get(planned.page as usize)?;
        if !optional_content_kept(pdf) {
            return None;
        }
        if let Some((overlay, overlay_page)) = planned.overlay {
            let overlay = pdfs.get(overlay as usize)?;
            overlay.pages().get(overlay_page as usize)?;
            if !optional_content_kept(overlay) || !matches!(page.rotation(), Rotation::None) {
                return None;
            }
        }
    }
    // A field is on a planned page that is not turned; a button's caption
    // is on a page of the sources.
    for field in &fields {
        let planned = plan.get(field.page as usize)?;
        let page = &pdfs[planned.source as usize].pages()[planned.page as usize];
        if !matches!(page.rotation(), Rotation::None) {
            return None;
        }
        if let pdf_form::FieldKind::Button {
            caption_page: Some((source, page)),
            ..
        } = &field.kind
        {
            pdfs.get(*source as usize)?.pages().get(*page as usize)?;
        }
    }
    // A link is on a planned page that is not turned.
    for page in links
        .iter()
        .map(|l| l.page)
        .chain(highlights.iter().map(|h| h.page))
    {
        let planned = plan.get(page as usize)?;
        let page = &pdfs[planned.source as usize].pages()[planned.page as usize];
        if !matches!(page.rotation(), Rotation::None) {
            return None;
        }
    }
    Copier::new(&pdfs, &plan, whole)?.finish(&plan, &fields, &actions, &links, &highlights)
}

/// A source object: its document and its identifier there.
type Key = (usize, ObjectIdentifier);

struct Copier<'p> {
    pdfs: &'p [Pdf],
    /// Copied (or queued) objects by source key; `None` for what is written
    /// as `null`.
    ids: HashMap<Key, Option<u32>>,
    /// The first new page each copied source page became.
    pages: HashMap<Key, u32>,
    queue: VecDeque<(Key, u32)>,
    next: u32,
    out: Vec<u8>,
    /// Each object's offset, by number (index 0 unused).
    offsets: Vec<usize>,
    /// Page-tree nodes read for what their pages inherit, by source key,
    /// so that a node is read once however many pages it has (#1128):
    /// reading it again for each page would cost its long `/Kids` array
    /// each time. Only nodes that read are kept ([`Copier::parent`]).
    nodes: HashMap<Key, Dict<'p>>,
    /// A reference to an object that exists but is not copied fails the
    /// copy instead of becoming `null` ([`assemble`]'s `whole`).
    whole: bool,
}

/// The new document's catalog and page tree; the pages follow.
const CATALOG: u32 = 1;
const PAGE_TREE: u32 = 2;
const FIRST_PAGE: u32 = 3;

impl<'p> Copier<'p> {
    fn new(pdfs: &'p [Pdf], plan: &[Planned], whole: bool) -> Option<Self> {
        let first_free = FIRST_PAGE.checked_add(u32::try_from(plan.len()).ok()?)?;
        let mut pages = HashMap::new();
        for (index, planned) in plan.iter().enumerate() {
            let page = &pdfs[planned.source as usize].pages()[planned.page as usize];
            if let Some(id) = page.raw().obj_id() {
                pages
                    .entry((planned.source as usize, id))
                    .or_insert(FIRST_PAGE + index as u32);
            }
        }
        let mut out = Vec::new();
        // A comment of bytes above 127 marks the file as binary.
        out.extend(b"%PDF-1.7\n%");
        out.extend(MARKER);
        out.push(b'\n');
        Some(Copier {
            pdfs,
            ids: HashMap::new(),
            pages,
            queue: VecDeque::new(),
            next: first_free,
            out,
            offsets: vec![0; first_free as usize],
            nodes: HashMap::new(),
            whole,
        })
    }

    fn finish(
        mut self,
        plan: &[Planned],
        fields: &[Field],
        actions: &[String],
        links: &[Link],
        highlights: &[Highlight],
    ) -> Option<Vec<u8>> {
        let pdfs = self.pdfs;
        // Each field's widget object, numbered now so that its page can
        // list it; written after the pages.
        let widgets = fields
            .iter()
            .map(|_| self.fresh())
            .collect::<Option<Vec<u32>>>()?;
        let mut on_page: HashMap<u32, Vec<u32>> = HashMap::new();
        for (field, number) in fields.iter().zip(&widgets) {
            on_page.entry(field.page).or_default().push(*number);
        }
        // So is each link's, after its page's widgets.
        let link_objects = links
            .iter()
            .map(|_| self.fresh())
            .collect::<Option<Vec<u32>>>()?;
        for (link, number) in links.iter().zip(&link_objects) {
            on_page.entry(link.page).or_default().push(*number);
        }
        let highlight_objects = highlights
            .iter()
            .map(|h| {
                Some((
                    if h.flattened {
                        None
                    } else {
                        Some(self.fresh()?)
                    },
                    self.fresh()?,
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        let mut baked: HashMap<u32, Vec<(u32, [f64; 2])>> = HashMap::new();
        for (h, (number, appearance)) in highlights.iter().zip(&highlight_objects) {
            if let Some(number) = number {
                on_page.entry(h.page).or_default().push(*number);
            } else {
                baked
                    .entry(h.page)
                    .or_default()
                    .push((*appearance, [h.rect[0], h.rect[1]]));
            }
        }
        for (index, planned) in plan.iter().enumerate() {
            let source = planned.source as usize;
            let page = pdfs[source].pages()[planned.page as usize].raw();
            let set = |key: &[u8]| planned.boxes.iter().any(|(kind, _)| kind.key() == key);
            // Drawn over: the page's resources and contents are written
            // below, with the overlay's form.
            let forms = baked.remove(&(index as u32)).unwrap_or_default();
            let drawn = planned.overlay.is_some() || !forms.is_empty();
            // Its fields' widgets join its own annotations, written below.
            let annotated = on_page.remove(&(index as u32)).unwrap_or_default();
            let replaced = |key: &[u8]| {
                set(key)
                    || drawn && matches!(key, b"Resources" | b"Contents")
                    || !annotated.is_empty() && key == b"Annots"
            };
            let mut body = String::from("<< /Type /Page /Parent 2 0 R");
            for (key, value) in page.entries() {
                let absent = INHERITED.contains(&&*key) && effective(page, &key).is_none();
                if matches!(&*key, b"Type" | b"Parent") || absent || replaced(&key) {
                    continue;
                }
                body.push(' ');
                name(&key, &mut body);
                body.push(' ');
                self.value(source, &value, &mut body, 0)?;
            }
            for key in INHERITED {
                if effective(page, key).is_some() || replaced(key) {
                    continue;
                }
                if let Some(value) = self.inherited(source, page, key) {
                    body.push(' ');
                    name(key, &mut body);
                    body.push(' ');
                    self.value(source, &value, &mut body, 0)?;
                }
            }
            for (kind, values) in &planned.boxes {
                body.push(' ');
                name(kind.key(), &mut body);
                body.push_str(" [");
                for (i, value) in values.iter().enumerate() {
                    if i > 0 {
                        body.push(' ');
                    }
                    real(*value, &mut body);
                }
                body.push(']');
            }
            if drawn {
                let over = planned.overlay.map(|(s, p)| (s as usize, p as usize));
                self.draw_over(source, page, over, &forms, &mut body)?;
            }
            if !annotated.is_empty() {
                body.push_str(" /Annots [");
                // The page's own annotations first, from an array direct or
                // in an object of its own; anything else there is refused
                // rather than dropped.
                match (page.get::<Array<'_>>(b"Annots"), effective(page, b"Annots")) {
                    (Some(items), _) => {
                        for item in items.raw_iter() {
                            self.value(source, &item, &mut body, 0)?;
                            body.push(' ');
                        }
                    }
                    (None, Some(_)) => return None,
                    (None, None) => {}
                }
                for number in &annotated {
                    let _ = write!(body, "{number} 0 R ");
                }
                body.push(']');
            }
            body.push_str(" >>");
            self.object(FIRST_PAGE + index as u32, body.as_bytes(), None);
        }
        let form = self.write_fields(fields, actions, &widgets)?;
        for (link, number) in links.iter().zip(&link_objects) {
            let body = pdf_form::link(link, FIRST_PAGE + link.page);
            self.object(*number, body.as_bytes(), None);
        }
        for (h, (number, appearance)) in highlights.iter().zip(&highlight_objects) {
            let (annot, head, content) = pdf_form::highlight(h, FIRST_PAGE + h.page, *appearance);
            if let Some(number) = number {
                self.object(*number, annot.as_bytes(), None);
            }
            self.object(*appearance, head.as_bytes(), Some(&content));
        }
        while let Some(((source, id), number)) = self.queue.pop_front() {
            let object = pdfs[source].xref().get::<Object<'_>>(id);
            let mut body = String::new();
            match &object {
                Some(Object::Stream(stream)) => {
                    let data = stream.raw_data();
                    body.push_str("<<");
                    for (key, value) in stream.dict().entries() {
                        if &*key == b"Length" {
                            continue;
                        }
                        body.push(' ');
                        name(&key, &mut body);
                        body.push(' ');
                        self.value(source, &value, &mut body, 0)?;
                    }
                    let _ = write!(body, " /Length {} >>", data.len());
                    self.object(number, body.as_bytes(), Some(&data));
                }
                Some(object) => {
                    self.direct(source, object, &mut body, 0)?;
                    self.object(number, body.as_bytes(), None);
                }
                None => self.object(number, b"null", None),
            }
        }
        if self.whole && !self.accounted() {
            return None;
        }
        let kids = (0..plan.len())
            .map(|index| format!("{} 0 R", FIRST_PAGE + index as u32))
            .collect::<Vec<_>>()
            .join(" ");
        let tree = format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", plan.len());
        self.object(PAGE_TREE, tree.as_bytes(), None);
        let catalog = format!("<< /Type /Catalog /Pages 2 0 R{form} >>");
        self.object(CATALOG, catalog.as_bytes(), None);
        let xref = self.out.len();
        let size = self.offsets.len();
        let _ = write!(
            Writer(&mut self.out),
            "xref\n0 {size}\n0000000000 65535 f \n"
        );
        for offset in &self.offsets[1..] {
            let _ = writeln!(Writer(&mut self.out), "{offset:010} 00000 n ");
        }
        let _ = write!(
            Writer(&mut self.out),
            "trailer\n<< /Size {size} /Root {CATALOG} 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        );
        Some(self.out)
    }

    /// Whether every object each source's cross-reference table lists in
    /// use is kept: copied, a copied page, a node of its page tree (whose
    /// every entry the new tree and pages carry, [`page_tree`]) or its
    /// catalog (whose every entry the new catalog carries,
    /// [`document_kept`]). What a whole copy would otherwise leave behind,
    /// however the document refers to it, if at all, is found here.
    fn accounted(&self) -> bool {
        self.pdfs.iter().enumerate().all(|(source, pdf)| {
            let (Some(table), Some(nodes)) = (table(pdf.data().as_ref()), page_tree(pdf)) else {
                return false;
            };
            let root = pdf.xref().root_id();
            table.in_use.iter().all(|&id| {
                let key = (source, id);
                id == root
                    || nodes.contains(&id)
                    || self.pages.contains_key(&key)
                    || self.ids.get(&key).is_some_and(Option::is_some)
            })
        })
    }

    /// The value `key` page `page` of `source` inherits from the nearest of
    /// its [`MAX_PARENTS`] nearest ancestors that has it.
    fn inherited(
        &mut self,
        source: usize,
        page: &Dict<'p>,
        key: &[u8],
    ) -> Option<MaybeRef<Object<'p>>> {
        let mut node = self.parent(source, page)?;
        for _ in 0..MAX_PARENTS {
            if let Some(value) = effective(&node, key) {
                return Some(value);
            }
            node = self.parent(source, &node)?;
        }
        None
    }

    /// The `/Parent` of `dict` in `source`, read once when it reads. A
    /// reference that does not resolve is looked up again each time: the
    /// failed lookup may have repaired the source's cross-reference table
    /// (hayro rebuilds it once, at the first object it cannot read), after
    /// which the same reference can resolve.
    fn parent(&mut self, source: usize, dict: &Dict<'p>) -> Option<Dict<'p>> {
        match dict.get_raw::<Object<'p>>(b"Parent")? {
            MaybeRef::Ref(reference) => {
                let key = (source, ObjectIdentifier::from(reference));
                if let Some(node) = self.nodes.get(&key) {
                    return Some(node.clone());
                }
                let node = self.pdfs[source].xref().get::<Dict<'p>>(key.1)?;
                self.nodes.insert(key, node.clone());
                Some(node)
            }
            MaybeRef::NotRef(_) => dict.get::<Dict<'p>>(b"Parent"),
        }
    }

    /// The fields' widgets as objects `widgets`, their appearance streams
    /// and the fonts these use; the catalog's `/AcroForm` entry listing
    /// them, empty for no fields.
    fn write_fields(
        &mut self,
        fields: &[Field],
        actions: &[String],
        widgets: &[u32],
    ) -> Option<String> {
        if fields.is_empty() {
            return Some(String::new());
        }
        let fonts = pdf_form::FONTS
            .iter()
            .map(|_| self.fresh())
            .collect::<Option<Vec<u32>>>()?;
        let font = |name: &str| {
            pdf_form::FONTS
                .iter()
                .position(|(n, _)| *n == name)
                .map(|i| fonts[i])
        };
        // Each action a field runs, written once however many run it.
        let mut action_objects: Vec<Option<u32>> = vec![None; actions.len()];
        for field in fields {
            if let Some(action) = field.action {
                let slot = &mut action_objects[action as usize];
                if slot.is_none() {
                    let number = self.fresh()?;
                    let body = pdf_form::javascript(&actions[action as usize]);
                    self.object(number, body.as_bytes(), None);
                    *slot = Some(number);
                }
            }
        }
        // Each field's appearance streams, then its widget: a field of its
        // own, or a kid of its name's group field.
        let groups = pdf_form::groups(fields);
        let mut listed = Vec::with_capacity(groups.len());
        for group in &groups {
            let members: Vec<&Field> = group.iter().map(|i| &fields[*i]).collect();
            let parent = if members.len() > 1 {
                Some(self.fresh()?)
            } else {
                None
            };
            let value = pdf_form::value_of(&members);
            for i in group {
                let field = &fields[*i];
                let (w, h) = (field.rect[2] - field.rect[0], field.rect[3] - field.rect[1]);
                let caption = match &field.kind {
                    pdf_form::FieldKind::Button {
                        caption_page: Some((source, page)),
                        ..
                    } => Some(self.page_form(*source as usize, *page as usize)?),
                    _ => None,
                };
                let mut streams = Vec::new();
                for appearance in pdf_form::appearances(field, caption) {
                    let number = self.fresh()?;
                    let mut head = String::from("<< /Type /XObject /Subtype /Form /BBox [0 0 ");
                    real(w, &mut head);
                    head.push(' ');
                    real(h, &mut head);
                    head.push(']');
                    if appearance.dingbats {
                        let _ = write!(
                            head,
                            " /Resources << /Font << /ZaDb {} 0 R >> >>",
                            font("ZaDb")?
                        );
                    } else if let Some(form) = appearance.caption {
                        let _ = write!(head, " /Resources << /XObject << /Cap {form} 0 R >> >>");
                    }
                    let _ = write!(head, " /Length {} >>", appearance.content.len());
                    self.object(number, head.as_bytes(), Some(appearance.content.as_bytes()));
                    streams.push((appearance.state, number));
                }
                let page = FIRST_PAGE + field.page;
                let action = field.action.and_then(|a| action_objects[a as usize]);
                let body = match parent {
                    Some(parent) => pdf_form::kid(field, parent, page, &streams, value, action),
                    None => pdf_form::widget(field, page, &streams, action),
                };
                self.object(widgets[*i], body.as_bytes(), None);
            }
            match parent {
                Some(parent) => {
                    let kids: Vec<u32> = group.iter().map(|i| widgets[*i]).collect();
                    let body = pdf_form::group(&members, &kids);
                    self.object(parent, body.as_bytes(), None);
                    listed.push(parent);
                }
                None => listed.push(widgets[group[0]]),
            }
        }
        let mut resources = String::new();
        for ((name, dict), number) in pdf_form::FONTS.iter().zip(&fonts) {
            self.object(*number, format!("<< {dict} >>").as_bytes(), None);
            let _ = write!(resources, " /{name} {number} 0 R");
        }
        let list = listed
            .iter()
            .map(|number| format!("{number} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        Some(format!(
            " /AcroForm << /Fields [{list}] /DR << /Font <<{resources} >> >> /DA (/Helv 0 Tf 0 g) >>"
        ))
    }

    /// Page `page` of source `source`, a page the desktop drew, as a form
    /// XObject: its one content stream as encoded, its resources, its
    /// media box as the form's box. The form's number and box.
    fn page_form(&mut self, source: usize, page: usize) -> Option<(u32, [f64; 4])> {
        let pdfs = self.pdfs;
        let over = &pdfs[source].pages()[page];
        let contents = match over.raw().get::<Object<'_>>(b"Contents") {
            Some(Object::Stream(stream)) => stream,
            _ => return None,
        };
        let data = contents.raw_data();
        let form = self.fresh()?;
        let mut form_body = String::from("<< /Type /XObject /Subtype /Form /BBox [");
        let media = over.media_box();
        let bbox = [media.x0, media.y0, media.x1, media.y1];
        for (i, value) in bbox.iter().enumerate() {
            if i > 0 {
                form_body.push(' ');
            }
            real(*value, &mut form_body);
        }
        form_body.push(']');
        for key in [&b"Filter"[..], b"DecodeParms"] {
            if let Some(value) = contents.dict().get_raw::<Object<'_>>(key) {
                form_body.push(' ');
                name(key, &mut form_body);
                form_body.push(' ');
                self.value(source, &value, &mut form_body, 0)?;
            }
        }
        if let Some(value) = effective(over.raw(), b"Resources") {
            form_body.push_str(" /Resources ");
            self.value(source, &value, &mut form_body, 0)?;
        }
        let _ = write!(form_body, " /Length {} >>", data.len());
        self.object(form, form_body.as_bytes(), Some(&data));
        Some((form, bbox))
    }

    /// A number for a new object.
    fn fresh(&mut self) -> Option<u32> {
        let number = self.next;
        if number >= MAX_OBJECTS {
            return None;
        }
        self.next += 1;
        self.offsets.push(0);
        Some(number)
    }

    /// The `/Resources` and `/Contents` of page `page` of `source` with
    /// Baked highlight appearances, then `overlay`'s page as form XObjects.
    /// The runtime composes pending text before marking new highlights as
    /// baked, so this overlay contains only text drawn after those highlights.
    fn draw_over(
        &mut self,
        source: usize,
        page: &Dict<'p>,
        overlay: Option<(usize, usize)>,
        baked: &[(u32, [f64; 2])],
        body: &mut String,
    ) -> Option<()> {
        let pdfs = self.pdfs;
        let mut forms = baked.to_vec();
        if let Some((source, page)) = overlay {
            forms.push((self.page_form(source, page)?.0, [0.0, 0.0]));
        }
        // The page's own resources, inline, with the form among its
        // XObjects under a name none of them has.
        let resources =
            effective(page, b"Resources").or_else(|| self.inherited(source, page, b"Resources"));
        let resolved = match resources {
            Some(MaybeRef::Ref(r)) => pdfs[source]
                .xref()
                .get::<Dict<'_>>(ObjectIdentifier::from(r)),
            Some(MaybeRef::NotRef(Object::Dict(dict))) => Some(dict),
            _ => None,
        };
        let xobjects = resolved
            .as_ref()
            .and_then(|dict| dict.get::<Dict<'_>>(b"XObject"));
        let mut suffix = 0;
        let mut named = Vec::with_capacity(forms.len());
        for (form, translation) in forms {
            let form_name = loop {
                let candidate = format!("OMDynaPDF{suffix}");
                suffix += 1;
                if !xobjects
                    .as_ref()
                    .is_some_and(|x| x.contains_key(candidate.as_bytes()))
                {
                    break candidate;
                }
            };
            named.push((form_name, form, translation));
        }
        body.push_str(" /Resources <<");
        if let Some(dict) = &resolved {
            for (key, value) in dict.entries() {
                if &*key == b"XObject" {
                    continue;
                }
                body.push(' ');
                name(&key, body);
                body.push(' ');
                self.value(source, &value, body, 0)?;
            }
        }
        body.push_str(" /XObject <<");
        if let Some(x) = &xobjects {
            for (key, value) in x.entries() {
                body.push(' ');
                name(&key, body);
                body.push(' ');
                self.value(source, &value, body, 0)?;
            }
        }
        for (form_name, form, _) in &named {
            body.push(' ');
            name(form_name.as_bytes(), body);
            let _ = write!(body, " {form} 0 R");
        }
        body.push_str(" >> >>");
        // The page's content in q ... Q, then the form.
        let open = self.fresh()?;
        self.object(open, b"<< /Length 2 >>", Some(b"q\n"));
        let mut draw = String::from("\nQ\n");
        for (form_name, _, [x, y]) in named {
            draw.push_str("q 1 0 0 1 ");
            real(x, &mut draw);
            draw.push(' ');
            real(y, &mut draw);
            let _ = writeln!(draw, " cm /{form_name} Do Q");
        }
        let close = self.fresh()?;
        let head = format!("<< /Length {} >>", draw.len());
        self.object(close, head.as_bytes(), Some(draw.as_bytes()));
        let _ = write!(body, " /Contents [{open} 0 R");
        // An array of streams, direct or not, is flattened into this one:
        // its items must be streams, not an array.
        match (
            page.get::<Array<'_>>(b"Contents"),
            effective(page, b"Contents"),
        ) {
            (Some(items), _) => {
                for item in items.raw_iter() {
                    body.push(' ');
                    self.value(source, &item, body, 0)?;
                }
            }
            (None, Some(value)) => {
                body.push(' ');
                self.value(source, &value, body, 0)?;
            }
            (None, None) => {}
        }
        let _ = write!(body, " {close} 0 R]");
        Some(())
    }

    /// Write object `number`: its body, and a stream's data after it.
    fn object(&mut self, number: u32, body: &[u8], stream: Option<&[u8]>) {
        self.offsets[number as usize] = self.out.len();
        let _ = writeln!(Writer(&mut self.out), "{number} 0 obj");
        self.out.extend(body);
        if let Some(data) = stream {
            self.out.extend(b"\nstream\n");
            self.out.extend(data);
            self.out.extend(b"\nendstream");
        }
        self.out.extend(b"\nendobj\n");
    }

    /// A value as written: a reference by its new number (or `null`), a
    /// direct object as it is.
    fn value(
        &mut self,
        source: usize,
        value: &MaybeRef<Object<'_>>,
        out: &mut String,
        depth: usize,
    ) -> Option<()> {
        match value {
            MaybeRef::Ref(reference) => {
                match self.reference(source, ObjectIdentifier::from(*reference))? {
                    Some(number) => {
                        let _ = write!(out, "{number} 0 R");
                    }
                    None => out.push_str("null"),
                }
                Some(())
            }
            MaybeRef::NotRef(object) => self.direct(source, object, out, depth),
        }
    }

    fn direct(
        &mut self,
        source: usize,
        object: &Object<'_>,
        out: &mut String,
        depth: usize,
    ) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        match object {
            Object::Null(_) => out.push_str("null"),
            Object::Boolean(value) => out.push_str(if *value { "true" } else { "false" }),
            Object::Number(value) => number(value, out),
            Object::String(value) => {
                out.push('<');
                for byte in value.as_bytes() {
                    let _ = write!(out, "{byte:02X}");
                }
                out.push('>');
            }
            Object::Name(value) => name(value, out),
            Object::Dict(dict) => {
                out.push_str("<<");
                for (key, value) in dict.entries() {
                    out.push(' ');
                    name(&key, out);
                    out.push(' ');
                    self.value(source, &value, out, depth + 1)?;
                }
                out.push_str(" >>");
            }
            Object::Array(array) => {
                out.push('[');
                for (index, item) in array.raw_iter().enumerate() {
                    if index > 0 {
                        out.push(' ');
                    }
                    self.value(source, &item, out, depth + 1)?;
                }
                out.push(']');
            }
            // A stream is an indirect object; one met as a value is not a
            // PDF this copies.
            Object::Stream(_) => return None,
        }
        Some(())
    }

    /// The new number of source object `id`, queued to be copied the first
    /// time; `None` inside for what becomes `null`.
    fn reference(&mut self, source: usize, id: ObjectIdentifier) -> Option<Option<u32>> {
        let key = (source, id);
        if let Some(page) = self.pages.get(&key) {
            return Some(Some(*page));
        }
        if let Some(known) = self.ids.get(&key) {
            return Some(*known);
        }
        let pdf = &self.pdfs[source];
        let object = pdf.xref().get::<Object<'_>>(id);
        let skipped = id == pdf.xref().root_id()
            || match &object {
                Some(Object::Dict(dict)) => page_tree_node(dict),
                Some(Object::Stream(stream)) => page_tree_node(stream.dict()),
                Some(_) => false,
                None => true,
            };
        // A reference to an object that does not exist is `null` as read
        // (PDF 1.7, 7.3.10); one to an object left behind is a loss.
        if skipped && self.whole && object.is_some() {
            return None;
        }
        let number = if skipped {
            None
        } else {
            let number = self.fresh()?;
            self.queue.push_back((key, number));
            Some(number)
        };
        self.ids.insert(key, number);
        Some(number)
    }
}

/// A page, a page tree node or a catalog: never copied by reference.
fn page_tree_node(dict: &Dict<'_>) -> bool {
    dict.get::<Name<'_>>(b"Type")
        .is_some_and(|kind| matches!(&*kind, b"Page" | b"Pages" | b"Catalog"))
}

/// `key`'s value in `dict` as stored, unless it is absent: missing,
/// `null`, or a reference to an object that does not exist.
fn effective<'a>(dict: &Dict<'a>, key: &[u8]) -> Option<MaybeRef<Object<'a>>> {
    let raw = dict.get_raw::<Object<'a>>(key)?;
    match dict.get::<Object<'a>>(key) {
        None | Some(Object::Null(_)) => None,
        Some(_) => Some(raw),
    }
}

/// A number as read: an integer as one, a real in its shortest exact form.
fn number(value: &Number, out: &mut String) {
    real(value.as_f64(), out);
}

/// `value` as a PDF number: an integer as one, a real in its shortest exact
/// form, anything not finite as 0.
pub(super) fn real(value: f64, out: &mut String) {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        let _ = write!(out, "{}", value as i64);
    } else if value.is_finite() {
        let _ = write!(out, "{value}");
    } else {
        out.push('0');
    }
}

/// A name, its bytes outside the regular characters written `#xx`.
pub(super) fn name(bytes: &[u8], out: &mut String) {
    out.push('/');
    for &byte in bytes {
        let regular = (b'!'..=b'~').contains(&byte) && !b"()<>[]{}/%#".contains(&byte);
        if regular {
            out.push(byte as char);
        } else {
            let _ = write!(out, "#{byte:02X}");
        }
    }
}

/// `write!` into a byte buffer.
struct Writer<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for Writer<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend(s.as_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::trailer_kept;

    /// A trailer is read from the cross-reference table `startxref` names,
    /// entry by entry, and kept only with `/Size` and `/Root`: a comment
    /// spelling another trailer stands in for nothing, any other key
    /// (escaped or not) refuses, values of any kind are skipped, and a
    /// footer or table that does not read refuses without a panic.
    #[test]
    fn trailers_are_read_from_their_table() {
        let file = |trailer: &str| {
            format!(
                "%PDF-1.7\nxref\n0 2\n0000000000 65535 f \n0000000009 00000 n \n\
                 {trailer}\nstartxref\n9\n%%EOF\n"
            )
            .into_bytes()
        };
        assert!(trailer_kept(&file("trailer\n<< /Size 2 /Root 1 0 R >>")));
        assert!(trailer_kept(&file(
            "trailer % a comment\n<< /Size 2 /Root << /A (x\\) y) /B [1 [2] <00>] >> >>"
        )));
        for trailer in [
            "% trailer << /Size 2 /Root 1 0 R >>\ntrailer\n<< /Size 2 /Root 1 0 R /ID [<01> <02>] >>",
            "trailer\n<< /Size 2 /Root 1 0 R /Encr#79pt 3 0 R >>",
            "trailer\n<< /Size 2 /Root 1 0 R /Info 4 0 R >>",
            "trailer\n<< /Size 2 >>",
            "trailer\n<< /Size 2 /Root 1 0 R",
        ] {
            assert!(!trailer_kept(&file(trailer)), "{trailer}");
        }
        let deep = format!("trailer\n<< /Size {} >>", "[".repeat(10_000));
        assert!(!trailer_kept(&file(&deep)));
        for footer in [
            &b"startxref 0"[..],
            b"xref % startxref\nstartxref\n0",
            b"%PDF\nxref\n0 1\nstartxref\n5",
            b"startxref\n99999999999999999999999",
            b"",
        ] {
            assert!(!trailer_kept(footer));
        }
    }
}
