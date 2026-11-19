//! The container analysis functions against FileMaker Server's engine: the
//! battery `tools/fms-calc-harness/batteries/fn-container-analysis.txt` run
//! in the English Contacts template (`tests/data/fn-container-analysis.en.tsv`)
//! and `fn-container-analysis-intl.txt` in the five Contacts locales. Every
//! row also runs through the evaluator, which answers the rows the literal
//! reader leaves to the host (`GetLiveText`, `GetLiveTextAsJSON`). The
//! samples are inline Base64; failures show them as `<n bytes>`.

mod support;

use om_calc::eval::{self, CalcHost, Context, NullHost, StoredContainer, VarScope, Variables};
use om_calc::{CalcError, Container, Date, LibraryRef, Locale, StreamType, Text, Value};

/// FileMaker keeps a converted copy of BMP, TIFF and WebP pictures and a
/// rendered page of a PDF beside the data; its size is not reproduced.
const CONVERTED: &str = "Internal Size counts the server's converted copy";
/// Thumbnails are the server's image library's output (PNG chunks with
/// creation dates); OpenMaker encodes its own.
const ENCODER: &str = "thumbnail bytes are the server's image library's";

/// Rows that are expected to differ, with the reason.
const KNOWN: &[(&str, &str)] = &[
    (
        "GetContainerAttribute ( Base64Decode ( <154 bytes> ; \"a.bmp\" ) ; \"all\" )",
        CONVERTED,
    ),
    (
        "GetContainerAttribute ( Base64Decode ( <2359 bytes> ; \"a.pdf\" ) ; \"all\" )",
        CONVERTED,
    ),
    (
        "GetContainerAttribute ( GetThumbnail ( Base64Decode ( <73 bytes> ; \"a.png\" ) ; 2 ; 2 ) ; \"general\" )",
        ENCODER,
    ),
    (
        "Substitute ( GetTextFromPDF ( Base64Decode ( <887 bytes> ; \"a.pdf\" ) ) ; [ \" \" ; \"_\" ] ; [ Char ( 9 ) ; \"<TAB>\" ] )",
        "a tab character code without a glyph name has no Unicode value in the PDF reader",
    ),
];

