//! File > Open... and File > Create New... in the real app on macOS:
//! eframe's own event loop and window, no file open.
//!
//! - File > Open..., chosen in the system menu bar (the menu's own action,
//!   as a click in it runs it), shows the system's open panel in Documents,
//!   limited to `.fmp12` files.
//! - File > Create New..., then Create in the Create an App sheet, shows
//!   the system's save panel proposing "Untitled.fmp12" in Documents,
//!   limited to `.fmp12` files.
//!
//! The panels are the system's (their contents run in another process on
//! current macOS), so the test reads their settings and does not answer
//! them; what a panel answers reaches the open or create flow as the app's
//! own chooser's answer does (`app.rs`, `file_panel_answered`, and its unit
//! test). Each case runs in a process of its own, since a process has one
//! event loop and a panel left unanswered keeps the window's file commands
//! waiting.
//!
//! `cargo test -p om-gui --test file_panels_native` opens a window and the
//! panels on macOS; elsewhere it does nothing. With `OM_PANEL_SHOTS` set to
//! a directory, each panel is saved there as PNG (the system's
//! `screencapture` of its window).

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("skipped: these are macOS's panels");
}

#[cfg(target_os = "macos")]
fn main() {
    const CASE: &str = "OM_PANEL_CASE";
    match std::env::var(CASE).as_deref() {
        Ok("open") => macos::run(macos::Phase::Starting),
        Ok("create") => macos::run(macos::Phase::Creating),
        _ => {
            let exe = std::env::current_exe().expect("the test's path");
            for case in ["open", "create"] {
                match std::process::Command::new(&exe).env(CASE, case).status() {
                    Ok(status) if status.success() => {}
                    other => {
                        eprintln!("FAIL: the {case} case: {other:?}");
                        std::process::exit(1);
                    }
                }
            }
            println!("PASS");
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem, NSOpenPanel, NSSavePanel};
    use om_data::mock::MockProvider;
    use om_gui::OpenMakerApp;

    fn fail(message: String) -> ! {
        eprintln!("FAIL: {message}");
        std::process::exit(1);
    }

    fn menu(title: &str) -> Option<Retained<NSMenu>> {
        let mtm = MainThreadMarker::new()?;
        let main = NSApplication::sharedApplication(mtm).mainMenu()?;
        main.itemArray()
            .iter()
            .find(|item| item.title().to_string() == title)?
            .submenu()
    }

    fn item(menu: &NSMenu, title: &str) -> Option<(isize, Retained<NSMenuItem>)> {
        menu.itemArray()
            .iter()
            .enumerate()
            .find(|(_, item)| item.title().to_string() == title)
            .map(|(index, item)| (index as isize, item))
    }

    /// File > `title`, enabled or not; `None` before the bar shows it.
    fn file_row(title: &str) -> Option<bool> {
        let file = menu("File")?;
        Some(item(&file, title)?.1.isEnabled())
    }

    /// Choose File > `title` through the menu's own action.
    fn choose(title: &str) {
        let Some(file) = menu("File") else {
            fail("no File menu".into());
        };
        let Some((index, _)) = item(&file, title) else {
            fail(format!("no File > {title}"));
        };
        println!("choosing File > {title}");
        file.performActionForItemAtIndex(index);
    }

    /// The app's visible save panel, an open panel or not.
    fn panel() -> Option<Retained<NSSavePanel>> {
        let mtm = MainThreadMarker::new()?;
        NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .filter(|window| window.isVisible())
            .find_map(|window| window.downcast::<NSSavePanel>().ok())
    }

    /// The extensions the panel limits its files to.
    fn extensions(panel: &NSSavePanel) -> Vec<String> {
        panel
            .allowedContentTypes()
            .iter()
            .filter_map(|kind| kind.preferredFilenameExtension())
            .map(|extension| extension.to_string())
            .collect()
    }

    /// The panel shows the user's Documents folder (`HOME`'s), or there is
    /// none to show.
    fn in_documents(panel: &NSSavePanel) -> bool {
        let folder = panel
            .directoryURL()
            .and_then(|url| url.path())
            .map(|path| path.to_string())
            .unwrap_or_default();
        let documents = std::env::var("HOME")
            .map(|home| std::path::Path::new(&home).join("Documents"))
            .unwrap_or_default();
        !documents.is_dir()
            || std::fs::canonicalize(&folder).ok() == std::fs::canonicalize(&documents).ok()
    }

    /// Save `panel` as `name`.png in `OM_PANEL_SHOTS` when it is set.
    fn shot(panel: &NSSavePanel, name: &str) {
        let Some(dir) = std::env::var_os("OM_PANEL_SHOTS") else {
            return;
        };
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        let saved = std::process::Command::new("screencapture")
            .args(["-x", "-o", &format!("-l{}", panel.windowNumber())])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        println!("screenshot {name}: {saved}");
    }

    #[derive(Debug, PartialEq)]
    pub enum Phase {
        Starting,
        Opening,
        /// The open panel drawn, until then.
        Showing(Instant),
        Creating,
        Sheet,
        Saving,
        Saved(Instant),
    }

    struct Driver {
        app: OpenMakerApp,
        phase: Phase,
        deadline: Instant,
        frames: u64,
    }

    impl Driver {
        fn step(&mut self) {
            self.frames += 1;
            if Instant::now() > self.deadline {
                fail(format!(
                    "timed out in {:?}; panel {}; error {:?}",
                    self.phase,
                    panel().is_some(),
                    self.app.error()
                ));
            }
            match self.phase {
                Phase::Starting => {
                    if file_row("Open...") != Some(true) {
                        return;
                    }
                    choose("Open...");
                    self.phase = Phase::Opening;
                }
                Phase::Opening => {
                    let Some(panel) = panel() else {
                        return;
                    };
                    let Ok(open) = panel.clone().downcast::<NSOpenPanel>() else {
                        fail("File > Open... shows a save panel".into());
                    };
                    let title = panel.title().to_string();
                    let kinds = extensions(&panel);
                    if title != "Open File" || kinds != ["fmp12"] || !in_documents(&panel) {
                        fail(format!("open panel {title:?} for {kinds:?}"));
                    }
                    if open.allowsMultipleSelection() || open.canChooseDirectories() {
                        fail("the open panel chooses more than one file".into());
                    }
                    println!("open panel {title:?} in Documents for {kinds:?}");
                    self.phase = Phase::Showing(Instant::now() + Duration::from_secs(5));
                }
                Phase::Showing(until) => {
                    if Instant::now() < until {
                        return;
                    }
                    if let Some(panel) = panel() {
                        shot(&panel, "open-panel");
                        panel.close();
                    }
                    std::process::exit(0);
                }
                Phase::Creating => {
                    if file_row(om_data::new_file::CREATE_FILE_LABEL) != Some(true) {
                        return;
                    }
                    choose(om_data::new_file::CREATE_FILE_LABEL);
                    self.phase = Phase::Sheet;
                }
                Phase::Sheet => {
                    // The Create an App sheet takes Return as Create
                    // (raw_input_hook); then the save panel shows.
                    let Some(panel) = panel() else {
                        return;
                    };
                    if panel.clone().downcast::<NSOpenPanel>().is_ok() {
                        fail("Create shows an open panel".into());
                    }
                    self.phase = Phase::Saving;
                }
                Phase::Saving => {
                    let Some(panel) = panel() else {
                        fail("the save panel went".into());
                    };
                    let name = panel.nameFieldStringValue().to_string();
                    let kinds = extensions(&panel);
                    if name != "Untitled.fmp12" || kinds != ["fmp12"] || !in_documents(&panel) {
                        fail(format!("save panel {name:?} for {kinds:?}"));
                    }
                    println!("save panel {name:?} in Documents for {kinds:?}");
                    self.phase = Phase::Saved(Instant::now() + Duration::from_secs(2));
                }
                Phase::Saved(until) => {
                    if Instant::now() < until {
                        return;
                    }
                    if let Some(panel) = panel() {
                        shot(&panel, "save-panel");
                        panel.close();
                    }
                    std::process::exit(0);
                }
            }
        }
    }

    impl eframe::App for Driver {
        fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
            eframe::App::raw_input_hook(&mut self.app, ctx, input);
            if self.phase == Phase::Sheet && self.frames % 20 == 10 {
                for pressed in [true, false] {
                    input.events.push(egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
            }
        }

        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            eframe::App::ui(&mut self.app, ui, frame);
            self.step();
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
    }

    pub fn run(phase: Phase) {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(120));
            eprintln!("FAIL: watchdog");
            std::process::exit(3);
        });
        let settings =
            std::env::temp_dir().join(format!("om-file-panels-native-{}.json", std::process::id()));
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("OpenMaker file panels test")
                .with_inner_size([900.0, 600.0]),
            ..om_gui::native_options()
        };
        let result = eframe::run_native(
            "OpenMaker file panels test",
            options,
            Box::new(move |cc| {
                om_gui::install_theme(&cc.egui_ctx);
                let app =
                    OpenMakerApp::new(Box::new(MockProvider::new())).with_settings_file(settings);
                Ok(Box::new(Driver {
                    app,
                    phase,
                    deadline: Instant::now() + Duration::from_secs(90),
                    frames: 0,
                }))
            }),
        );
        fail(format!("the event loop ended first: {result:?}"));
    }
}
