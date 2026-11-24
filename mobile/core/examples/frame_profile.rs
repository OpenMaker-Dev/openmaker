//! Time the frames the engine makes while a pan scrolls a file, as the apps
//! drive it: one pan move per frame taken (docs/performance/mobile-frames.md).
//!
//! The file comes from the environment, so that no server, file or account
//! is written down: GO_PROFILE_PATH=<local .fmp12>, or GO_PROFILE_HOST,
//! GO_PROFILE_PORT (5003), GO_PROFILE_FILE and GO_PROFILE_LICENSE; then
//! GO_PROFILE_ACCOUNT and GO_PROFILE_PASSWORD. GO_PROFILE_SIZE=<w>x<h> in
//! points (1317x700), GO_PROFILE_SCALE (1.75) and GO_PROFILE_FRAMES (120).
use std::sync::Arc;
use std::time::{Duration, Instant};

use openmaker_go::{Document, DocumentObserver, Frame, Host, Input, SignIn, Source, Viewport};

struct Headless;

impl DocumentObserver for Headless {
    fn frame_ready(&self) {}
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn report(name: &str, mut values: Vec<f64>, unit: &str) {
    if values.is_empty() {
        return;
    }
    values.sort_by(f64::total_cmp);
    let at = |q: f64| values[((values.len() - 1) as f64 * q).round() as usize];
    println!(
        "{name:>14}: median {:8.2} {unit}  p90 {:8.2} {unit}  max {:8.2} {unit}",
        at(0.5),
        at(0.9),
        at(1.0)
    );
}

fn next_frame(doc: &Document, within: Duration) -> Option<Frame> {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if let Some(frame) = doc.take_frame() {
            return Some(frame);
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    None
}

fn main() {
    let (width, height) = env("GO_PROFILE_SIZE")
        .and_then(|size| {
            let (w, h) = size.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((1317.0, 700.0));
    let scale = env("GO_PROFILE_SCALE").map_or(1.75, |s| s.parse().unwrap());
    let frames: usize = env("GO_PROFILE_FRAMES").map_or(120, |s| s.parse().unwrap());
    let source = match env("GO_PROFILE_PATH") {
        Some(path) => Source::Local { path },
        None => Source::Hosted {
            host: Host {
                address: env("GO_PROFILE_HOST").expect("GO_PROFILE_HOST or GO_PROFILE_PATH"),
                port: env("GO_PROFILE_PORT").map_or(5003, |p| p.parse().unwrap()),
                certificate: String::new(),
            },
            file: env("GO_PROFILE_FILE").expect("GO_PROFILE_FILE"),
        },
    };
    let sign_in = SignIn {
        account: env("GO_PROFILE_ACCOUNT").unwrap_or_default(),
        password: env("GO_PROFILE_PASSWORD").unwrap_or_default(),
        license: env("GO_PROFILE_LICENSE").unwrap_or_default(),
    };
    let viewport = Viewport {
        width,
        height,
        scale,
        max_texture_side: 8192,
    };
    let started = Instant::now();
    let doc = match Document::open(source, sign_in, viewport, Arc::new(Headless)) {
        Ok(doc) => doc,
        Err(error) => {
            eprintln!("open failed: {error}");
            std::process::exit(1);
        }
    };
    println!("opened in {:.2} s", started.elapsed().as_secs_f64());
    // Settle: records, portals and pictures arrive.
    let settle = Instant::now();
    while next_frame(&doc, Duration::from_secs(2)).is_some()
        && settle.elapsed() < Duration::from_secs(60)
    {}
    println!("settled in {:.2} s", settle.elapsed().as_secs_f64());

    doc.send(Input::PanBegan {
        x: width / 2.0,
        y: height / 2.0,
    });
    // A frame the pan's start made, if it made one, answers no move: each
    // move below waits for its own. A layout that keeps drawing (a timer
    // script) is waited for no longer than a second; frames carry no input
    // id, so a frame it draws can still be counted as a move's.
    let drain = Instant::now();
    while drain.elapsed() < Duration::from_secs(1)
        && next_frame(&doc, Duration::from_millis(200)).is_some()
    {}
    let (mut pass, mut tessellate, mut pack, mut latency) = (vec![], vec![], vec![], vec![]);
    let (mut vertices, mut bytes) = (0, vec![]);
    for i in 0..frames {
        let dy = if (i / 30) % 2 == 0 { -24.0 } else { 24.0 };
        let sent = Instant::now();
        doc.send(Input::PanMoved { dx: 0.0, dy });
        let Some(frame) = next_frame(&doc, Duration::from_secs(5)) else {
            println!("no frame for pan move {i}");
            continue;
        };
        latency.push(sent.elapsed().as_secs_f64() * 1000.0);
        pass.push(f64::from(frame.timings.pass) / 1000.0);
        tessellate.push(f64::from(frame.timings.tessellate) / 1000.0);
        pack.push(f64::from(frame.timings.pack) / 1000.0);
        vertices = frame.vertices.len() / 20;
        let texture_bytes: usize = frame.textures.iter().map(|t| t.pixels.len()).sum();
        bytes.push((frame.vertices.len() + frame.indices.len() + texture_bytes) as f64 / 1024.0);
    }
    doc.send(Input::PanEnded { vx: 0.0, vy: 0.0 });
    println!(
        "{} pan frames at {width}x{height} points, {scale} pixels a point; {vertices} vertices",
        latency.len()
    );
    report("pass", pass, "ms");
    report("tessellate", tessellate, "ms");
    report("pack", pack, "ms");
    report("move to frame", latency, "ms");
    report("frame size", bytes, "KiB");
    if !doc.close(10_000) {
        eprintln!("the file did not close");
    }
}
