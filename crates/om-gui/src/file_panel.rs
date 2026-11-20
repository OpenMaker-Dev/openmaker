//! The platform's file panel for the Insert commands
//! ([#50](https://github.com/OpenMaker-Dev/openmaker/issues/50)).
//! FileMaker Pro's Insert > Picture, Audio/Video, PDF and File open the
//! system's file dialog (Claris Help: Insert Picture, Insert PDF, Insert
//! Audio/Video, Insert File). On macOS OpenMaker shows an `NSOpenPanel`,
//! modeless, while the window's prompt keeps the window waiting for it; the
//! answer is the path the user chose, or nothing for Cancel.
//!
//! Where there is no panel to show, [`OpenPanel::open`] answers `None` and
//! the caller asks in its own dialog, which takes a typed path: on Linux and
//! Windows for now, and wherever the app has no native window (the GUI
//! tests, which drive the in-app dialog). The panel offers no "Store only a
//! reference to the file": how FileMaker stores a reference is unmeasured
//! (docs/re/containers.md, "External storage: still open"), and such a step
//! still stops as unsupported.

use std::path::PathBuf;
use std::sync::mpsc;

/// An open panel the user is answering. Dropping it closes the panel (the
/// prompt it answers went away: the script was halted).
pub(crate) struct OpenPanel {
    answer: mpsc::Receiver<Option<PathBuf>>,
    #[cfg(target_os = "macos")]
    panel: objc2::rc::Retained<objc2_app_kit::NSOpenPanel>,
}

impl OpenPanel {
    /// Show a panel titled `title` (the command's name), saying `message`
    /// (the field the file goes into), that chooses one existing file.
    /// `wake` is called once the user has answered, from the main thread.
    /// `None` where there is no panel to show.
    #[cfg(target_os = "macos")]
    pub(crate) fn open(title: &str, message: &str, wake: impl Fn() + 'static) -> Option<OpenPanel> {
        use objc2::rc::Weak;
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSModalResponse, NSModalResponseOK, NSOpenPanel};
        use objc2_foundation::NSString;
        let mtm = MainThreadMarker::new()?;
        let panel = NSOpenPanel::openPanel(mtm);
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        panel.setResolvesAliases(true);
        panel.setTitle(Some(&NSString::from_str(title)));
        panel.setMessage(Some(&NSString::from_str(message)));
        let (send, answer) = mpsc::channel();
        // The panel keeps the block: the block holds the panel weakly.
        let weak = Weak::from_retained(&panel);
        let done = block2::RcBlock::new(move |response: NSModalResponse| {
            let chosen = weak
                .load()
                .filter(|_| response == NSModalResponseOK)
                .and_then(|panel| panel.URLs().firstObject())
                .and_then(|url| url.path())
                .map(|path| PathBuf::from(path.to_string()));
            let _ = send.send(chosen);
            wake();
        });
        panel.beginWithCompletionHandler(&done);
        Some(OpenPanel { answer, panel })
    }

    /// No panel off macOS yet: the in-app dialog asks.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn open(
        _title: &str,
        _message: &str,
        _wake: impl Fn() + 'static,
    ) -> Option<OpenPanel> {
        None
    }

    /// The user's answer once given: `Some(Some(path))` for a file chosen,
    /// `Some(None)` for Cancel, `None` while the panel is open.
    pub(crate) fn answer(&self) -> Option<Option<PathBuf>> {
        match self.answer.try_recv() {
            Ok(chosen) => Some(chosen),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(None),
        }
    }
}

impl Drop for OpenPanel {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if self.panel.isVisible() {
            self.panel.close();
        }
    }
}
