//! The page host of web viewers in native Wayland windows: `om-gui` runs it
//! from beside its own executable (`om_gui::web_viewer_host`).
fn main() -> std::process::ExitCode {
    om_gui::web_viewer_host()
}
