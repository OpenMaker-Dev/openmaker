//! Report SVG pictures in the worker process (`om_gui::report::svg_worker`):
//! an ordinary picture comes back drawn at the size asked for (or its own),
//! and SVGs that recurse, clone or allocate without bound before any check
//! could stop them fail in the worker, within its memory cap and deadline,
//! while this process carries on.

use std::path::Path;
use std::time::{Duration, Instant};

use om_gui::report::svg_worker::{draw_with, DEADLINE};

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
    let (w, h, rgba) = draw_with(worker(), stripes, Some([56, 20])).unwrap();
    assert_eq!((w, h, rgba.len()), (56, 20, 56 * 20 * 4));
    assert_eq!(&rgba[..4], &[255, 0, 0, 255]);
    assert_eq!(&rgba[rgba.len() - 4..], &[0, 0, 255, 255]);
    let (w, h, _) = draw_with(worker(), stripes, None).unwrap();
    assert_eq!((w, h), (112, 40));
    // A refusal's reason comes back.
    let error = draw_with(
        worker(),
        &svg(r#"<image href="/x.png" width="9" height="9"/>"#),
        None,
    )
    .unwrap_err();
    assert!(error.contains("images"), "{error}");

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
        let error = draw_with(worker(), &data, Some([1024, 1024])).unwrap_err();
        assert!(
            started.elapsed() < DEADLINE + Duration::from_secs(5),
            "{name}: {:?}",
            started.elapsed()
        );
        eprintln!("{name}: refused in {:?}: {error}", started.elapsed());
    }
}
