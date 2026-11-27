//! OpenMaker's desktop GUI: connect to a FileMaker Server and open a hosted
//! file, or open a file on this computer (File > Open), and browse its
//! layouts and records in a window whose application chrome
//! follows FileMaker Pro's (menu bar, status toolbar, layout bar, modes). See
//! `docs/re/client-ui.md` for FileMaker Pro's chrome and
//! `docs/desktop-client.md` for what this crate builds of it.
//!
//! The GUI talks to data only through [`om_data::DataProvider`], which it runs
//! on a background thread. [`OpenMakerApp::new`] takes any provider.
//!
//! # Choosing the provider
//!
//! The `om-gui` binary picks its provider with [`provider_from_env`], driven
//! by the `OPENMAKER_PROVIDER` environment variable (default `native`).
//! Sample data requires an explicit `mock` selection and is labeled in the
//! window. Providers can also be supplied directly to [`run`]:
//!
//! ```no_run
//! # use om_data::mock::MockProvider;
//! om_gui::run(Box::new(MockProvider::new())).expect("OpenMaker failed");
//! ```

// OpenMaker Go (mobile/) builds this crate for phones and tablets without
// the desktop event loop ([`run`], eframe and winit): what only that loop
// uses (the OS keychain, the native window's handle, environment defaults)
// is unused there. Desktop builds check for dead code.
#![cfg_attr(any(target_os = "ios", target_os = "android"), allow(dead_code))]

mod app;
mod calc;
mod connect;
mod content;
mod create_new;
mod database;
mod database_graph;
mod database_lookup;
mod database_options;
mod database_relationship;
mod database_summary;
mod desktop;
mod document;
mod file_history;
mod file_panel;
mod home;
mod icons;
mod inspector;
pub mod keypad;
mod layout_bar;
mod layout_canvas;
mod layout_objects;
mod layout_setup;
pub mod licensing;
mod list_build;
mod manage_layouts;
mod menu_record;
mod menubar;
mod new_layout;
mod open_file;
mod page_setup;
mod part_setup;
mod plugin_ui;
mod portal_setup;
mod preview;
mod printing;
pub mod report;
#[cfg(test)]
mod scorecard;
mod script_workspace;
mod scripting;
mod security;
mod session;
pub mod settings;
pub mod svg_loader;
#[cfg(target_os = "macos")]
mod system_menu;
#[cfg(test)]
mod test_renderer;
mod theme;
mod toolbar;
mod value_lists;
mod web_viewer;
mod web_viewer_setup;
mod widgets;
mod worker;

pub use app::mobile::{ChromeCommand, ChromeMenu, ChromeState, MenuRow, MenuRowKind};
pub use app::{CardFrame, Mode, OpenMakerApp, ViewMode, APP_TITLE};
pub use connect::host_address;
pub use document::PdfViewer;
pub use theme::install as install_theme;

use om_data::{mock::MockProvider, DataProvider};

/// The environment variable that selects the data provider.
pub const PROVIDER_ENV: &str = "OPENMAKER_PROVIDER";

/// Build the data provider named by [`PROVIDER_ENV`].
///
/// Recognised values: `native` (default) and `mock` (explicit sample data).
pub fn provider_from_env() -> Result<Box<dyn DataProvider>, String> {
    let name = match std::env::var(PROVIDER_ENV) {
        Ok(name) => name,
        Err(std::env::VarError::NotPresent) => String::new(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(format!("{PROVIDER_ENV} is not valid UTF-8"));
        }
    };
    match name.trim() {
        // A hosted file's related files open when its layouts and
        // calculations first read them (#983).
        "" | "native" => om_provider::NativeProvider::from_env()
            .map(|provider| {
                Box::new(provider.with_related_on_demand(true)) as Box<dyn DataProvider>
            })
            .map_err(|err| err.to_string()),
        "mock" => Ok(Box::new(MockProvider::new())),
        other => Err(format!(
            "unknown {PROVIDER_ENV} value {other:?} (supported: native, mock)"
        )),
    }
}

/// Native window options for the OpenMaker window. With the `web-viewer`
/// feature on Linux they choose winit's X11 backend whenever an X server
/// answers, also on a Wayland session (through XWayland), since the web
/// viewers are X11 child windows.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn native_options() -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_TITLE)
        .with_app_id("openmaker")
        // FileMaker's document window in the captures is 1060 pt wide.
        .with_inner_size([1060.0, 760.0])
        .with_min_inner_size([720.0, 480.0]);
    // Without it eframe shows its own default, egui's logo.
    viewport.icon = app_icon();
    eframe::NativeOptions {
        viewport,
        event_loop_builder: web_viewer::event_loop_hook(),
        ..Default::default()
    }
}

