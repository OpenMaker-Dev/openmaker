//! SVG pictures in the worker process (`om_gui::report::svg_worker`), for
//! reports and for Browse (`om_gui::svg_loader`): an ordinary picture comes
//! back drawn at the size asked for (or its own), and SVGs that recurse,
//! clone or allocate without bound before any check could stop them fail
//! in the worker, within its memory cap and deadline, while this process
//! carries on.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::load::{ImagePoll, SizeHint};
use om_gui::report::svg_worker::{draw_with, Unsupported, DEADLINE};

fn worker() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_om-gui-pdf-worker"))
}

fn svg(body: &str) -> Vec<u8> {
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256">{body}</svg>"#)
        .into_bytes()
}

#[test]
fn the_worker_draws_ordinary_svg_and_contains_what_has_no_bound() {
    // Red above blue, at the size asked for and at the SVG's own (288 dpi).
    let stripes = br#"<svg xmlns="http://www.w3.org/2000/svg" width="28" height="10"><rect width="28" height="5" fill="red"/><rect y="5" width="28" height="5" fill="blue"/></svg>"#;
    let (w, h, rgba) = draw_with(worker(), stripes, Some([56, 20]), Unsupported::Refuse).unwrap();
    assert_eq!((w, h, rgba.len()), (56, 20, 56 * 20 * 4));
    assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
    assert_eq!(&rgba[rgba.len() - 4..], &[0, 0, 255, 255]);
    let (w, h, _) = draw_with(worker(), stripes, None, Unsupported::Refuse).unwrap();
    assert_eq!((w, h), (112, 40));
    // A refusal's reason comes back.
    let error = draw_with(
        worker(),
        &svg(r#"<image href="/x.png" width="9" height="9"/>"#),
        None,
        Unsupported::Refuse,
    )
    .unwrap_err();
    assert!(error.contains("images"), "{error}");
    // Past a report picture's 4 MiB it is refused before a worker starts.
    let large = svg(&" ".repeat(5 << 20));
    let started = Instant::now();
    let error =
        draw_with(Path::new("/nonexistent"), &large, None, Unsupported::Refuse).unwrap_err();
    assert!(error.contains("exceeds"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(1));

    let clip_cycle = svg(
        r#"<clipPath id="a"><rect width="1" height="1" clip-path="url(#b)"/></clipPath>
           <clipPath id="b"><rect width="1" height="1" clip-path="url(#c)"/></clipPath>
           <clipPath id="c"><rect width="1" height="1" clip-path="url(#a)"/></clipPath>
           <rect width="9" height="9" clip-path="url(#a)"/>"#,
    );
    let pattern_clones = svg(&{
        let mut body = String::new();
        for level in 0..8 {
            body += &format!(r#"<pattern id="p{level}" width="1" height="1">"#);
            body += &format!(
                r#"<rect width="1" height="1" fill="url(#p{})"/>"#,
                level + 1
            )
            .repeat(20);
            body += "</pattern>";
        }
        body + &r#"<rect width="9" height="9" fill="url(#p0)"/>"#.repeat(20)
    });
    let filter_region = svg(
        r#"<filter id="f" filterUnits="userSpaceOnUse" x="-1e6" y="-1e6" width="2e6" height="2e6"><feFlood/></filter>
           <g filter="url(#f)"><rect width="9" height="9"/></g>"#,
    );
    for (name, data) in [
        ("clip-path cycle", clip_cycle),
        ("pattern clones", pattern_clones),
        ("filter region", filter_region),
    ] {
        let started = Instant::now();
        let error =
            draw_with(worker(), &data, Some([1024, 1024]), Unsupported::Refuse).unwrap_err();
        assert!(
            started.elapsed() < DEADLINE + Duration::from_secs(5),
            "{name}: {:?}",
            started.elapsed()
        );
        eprintln!("{name}: refused in {:?}: {error}", started.elapsed());
    }
}

/// Browse draws a layout's SVG in the worker (#303): what reports refuse
/// draws, less what the worker never reads, and what has no bound fails
/// there, while the window carries on.
#[test]
fn browse_draws_svg_in_the_worker() {
    om_gui::report::svg_worker::set_default(worker().to_path_buf());
    let ctx = egui::Context::default();
    om_gui::svg_loader::install(&ctx);
    let load = |name: &str, data: Vec<u8>, size: [u32; 2]| {
        let uri = format!("bytes://test/{name}.svg");
        ctx.include_bytes(uri.clone(), data);
        let hint = SizeHint::Size {
            width: size[0],
            height: size[1],
            maintain_aspect_ratio: false,
        };
        let started = Instant::now();
        loop {
            match ctx.try_load_image(&uri, hint) {
                Ok(ImagePoll::Ready { image }) => break Ok(image),
                Ok(ImagePoll::Pending { .. }) => {
                    assert!(
                        started.elapsed() < DEADLINE + Duration::from_secs(5),
                        "{name}"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => break Err(error.to_string()),
            }
        }
    };
    let pixel = |image: &Arc<egui::ColorImage>, x: usize, y: usize| {
        image.pixels[y * image.size[0] + x].to_srgba_unmultiplied()
    };

    // Illustrator declares its namespaces as entities: Browse draws them
    // (reports refuse them). An image the SVG names is never read, here a
    // red SVG on the local disk.
    let red = tempfile::NamedTempFile::with_suffix(".svg").unwrap();
    std::fs::write(
        red.path(),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="red"/></svg>"#,
    )
    .unwrap();
    let illustrator = format!(
        r#"<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY ns_svg "http://www.w3.org/2000/svg">]><svg xmlns="&ns_svg;" width="28" height="10"><rect width="28" height="10" fill="blue"/><image href="{}" width="28" height="10"/></svg>"#,
        red.path().display()
    );
    let image = load("illustrator", illustrator.clone().into_bytes(), [56, 20]).unwrap();
    assert_eq!(image.size, [56, 20]);
    assert!(
        image
            .pixels
            .iter()
            .all(|p| p.to_srgba_unmultiplied() == [0, 0, 255, 255]),
        "blue, with no red from the disk"
    );
    assert!(draw_with(worker(), illustrator.as_bytes(), None, Unsupported::Refuse).is_err());
    // A raster larger than the worker draws is drawn smaller, in proportion.
    let stripes = br#"<svg xmlns="http://www.w3.org/2000/svg" width="28" height="10"><rect width="28" height="5" fill="red"/><rect y="5" width="28" height="5" fill="blue"/></svg>"#;
    let large = load("stripes", stripes.to_vec(), [40_000, 2_000]).unwrap();
    assert_eq!(large.size, [2048, 102]);
    assert_eq!(pixel(&large, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&large, 2047, 101), [0, 0, 255, 255]);

    // The cases #303 names, each a few KiB: a marker copied at each of
    // 200,000 vertices, a 255-level entity expansion, a pattern tile of
    // 100,000 units square, and a chain of masks over the canvas.
    let marker = svg(&format!(
        r#"<marker id="m" markerWidth="9" markerHeight="9">{}</marker><path marker-mid="url(#m)" d="M0 0{}"/>"#,
        r#"<rect width="1" height="1"/>"#.repeat(1_000),
        "l1 0".repeat(200_000)
    ));
    let entities = format!(
        r#"<!DOCTYPE svg [<!ENTITY e0 "aaaaaaaaaaaaaaaa">{}]><svg xmlns="http://www.w3.org/2000/svg" width="9" height="9"><text>&e254;</text></svg>"#,
        (1..255)
            .map(|n| format!(r#"<!ENTITY e{n} "&e{p};&e{p};">"#, p = n - 1))
            .collect::<String>()
    )
    .into_bytes();
    let pattern = svg(
        r#"<pattern id="p" patternUnits="userSpaceOnUse" width="100000" height="100000"><rect width="9" height="9"/></pattern><rect width="9" height="9" fill="url(#p)"/>"#,
    );
    let masks = svg(&format!(
        "{}<rect width=\"256\" height=\"256\" mask=\"url(#m2999)\"/>",
        (0..3_000)
            .map(|n| {
                let inner = if n == 0 {
                    String::new()
                } else {
                    format!(r#" mask="url(#m{})""#, n - 1)
                };
                format!(
                    r#"<mask id="m{n}"><rect width="256" height="256" fill="white"{inner}/></mask>"#
                )
            })
            .collect::<String>()
    ));
    for (name, data) in [
        ("marker", marker),
        ("entities", entities),
        ("pattern", pattern),
        ("masks", masks),
    ] {
        let started = Instant::now();
        let error = load(name, data, [1024, 1024]).unwrap_err();
        eprintln!("{name}: refused in {:?}: {error}", started.elapsed());
    }
}
