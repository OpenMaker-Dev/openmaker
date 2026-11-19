//! Shared helpers for driving [`OpenMakerApp`] headlessly with egui_kittest.

#![allow(dead_code)] // Each test binary uses a different subset.

pub mod workspace;

#[path = "../../src/test_renderer.rs"]
mod test_renderer;
pub use test_renderer::renderer;

use std::thread;
use std::time::{Duration, Instant};

use egui::Role;
use egui_kittest::kittest::{By, Queryable};
use egui_kittest::Harness;
use om_data::{
    mock::MockProvider, Connection, DataError, DataProvider, GeomRect, HostedFile, LayoutGeometry,
    LayoutInfo, ObjectGeom, PartGeom, RecordPage,
};
use om_gui::OpenMakerApp;

pub type App = Harness<'static, OpenMakerApp>;

/// A harness sized like the default window (FileMaker's document window in
/// the reference captures is 1060 pt wide).
pub fn harness(provider: Box<dyn DataProvider>) -> App {
    harness_sized(provider, egui::vec2(1060.0, 760.0))
}

/// A harness of `size`: a taller one shows a long Scripts menu whole (the
/// menu's items past the screen's bottom are not drawn, nor reachable).
pub fn harness_sized(provider: Box<dyn DataProvider>, size: egui::Vec2) -> App {
    use_svg_worker();
    Harness::builder()
        .with_size(size)
        .renderer(renderer())
        .build_eframe(move |_cc| OpenMakerApp::new(provider))
}

/// Draw SVG pictures and icons with this package's worker, as the app
/// draws them beside its executable (`om_gui::svg_loader`); without it
/// they show egui's error mark.
pub fn use_svg_worker() {
    om_gui::report::svg_worker::set_default(env!("CARGO_BIN_EXE_om-gui-pdf-worker").into());
}

pub fn mock() -> App {
    harness(Box::new(MockProvider::new()))
}

