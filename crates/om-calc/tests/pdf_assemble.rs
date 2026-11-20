//! A new PDF of pages of others, made by the PDF worker executable
//! (`pdf_worker::assemble`, the MBS plug-in subset's DynaPDF documents):
//! pages are copied with what they refer to, in the plan's order, shared
//! objects once, streams as encoded, inherited boxes on the page, and a
//! copied page's annotations pointing at the new page. The sources are made
//! here; no other document is read.

use hayro::hayro_interpret::hayro_syntax::object::{Array, Dict, MaybeRef, Name, Object, Rect};
use hayro::hayro_interpret::hayro_syntax::Pdf as Parsed;
use om_calc::functions::pdf_worker;
use pdf_writer::{Finish, Name as N, Pdf, Rect as R, Ref};

fn worker() {
    pdf_worker::set_default(env!("CARGO_BIN_EXE_om-pdf-worker").into());
}

/// `text` shown in Helvetica at the bottom left.
fn content(text: &str) -> Vec<u8> {
    format!("BT /F1 24 Tf 72 72 Td ({text}) Tj ET").into_bytes()
}

/// Source A: three pages of different sizes and trim boxes sharing one
/// font object, the first page's content ASCII-hex encoded.
fn source_a() -> (Vec<u8>, Vec<u8>) {
    let sizes = [(612.0, 792.0), (595.0, 842.0), (500.0, 400.0)];
    let mut pdf = Pdf::new();
    pdf.catalog(Ref::new(1)).pages(Ref::new(2));
    pdf.pages(Ref::new(2)).kids((4..7).map(Ref::new)).count(3);
    pdf.indirect(Ref::new(3))
        .dict()
        .pair(N(b"Type"), N(b"Font"))
        .pair(N(b"Subtype"), N(b"Type1"))
        .pair(N(b"BaseFont"), N(b"Helvetica"));
    let hex: Vec<u8> = content("A1")
        .iter()
        .flat_map(|b| format!("{b:02X}").into_bytes())
        .chain(*b">")
        .collect();
    for (i, (w, h)) in sizes.iter().enumerate() {
        let page = Ref::new(4 + i as i32);
        let stream = Ref::new(7 + i as i32);
        let mut dict = pdf.indirect(page).dict();
        dict.pair(N(b"Type"), N(b"Page"));
        dict.pair(N(b"Parent"), Ref::new(2));
        dict.pair(N(b"MediaBox"), R::new(0.0, 0.0, *w, *h));
        dict.pair(N(b"TrimBox"), R::new(9.0, 9.0, w - 9.0, h - 9.0));
        dict.pair(N(b"Contents"), stream);
        dict.insert(N(b"Resources"))
            .dict()
            .insert(N(b"Font"))
            .dict()
            .pair(N(b"F1"), Ref::new(3));
        dict.finish();
        if i == 0 {
            pdf.stream(stream, &hex)
                .pair(N(b"Filter"), N(b"ASCIIHexDecode"));
        } else {
            pdf.stream(stream, &content(&format!("A{}", i + 1)));
        }
    }
    (pdf.finish(), hex)
}

/// Source B: one page whose box and resources it inherits from its page
/// tree (its own `/Resources` is `null`, which is none), with a link to
/// itself.
fn source_b() -> Vec<u8> {
    let mut pdf = Pdf::new();
    pdf.catalog(Ref::new(1)).pages(Ref::new(2));
    let mut tree = pdf.indirect(Ref::new(2)).dict();
    tree.pair(N(b"Type"), N(b"Pages"));
    tree.insert(N(b"Kids")).array().item(Ref::new(4));
    tree.pair(N(b"Count"), 1);
    tree.pair(N(b"MediaBox"), R::new(0.0, 0.0, 300.0, 300.0));
    tree.insert(N(b"Resources"))
        .dict()
        .insert(N(b"Font"))
        .dict()
        .pair(N(b"F1"), Ref::new(3));
    tree.finish();
    pdf.indirect(Ref::new(3))
        .dict()
        .pair(N(b"Type"), N(b"Font"))
        .pair(N(b"Subtype"), N(b"Type1"))
        .pair(N(b"BaseFont"), N(b"Courier"));
    let mut page = pdf.indirect(Ref::new(4)).dict();
    page.pair(N(b"Type"), N(b"Page"));
    page.pair(N(b"Parent"), Ref::new(2));
    page.pair(N(b"Contents"), Ref::new(5));
    page.pair(N(b"Resources"), pdf_writer::Null);
    let mut annots = page.insert(N(b"Annots")).array();
    let mut link = annots.push().dict();
    link.pair(N(b"Type"), N(b"Annot"));
    link.pair(N(b"Subtype"), N(b"Link"));
    link.pair(N(b"Rect"), R::new(10.0, 10.0, 50.0, 50.0));
    link.pair(N(b"P"), Ref::new(4));
    link.insert(N(b"Dest"))
        .array()
        .item(Ref::new(4))
        .item(N(b"Fit"));
    link.finish();
    annots.finish();
    page.finish();
    pdf.stream(Ref::new(5), &content("B1"));
    pdf.finish()
}

fn rect(r: Rect) -> [f64; 4] {
    [r.x0, r.y0, r.x1, r.y1]
}

/// The object a page entry names by reference: its number.
fn reference(dict: &Dict<'_>, key: &[u8]) -> i32 {
    match dict.get_raw::<Object<'_>>(key) {
        Some(MaybeRef::Ref(r)) => r.obj_number,
        other => panic!(
            "{} is not a reference: {other:?}",
            String::from_utf8_lossy(key)
        ),
    }
}