/// `expr` with each Base64 literal shown as its decoded length.
fn short(expr: &str) -> String {
    let mut out = String::new();
    let mut rest = expr;
    while let Some(start) = rest.find('"') {
        let Some(len) = rest[start + 1..].find('"') else {
            break;
        };
        let literal = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        if literal.len() > 40
            && literal
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        {
            let pad = literal.bytes().rev().take_while(|&b| b == b'=').count();
            out.push_str(&format!("<{} bytes>", literal.len() / 4 * 3 - pad));
        } else {
            out.push_str(&rest[start..start + len + 2]);
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

fn known(expr: &str) -> Option<usize> {
    let expr = short(expr);
    KNOWN.iter().position(|(k, _)| *k == expr)
}

/// PDF data is read by this crate's worker executable (`pdf_worker`).
fn pdf_worker() {
    om_calc::functions::pdf_worker::set_default(env!("CARGO_BIN_EXE_om-pdf-worker").into());
}

fn fixture(name: &str) -> String {
    format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn english() {
    pdf_worker();
    let (rows, covered, failures) =
        support::check_harness_results(&fixture("fn-container-analysis.en.tsv"), &Locale::en_us());
    let mut seen = vec![false; KNOWN.len()];
    let mut unexpected = Vec::new();
    for f in &failures {
        let expr = &f[..f.rfind(": got ").expect("a failure line")];
        match known(expr) {
            Some(i) => seen[i] = true,
            None => unexpected.push(short(f)),
        }
    }
    println!(
        "fn-container-analysis.en: {rows} rows, {covered} covered, {} passed, {} known",
        covered - failures.len(),
        failures.len() - unexpected.len()
    );
    for f in &unexpected {
        println!("FAIL {f}");
    }
    assert!(unexpected.is_empty(), "{} rows failed", unexpected.len());
    for ((expr, _), seen) in KNOWN.iter().zip(seen) {
        assert!(seen, "{expr} now passes or is gone: remove it from KNOWN");
    }
}

#[test]
fn locales() {
    pdf_worker();
    support::check_fixture_set("fn-container-analysis-intl", &[]);
}

/// Every row through `om_formula`'s compiler and the evaluator, with a host
/// that has no text recognition.
#[test]
fn evaluator() {
    pdf_worker();
    let tsv = std::fs::read_to_string(fixture("fn-container-analysis.en.tsv")).unwrap();
    let mut host = NullHost::new(Locale::en_us(), Date::from_ymd(2026, 10, 28).unwrap());
    let (mut rows, mut failures) = (0, Vec::new());
    for line in tsv.lines().filter(|l| !l.starts_with('#')) {
        let [expr, error, _, want] = line.splitn(4, '\t').collect::<Vec<_>>()[..] else {
            continue;
        };
        rows += 1;
        let expr = support::unescape(expr);
        let result = eval::evaluate_text(&expr, &mut host, &Context::default());
        let got = eval::display(&result.value, &host);
        let differs = got != support::unescape(want) || result.code().to_string() != error;
        if differs && known(&expr).is_none() {
            failures.push(format!(
                "{}: got {got:?} error {}",
                short(&expr),
                result.code()
            ));
        }
    }
    println!(
        "fn-container-analysis.en through the evaluator: {rows} rows, {} failed",
        failures.len()
    );
    for f in &failures {
        println!("FAIL {f}");
    }
    assert!(failures.is_empty());
}

/// A host whose container field values arrive by reference (`$c`) and
/// which loads their data, as a host over a hosted file does.
struct Library {
    inner: NullHost,
    stored: Option<StoredContainer>,
}

impl CalcHost for Library {
    fn locale(&self) -> &Locale {
        self.inner.locale()
    }

    fn today(&self) -> Date {
        self.inner.today()
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        self.inner.variables(scope)
    }

    fn container_data(
        &mut self,
        container: &Container,
        _context: &Context,
    ) -> Result<StoredContainer, CalcError> {
        assert_eq!(container.text.as_str(), "photo.png");
        assert_eq!(container.library.as_ref().unwrap().member, [1]);
        self.stored.clone().ok_or(CalcError::NOT_SUPPORTED)
    }
}

#[test]
fn referenced_containers_load_through_the_host() {
    pdf_worker();
    // 3 x 2 red PNG, as a library member holds it.
    const PNG: &str = "89504e470d0a1a0a0000000d49484452000000030000000208020000001216f14d000000104944415408d763fccf00054c30060300132901033bb1691e0000000049454e44ae426082";
    let png: Vec<u8> = (0..PNG.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&PNG[i..i + 2], 16).unwrap())
        .collect();
    let stored = StoredContainer {
        data: Container {
            text: Text::plain("photo.png"),
            streams: vec![
                (StreamType(*b"PNGf"), png),
                (StreamType(*b"SIZE"), vec![0, 3, 0, 2]),
            ],
            path_type: Some("image"),
            ..Container::default()
        },
        intact: true,
        external: true,
    };
    // A hosted container field's value: the file name and the library
    // member, its data not loaded.
    let member = LibraryRef {
        source: None,
        owner: None,
        table: 0x8002,
        member: vec![1],
        md5: [7; 16],
    };
    let reference = Value::Container(Container {
        text: Text::plain("photo.png"),
        library: Some(std::sync::Arc::new(member.clone())),
        ..Container::default()
    });
    let run = |stored: Option<StoredContainer>, expr: &str| {
        let mut host = Library {
            inner: NullHost::new(Locale::en_us(), Date::from_ymd(2026, 10, 28).unwrap()),
            stored,
        };
        host.variables(VarScope::Local)
            .set("$c", 1, reference.clone());
        let result = eval::evaluate_text(expr, &mut host, &Context::default());
        (eval::display(&result.value, &host), result.code())
    };
    let loaded = |expr| run(Some(stored.clone()), expr);
    assert_eq!(
        loaded("GetWidth ( $c ) & \"x\" & GetHeight ( $c )"),
        ("3x2".into(), 0)
    );
    assert_eq!(
        loaded("GetContainerAttribute ( $c ; \"filename\" )"),
        ("photo.png".into(), 0)
    );
    // PNGf 73 + 8, SIZE 4 + 8, FNAM "image" "photo.png" 21 + 8.
    assert_eq!(
        loaded("GetContainerAttribute ( $c ; \"internalSize\" )"),
        ("122".into(), 0)
    );
    assert_eq!(loaded("VerifyContainer ( $c )"), ("1".into(), 0));
    assert_eq!(
        loaded("GetAsText ( GetThumbnail ( $c ; 2 ; 2 ) )"),
        ("photo.png".into(), 0)
    );
    let damaged = StoredContainer {
        intact: false,
        ..stored.clone()
    };
    assert_eq!(
        run(Some(damaged.clone()), "VerifyContainer ( $c )"),
        ("0".into(), 0)
    );
    for formula in ["Base64Encode ( $c )", "Length ( $c )", "GetWidth ( $c )"] {
        assert_eq!(run(Some(damaged.clone()), formula), ("?".into(), 1225));
    }
    // Header-only reads still expose the name; inaccessible streams are
    // never represented as a successful empty or partial data value.
    assert_eq!(
        run(Some(damaged), "GetAsText ( $c )"),
        ("photo.png".into(), 0)
    );
    // Embedded data is not verified (script oracle s34: `?`).
    let embedded = StoredContainer {
        external: false,
        ..stored.clone()
    };
    assert_eq!(
        run(Some(embedded), "VerifyContainer ( $c )"),
        ("?".into(), 0)
    );
    // A host without container data.
    assert_eq!(run(None, "GetWidth ( $c )"), ("?".into(), 1225));
    assert_eq!(run(None, "VerifyContainer ( $c )"), ("?".into(), 1225));
    // Text functions see the file name without loading the data; a member
    // is container data, 0 as a number.
    assert_eq!(run(None, "GetAsText ( $c )"), ("photo.png".into(), 0));
    assert_eq!(run(None, "GetAsNumber ( $c )"), ("0".into(), 0));
    assert_eq!(run(None, "IsEmpty ( $c )"), ("0".into(), 0));
    // Length and the encoders read the data.
    assert_eq!(loaded("Length ( $c )"), ("73".into(), 0));
    assert_eq!(run(None, "Length ( $c )"), ("?".into(), 1225));
    assert_eq!(loaded("HexEncode ( $c )"), (PNG.to_ascii_uppercase(), 0));

    // Data a host read from storage carries its verification; data a
    // calculation holds has none to check.
    let mut host = Library {
        inner: NullHost::new(Locale::en_us(), Date::from_ymd(2026, 10, 28).unwrap()),
        stored: None,
    };
    let read = |intact| {
        Value::Container(Container {
            verified: Some(intact),
            ..stored.data.clone()
        })
    };
    host.variables(VarScope::Local).set("$read", 1, read(true));
    host.variables(VarScope::Local)
        .set("$damaged", 1, read(false));
    host.variables(VarScope::Local)
        .set("$made", 1, Value::Container(stored.data.clone()));
    let verify = |host: &mut Library, expr: &str| {
        let result = eval::evaluate_text(expr, host, &Context::default());
        (eval::display(&result.value, host), result.code())
    };
    assert_eq!(
        verify(&mut host, "VerifyContainer ( $read )"),
        ("1".into(), 0)
    );
    assert_eq!(
        verify(&mut host, "VerifyContainer ( $damaged )"),
        ("0".into(), 0)
    );
    assert_eq!(
        verify(&mut host, "VerifyContainer ( $made )"),
        ("?".into(), 0)
    );
    // The verification is not part of the value: `=` compares name and data.
    assert_eq!(verify(&mut host, "$read = $made"), ("1".into(), 0));
    // Two members compare by their MD5, without their data.
    let other = |md5| {
        Value::Container(Container {
            text: Text::plain("photo.png"),
            library: Some(std::sync::Arc::new(LibraryRef {
                source: None,
                member: vec![2],
                md5,
                ..member.clone()
            })),
            ..Container::default()
        })
    };
    host.variables(VarScope::Local)
        .set("$c", 1, reference.clone());
    host.variables(VarScope::Local)
        .set("$same", 1, other([7; 16]));
    host.variables(VarScope::Local)
        .set("$other", 1, other([8; 16]));
    assert_eq!(verify(&mut host, "$c = $same"), ("1".into(), 0));
    assert_eq!(verify(&mut host, "$c = $other"), ("0".into(), 0));
}

/// Every sample of the battery cut short at many lengths and with bytes
/// changed, under each of its file names, through every analysis function:
/// damaged data gives a result, never a panic.
#[test]
fn damaged_data_never_panics() {
    pdf_worker();
    use om_calc::functions;
    let tsv = std::fs::read_to_string(fixture("fn-container-analysis.en.tsv")).unwrap();
    let mut samples = std::collections::BTreeSet::new();
    for line in tsv.lines() {
        let mut rest = line;
        while let Some(at) = rest.find("Base64Decode ( \"") {
            rest = &rest[at + 16..];
            let data = &rest[..rest.find('"').unwrap()];
            let name = rest[data.len()..].split('"').nth(2).unwrap_or_default();
            samples.insert((data.to_string(), name.to_string()));
        }
    }
    let en = Locale::en_us();
    let cx = om_calc::Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap());
    let decode = functions::lookup("Base64Decode").unwrap();
    let call = |name: &str, args: &[Value]| match functions::lookup(name).unwrap().implementation {
        functions::Implementation::Pure(f) => {
            let _ = f(args, &cx);
        }
        functions::Implementation::Aggregate(_) => unreachable!(),
    };
    for (data, name) in &samples {
        let functions::Implementation::Pure(decode) = decode.implementation else {
            unreachable!()
        };
        let Ok(Value::Container(c)) = decode(&[Value::text(data.clone()), Value::text("x")], &cx)
        else {
            continue;
        };
        let bytes = c.data().to_vec();
        let step = (bytes.len() / 16).max(1);
        let mut variants: Vec<Vec<u8>> = (0..bytes.len())
            .step_by(step)
            .map(|n| bytes[..n].to_vec())
            .collect();
        for at in (0..bytes.len()).step_by(step) {
            let mut v = bytes.clone();
            v[at] ^= 0xff;
            variants.push(v);
        }
        for v in variants {
            let value = Value::Container(Container::file(name.clone(), v));
            for f in [
                "GetWidth",
                "GetHeight",
                "ReadQRCode",
                "GetTextFromPDF",
                "VerifyContainer",
            ] {
                call(f, std::slice::from_ref(&value));
            }
            call(
                "GetContainerAttribute",
                &[value.clone(), Value::text("all")],
            );
            call(
                "GetThumbnail",
                &[value.clone(), Value::number(2), Value::number(2)],
            );
        }
    }
}

/// A host that counts the container data it is asked for and has none.
struct Counting {
    inner: NullHost,
    loads: usize,
}

impl CalcHost for Counting {
    fn locale(&self) -> &Locale {
        self.inner.locale()
    }

    fn today(&self) -> Date {
        self.inner.today()
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        self.inner.variables(scope)
    }

    fn container_data(
        &mut self,
        _container: &Container,
        _context: &Context,
    ) -> Result<StoredContainer, CalcError> {
        self.loads += 1;
        Err(CalcError::NOT_SUPPORTED)
    }
}

#[test]
fn a_referenced_containers_file_name_attribute_reads_no_data() {
    pdf_worker();
    let referenced = |name: &str| {
        Value::Container(Container {
            text: Text::plain(name),
            library: Some(std::sync::Arc::new(LibraryRef {
                source: None,
                owner: None,
                table: 0x8002,
                member: vec![1],
                md5: [7; 16],
            })),
            ..Container::default()
        })
    };
    let run = |value: Value, expr: &str| {
        let mut host = Counting {
            inner: NullHost::new(Locale::en_us(), Date::from_ymd(2026, 10, 30).unwrap()),
            loads: 0,
        };
        host.variables(VarScope::Local).set("$c", 1, value);
        let result = eval::evaluate_text(expr, &mut host, &Context::default());
        (
            eval::display(&result.value, &host),
            result.code(),
            host.loads,
        )
    };
    // The file name a host read with the reference is the answer, however
    // the attribute is spelled.
    for attribute in ["filename", "File Name", "FILENAME"] {
        let expr = format!("GetContainerAttribute ( $c ; \"{attribute}\" )");
        assert_eq!(
            run(referenced("photo.png"), &expr),
            ("photo.png".into(), 0, 0),
            "{attribute}"
        );
    }
    // Every other attribute, and a member without a name, reads the data.
    for (name, expr) in [
        ("photo.png", "GetContainerAttribute ( $c ; \"md5\" )"),
        (
            "photo.png",
            "GetContainerAttribute ( $c ; \"storageType\" )",
        ),
        ("photo.png", "GetContainerAttribute ( $c ; \"all\" )"),
        ("", "GetContainerAttribute ( $c ; \"filename\" )"),
    ] {
        assert_eq!(
            run(referenced(name), expr),
            ("?".into(), 1225, 1),
            "{name:?} {expr}"
        );
    }
    // Data a calculation holds has its name in hand.
    assert_eq!(
        run(
            Value::Container(Container::file("a.txt", b"hello".to_vec())),
            "GetContainerAttribute ( $c ; \"filename\" )"
        ),
        ("a.txt".into(), 0, 0)
    );
}

/// zlib data of `count` copies of `byte`: one literal, then copies of 258
/// bytes from one back, in a fixed Huffman block (RFC 1951 3.2.6); about
/// 1.6 bits a kilobyte, so hundreds of megabytes fit in a few.
fn zlib_run(byte: u8, count: u64) -> Vec<u8> {
    struct Bits(Vec<u8>, u8);
    impl Bits {
        /// `n` bits of `value`, least significant first.
        fn put(&mut self, value: u32, n: u8) {
            for i in 0..n {
                if self.1 == 0 {
                    self.0.push(0);
                }
                if value >> i & 1 == 1 {
                    *self.0.last_mut().unwrap() |= 1 << self.1;
                }
                self.1 = (self.1 + 1) % 8;
            }
        }
        /// A Huffman code, most significant bit first.
        fn code(&mut self, code: u32, n: u8) {
            let reversed = (0..n).fold(0, |r, i| r << 1 | (code >> i & 1));
            self.put(reversed, n);
        }
    }
    let mut bits = Bits(Vec::new(), 0);
    bits.put(1, 1); // the final block
    bits.put(1, 2); // fixed Huffman codes
    bits.code(0x30 + u32::from(byte), 8);
    let mut left = count - 1;
    while left >= 258 {
        bits.code(0b1100_0101, 8); // length 258 (code 285)
        bits.code(0, 5); // distance 1
        left -= 258;
    }
    for _ in 0..left {
        bits.code(0x30 + u32::from(byte), 8);
    }
    bits.code(0, 7); // end of block
    let (m, n, b) = (65521u64, count % 65521, u64::from(byte));
    let a = (1 + n * b) % m;
    let s = (count + b * ((count % (2 * m)) * ((count + 1) % (2 * m)) / 2 % m)) % m;
    let mut out = vec![0x78, 0x01];
    out.extend(bits.0);
    out.extend(((s << 16 | a) as u32).to_be_bytes());
    out
}

/// A one-page PDF whose 100 x 100 point page draws `content`, with the
/// page's resources and further objects (from object 5), and a
/// cross-reference table; or, with `xref` given, a cross-reference stream
/// holding those zlib bytes instead.
fn pdf(content: &[u8], resources: &str, more: &[Vec<u8>], xref: Option<&[u8]>) -> Vec<u8> {
    sized_pdf("[0 0 100 100]", content, resources, more, xref)
}

/// [`pdf`], its page `media_box`.
fn sized_pdf(
    media_box: &str,
    content: &[u8],
    resources: &str,
    more: &[Vec<u8>],
    xref: Option<&[u8]>,
) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {media_box} /Resources {resources} \
             /Contents 4 0 R >>"
        )
        .into_bytes(),
        [content, b""].concat(),
    ];
    objects.extend(more.iter().cloned());
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let at = out.len();
    match xref {
        Some(data) => {
            out.extend(
                format!(
                    "{} 0 obj\n<< /Type /XRef /Size {} /Root 1 0 R /W [1 4 2] \
                     /Filter /FlateDecode /Length {} >>\nstream\n",
                    objects.len() + 1,
                    objects.len() + 2,
                    data.len()
                )
                .as_bytes(),
            );
            out.extend(data);
            out.extend(b"\nendstream\nendobj\n");
        }
        None => {
            out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
            for offset in offsets {
                out.extend(format!("{offset:010} 00000 n \n").as_bytes());
            }
            out.extend(
                format!("trailer\n<< /Size {} /Root 1 0 R >>\n", objects.len() + 1).as_bytes(),
            );
        }
    }
    out.extend(format!("startxref\n{at}\n%%EOF\n").as_bytes());
    out
}