/// Step the app until `done` holds. Provider replies arrive from another
/// thread, so this sleeps between frames rather than spinning.
pub fn wait_until(app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    // Generous: under a full parallel test run a provider round trip has
    // taken over 10 s; a passing wait returns as soon as `done` holds.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        app.step();
        if done(app) {
            // Let layout settle (column widths, scroll areas) before callers
            // look at or render the frame.
            app.run_steps(3);
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Step until a node with exactly this label exists.
pub fn wait_for_label(app: &mut App, label: &str) {
    wait_until(app, label, |app| app.query_by_label(label).is_some());
}

pub fn wait_for_value(app: &mut App, value: &str) {
    wait_until(app, value, |app| {
        app.query(By::new().role(Role::TextInput).value(value))
            .is_some()
    });
}

/// The text or password field labelled `label`, if it is on screen: for
/// conditions polled by [`wait_until`], which must not panic while the
/// worker's answer is still on its way.
pub fn find_text_field<'a>(app: &'a App, label: &'a str) -> Option<egui_kittest::Node<'a>> {
    app.query_by_role_and_label(Role::TextInput, label)
        .or_else(|| app.query_by_role_and_label(Role::PasswordInput, label))
}

/// The text or password field labelled `label`.
pub fn text_field<'a>(app: &'a App, label: &'a str) -> egui_kittest::Node<'a> {
    app.query_by_role_and_label(Role::TextInput, label)
        .unwrap_or_else(|| app.get_by_role_and_label(Role::PasswordInput, label))
}

/// A native absolute file path as a `file:` path list line of this
/// platform, for typing where FileMaker takes a path list (script-runtime.md,
/// "Data files and paths"), as `scripting::files::filemaker_path` writes it:
/// a macOS path starts with its volume's name (a mounted volume's, or the
/// startup disk's, the one under `/Volumes` that links to `/`), a Windows
/// one with its drive or, for a UNC path, `//computer/share`.
pub fn file_line(path: &std::path::Path) -> String {
    let text = path.to_str().expect("a UTF-8 path");
    if cfg!(target_os = "macos") {
        if let Some(mounted) = text.strip_prefix("/Volumes/") {
            return format!("file:/{mounted}");
        }
        let volume = std::fs::read_dir("/Volumes")
            .into_iter()
            .flatten()
            .flatten()
            .find(|e| std::fs::read_link(e.path()).is_ok_and(|t| t == std::path::Path::new("/")))
            .map_or_else(
                || "Macintosh HD".to_owned(),
                |e| e.file_name().to_string_lossy().into_owned(),
            );
        format!("file:/{volume}{text}")
    } else if cfg!(windows) {
        let text = text.replace('\\', "/");
        match text.strip_prefix("//") {
            Some(unc) => format!("file://{unc}"),
            None => format!("file:/{text}"),
        }
    } else {
        format!("file:{text}")
    }
}

/// Type into the text field labelled `label`.
pub fn fill(app: &mut App, label: &str, text: &str) {
    text_field(app, label).focus();
    app.step();
    text_field(app, label).type_text(text);
    app.run_steps(2);
}

/// Commit the window's record as a user does without leaving it:
/// Ctrl+Enter (Cmd+Enter on macOS), as the removed "Save Record" button did
/// (#871).
pub fn save_record(app: &mut App) {
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    app.run_steps(2);
}

/// Records > Revert Record..., confirmed in its dialog (#871), and
/// waited for: the menu runs Revert Record/Request (51) as a script step,
/// which reverts on the worker.
pub fn revert_record(app: &mut App) {
    open_menu(app, "Records");
    click(app, "Revert Record...");
    wait_for_label(app, om_data::scripts::revert_record::MESSAGE);
    click(app, "Revert");
    wait_until(app, "the revert", |app| {
        !app.state().script_running() && !editing(app)
    });
}

/// The record has changes not committed and no save of it is in flight
/// (what the removed bar read as "Unsaved record changes").
pub fn unsaved(app: &App) -> bool {
    app.state().record_edit_state() == Some(false)
}

/// A save of the record is in flight ("Saving record…").
pub fn saving(app: &App) -> bool {
    app.state().record_edit_state() == Some(true)
}

/// Either: the removed bar, and its Save Record button, would show.
pub fn editing(app: &App) -> bool {
    app.state().record_edit_state().is_some()
}

pub fn click(app: &mut App, label: &str) {
    // A control that shows once data arrives, as a script in the Scripts
    // menu does: the menu reads the file's catalog when first opened
    // (#1053). One already shown is clicked at once.
    if app.query_by_label(label).is_none() {
        wait_for_label(app, label);
    }
    app.get_by_label(label).click();
    app.run_steps(2);
}

/// From launch: fill in the server in the Hosts dialog, list its files, pick
/// one, and open it, waiting until its first layout is on screen. That is
/// "Contact Details", which Form view draws from the mock's geometry (parts
/// and object boxes, no record values).
pub fn connect_to_mock(app: &mut App) {
    fill(app, "Host", "fms.example.com");
    fill(app, "Account", "admin");
    fill(app, "Password", "secret");
    click(app, "List files");
    wait_for_label(app, "Contacts.fmp12");
    click(app, "Contacts.fmp12");
    click(app, "Open");
    wait_for_label(app, "Body");
}

/// The position shown in the status toolbar's record number box.
pub fn record_number(app: &App) -> String {
    text_field(app, "Record Number").value().unwrap_or_default()
}

/// The record number once the current layout's page has loaded. Right after
/// a layout change its label can be drawn before the page arrives, while the
/// record number is still empty.
pub fn loaded_record_number(app: &mut App) -> String {
    wait_until(app, "record number", |app| {
        find_text_field(app, "Record Number")
            .and_then(|field| field.value())
            .is_some_and(|number| !number.is_empty())
    });
    record_number(app)
}

/// Switch layouts with the layout bar's popup.
pub fn select_layout(app: &mut App, layout: &str) {
    click(app, "Layout");
    click(app, layout);
}

/// Open the menu-bar menu `menu` (a button, unlike same-named captions).
/// The window rendered offscreen, with no pointer drawn.
pub fn rendered(app: &mut App) -> image::RgbaImage {
    app.run_steps(2);
    app.set_render_cursor(false);
    app.render().expect("offscreen render")
}

/// Whether `card` is painted in `after`, the window rendered with the
/// card open, over `before`, rendered just before it opened: outside the
/// card and its shadow, its parent's layout area dimmed when the card dims
/// it; inside, not its parent as it would show there. It reads pixels, not
/// the accessibility tree, which has a card's controls whether they are
/// painted or not (#865). `Err` says what is not painted.
pub fn card_painted(
    before: &image::RgbaImage,
    after: &image::RgbaImage,
    pixels_per_point: f32,
    card: &om_gui::CardFrame,
) -> Result<(), String> {
    // As the window dims, in gamma space: black at alpha 96 over the
    // parent (`window_styles.rs`, `DIM`), so 255 shows as 159.
    let dimmed = |c: u8| (f32::from(c) * 159.0 / 255.0).round();
    let like = |shown: [u8; 4], parent: [u8; 4]| {
        (0..3).all(|i| {
            let parent = if card.dims {
                dimmed(parent[i])
            } else {
                f32::from(parent[i])
            };
            (f32::from(shown[i]) - parent).abs() <= 6.0
        })
    };
    let shadow = card.frame.expand(24.0);
    let inner = card.frame.shrink(2.0);
    let (mut outside, mut outside_dimmed, mut inside, mut inside_parent) = (0, 0, 0, 0);
    // Every fourth point of the parent's layout area.
    let area = card.parent_area;
    let mut y = area.top() + 2.0;
    while y < area.bottom() {
        let mut x = area.left() + 2.0;
        while x < area.right() {
            let point = egui::pos2(x, y);
            let (px, py) = ((x * pixels_per_point) as u32, (y * pixels_per_point) as u32);
            x += 4.0;
            if px >= before.width().min(after.width()) || py >= before.height().min(after.height())
            {
                continue;
            }
            let parent = before.get_pixel(px, py).0;
            let shown = after.get_pixel(px, py).0;
            if inner.contains(point) {
                inside += 1;
                inside_parent += usize::from(like(shown, parent));
            } else if card.dims && !shadow.contains(point) {
                outside += 1;
                outside_dimmed += usize::from(like(shown, parent));
            }
        }
        y += 4.0;
    }
    if outside_dimmed * 10 < outside * 9 {
        return Err(format!(
            "card {:?}: its parent is not dimmed ({outside_dimmed} of {outside} points)",
            card.name
        ));
    }
    if inside == 0 || inside_parent * 2 > inside {
        return Err(format!(
            "card {:?} is not painted: {inside_parent} of {inside} points show its parent",
            card.name
        ));
    }
    Ok(())
}

pub fn open_menu(app: &mut App, menu: &str) {
    app.get_by_role_and_label(Role::Button, menu).click();
    app.run_steps(2);
}

/// A provider whose open always fails, as with bad credentials.
pub struct FailingProvider;

impl DataProvider for FailingProvider {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Err(DataError::new("Connection refused (fms.example.com:5003)"))
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Err(DataError::new(
            "The account name and password you entered could not be authenticated.",
        ))
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Err(DataError::new("not connected"))
    }
    fn records(&mut self, _: u32, _: u64, _: u32) -> Result<RecordPage, DataError> {
        Err(DataError::new("not connected"))
    }
    fn close(&mut self) {}
}

/// A provider whose open fails with its message.
pub struct FailingWith(pub String);

impl DataProvider for FailingWith {
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Err(DataError::new("not connected"))
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Err(DataError::new(self.0.clone()))
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Err(DataError::new("not connected"))
    }
    fn records(&mut self, _: u32, _: u64, _: u32) -> Result<RecordPage, DataError> {
        Err(DataError::new("not connected"))
    }
    fn close(&mut self) {}
}