#[test]
fn selected_and_whole_imports_keep_pages_resources_and_streams() {
    worker();
    let (a, hex) = source_a();
    // Search stays on its requested page and reads encoded streams.
    assert_eq!(pdf_worker::find(&a, 0, "A1"), Ok(Some(1)));
    assert_eq!(pdf_worker::find(&a, 1, "A1"), Ok(Some(0)));
    assert_eq!(pdf_worker::find(&a, 0, "a1"), Ok(Some(0)));
    assert_eq!(pdf_worker::find(&a, 3, "A1"), Ok(None));
    assert_eq!(pdf_worker::find(&a, 0, ""), Ok(None));
    let b = source_b();
    // Not one OpenMaker wrote (no `MARKER`): its pages are imported, but it
    // is not kept whole for an append.
    let info = pdf_worker::info_whole(&a).unwrap().unwrap();
    assert_eq!(
        (info.pages, info.optional_content_kept, info.document_kept),
        (3, true, Some(false))
    );
    // Not asked, the reply is the one a host that does not ask reads.
    assert_eq!(pdf_worker::info(&a).unwrap().unwrap().document_kept, None);
    // Page 3 and page 1 of A, then all of B.
    let out = pdf_worker::assemble(&[&a, &b], &[(0, 2), (0, 0), (1, 0)])
        .unwrap()
        .expect("a document");
    assert_eq!(
        pdf_worker::text(&out).unwrap().as_deref(),
        Some("A3\rA1\rB1")
    );
    // What the assembler writes carries OpenMaker's mark and, without
    // annotations, keeps all it holds when copied again (Save Records as
    // PDF appends to it); with B's link it does not.
    assert!(out.starts_with(&[b"%PDF-1.7\n%".as_slice(), pdf_worker::MARKER].concat()));
    let pages_of_a = pdf_worker::assemble(&[&a], &[(0, 0), (0, 1)])
        .unwrap()
        .expect("a document");
    assert_eq!(
        pdf_worker::info_whole(&pages_of_a)
            .unwrap()
            .unwrap()
            .document_kept,
        Some(true)
    );
    assert_eq!(
        pdf_worker::info_whole(&out).unwrap().unwrap().document_kept,
        Some(false)
    );
    let parsed = Parsed::new(out.clone()).unwrap();
    let pages = parsed.pages();
    assert_eq!(pages.len(), 3);
    assert_eq!(rect(pages[0].media_box()), [0.0, 0.0, 500.0, 400.0]);
    assert_eq!(rect(pages[1].media_box()), [0.0, 0.0, 612.0, 792.0]);
    // B's box is inherited from its page tree, and written on the page.
    assert!(pages[2].raw().get::<Rect>(b"MediaBox").is_some());
    assert_eq!(rect(pages[2].media_box()), [0.0, 0.0, 300.0, 300.0]);
    let trim = |i: usize| rect(pages[i].raw().get::<Rect>(b"TrimBox").unwrap());
    assert_eq!(trim(0), [9.0, 9.0, 491.0, 391.0]);
    assert_eq!(trim(1), [9.0, 9.0, 603.0, 783.0]);
    // A's font is one object for both its pages; B has its own.
    let fonts = parsed
        .objects()
        .into_iter()
        .filter(|o| match o {
            Object::Dict(d) => d.get::<Name<'_>>(b"Type").is_some_and(|t| &*t == b"Font"),
            _ => false,
        })
        .count();
    assert_eq!(fonts, 2);
    let font = |i: usize| {
        let resources = pages[i].raw().get::<Dict<'_>>(b"Resources").unwrap();
        reference(&resources.get::<Dict<'_>>(b"Font").unwrap(), b"F1")
    };
    assert_eq!(font(0), font(1));
    // B's page inherits its tree's font through its `null` resources.
    assert_ne!(font(2), font(0));
    // A's first page keeps its encoded stream and filter.
    let contents = match pages[1].raw().get::<Object<'_>>(b"Contents") {
        Some(Object::Stream(stream)) => stream,
        other => panic!("contents: {other:?}"),
    };
    assert_eq!(&*contents.raw_data(), &hex[..]);
    let filter: Name<'_> = contents.dict().get(b"Filter").unwrap();
    assert_eq!(&*filter, b"ASCIIHexDecode");
    // B's link points at its page in the new document.
    let page_b = pages[2].raw().obj_id().unwrap().obj_number;
    let annots: Array<'_> = pages[2].raw().get(b"Annots").unwrap();
    let link: Dict<'_> = annots.iter::<Dict<'_>>().next().unwrap();
    assert_eq!(reference(&link, b"P"), page_b);
    let dest: Array<'_> = link.get(b"Dest").unwrap();
    match dest.raw_iter().next() {
        Some(MaybeRef::Ref(r)) => assert_eq!(r.obj_number, page_b),
        other => panic!("dest: {other:?}"),
    }
}

/// A one-page PDF with an optional content group, its square drawn only
/// while a membership dictionary's `/P /AnyOff` holds: hidden while the
/// group is on. `off` turns the group off in the default configuration
/// instead. Either way a copy without the configuration would look
/// different.
fn layered(off: bool) -> Vec<u8> {
    let mut pdf = Pdf::new();
    let mut catalog = pdf.indirect(Ref::new(1)).dict();
    catalog.pair(N(b"Type"), N(b"Catalog"));
    catalog.pair(N(b"Pages"), Ref::new(2));
    let mut properties = catalog.insert(N(b"OCProperties")).dict();
    properties.insert(N(b"OCGs")).array().item(Ref::new(4));
    let mut config = properties.insert(N(b"D")).dict();
    if off {
        config.insert(N(b"OFF")).array().item(Ref::new(4));
    }
    config.finish();
    properties.finish();
    catalog.finish();
    pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
    let mut page = pdf.indirect(Ref::new(3)).dict();
    page.pair(N(b"Type"), N(b"Page"));
    page.pair(N(b"Parent"), Ref::new(2));
    page.pair(N(b"MediaBox"), R::new(0.0, 0.0, 200.0, 200.0));
    page.pair(N(b"Contents"), Ref::new(6));
    page.insert(N(b"Resources"))
        .dict()
        .insert(N(b"Properties"))
        .dict()
        .pair(N(b"MC0"), Ref::new(5));
    page.finish();
    pdf.indirect(Ref::new(4))
        .dict()
        .pair(N(b"Type"), N(b"OCG"))
        .pair(N(b"Name"), pdf_writer::Str(b"Layer"));
    let mut membership = pdf.indirect(Ref::new(5)).dict();
    membership.pair(N(b"Type"), N(b"OCMD"));
    membership.pair(N(b"OCGs"), Ref::new(4));
    membership.pair(N(b"P"), N(b"AnyOff"));
    membership.finish();
    pdf.stream(Ref::new(6), b"/OC /MC0 BDC 0 0 200 200 re f EMC");
    pdf.finish()
}

/// The worker gives every page's size and draws any page, as a viewer of
/// the whole document reads it (#200): source A's three pages, each drawn
/// at its own size, the third different from the first, and no fourth.
#[test]
fn every_page_is_sized_and_drawn() {
    worker();
    let (a, _) = source_a();
    assert_eq!(
        pdf_worker::page_sizes(&a).unwrap(),
        Some(vec![(612, 792), (595, 842), (500, 400)])
    );
    let first = pdf_worker::render_page(&a, 0, 61, 79).unwrap().unwrap();
    assert_eq!(first.dimensions(), (61, 79));
    assert_eq!(
        pdf_worker::render(&a, 61, 79).unwrap().as_ref(),
        Some(&first),
        "the first page is the one `render` draws"
    );
    let third = pdf_worker::render_page(&a, 2, 50, 40).unwrap().unwrap();
    assert_eq!(third.dimensions(), (50, 40));
    let first_small = pdf_worker::render_page(&a, 0, 50, 40).unwrap().unwrap();
    assert_ne!(third, first_small, "page 3 shows its own content");
    assert_eq!(pdf_worker::render_page(&a, 3, 50, 40).unwrap(), None);
    assert_eq!(pdf_worker::page_sizes(b"not a pdf").unwrap(), None);
}

#[test]
fn what_does_not_read_or_names_a_missing_page_makes_nothing() {
    worker();
    let (a, _) = source_a();
    assert_eq!(pdf_worker::info(b"not a PDF"), Ok(None));
    // Pages of a PDF with layers are not copied: without its configuration
    // a group it turns off shows, and so does content it hides while its
    // groups are on.
    for off in [true, false] {
        let layered = layered(off);
        let info = pdf_worker::info_whole(&layered).unwrap().unwrap();
        assert_eq!(
            (info.pages, info.optional_content_kept, info.document_kept),
            (1, false, Some(false)),
            "{off}"
        );
        assert_eq!(
            pdf_worker::assemble(&[&layered], &[(0, 0)]),
            Ok(None),
            "{off}"
        );
    }
    assert_eq!(pdf_worker::assemble(&[b"not a PDF"], &[(0, 0)]), Ok(None));
    assert_eq!(pdf_worker::assemble(&[&a], &[(0, 3)]), Ok(None));
    assert_eq!(pdf_worker::assemble(&[&a], &[(1, 0)]), Ok(None));
}

