//! The other viewports' native windows in eframe's real event loop under an
//! X server (skipped without one): the assumptions `ViewportWindows` rests
//! on hold for the eframe and winit versions the app uses.

use std::sync::{Arc, Mutex};

use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

use super::*;

const TITLES: [&str; 2] = ["om viewport one", "om viewport two"];
/// The frame on which viewport one's builder changes so that eframe
/// recreates its window, and the frame the test ends on.
const RECREATE: u32 = 3;
const LAST: u32 = 6;

type Row = Vec<Result<NativeParent, Unavailable>>;

#[derive(Default)]
struct Seen {
    rows: Vec<Row>,
    /// At the last frame: each found window's `WM_NAME`, and whether the
    /// window found before the recreation still exists.
    titles: Vec<Option<String>>,
    first_alive: Option<bool>,
}

fn x11(parent: &Result<NativeParent, Unavailable>) -> Option<u32> {
    match parent {
        Ok(NativeParent::X11(w)) => u32::try_from(*w).ok(),
        Err(_) => None,
    }
}

/// Each immediate viewport gets an X11 window of its own, made by eframe's
/// connection and titled as the viewport, not the root's; a window eframe
/// recreates for a builder change is replaced by the new one, and the other
/// viewport keeps its own.
#[test]
fn immediate_viewports_get_their_own_x11_windows() {
    let Some(display) = std::env::var_os("DISPLAY") else {
        eprintln!("skipped: no X display");
        return;
    };
    // A display set but gone is the environment's fault, as when
    // `xvfb-run -a` gave it to two jobs and the other one stopped it.
    if let Err(e) = x11rb::connect(None) {
        panic!(
            "the X display {display:?} cannot be opened ({e}): run under \
             tools/xvfb-run.sh (docs/development.md, Checks)"
        );
    }
    let seen = Arc::new(Mutex::new(Seen::default()));
    let record = Arc::clone(&seen);
    let mut windows = ViewportWindows::default();
    let mut frame = 0u32;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("om viewport root")
            .with_inner_size([200.0, 120.0]),
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::x11::EventLoopBuilderExtX11 as _;
            // X11 even where Wayland is also offered (XWayland).
            builder.with_x11().with_any_thread(true);
        })),
        ..Default::default()
    };
    eframe::run_ui_native("om-gui viewport windows", options, move |ui, native| {
        let root = NativeParent::of(native);
        windows.begin(root, true);
        let mut row = vec![root];
        for (id, title) in [(1u32, TITLES[0]), (2, TITLES[1])] {
            let mut builder = egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([160.0, 100.0]);
            if id == 1 && frame >= RECREATE {
                builder = builder.with_minimize_button(false);
            }
            windows.before_show();
            let mut parent = Err(Unavailable::Window);
            ui.ctx().show_viewport_immediate(
                egui::ViewportId::from_hash_of(("om viewport test", id)),
                builder,
                |_, _| parent = windows.parent(id, title),
            );
            row.push(parent);
        }
        let mut seen = record.lock().expect("not poisoned");
        if frame == LAST {
            let (conn, _) = x11rb::connect(None).expect("the display");
            let name = |w: u32| {
                let reply = conn
                    .get_property(false, w, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 64)
                    .ok()?
                    .reply()
                    .ok()?;
                String::from_utf8(reply.value).ok()
            };
            seen.titles = row[1..].iter().map(|p| x11(p).and_then(name)).collect();
            let first = seen
                .rows
                .get(RECREATE as usize - 1)
                .and_then(|r| x11(&r[1]));
            seen.first_alive = first.map(|w| {
                conn.get_window_attributes(w)
                    .is_ok_and(|c| c.reply().is_ok())
            });
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            ui.ctx().request_repaint();
        }
        seen.rows.push(row);
        frame += 1;
    })
    .expect("eframe runs");

    let seen = seen.lock().expect("not poisoned");
    let rows = &seen.rows;
    assert!(rows.len() > LAST as usize, "{rows:?}");
    let root = x11(&rows[0][0]).expect("the root viewport's X11 window");
    assert!(rows.iter().all(|r| x11(&r[0]) == Some(root)), "{rows:?}");
    // Before the recreation: two windows of their own, the same each frame,
    // once eframe has made them.
    let before = &rows[RECREATE as usize - 1];
    let (one, two) = (x11(&before[1]), x11(&before[2]));
    let (Some(one), Some(two)) = (one, two) else {
        panic!("both viewports found by frame {}: {rows:?}", RECREATE - 1);
    };
    assert!(one != two && one != root && two != root, "{rows:?}");
    for r in rows[..RECREATE as usize].iter().filter(|r| r[1].is_ok()) {
        assert_eq!((x11(&r[1]), x11(&r[2])), (Some(one), Some(two)), "{rows:?}");
    }
    // After it: viewport one's new window, viewport two's own.
    for r in &rows[RECREATE as usize..] {
        let new = x11(&r[1]).expect("the recreated window found");
        assert!(new != one && new != two && new != root, "{rows:?}");
        assert_eq!(x11(&r[2]), Some(two), "{rows:?}");
    }
    assert_eq!(
        seen.first_alive,
        Some(false),
        "the first window was recreated"
    );
    assert_eq!(
        seen.titles,
        TITLES.map(|t| Some(t.to_owned())).to_vec(),
        "each window titled as its viewport"
    );
}
