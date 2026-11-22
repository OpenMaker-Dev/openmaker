//! The Insert commands' file panel in the real app (#50): eframe's own
//! event loop and window, with the Tasks file of
//! `om-server/tests/fixtures/tasks.rs` hosted by an in-process server.
//!
//! The file's OnFirstWindowOpen script runs Insert File [Tasks::Photo] with
//! no path. On macOS the system's open panel asks for the file, titled
//! "Insert File", in place of the in-app dialog, while the script waits on
//! its prompt. The panel is the system's (its contents run in another
//! process on current macOS), so the test does not answer it: what the
//! panel answers reaches the step as the in-app dialog's answer does.
//!
//! `cargo test -p om-gui --test insert_panel_native` opens a window and the
//! panel on macOS; elsewhere it does nothing. No license store is read.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("skipped: the open panel is macOS's");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod common;
#[cfg(target_os = "macos")]
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[cfg(target_os = "macos")]
mod hosted;

#[cfg(target_os = "macos")]
mod macos {
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSOpenPanel};
    use om_data::DataError;
    use om_gui::licensing::{LicenseKey, LicenseStore};
    use om_gui::OpenMakerApp;
    use om_hbam::OwnedNode;
    use om_scripts::{FieldSpec, ScriptStep, StoredKey};

    use super::{fixture, hosted};

    fn fail(message: String) -> ! {
        eprintln!("FAIL: {message}");
        std::process::exit(1);
    }

    /// No license: these loopback servers need none, and the test reads no
    /// credential store.
    struct NoLicense;

    impl LicenseStore for NoLicense {
        fn load(&mut self) -> Result<Option<LicenseKey>, DataError> {
            Ok(None)
        }
        fn save(&mut self, _: &LicenseKey) -> Result<(), DataError> {
            Ok(())
        }
        fn remove(&mut self) -> Result<(), DataError> {
            Ok(())
        }
    }

    /// The app's visible open panel, if one shows.
    fn open_panel() -> Option<Retained<NSOpenPanel>> {
        let mtm = MainThreadMarker::new()?;
        NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .filter(|window| window.isVisible())
            .find_map(|window| window.downcast::<NSOpenPanel>().ok())
    }

    #[derive(Debug, PartialEq)]
    enum Phase {
        Opening,
    }

    struct Driver {
        app: OpenMakerApp,
        phase: Phase,
        deadline: Instant,
        _hosted: hosted::Hosted,
    }

    impl Driver {
        fn step(&mut self) {
            if Instant::now() > self.deadline {
                fail(format!(
                    "timed out in {:?}; prompt {:?}; panel {}; error {:?}",
                    self.phase,
                    self.app.script_prompt(),
                    open_panel().is_some(),
                    self.app.error()
                ));
            }
            match self.phase {
                Phase::Opening => {
                    let Some(panel) = open_panel() else {
                        return;
                    };
                    if self.app.script_prompt() != Some("file dialog") {
                        return;
                    }
                    let title = panel.title().to_string();
                    if title != "Insert File" {
                        fail(format!("the panel is titled {title:?}"));
                    }
                    println!("open panel {title:?} while the script waits on its prompt");
                    println!("PASS");
                    std::process::exit(0);
                }
            }
        }
    }

    impl eframe::App for Driver {
        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            eframe::App::ui(&mut self.app, ui, frame);
            self.step();
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
    }

    /// The Tasks file with a Photo container, whose OnFirstWindowOpen
    /// script inserts a file into it with no path.
    fn tasks() -> OwnedNode {
        let mut insert = ScriptStep::new(131);
        insert.set_field_spec(
            0,
            Some(FieldSpec::new(
                StoredKey::from_id(fixture::OCCURRENCE).unwrap(),
                StoredKey::from_id(fixture::PHOTO).unwrap(),
                0,
            )),
        );
        let scripts = [fixture::script(1, "Insert", vec![insert])];
        let mut root = fixture::tree(&scripts, true);
        fixture::add_photo(&mut root);
        // OnFirstWindowOpen runs script 1 (file-metadata.md 3.1).
        root.put(
            &[&[2], &[0x18]],
            OwnedNode::leaf(Vec::new(), vec![0, 0, 4, 3, 0, 0, 2, 1, 1, 0, 0]),
        );
        root
    }

    pub fn run() {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(120));
            eprintln!("FAIL: watchdog");
            std::process::exit(3);
        });
        let file = fixture::TempFile::from_tree("insert-panel-native", &tasks());
        let hosted = hosted::host_file(
            "insert-panel-native",
            &file.0,
            fixture::FILE,
            (fixture::ACCOUNT, fixture::PASSWORD),
        );
        let connection = hosted.connection.clone();
        let provider = hosted.provider();
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("OpenMaker open panel test")
                .with_inner_size([900.0, 600.0]),
            ..om_gui::native_options()
        };
        let result = eframe::run_native(
            "OpenMaker open panel test",
            options,
            Box::new(move |cc| {
                om_gui::install_theme(&cc.egui_ctx);
                let app = OpenMakerApp::with_license_store(Box::new(provider), Box::new(NoLicense))
                    .open_on_launch(connection);
                Ok(Box::new(Driver {
                    app,
                    phase: Phase::Opening,
                    deadline: Instant::now() + Duration::from_secs(90),
                    _hosted: hosted,
                }))
            }),
        );
        drop(file);
        fail(format!("the event loop ended first: {result:?}"));
    }
}