/// What a new document of a PDF's pages would leave behind makes it not
/// kept whole (`Info::document_kept`, which appending asks for): document
/// information of any kind, file identifiers and any trailer entry past
/// `/Size` and `/Root`, however its name is written. Text that only spells
/// such a name is not one.
#[test]
fn what_a_page_copy_leaves_behind_is_not_kept_whole() {
    worker();
    let whole = |pdf: &[u8]| {
        pdf_worker::info_whole(pdf)
            .unwrap()
            .and_then(|info| info.document_kept)
    };
    let plain = |change: &dyn Fn(&mut Pdf)| {
        let mut pdf = Pdf::new();
        pdf.set_binary_marker(pdf_worker::MARKER);
        pdf.catalog(Ref::new(1)).pages(Ref::new(2));
        pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        pdf.page(Ref::new(3))
            .parent(Ref::new(2))
            .media_box(R::new(0.0, 0.0, 200.0, 200.0))
            .contents(Ref::new(4));
        pdf.stream(Ref::new(4), &content("/Encrypt"));
        change(&mut pdf);
        pdf.finish()
    };
    assert_eq!(whole(&plain(&|_| {})), Some(true));
    // The same PDF without OpenMaker's mark: another program's, whose
    // every part no check here can vouch for.
    let foreign = plain(&|pdf| pdf.set_binary_marker(b"\xe2\xe3\xcf\xd3"));
    assert_eq!(whole(&foreign), Some(false));
    // A page with annotations, whose strings a copy may not spell as they
    // were.
    let annotated = {
        let mut pdf = Pdf::new();
        pdf.set_binary_marker(pdf_worker::MARKER);
        pdf.catalog(Ref::new(1)).pages(Ref::new(2));
        pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        pdf.page(Ref::new(3))
            .parent(Ref::new(2))
            .media_box(R::new(0.0, 0.0, 200.0, 200.0))
            .annotations([Ref::new(4)]);
        pdf.indirect(Ref::new(4))
            .dict()
            .pair(N(b"Type"), N(b"Annot"))
            .pair(N(b"Subtype"), N(b"Text"));
        pdf.finish()
    };
    assert_eq!(whole(&annotated), Some(false));
    let custom = plain(&|pdf| {
        pdf.document_info(Ref::new(9))
            .pair(N(b"ProjectCode"), pdf_writer::TextStr("Q42"));
    });
    assert_eq!(whole(&custom), Some(false));
    let identified = plain(&|pdf| {
        pdf.set_file_id((b"first".to_vec(), b"second".to_vec()));
    });
    assert_eq!(whole(&identified), Some(false));
    // An extra trailer key, an escaped one among them.
    let bytes = plain(&|_| {});
    for key in ["/Foo 1", "/F#6fo 1"] {
        let at = bytes
            .windows(b"/Root".len())
            .rposition(|w| w == b"/Root")
            .unwrap();
        let mut extra = bytes[..at].to_vec();
        extra.extend(format!("{key} ").bytes());
        extra.extend(&bytes[at..]);
        assert_eq!(whole(&extra), Some(false), "{key}");
    }
    // A comment that spells a trailer of its own before the real one, which
    // has file identifiers.
    let identified = plain(&|pdf| {
        pdf.set_file_id((b"first".to_vec(), b"second".to_vec()));
    });
    let at = identified
        .windows(b"trailer".len())
        .rposition(|w| w == b"trailer")
        .unwrap();
    let mut masked = identified[..at].to_vec();
    masked.extend(b"% trailer << /Size 5 /Root 1 0 R >>\n");
    masked.extend(&identified[at..]);
    assert_ne!(whole(&masked), Some(true));
    // A page whose `/Parent` does not resolve, though the tree above it
    // gives it its box: the copy, reading up its parents, would lose it.
    let mut orphan = Pdf::new();
    orphan.set_binary_marker(pdf_worker::MARKER);
    orphan.catalog(Ref::new(1)).pages(Ref::new(2));
    orphan
        .pages(Ref::new(2))
        .kids([Ref::new(3)])
        .count(1)
        .media_box(R::new(0.0, 0.0, 200.0, 250.0));
    orphan.page(Ref::new(3)).parent(Ref::new(9));
    assert_eq!(whole(&orphan.finish()), Some(false));
    // A page whose own data refers to the page tree: the whole copy fails
    // rather than write `null` there, where a page import does.
    let private = {
        let mut pdf = Pdf::new();
        pdf.set_binary_marker(pdf_worker::MARKER);
        pdf.catalog(Ref::new(1)).pages(Ref::new(2));
        pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        let mut page = pdf.page(Ref::new(3));
        page.parent(Ref::new(2))
            .media_box(R::new(0.0, 0.0, 200.0, 200.0));
        page.pair(N(b"PieceInfo"), Ref::new(5));
        page.finish();
        pdf.indirect(Ref::new(5))
            .dict()
            .insert(N(b"App"))
            .dict()
            .pair(N(b"Tree"), Ref::new(2));
        pdf.finish()
    };
    assert!(pdf_worker::assemble(&[&private], &[(0, 0)])
        .unwrap()
        .is_some());
    assert_eq!(pdf_worker::assemble_whole(&[&private], &[(0, 0)]), Ok(None));
    // A comment naming another revision's table: which revision a reader
    // takes is no longer this table's alone.
    let mut revised = plain(&|_| {});
    revised.extend(b"% startxref 9\n");
    assert_ne!(whole(&revised), Some(true));
    // An inheritable attribute of the wrong type on a node, which readers
    // pass over (inheriting the root's box) and the copy would write.
    let mut typed = Pdf::new();
    typed.set_binary_marker(pdf_worker::MARKER);
    typed.catalog(Ref::new(1)).pages(Ref::new(2));
    typed
        .pages(Ref::new(2))
        .kids([Ref::new(3)])
        .count(1)
        .media_box(R::new(0.0, 0.0, 200.0, 250.0));
    let mut node = typed.indirect(Ref::new(3)).dict();
    node.pair(N(b"Type"), N(b"Pages"));
    node.pair(N(b"Parent"), Ref::new(2));
    node.pair(N(b"Count"), 1);
    node.insert(N(b"Kids")).array().item(Ref::new(4));
    node.pair(N(b"MediaBox"), 5);
    node.finish();
    typed.page(Ref::new(4)).parent(Ref::new(3));
    assert_eq!(whole(&typed.finish()), Some(false));
    // A page-tree node with data of its own, which the new tree would not
    // carry.
    let mut noted = Pdf::new();
    noted.set_binary_marker(pdf_worker::MARKER);
    noted.catalog(Ref::new(1)).pages(Ref::new(2));
    noted
        .pages(Ref::new(2))
        .kids([Ref::new(3)])
        .count(1)
        .pair(N(b"PieceInfo"), Ref::new(5));
    noted
        .page(Ref::new(3))
        .parent(Ref::new(2))
        .media_box(R::new(0.0, 0.0, 200.0, 200.0));
    noted.indirect(Ref::new(5)).dict().pair(N(b"Private"), true);
    assert_eq!(whole(&noted.finish()), Some(false));
    // Whatever else the file holds, though nothing above names it (an
    // object no page refers to), the whole copy finds object by object
    // and refuses; a file with nothing else is copied whole.
    let orphaned = plain(&|pdf| {
        pdf.indirect(Ref::new(20)).dict().pair(N(b"Lost"), true);
    });
    assert_eq!(whole(&orphaned), Some(true));
    assert_eq!(
        pdf_worker::assemble_whole(&[&orphaned], &[(0, 0)]),
        Ok(None)
    );
    assert!(pdf_worker::assemble_whole(&[&plain(&|_| {})], &[(0, 0)])
        .unwrap()
        .is_some());
}

