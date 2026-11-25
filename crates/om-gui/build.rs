//! On macOS, the desktop client's executable (`om-gui`, not the PDF worker
//! in src/bin) carries an Info.plist in its `__TEXT,__info_plist` section.
//! It is run straight from the build, with no .app bundle around it, and
//! AppKit names the app in the menu bar after the main bundle's
//! `CFBundleName`, which for such an executable comes from that section;
//! without it the menu bar shows the executable's name, "om-gui". Measured
//! on macOS 27: the plist changes the menu bar's name, while setting the
//! process name does not, and the Dock and Command-Tab keep the
//! executable's name either way: LaunchServices names an unbundled process
//! after its file ([#821](https://github.com/OpenMaker-Dev/openmaker/issues/821)).

use std::path::PathBuf;

/// The app's name: `APP_TITLE` in src/app.rs.
const NAME: &str = "OpenMaker";
/// The desktop client's bundle identifier; OpenMaker Go's is
/// `dev.openmaker.go`.
const IDENTIFIER: &str = "dev.openmaker.desktop";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>{NAME}</string>
	<key>CFBundleDisplayName</key>
	<string>{NAME}</string>
	<key>CFBundleIdentifier</key>
	<string>{IDENTIFIER}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
</dict>
</plist>
"#
    );
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let path = out.join("Info.plist");
    std::fs::write(&path, plist).expect("write Info.plist");
    // Each word through -Xlinker, so a path with a comma or a space reaches
    // the linker whole.
    for arg in [
        "-sectcreate",
        "__TEXT",
        "__info_plist",
        path.to_str().expect("OUT_DIR is UTF-8"),
    ] {
        println!("cargo:rustc-link-arg-bin=om-gui=-Xlinker");
        println!("cargo:rustc-link-arg-bin=om-gui={arg}");
    }
}