/// A stream object of `data` under `filters` (and other entries).
fn stream(entries: &str, data: &[u8]) -> Vec<u8> {
    [
        format!("<< {entries} /Length {} >>\nstream\n", data.len()).as_bytes(),
        data,
        b"\nendstream",
    ]
    .concat()
}

/// Hostile PDFs (#566) that asked hayro for hundreds of megabytes or more,
/// each read in a worker whose allocations are capped: every reading
/// function answers as for a PDF that does not read, in bounded time,
/// and this process goes on, its next PDF read as usual.
#[test]
fn hostile_pdfs_are_read_in_a_bounded_worker() {
    use om_calc::functions;
    pdf_worker();
    let en = Locale::en_us();
    let cx = om_calc::Ctx::new(&en, Date::from_ymd(2026, 11, 16).unwrap());
    let call = |name: &str, args: &[Value]| match functions::lookup(name).unwrap().implementation {
        functions::Implementation::Pure(f) => f(args, &cx),
        functions::Implementation::Aggregate(_) => unreachable!(),
    };
    let file = |data: Vec<u8>| Value::Container(Container::file("a.pdf", data));
    let blank = b"/Length 0 >>\nstream\n\nendstream";
    let cases: [(&str, Vec<u8>); 4] = [
        // The review's case: 300 MiB of spaces in the page's content.
        (
            "content",
            pdf(
                &stream("/Filter /FlateDecode", &zlib_run(b' ', 300 << 20)),
                "<< >>",
                &[],
                None,
            ),
        ),
        // 64 MiB of ASCII85 `z`, four zero bytes each, behind Flate.
        (
            "ascii85",
            pdf(
                &stream(
                    "/Filter [/FlateDecode /ASCII85Decode]",
                    &zlib_run(b'z', 64 << 20),
                ),
                "<< >>",
                &[],
                None,
            ),
        ),
        // A 1 x 1 fax image whose decoding parameters ask for a gigabyte.
        (
            "ccitt",
            pdf(
                &stream("", b"q 100 0 0 100 0 0 cm /I Do Q"),
                "<< /XObject << /I 5 0 R >> >>",
                &[stream(
                    "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 1 \
                     /ColorSpace /DeviceGray /Filter /CCITTFaxDecode \
                     /DecodeParms << /K -1 /Columns 1073741824 /Rows 1 >>",
                    &[0u8; 16],
                )],
                None,
            ),
        ),
        // A cross-reference stream of 300 MiB, read before any page.
        (
            "xref",
            pdf(
                &[b"<< ", &blank[..]].concat(),
                "<< >>",
                &[],
                Some(&zlib_run(0, 300 << 20)),
            ),
        ),
    ];
    for (case, data) in cases {
        let started = std::time::Instant::now();
        let value = file(data);
        let thumbnail = call(
            "GetThumbnail",
            &[value.clone(), Value::number(50), Value::number(50)],
        );
        let text = call("GetTextFromPDF", std::slice::from_ref(&value));
        let width = call("GetWidth", std::slice::from_ref(&value));
        // Text extraction does not decode images: the fax page has no text.
        let unread: &[(&str, &Result<Value, CalcError>)] = match case {
            "ccitt" => &[("GetThumbnail", &thumbnail)],
            _ => &[("GetThumbnail", &thumbnail), ("GetTextFromPDF", &text)],
        };
        for (function, result) in unread {
            assert!(
                matches!(result, Ok(v) if v.is_invalid()),
                "{case}: {function} gave {result:?}"
            );
        }
        println!("{case}: {:?}", started.elapsed());
        assert!(
            matches!(&width, Ok(Value::Number(_))),
            "{case}: GetWidth gave {width:?}"
        );
        assert!(
            started.elapsed().as_secs() < 100,
            "{case} took {:?}",
            started.elapsed()
        );
    }
    // A page and a box so large that the picture's size overflows: no
    // worker is asked, and nothing overflows here (review of #975).
    let huge = file(sized_pdf(
        "[0 0 4000000000 4000000000]",
        &stream("", b""),
        "<< >>",
        &[],
        None,
    ));
    let thumbnail = call(
        "GetThumbnail",
        &[
            huge,
            Value::number(3_000_000_000_i64),
            Value::number(3_000_000_000_i64),
        ],
    );
    assert!(
        matches!(&thumbnail, Ok(v) if v.is_invalid()),
        "{thumbnail:?}"
    );
    // An ordinary page reads as before.
    let ordinary = file(pdf(
        &stream("", b"BT /F 12 Tf 10 50 Td (Hello) Tj ET"),
        "<< /Font << /F 5 0 R >> >>",
        &[b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()],
        None,
    ));
    let width = call("GetWidth", std::slice::from_ref(&ordinary)).unwrap();
    assert_eq!(support::text(&width, &en), "100");
    let thumbnail = call(
        "GetThumbnail",
        &[ordinary.clone(), Value::number(50), Value::number(50)],
    )
    .unwrap();
    assert!(matches!(thumbnail, Value::Container(_)), "{thumbnail:?}");
}