/// Five pages whose `/Parent` is not their page tree but objects 10, 12,
/// 11, 10 and 12, in a PDF whose cross-reference table is broken: objects
/// 10 and 6 (the media box 12 refers to) are listed free and object 11 at a
/// wrong offset. Reading 11 makes hayro rebuild the table, after which 10
/// and 6 resolve too.
fn broken_xref() -> Vec<u8> {
    let objects = [
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 7 0 R 8 0 R] /Count 5 >>",
        ),
        (3, "<< /Type /Page /Parent 10 0 R >>"),
        (4, "<< /Type /Page /Parent 12 0 R >>"),
        (5, "<< /Type /Page /Parent 11 0 R >>"),
        (6, "[0 0 500 500]"),
        (7, "<< /Type /Page /Parent 10 0 R >>"),
        (8, "<< /Type /Page /Parent 12 0 R >>"),
        (10, "<< /Type /Pages /MediaBox [0 0 300 300] >>"),
        (11, "<< /Type /Pages /MediaBox [0 0 400 400] >>"),
        (12, "<< /Type /Pages /MediaBox 6 0 R >>"),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = [None; 13];
    for (id, body) in objects {
        offsets[id] = Some(out.len());
        out.extend(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref = out.len();
    out.extend(b"xref\n0 13\n0000000000 65535 f \n");
    for (id, offset) in offsets.iter().enumerate().skip(1) {
        let line = match (id, offset) {
            (6 | 10, _) | (_, None) => "0000000000 00000 f \n".to_owned(),
            (11, Some(_)) => "0000000001 00000 n \n".to_owned(),
            (_, Some(offset)) => format!("{offset:010} 00000 n \n"),
        };
        out.extend(line.as_bytes());
    }
    out.extend(
        format!("trailer\n<< /Size 13 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// A PDF of a page for each of `texts`, showing it at 72 / y in
/// Helvetica, as the desktop draws DynaPDF's text: the page box around it.
fn overlay(texts: &[(&str, i32)]) -> Vec<u8> {
    let mut pdf = Pdf::new();
    pdf.catalog(Ref::new(1)).pages(Ref::new(2));
    let kids = (0..texts.len() as i32).map(|i| Ref::new(4 + 2 * i));
    pdf.pages(Ref::new(2)).kids(kids).count(texts.len() as i32);
    pdf.indirect(Ref::new(3))
        .dict()
        .pair(N(b"Type"), N(b"Font"))
        .pair(N(b"Subtype"), N(b"Type1"))
        .pair(N(b"BaseFont"), N(b"Helvetica"));
    for (i, (text, y)) in texts.iter().enumerate() {
        let id = 4 + 2 * i as i32;
        let mut page = pdf.indirect(Ref::new(id)).dict();
        page.pair(N(b"Type"), N(b"Page"));
        page.pair(N(b"Parent"), Ref::new(2));
        page.pair(N(b"MediaBox"), R::new(0.0, 0.0, 800.0, 800.0));
        page.pair(N(b"Contents"), Ref::new(id + 1));
        page.insert(N(b"Resources"))
            .dict()
            .insert(N(b"Font"))
            .dict()
            .pair(N(b"F1"), Ref::new(3));
        page.finish();
        let content = format!("BT /F1 12 Tf 72 {y} Td ({text}) Tj ET");
        pdf.stream(Ref::new(id + 1), content.as_bytes());
    }
    pdf.finish()
}

/// A page written on: its own content kept, in `q` ... `Q`, with the text
/// drawn over it as a form, and the box set. Written on again, it keeps
/// the first form under its name; a page whose content is an array kept
/// in an object of its own keeps that content. A turned page is not drawn
/// on.
#[test]
fn a_page_written_on_keeps_its_content_and_gets_its_boxes() {
    use pdf_worker::{BoxKind, Planned};
    worker();
    let (a, hex) = source_a();
    let written = overlay(&[("X", 700), ("OV", 700)]);
    let plan = [
        Planned {
            source: 0,
            page: 0,
            overlay: Some((1, 1)),
            boxes: vec![(BoxKind::Media, [0.0, 0.0, 782.0, 792.0])],
        },
        Planned::from((0, 1)),
    ];
    let out = pdf_worker::assemble_planned(&[&a, &written], &plan, &[], &[], &[])
        .unwrap()
        .expect("a document");
    assert_eq!(
        pdf_worker::text(&out).unwrap().as_deref(),
        Some("A1\rOV\rA2")
    );
    let parsed = Parsed::new(out.clone()).unwrap();
    let pages = parsed.pages();
    assert_eq!(rect(pages[0].media_box()), [0.0, 0.0, 782.0, 792.0]);
    assert_eq!(rect(pages[1].media_box()), [0.0, 0.0, 595.0, 842.0]);
    // The page's own stream, as it was, between the q and the form.
    let contents: Array<'_> = pages[0].raw().get(b"Contents").unwrap();
    let streams: Vec<_> = contents
        .iter::<hayro::hayro_interpret::hayro_syntax::object::Stream<'_>>()
        .collect();
    assert_eq!(streams.len(), 3);
    assert_eq!(&*streams[0].raw_data(), b"q\n");
    assert_eq!(&*streams[1].raw_data(), &hex[..]);
    assert!(String::from_utf8_lossy(&streams[2].raw_data()).contains("Do"));
    // Written on again (a proof of a proof), the new form takes a name of
    // its own: the first still draws what it drew.
    let again = overlay(&[("OV2", 400)]);
    let plan = [Planned {
        overlay: Some((1, 0)),
        ..Planned::from((0, 0))
    }];
    let twice = pdf_worker::assemble_planned(&[&out, &again], &plan, &[], &[], &[])
        .unwrap()
        .expect("a document");
    assert_eq!(
        pdf_worker::text(&twice).unwrap().as_deref(),
        Some("A1\rOV\rOV2")
    );
    // Content in an array of its own object is drawn under the text.
    let mut indirect = Pdf::new();
    indirect.catalog(Ref::new(1)).pages(Ref::new(2));
    indirect.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
    let mut page = indirect.indirect(Ref::new(3)).dict();
    page.pair(N(b"Type"), N(b"Page"));
    page.pair(N(b"Parent"), Ref::new(2));
    page.pair(N(b"MediaBox"), R::new(0.0, 0.0, 800.0, 800.0));
    page.pair(N(b"Contents"), Ref::new(4));
    page.insert(N(b"Resources"))
        .dict()
        .insert(N(b"Font"))
        .dict()
        .pair(N(b"F1"), Ref::new(6));
    page.finish();
    indirect.indirect(Ref::new(4)).array().item(Ref::new(5));
    indirect.stream(Ref::new(5), &content("C1"));
    indirect
        .indirect(Ref::new(6))
        .dict()
        .pair(N(b"Type"), N(b"Font"))
        .pair(N(b"Subtype"), N(b"Type1"))
        .pair(N(b"BaseFont"), N(b"Helvetica"));
    let indirect = indirect.finish();
    let drawn = pdf_worker::assemble_planned(&[&indirect, &again], &plan, &[], &[], &[])
        .unwrap()
        .expect("a document");
    assert_eq!(
        pdf_worker::text(&drawn).unwrap().as_deref(),
        Some("C1\rOV2")
    );
    // A turned page is not drawn over.
    let mut turned = Pdf::new();
    turned.catalog(Ref::new(1)).pages(Ref::new(2));
    turned.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
    turned
        .indirect(Ref::new(3))
        .dict()
        .pair(N(b"Type"), N(b"Page"))
        .pair(N(b"Parent"), Ref::new(2))
        .pair(N(b"MediaBox"), R::new(0.0, 0.0, 300.0, 300.0))
        .pair(N(b"TrimBox"), R::new(9.0, 9.0, 290.55, 290.55))
        .pair(N(b"Rotate"), 90);
    let turned = turned.finish();
    // Info says which pages are turned, so nothing is written on one, and
    // each page's boxes: written on it, or (the media box) inherited, at
    // the precision written (290.55, not a single float's 290.549987...).
    let info = pdf_worker::info(&turned).unwrap().unwrap();
    assert!(info.turned(0));
    assert_eq!(
        info.geometry(0).unwrap().boxes[BoxKind::Trim as usize],
        Some([9.0, 9.0, 290.55, 290.55])
    );
    let first = pdf_worker::info(&a).unwrap().unwrap();
    assert!(!first.turned(0));
    let boxes = first.geometry(0).unwrap().boxes;
    assert_eq!(
        boxes[BoxKind::Media as usize],
        Some([0.0, 0.0, 612.0, 792.0])
    );
    assert_eq!(
        boxes[BoxKind::Trim as usize],
        Some([9.0, 9.0, 603.0, 783.0])
    );
    assert_eq!(boxes[BoxKind::Crop as usize], None);
    let inherited = pdf_worker::info(&source_b()).unwrap().unwrap();
    assert_eq!(
        inherited.geometry(0).unwrap().boxes[BoxKind::Media as usize],
        Some([0.0, 0.0, 300.0, 300.0])
    );
    // 10,001 pages under one parent read it once, within the worker's
    // deadline (once read for each page, past it); the pages past 10,000
    // are not described, and a box of five entries is none, even when the
    // fifth is a reference to nothing.
    let mut flat = Pdf::new();
    flat.catalog(Ref::new(1)).pages(Ref::new(2));
    let count = 10_001;
    let mut tree = flat.indirect(Ref::new(2)).dict();
    tree.pair(N(b"Type"), N(b"Pages"));
    tree.insert(N(b"Kids"))
        .array()
        .items((0..count).map(|i| Ref::new(3 + i)));
    tree.pair(N(b"Count"), count);
    tree.pair(N(b"MediaBox"), R::new(0.0, 0.0, 612.0, 792.0));
    tree.finish();
    for i in 0..count {
        let mut page = flat.indirect(Ref::new(3 + i)).dict();
        page.pair(N(b"Type"), N(b"Page"));
        page.pair(N(b"Parent"), Ref::new(2));
        if i == 0 {
            let mut trim = page.insert(N(b"TrimBox")).array();
            trim.items([0.0f32, 0.0, 612.0, 792.0]);
            trim.item(Ref::new(99_999));
        }
    }
    let flat_pdf = flat.finish();
    let flat = pdf_worker::info(&flat_pdf).unwrap().unwrap();
    assert_eq!(flat.pages, 10_001);
    let last = flat.geometry(9_999).unwrap().boxes;
    assert_eq!(
        last[BoxKind::Media as usize],
        Some([0.0, 0.0, 612.0, 792.0])
    );
    assert_eq!(
        flat.geometry(0).unwrap().boxes[BoxKind::Trim as usize],
        None
    );
    assert!(flat.geometry(10_000).is_none() && flat.turned(10_000));
    // Copied, every page gets the box it inherits, the parent read once
    // (#1128; read for each page, 3,000 pages took 20 s in a debug worker).
    let every: Vec<(u32, u32)> = (0..10_001).map(|page| (0, page)).collect();
    let copied = pdf_worker::assemble(&[&flat_pdf], &every)
        .unwrap()
        .expect("a document");
    let copied = Parsed::new(copied).unwrap();
    assert_eq!(copied.pages().len(), 10_001);
    assert_eq!(
        rect(copied.pages()[10_000].media_box()),
        [0.0, 0.0, 612.0, 792.0]
    );
    // A parent, or a box kept in an object of its own, that did not
    // resolve is looked up again: once a later lookup has repaired the
    // cross-reference table, the fourth and fifth pages get the boxes the
    // first and second did not.
    let broken = broken_xref();
    let expected = [
        None,
        None,
        Some([0.0, 0.0, 400.0, 400.0]),
        Some([0.0, 0.0, 300.0, 300.0]),
        Some([0.0, 0.0, 500.0, 500.0]),
    ];
    let info = pdf_worker::info(&broken).unwrap().unwrap();
    let media = |page| info.geometry(page).unwrap().boxes[BoxKind::Media as usize];
    assert_eq!([0, 1, 2, 3, 4].map(media), expected);
    let every: Vec<(u32, u32)> = (0..5).map(|page| (0, page)).collect();
    let copied = pdf_worker::assemble(&[&broken], &every)
        .unwrap()
        .expect("a document");
    let copied = Parsed::new(copied).unwrap();
    let boxed = |page: usize| {
        copied.pages()[page]
            .raw()
            .get::<Rect>(b"MediaBox")
            .map(rect)
    };
    assert_eq!([0, 1, 2, 3, 4].map(boxed), expected);
    // A box is inherited from 64 parents up at most, counted from each
    // page whatever page was read first: here a deep page is read first,
    // through a node a shallow page shares, two parents below the box.
    let mut deep = Pdf::new();
    deep.catalog(Ref::new(1)).pages(Ref::new(2));
    let mut root = deep.indirect(Ref::new(2)).dict();
    root.pair(N(b"Type"), N(b"Pages"));
    root.insert(N(b"Kids")).array().item(Ref::new(3));
    root.pair(N(b"Count"), 2);
    root.pair(N(b"MediaBox"), R::new(0.0, 0.0, 612.0, 792.0));
    root.finish();
    // Node 3 holds the chain (10..73) and the shallow page (5); the deep
    // page (4) is under the chain's last node, 65 parents below the box.
    let chain = 63;
    deep.indirect(Ref::new(3))
        .dict()
        .pair(N(b"Type"), N(b"Pages"))
        .pair(N(b"Parent"), Ref::new(2))
        .pair(N(b"Count"), 2)
        .insert(N(b"Kids"))
        .array()
        .items([Ref::new(10), Ref::new(5)]);
    for i in 0..chain {
        let kid = if i + 1 == chain { 4 } else { 11 + i };
        deep.indirect(Ref::new(10 + i))
            .dict()
            .pair(N(b"Type"), N(b"Pages"))
            .pair(N(b"Parent"), Ref::new(if i == 0 { 3 } else { 9 + i }))
            .pair(N(b"Count"), 1)
            .insert(N(b"Kids"))
            .array()
            .item(Ref::new(kid));
    }
    for (id, parent) in [(4, 9 + chain), (5, 3)] {
        deep.indirect(Ref::new(id))
            .dict()
            .pair(N(b"Type"), N(b"Page"))
            .pair(N(b"Parent"), Ref::new(parent));
    }
    let deep = deep.finish();
    // A page copy would lose the deep page's box: appending refuses it.
    assert_eq!(
        pdf_worker::info_whole(&deep)
            .unwrap()
            .unwrap()
            .document_kept,
        Some(false)
    );
    let deep = pdf_worker::info(&deep).unwrap().unwrap();
    assert_eq!(deep.pages, 2);
    assert_eq!(
        deep.geometry(0).unwrap().boxes[BoxKind::Media as usize],
        None
    );
    assert_eq!(
        deep.geometry(1).unwrap().boxes[BoxKind::Media as usize],
        Some([0.0, 0.0, 612.0, 792.0])
    );
    let plan = [Planned {
        overlay: Some((1, 0)),
        ..Planned::from((0, 0))
    }];
    assert_eq!(
        pdf_worker::assemble_planned(&[&turned, &written], &plan, &[], &[], &[]),
        Ok(None)
    );
}

/// Form fields the DynaPDF commands create: each a widget on its page,
/// after the page's own annotations, and a field of the document's
/// interactive form, with an appearance for each state. Fields of one name
/// and type are one field, a group whose kids are their widgets: check
/// boxes of three export values, as a proof form's approval boxes are.
#[test]
fn form_fields_are_widgets_of_an_interactive_form() {
    use pdf_worker::{Field, FieldKind, Planned};
    worker();
    let b = source_b();
    let field = |name: &str, kind: FieldKind, rect: [f64; 4]| Field {
        page: 0,
        name: name.to_owned(),
        kind,
        rect,
        border_width: 1.0,
        text: [0.0, 0.0, 1.0],
        border: Some([1.0, 0.0, 0.0]),
        back: None,
        size: 0.0,
        action: None,
    };
    let fields = [
        Field {
            back: Some([1.0, 1.0, 1.0]),
            size: 10.0,
            ..field(
                "Notes",
                FieldKind::Text {
                    bold: false,
                    multiline: true,
                    max_len: 40,
                },
                [72.0, 100.0, 272.0, 140.0],
            )
        },
        field(
            "Approved",
            FieldKind::CheckBox {
                export: "Yes".into(),
                checked: true,
            },
            [72.0, 150.0, 86.0, 164.0],
        ),
        field(
            "Prüfer",
            FieldKind::CheckBox {
                export: "Yes".into(),
                checked: false,
            },
            [100.0, 150.0, 114.0, 164.0],
        ),
        field(
            "Signature",
            FieldKind::Signature,
            [150.0, 20.0, 290.0, 50.0],
        ),
        // Made later, of names made before: kids of their groups.
        field(
            "Notes",
            FieldKind::Text {
                bold: false,
                multiline: true,
                max_len: 40,
            },
            [72.0, 60.0, 272.0, 90.0],
        ),
        field(
            "Approved",
            FieldKind::CheckBox {
                export: "Changes".into(),
                checked: false,
            },
            [72.0, 170.0, 86.0, 184.0],
        ),
        field(
            "Approved",
            FieldKind::CheckBox {
                export: "Rejected".into(),
                checked: false,
            },
            [72.0, 190.0, 86.0, 204.0],
        ),
    ];
    let out = pdf_worker::assemble_planned(&[&b], &[Planned::from((0, 0))], &fields, &[], &[])
        .unwrap()
        .expect("a document");
    let parsed = Parsed::new(out).unwrap();
    let catalog: Dict<'_> = parsed.xref().get(parsed.xref().root_id()).unwrap();
    let form: Dict<'_> = catalog.get(b"AcroForm").unwrap();
    let listed: Vec<Dict<'_>> = form
        .get::<Array<'_>>(b"Fields")
        .unwrap()
        .iter::<Dict<'_>>()
        .collect();
    let named = |dict: &Dict<'_>| {
        let bytes = dict
            .get::<hayro::hayro_interpret::hayro_syntax::object::String<'_>>(b"T")
            .unwrap()
            .as_bytes()
            .to_vec();
        match bytes.strip_prefix(&[0xFE, 0xFF]) {
            Some(utf16) => std::string::String::from_utf16(
                &utf16
                    .chunks(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            None => std::string::String::from_utf8(bytes).unwrap(),
        }
    };
    let kind = |dict: &Dict<'_>| dict.get::<Name<'_>>(b"FT").unwrap().as_str().to_owned();
    assert_eq!(
        listed
            .iter()
            .map(|d| (named(d), kind(d)))
            .collect::<Vec<_>>(),
        [
            ("Notes".to_owned(), "Tx".to_owned()),
            ("Approved".to_owned(), "Btn".to_owned()),
            ("Prüfer".to_owned(), "Btn".to_owned()),
            ("Signature".to_owned(), "Sig".to_owned()),
        ]
    );
    fn kids<'a>(dict: &Dict<'a>) -> Vec<Dict<'a>> {
        dict.get::<Array<'a>>(b"Kids")
            .map(|kids| kids.iter::<Dict<'a>>().collect())
            .unwrap_or_default()
    }
    // The text field group: multiline, its length limit and font on the
    // field, each widget its own colors and no name.
    let notes = &listed[0];
    assert_eq!(notes.get::<i32>(b"Ff"), Some(4096));
    assert_eq!(notes.get::<i32>(b"MaxLen"), Some(40));
    let da = notes
        .get::<hayro::hayro_interpret::hayro_syntax::object::String<'_>>(b"DA")
        .unwrap();
    assert_eq!(da.as_bytes(), b"/Helv 10 Tf 0 0 1 rg");
    let note_kids = kids(notes);
    assert_eq!(note_kids.len(), 2);
    assert!(note_kids
        .iter()
        .all(|kid| !kid.contains_key(b"T") && kid.contains_key(b"Parent")));
    let backs: Vec<bool> = note_kids
        .iter()
        .map(|kid| kid.get::<Dict<'_>>(b"MK").unwrap().contains_key(b"BG"))
        .collect();
    assert_eq!(backs, [true, false]);
    // The check box group: its value the checked box's export value; each
    // widget on when its own export value is the value, off otherwise,
    // with an appearance of its own export value and of Off.
    let name_of =
        |dict: &Dict<'_>, key: &[u8]| dict.get::<Name<'_>>(key).unwrap().as_str().to_owned();
    let approved = &listed[1];
    assert_eq!(
        (name_of(approved, b"V"), name_of(approved, b"DV")),
        ("Yes".into(), "Yes".into())
    );
    let boxes = kids(approved);
    assert_eq!(
        boxes
            .iter()
            .map(|kid| name_of(kid, b"AS"))
            .collect::<Vec<_>>(),
        ["Yes", "Off", "Off"]
    );
    for (kid, export) in boxes.iter().zip(["Yes", "Changes", "Rejected"]) {
        assert!(!kid.contains_key(b"T"));
        let normal: Dict<'_> = kid.get::<Dict<'_>>(b"AP").unwrap().get(b"N").unwrap();
        assert!(normal.contains_key(export.as_bytes()) && normal.contains_key(b"Off"));
    }
    // A check box alone: field and widget in one.
    assert_eq!(name_of(&listed[2], b"AS"), "Off");
    // The page keeps its link, then lists the widgets.
    let page = parsed.pages()[0].raw().clone();
    let annots: Vec<Dict<'_>> = page
        .get::<Array<'_>>(b"Annots")
        .unwrap()
        .iter::<Dict<'_>>()
        .collect();
    let subtypes: Vec<_> = annots
        .iter()
        .map(|a| a.get::<Name<'_>>(b"Subtype").unwrap().as_str().to_owned())
        .collect();
    assert_eq!(
        subtypes,
        ["Link", "Widget", "Widget", "Widget", "Widget", "Widget", "Widget", "Widget"]
    );
    // A field on a turned page, or with an empty name, makes no document.
    let mut turned = Pdf::new();
    turned.catalog(Ref::new(1)).pages(Ref::new(2));
    turned.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
    turned
        .indirect(Ref::new(3))
        .dict()
        .pair(N(b"Type"), N(b"Page"))
        .pair(N(b"Parent"), Ref::new(2))
        .pair(N(b"MediaBox"), R::new(0.0, 0.0, 300.0, 300.0))
        .pair(N(b"Rotate"), 90);
    let turned = turned.finish();
    let plan = [Planned::from((0, 0))];
    assert_eq!(
        pdf_worker::assemble_planned(&[&turned], &plan, &fields[..1], &[], &[]),
        Ok(None)
    );
    // Nor does a field PDF cannot hold: no name, a name with a period (a
    // child's), an export value that is the off state's or holds NUL, a
    // check box without room for its check, or corners whose distance
    // overflows.
    let check = |export: &str| Field {
        kind: FieldKind::CheckBox {
            export: export.into(),
            checked: true,
        },
        ..fields[1].clone()
    };
    let refused = [
        Field {
            name: String::new(),
            ..fields[3].clone()
        },
        Field {
            name: "a.b".into(),
            ..fields[3].clone()
        },
        check("Off"),
        check("A\0B"),
        // A 14-point box whose 7-point border leaves no room for an
        // automatic check.
        Field {
            border_width: 7.0,
            size: 0.0,
            ..check("Yes")
        },
        Field {
            rect: [-1e308, -1e308, 1e308, 1e308],
            ..fields[3].clone()
        },
    ];
    for field in refused {
        let made =
            pdf_worker::assemble_planned(&[&b], &plan, std::slice::from_ref(&field), &[], &[])
                .map(|out| out.is_some());
        assert_eq!(made, Ok(false), "{field:?}");
    }
    // Nor do fields of one name that cannot be one field: of two types,
    // text widgets of different flags, two export values checked, or a
    // box of the checked value left unchecked.
    let text = |multiline| Field {
        kind: FieldKind::Text {
            bold: false,
            multiline,
            max_len: 40,
        },
        ..fields[0].clone()
    };
    let boxed = |export: &str, checked| Field {
        kind: FieldKind::CheckBox {
            export: export.into(),
            checked,
        },
        ..fields[1].clone()
    };
    let not_one = [
        [
            text(true),
            Field {
                name: "Notes".into(),
                ..boxed("Yes", false)
            },
        ],
        [text(true), text(false)],
        [boxed("Yes", true), boxed("Changes", true)],
        [boxed("Yes", true), boxed("Yes", false)],
    ];
    for group in not_one {
        let made =
            pdf_worker::assemble_planned(&[&b], &plan, &group, &[], &[]).map(|out| out.is_some());
        assert_eq!(made, Ok(false), "{group:?}");
    }
}

/// A button the DynaPDF commands create: a push button that does not
/// print, its caption a page the desktop drew, drawn in its appearance as a
/// form; a JavaScript action it runs when clicked, written once for every
/// field that runs it, never run here.
#[test]
fn a_button_shows_its_caption_and_runs_its_action() {
    use hayro::hayro_interpret::hayro_syntax::object::{Stream, String as PdfString};
    use pdf_worker::{Field, FieldKind, Planned};
    worker();
    let b = source_b();
    let captions = overlay(&[("Next", 10)]);
    let field = |name: &str, kind: FieldKind, rect: [f64; 4]| Field {
        page: 0,
        name: name.to_owned(),
        kind,
        rect,
        border_width: 1.0,
        text: [0.0; 3],
        border: Some([0.0; 3]),
        back: Some([0.9, 0.9, 0.9]),
        size: 9.0,
        action: Some(0),
    };
    let button = field(
        "Next",
        FieldKind::Button {
            bold: false,
            caption: "Next".into(),
            caption_page: Some((1, 0)),
        },
        [200.0, 20.0, 260.0, 40.0],
    );
    let check = field(
        "Seen",
        FieldKind::CheckBox {
            export: "Yes".into(),
            checked: false,
        },
        [270.0, 20.0, 284.0, 34.0],
    );
    let actions = ["this.pageNum++;".to_owned()];
    let plan = [Planned::from((0, 0))];
    // And a web link, after the widgets on the page.
    let link = pdf_worker::Link {
        page: 0,
        rect: [20.0, 60.0, 120.0, 80.0],
        border_width: 1.0,
        color: [0.0; 3],
        uri: "https://example.com/page".into(),
    };
    let out = pdf_worker::assemble_planned(
        &[&b, &captions],
        &plan,
        &[button.clone(), check],
        &actions,
        std::slice::from_ref(&link),
    )
    .unwrap()
    .expect("a document");
    let parsed = Parsed::new(out).unwrap();
    let catalog: Dict<'_> = parsed.xref().get(parsed.xref().root_id()).unwrap();
    let fields: Vec<Dict<'_>> = catalog
        .get::<Dict<'_>>(b"AcroForm")
        .unwrap()
        .get::<Array<'_>>(b"Fields")
        .unwrap()
        .iter::<Dict<'_>>()
        .collect();
    let next = &fields[0];
    assert_eq!(next.get::<Name<'_>>(b"FT").unwrap().as_str(), "Btn");
    assert_eq!(next.get::<i32>(b"Ff"), Some(65536));
    assert!(!next.contains_key(b"F"), "a button does not print");
    let mk: Dict<'_> = next.get(b"MK").unwrap();
    assert_eq!(mk.get::<PdfString<'_>>(b"CA").unwrap().as_bytes(), b"Next");
    // Its appearance draws the caption page as a form.
    let normal: Stream<'_> = next.get::<Dict<'_>>(b"AP").unwrap().get(b"N").unwrap();
    let resources: Dict<'_> = normal.dict().get(b"Resources").unwrap();
    assert!(resources
        .get::<Dict<'_>>(b"XObject")
        .unwrap()
        .contains_key(b"Cap"));
    assert!(String::from_utf8_lossy(&normal.decoded().unwrap()).contains("/Cap Do"));
    // The action: JavaScript, as given, one object for both fields.
    let action: Dict<'_> = next.get(b"A").unwrap();
    assert_eq!(action.get::<Name<'_>>(b"S").unwrap().as_str(), "JavaScript");
    assert_eq!(
        action.get::<PdfString<'_>>(b"JS").unwrap().as_bytes(),
        b"this.pageNum++;"
    );
    let target = |dict: &Dict<'_>| reference(dict, b"A");
    assert_eq!(target(next), target(&fields[1]));
    // The link: last of the page's annotations (its own link first), a URI
    // action, no highlight, a border of the line width.
    let annots: Vec<Dict<'_>> = parsed.pages()[0]
        .raw()
        .get::<Array<'_>>(b"Annots")
        .unwrap()
        .iter::<Dict<'_>>()
        .collect();
    let web = annots.last().unwrap();
    assert_eq!(web.get::<Name<'_>>(b"Subtype").unwrap().as_str(), "Link");
    assert_eq!(web.get::<Name<'_>>(b"H").unwrap().as_str(), "N");
    assert_eq!(
        web.get::<Array<'_>>(b"Rect")
            .unwrap()
            .iter::<f64>()
            .collect::<Vec<_>>(),
        link.rect
    );
    assert_eq!(
        web.get::<Array<'_>>(b"Border")
            .unwrap()
            .iter::<f64>()
            .collect::<Vec<_>>(),
        [0.0, 0.0, link.border_width]
    );
    let uri: Dict<'_> = web.get(b"A").unwrap();
    assert_eq!(uri.get::<Name<'_>>(b"S").unwrap().as_str(), "URI");
    assert_eq!(
        uri.get::<PdfString<'_>>(b"URI").unwrap().as_bytes(),
        b"https://example.com/page"
    );
    assert_eq!(
        annots.len(),
        4,
        "source B's link, two widgets, the web link"
    );
    // Links work without a form and refuse a missing destination page.
    let alone = pdf_worker::assemble_planned(&[&b], &plan, &[], &[], std::slice::from_ref(&link))
        .unwrap()
        .expect("a link without fields");
    let alone = Parsed::new(alone).unwrap();
    assert_eq!(
        alone.pages()[0]
            .raw()
            .get::<Array<'_>>(b"Annots")
            .unwrap()
            .iter::<Dict<'_>>()
            .count(),
        2
    );
    let missing = pdf_worker::Link {
        page: 1,
        ..link.clone()
    };
    assert_eq!(
        pdf_worker::assemble_planned(&[&b], &plan, &[], &[], &[missing]),
        Ok(None)
    );
    // A URL beyond 7-bit ASCII makes no document.
    let wide = pdf_worker::Link {
        uri: "https://example.com/pr\u{fc}fung".into(),
        ..link
    };
    let made_link =
        pdf_worker::assemble_planned(&[&b], &plan, &[], &[], &[wide]).map(|out| out.is_some());
    assert_eq!(made_link, Ok(false));
    // A second button of a name, or an action past the list, makes no
    // document.
    let made = |fields: &[Field], actions: &[String]| {
        pdf_worker::assemble_planned(&[&b, &captions], &plan, fields, actions, &[])
            .map(|out| out.is_some())
    };
    assert_eq!(made(&[button.clone(), button.clone()], &actions), Ok(false));
    assert_eq!(made(std::slice::from_ref(&button), &[]), Ok(false));
    // Nor does a caption without the page it is drawn on.
    let pageless = Field {
        kind: FieldKind::Button {
            bold: false,
            caption: "Next".into(),
            caption_page: None,
        },
        ..button.clone()
    };
    assert_eq!(made(&[pageless], &actions), Ok(false));
    // A script with a control PDFDocEncoding would read as another
    // character (U+0018 is a breve there) goes as UTF-16BE.
    let control = ["a = '\u{18}';".to_owned()];
    let out = pdf_worker::assemble_planned(&[&b, &captions], &plan, &[button], &control, &[])
        .unwrap()
        .expect("a document");
    let parsed = Parsed::new(out).unwrap();
    let catalog: Dict<'_> = parsed.xref().get(parsed.xref().root_id()).unwrap();
    let first: Dict<'_> = catalog
        .get::<Dict<'_>>(b"AcroForm")
        .unwrap()
        .get::<Array<'_>>(b"Fields")
        .unwrap()
        .iter::<Dict<'_>>()
        .next()
        .unwrap();
    let script = first
        .get::<Dict<'_>>(b"A")
        .unwrap()
        .get::<PdfString<'_>>(b"JS")
        .unwrap()
        .as_bytes()
        .to_vec();
    assert_eq!(&script[..2], [0xFE, 0xFF]);
    assert!(script.windows(2).any(|w| w == [0x00, 0x18]));
}

#[test]
fn highlights_have_located_quads_and_a_multiply_appearance() {
    worker();
    // Raw glyph strings must not compensate for missing matches across
    // positioning gaps/runs: return no partial/wrong set of rectangles.
    for (pattern, content) in [
        (
            "AB",
            &b"BT /F1 12 Tf 20 20 Td [(A) -2000 (B)] TJ ET BT /F1 12 Tf 20 40 Td (A) Tj (B) Tj ET"
                [..],
        ),
        ("AA", &b"BT /F1 12 Tf 20 20 Td (A) Tj (AA) Tj ET"[..]),
    ] {
        let mut gap = Pdf::new();
        gap.catalog(Ref::new(1)).pages(Ref::new(2));
        gap.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        gap.page(Ref::new(3))
            .parent(Ref::new(2))
            .media_box(R::new(0.0, 0.0, 300.0, 300.0))
            .contents(Ref::new(4))
            .resources()
            .fonts()
            .pair(N(b"F1"), Ref::new(5));
        gap.indirect(Ref::new(5))
            .dict()
            .pair(N(b"Type"), N(b"Font"))
            .pair(N(b"Subtype"), N(b"Type1"))
            .pair(N(b"BaseFont"), N(b"Helvetica"));
        gap.stream(Ref::new(4), content);
        let gap = gap.finish();
        assert_eq!(
            pdf_worker::highlight_positions(&gap, 0, pattern, false),
            Ok(None)
        );
    }
    let (a, _) = source_a();
    let found = pdf_worker::highlight_positions(&a, 0, "A1", false)
        .unwrap()
        .expect("horizontal outline text");
    assert_eq!(found.len(), 1);
    assert_eq!(
        pdf_worker::highlight_positions(&a, 0, "a1", false),
        Ok(Some(vec![]))
    );
    assert_eq!(
        pdf_worker::highlight_positions(&a, 0, "a1", true),
        Ok(Some(found.clone()))
    );
    assert_eq!(
        pdf_worker::highlight_positions(&a, 3, "A1", false),
        Ok(None)
    );
    let r = found[0];
    assert!(
        r[0] >= 72.0 && r[2] < 110.0 && r[1] >= 71.0 && r[3] < 100.0,
        "{r:?}"
    );
    let h = pdf_worker::Highlight {
        page: 0,
        rect: r,
        color: [1.0, 1.0, 0.0],
        flattened: false,
    };
    let plan = [pdf_worker::Planned::from((0, 0))];
    let out =
        pdf_worker::assemble_highlighted(&[&a], &plan, &[], &[], &[], std::slice::from_ref(&h))
            .unwrap()
            .unwrap();
    assert_eq!(pdf_worker::text(&out).unwrap().as_deref(), Some("A1"));
    let parsed = Parsed::new(out).unwrap();
    let annots = parsed.pages()[0].raw().get::<Array<'_>>(b"Annots").unwrap();
    let markup = annots.iter::<Dict<'_>>().next().unwrap();
    assert_eq!(
        markup.get::<Name<'_>>(b"Subtype").unwrap().as_str(),
        "Highlight"
    );
    assert_eq!(
        markup
            .get::<Array<'_>>(b"QuadPoints")
            .unwrap()
            .iter::<f64>()
            .count(),
        8
    );
    assert_eq!(
        markup
            .get::<Array<'_>>(b"C")
            .unwrap()
            .iter::<f64>()
            .collect::<Vec<_>>(),
        h.color
    );
    let ap = markup
        .get::<Dict<'_>>(b"AP")
        .unwrap()
        .get::<hayro::hayro_interpret::hayro_syntax::object::Stream<'_>>(b"N")
        .unwrap();
    let gs = ap
        .dict()
        .get::<Dict<'_>>(b"Resources")
        .unwrap()
        .get::<Dict<'_>>(b"ExtGState")
        .unwrap()
        .get::<Dict<'_>>(b"GS0")
        .unwrap();
    assert_eq!(gs.get::<Name<'_>>(b"BM").unwrap().as_str(), "Multiply");
    assert_eq!(pdf_worker::annotation_free(&a, &[0, 2]), Ok(Some(true)));
    let b = source_b();
    assert_eq!(pdf_worker::annotation_free(&b, &[0]), Ok(Some(false)));
    assert_eq!(pdf_worker::annotation_free(&a, &[3]), Ok(None));
    let baked = pdf_worker::Highlight {
        flattened: true,
        ..h.clone()
    };
    let flat = pdf_worker::assemble_highlighted(&[&a], &plan, &[], &[], &[], &[baked])
        .unwrap()
        .unwrap();
    assert_eq!(pdf_worker::text(&flat).unwrap().as_deref(), Some("A1"));
    let parsed_flat = Parsed::new(flat.clone()).unwrap();
    assert!(parsed_flat.pages()[0]
        .raw()
        .get::<Array<'_>>(b"Annots")
        .is_none());
    let raw = String::from_utf8_lossy(&flat);
    assert!(!raw.contains("/Subtype /Highlight"));
    assert!(raw.contains("/BM /Multiply") && raw.contains(" cm /OMDynaPDF0 Do Q"));
    if let Some(folder) = std::env::var_os("OM_PDF_ARTIFACT_DIR") {
        // Both files are synthetic: permit visual comparison without a viewer.
        let annotated =
            pdf_worker::assemble_highlighted(&[&a], &plan, &[], &[], &[], std::slice::from_ref(&h))
                .unwrap()
                .unwrap();
        std::fs::write(
            std::path::Path::new(&folder).join("flatten-annotations.pdf"),
            annotated,
        )
        .unwrap();
        std::fs::write(
            std::path::Path::new(&folder).join("flatten-content.pdf"),
            flat,
        )
        .unwrap();
    }
    let missing = pdf_worker::Highlight { page: 1, ..h };
    assert_eq!(
        pdf_worker::assemble_highlighted(&[&a], &plan, &[], &[], &[], &[missing]),
        Ok(None)
    );
}