/// OpenMaker's logo as the app's icon: the favicon's tile at 412 pixels in
/// a 512-pixel canvas, the macOS icon grid eframe's default icon also
/// follows (docs/assets/README.md). eframe shows the root window's icon in
/// the Dock on macOS and as its large taskbar icon on Windows; each
/// window's is its window icon on Windows and X11. Wayland has no window
/// icons (winit ignores them), so the shell uses the desktop entry the app
/// id names (docs/desktop-client.md). Decoded once and shared, so a
/// viewport builder made every frame keeps the same icon (egui compares it
/// by pointer). `None` only when the bundled PNG does not decode, which
/// leaves eframe's default.
pub(crate) fn app_icon() -> Option<std::sync::Arc<egui::IconData>> {
    static ICON: std::sync::OnceLock<Option<std::sync::Arc<egui::IconData>>> =
        std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let image = image::load_from_memory_with_format(
            include_bytes!("../assets/openmaker-icon.png"),
            image::ImageFormat::Png,
        )
        .ok()?
        .into_rgba8();
        Some(std::sync::Arc::new(egui::IconData {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        }))
    })
    .clone()
}

/// Settle what web viewers need from the process environment: call it
/// before any thread starts (`run` does). With the `web-viewer` feature,
/// WebKitGTK's DMA-BUF renderer is turned off on an X server without DRI3,
/// and sends its pages through shared memory where DRI3 hands out a device
/// of NVIDIA's driver, unless `WEBKIT_DISABLE_DMABUF_RENDERER` or
/// `WEBKIT_DMABUF_RENDERER_FORCE_SHM` is already set; otherwise it does
/// nothing. Only the first call looks: a later one, such as `run`'s after
/// the `om-gui` program's own, may come once threads run.
pub fn prepare_web_viewers() {
    static PREPARED: std::sync::Once = std::sync::Once::new();
    PREPARED.call_once(web_viewer::prepare_environment);
}

/// The page host of web viewers in native Wayland windows: the
/// `om-gui-webview` program, which `om-gui` runs from beside itself with
/// `OPENMAKER_WEB_VIEWER_BACKEND=wayland` (`web_viewer::host`). It fails
/// where that backend does not exist.
pub fn web_viewer_host() -> std::process::ExitCode {
    #[cfg(all(feature = "web-viewer", target_os = "linux"))]
    {
        web_viewer::host::serve()
    }
    #[cfg(not(all(feature = "web-viewer", target_os = "linux")))]
    {
        eprintln!("om-gui-webview: this build has no Wayland web viewers");
        std::process::ExitCode::FAILURE
    }
}

/// Install the logger [`run`] reads the error that ends its event loop
/// with, passed through `wrap`, as the process's logger at `level`: for a
/// program that reads the log too (the `om-gui` program's error reports),
/// before `run`. `wrap` must pass every record on to the logger it is
/// given. Whether that is now the process's logger: not when the process
/// installed another first, and then the keypad's Enter is not told apart
/// from Return (`keypad`).
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn install_logger<L: log::Log + 'static>(
    wrap: impl FnOnce(Box<dyn log::Log>) -> L,
    level: log::LevelFilter,
) -> bool {
    keypad::install_logger(wrap, level)
}

/// Open the OpenMaker window with `provider` and run until it is closed.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub fn run(provider: Box<dyn DataProvider>) -> eframe::Result {
    prepare_web_viewers();
    // The User Name the account chose last time (Settings).
    if let Some(path) = settings::default_path() {
        if let Err(err) = settings::apply_saved(&path) {
            eprintln!("openmaker: {err}");
        }
    }
    // On an event loop of the app's own, which tells the keypad's Enter
    // from Return (`keypad`).
    keypad::run(
        APP_TITLE,
        native_options(),
        Box::new(|cc| {
            install_theme(&cc.egui_ctx);
            Ok(Box::new(OpenMakerApp::new(provider).with_desktop_startup()))
        }),
    )
}

mod layout_menu;

#[cfg(test)]
mod icon_tests {
    /// The bundled icon decodes: a broken file would leave eframe's
    /// default icon without a word.
    #[test]
    fn the_app_icon_decodes() {
        let icon = super::app_icon().expect("the bundled icon decodes");
        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
    }
}