/// Without a worker the PDF functions fail with error 3; nothing reads
/// the data in this process. (A worker named in the environment that is
/// not there counts as none.)
#[test]
fn without_a_worker_pdf_functions_fail_with_error_3() {
    let missing = std::env::temp_dir().join("om-pdf-worker-that-is-not-there");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "unavailable_worker_child",
            "--nocapture",
            "--include-ignored",
        ])
        .env(om_calc::functions::pdf_worker::WORKER_VARIABLE, &missing)
        .status()
        .unwrap();
    assert!(status.success());
}

/// Run by [`without_a_worker_pdf_functions_fail_with_error_3`] in a process
/// of its own, where no worker is set.
#[test]
#[ignore = "run by without_a_worker_pdf_functions_fail_with_error_3 in its own process"]
fn unavailable_worker_child() {
    use om_calc::functions;
    let en = Locale::en_us();
    let cx = om_calc::Ctx::new(&en, Date::from_ymd(2026, 11, 16).unwrap());
    let value = Value::Container(Container::file(
        "a.pdf",
        pdf(b"<< /Length 0 >>\nstream\n\nendstream", "<< >>", &[], None),
    ));
    for name in ["GetTextFromPDF", "GetWidth", "GetHeight"] {
        let functions::Implementation::Pure(f) = functions::lookup(name).unwrap().implementation
        else {
            unreachable!()
        };
        assert_eq!(
            f(std::slice::from_ref(&value), &cx).unwrap_err().code(),
            3,
            "{name}"
        );
    }
}
