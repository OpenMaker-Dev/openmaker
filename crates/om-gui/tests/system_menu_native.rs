//! The macOS menu bar in the real app (#55): eframe's own event loop and
//! window, with the mock provider's file open.
//!
//! - The system menu bar shows the front window's menus, in FileMaker's
//!   order, the app menu with macOS's own Hide Others, and a row's shortcut
//!   as its key equivalent (Open... is ⌘O).
//! - Choosing View > Find Mode there (the menu's own action, as a click in
//!   it runs it) puts the window in Find mode: Records gives way to
//!   Requests, and Find Mode is checked.
//!
//! `cargo test -p om-gui --test system_menu_native` opens a window on
//! macOS; elsewhere it does nothing. With `OM_MENU_SHOTS` set to a
//! directory, the window is saved there as PNG (the system's
//! `screencapture` of the window alone).

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("skipped: the system menu bar is macOS's");
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use om_data::{mock::MockProvider, Connection};
    use om_gui::{Mode, OpenMakerApp};

    fn fail(message: String) -> ! {
        eprintln!("FAIL: {message}");
        std::process::exit(1);
    }

    /// The app's menu bar: each menu's title and menu.
    fn bar() -> Vec<(String, Retained<NSMenu>)> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Vec::new();
        };
        let Some(main) = NSApplication::sharedApplication(mtm).mainMenu() else {
            return Vec::new();
        };
        main.itemArray()
            .iter()
            .filter_map(|item| Some((item.title().to_string(), item.submenu()?)))
            .collect()
    }

    fn item(menu: &NSMenu, title: &str) -> Option<(isize, Retained<NSMenuItem>)> {
        menu.itemArray()
            .iter()
            .enumerate()
            .find(|(_, item)| item.title().to_string() == title)
            .map(|(index, item)| (index as isize, item))
    }

    /// Save the app's window as PNG in `OM_MENU_SHOTS` when it is set.
    fn shot(name: &str) {
        let Some(dir) = std::env::var_os("OM_MENU_SHOTS") else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let windows = NSApplication::sharedApplication(mtm).windows();
        let Some(window) = windows.iter().find(|w| w.isVisible()) else {
            return;
        };
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        let saved = std::process::Command::new("screencapture")
            .args(["-x", "-o", &format!("-l{}", window.windowNumber())])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        println!("screenshot {name}: {saved}");
    }

    fn menu(title: &str) -> Option<Retained<NSMenu>> {
        bar().into_iter().find(|(t, _)| t == title).map(|(_, m)| m)
    }

    #[derive(Debug, PartialEq)]
    enum Phase {
        Opening,
        Finding,
    }

    struct Driver {
        app: OpenMakerApp,
        phase: Phase,
        deadline: Instant,
    }

    impl Driver {
        fn step(&mut self) {
            if Instant::now() > self.deadline {
                let titles: Vec<String> = bar().into_iter().map(|(t, _)| t).collect();
                shot("timeout");
                fail(format!(
                    "timed out in {:?}; menus {titles:?}; mode {:?}; error {:?}",
                    self.phase,
                    self.app.mode(),
                    self.app.error()
                ));
            }
            match self.phase {
                Phase::Opening => {
                    if !self.app.is_open() || !self.app.is_settled() {
                        return;
                    }
                    let titles: Vec<String> = bar().into_iter().map(|(t, _)| t).collect();
                    let expected = [
                        "File", "Edit", "View", "Insert", "Format", "Records", "Scripts", "Tools",
                        "Window", "Help",
                    ];
                    if titles.len() != expected.len() + 1 || titles[1..] != expected {
                        return;
                    }
                    println!("menus {titles:?}");
                    let app_menu = &bar()[0].1;
                    if item(app_menu, "Hide Others").is_none() {
                        fail("the app menu has no Hide Others".into());
                    }
                    let file = menu("File").expect("File");
                    let (_, open) = item(&file, "Open...").expect("Open...");
                    if open.keyEquivalent().to_string() != "o" {
                        fail(format!(
                            "Open... key {:?}",
                            open.keyEquivalent().to_string()
                        ));
                    }
                    let view = menu("View").expect("View");
                    let (index, find) = item(&view, "Find Mode").expect("Find Mode");
                    if !find.isEnabled() {
                        fail("Find Mode is disabled".into());
                    }
                    println!("choosing View > Find Mode");
                    view.performActionForItemAtIndex(index);
                    self.phase = Phase::Finding;
                }
                Phase::Finding => {
                    if self.app.mode() != Mode::Find {
                        return;
                    }
                    let titles: Vec<String> = bar().into_iter().map(|(t, _)| t).collect();
                    if !titles.iter().any(|t| t == "Requests") {
                        return;
                    }
                    if titles.iter().any(|t| t == "Records") {
                        fail(format!("Records is still there: {titles:?}"));
                    }
                    let view = menu("View").expect("View");
                    let (_, find) = item(&view, "Find Mode").expect("Find Mode");
                    if find.state() != 1 {
                        return;
                    }
                    println!("menus {titles:?}; Find Mode checked");
                    shot("find-mode");
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

    pub fn run() {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(120));
            eprintln!("FAIL: watchdog");
            std::process::exit(3);
        });
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title("OpenMaker menu bar test")
                .with_inner_size([900.0, 600.0]),
            ..om_gui::native_options()
        };
        let result = eframe::run_native(
            "OpenMaker menu bar test",
            options,
            Box::new(|cc| {
                om_gui::install_theme(&cc.egui_ctx);
                let app =
                    OpenMakerApp::new(Box::new(MockProvider::new())).open_on_launch(Connection {
                        host: "menu.test.invalid".into(),
                        port: 5003,
                        file: "Mock.fmp12".into(),
                        account: "Admin".into(),
                        password: String::new(),
                    });
                Ok(Box::new(Driver {
                    app,
                    phase: Phase::Opening,
                    deadline: Instant::now() + Duration::from_secs(90),
                }))
            }),
        );
        fail(format!("the event loop ended first: {result:?}"));
    }
}