/// The mock, except that every layout's geometry is far larger than the
/// window: a 2000 x 1500 pt Body with an object labelled "Corner" in its
/// bottom-right corner.
pub struct LargeLayoutProvider(pub MockProvider);

impl DataProvider for LargeLayoutProvider {
    fn list_files(&mut self, host: &str, port: u16) -> Result<Vec<HostedFile>, DataError> {
        self.0.list_files(host, port)
    }
    fn open(&mut self, connection: &Connection) -> Result<(), DataError> {
        self.0.open(connection)
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        self.0.layouts()
    }
    fn records(&mut self, layout: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        self.0.records(layout, offset, limit)
    }
    fn layout_geometry(&mut self, _: u32) -> Result<Option<LayoutGeometry>, DataError> {
        Ok(Some(LayoutGeometry {
            revision: None,
            width: 2000.0,
            height: 1500.0,
            parts: vec![PartGeom {
                kind_name: "Body".into(),
                top: 0.0,
                height: 1500.0,
            }],
            objects: vec![ObjectGeom {
                kind_name: "Corner".into(),
                rect: GeomRect {
                    top: 1460.0,
                    left: 1900.0,
                    bottom: 1490.0,
                    right: 1990.0,
                },
                field_name: None,
                ..Default::default()
            }],
        }))
    }
    fn close(&mut self) {
        self.0.close();
    }
}
