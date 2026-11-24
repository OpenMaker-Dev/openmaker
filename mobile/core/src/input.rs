//! What the native app sends a document. Positions are in points (Android's
//! dp) from the canvas's top left.

use crate::Viewport;

#[derive(Clone, Debug, uniffi::Enum)]
pub enum Input {
    /// The canvas changed size or pixel density.
    Resize { viewport: Viewport },
    /// A finger tapped here.
    Tap { x: f32, y: f32 },
    /// A finger began to pan here: what is under it scrolls with it.
    PanBegan { x: f32, y: f32 },
    /// The pan moved this far since its last report.
    PanMoved { dx: f32, dy: f32 },
    /// The finger lifted moving this fast, in points a second: the content
    /// glides on and slows to a stop, as a native scroll view's does.
    PanEnded { vx: f32, vy: f32 },
    /// Text typed or committed by the keyboard.
    Text { text: String },
    /// An input method's composition so far (marked text); empty when it
    /// was cancelled. The text it commits comes as [`Input::Text`].
    Compose { text: String },
    /// A key that edits or moves rather than types. (Not `Key`: inside
    /// Kotlin's `Input` that name would hide the `Key` enum.)
    Press {
        key: Key,
        shift: bool,
        /// iOS's Command key, Android's Ctrl.
        command: bool,
    },
    /// Text pasted into the field being edited.
    Paste { text: String },
    /// Paste what this document's last Copy or Cut copied: Android sends it
    /// while that text has yet to reach the clipboard, so a paste right
    /// after a cut takes the cut text, in its place among what was typed.
    PasteCopied,
    /// An input method deletes text before and after the field's
    /// selection (or composition) and keeps it, at most 64 units each way:
    /// counted in UTF-16 code units when `utf16` (Android's
    /// `deleteSurroundingText`), in characters otherwise
    /// (`deleteSurroundingTextInCodePoints`).
    DeleteAround {
        before: u32,
        after: u32,
        utf16: bool,
    },
    /// An accessibility element's action ([`crate::Element::id`]).
    Activate { id: u64 },
    /// A menu row chosen ([`crate::MenuRow::id`]).
    Menu { id: String },
    /// A control of the chrome. (Not `Command`, for the same reason.)
    Run { command: Command },
    /// The app came to the foreground or left it. Leaving it saves the
    /// record's changes, as Done does.
    Active { active: bool },
    /// A screen reader started or stopped: frames carry the visible
    /// fields, labels and buttons while one runs.
    Accessibility { enabled: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Key {
    Backspace,
    Delete,
    /// Return.
    Enter,
    /// A hardware keyboard's numeric keypad Enter, which FileMaker tells
    /// apart from Return (it commits the record in Browse).
    KeypadEnter,
    /// A hardware keyboard's Space: it types a space in a field and
    /// selects a checkbox or radio button item.
    Space,
    /// With `command`: Select All in the field being edited.
    A,
    /// With `command`: Copy the field's selection.
    C,
    /// With `command`: Cut the field's selection.
    X,
    /// With `command`: Undo the field's typing (with `shift`, Redo).
    Z,
    Tab,
    Escape,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Home,
    End,
    PageUp,
    PageDown,
}

impl Key {
    pub(crate) fn egui(self) -> egui::Key {
        match self {
            Key::Backspace => egui::Key::Backspace,
            Key::Delete => egui::Key::Delete,
            Key::Enter | Key::KeypadEnter => egui::Key::Enter,
            Key::Space => egui::Key::Space,
            Key::A => egui::Key::A,
            Key::C => egui::Key::C,
            Key::X => egui::Key::X,
            Key::Z => egui::Key::Z,
            Key::Tab => egui::Key::Tab,
            Key::Escape => egui::Key::Escape,
            Key::ArrowLeft => egui::Key::ArrowLeft,
            Key::ArrowRight => egui::Key::ArrowRight,
            Key::ArrowUp => egui::Key::ArrowUp,
            Key::ArrowDown => egui::Key::ArrowDown,
            Key::Home => egui::Key::Home,
            Key::End => egui::Key::End,
            Key::PageUp => egui::Key::PageUp,
            Key::PageDown => egui::Key::PageDown,
        }
    }
}

/// A control of the chrome ([`om_gui::ChromeCommand`]); each has its
/// `can_` flag in [`crate::DocumentState`].
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Command {
    Previous,
    Next,
    /// The record of this number in the found set, from 1.
    GoToRecord {
        number: u64,
    },
    Find,
    PerformFind,
    CancelFind,
    ToggleOmit,
    QuickFind {
        words: String,
    },
    Done,
    /// Close the file the app showed in front ([`crate::DocumentState`]'s
    /// `file`): a file brought forward since is not closed.
    Close {
        file: u64,
    },
}

impl Command {
    pub(crate) fn chrome(self) -> om_gui::ChromeCommand {
        use om_gui::ChromeCommand as C;
        match self {
            Command::Previous => C::Previous,
            Command::Next => C::Next,
            Command::GoToRecord { number } => C::GoToRecord(number),
            Command::Find => C::Find,
            Command::PerformFind => C::PerformFind,
            Command::CancelFind => C::CancelFind,
            Command::ToggleOmit => C::ToggleOmit,
            Command::QuickFind { words } => C::QuickFind(words),
            Command::Done => C::Done,
            Command::Close { .. } => C::Close,
        }
    }
}

/// The modifiers of a key from the device's keyboard.
pub(crate) fn modifiers(shift: bool, command: bool) -> egui::Modifiers {
    egui::Modifiers {
        shift,
        command,
        // egui reads Command as the Mac's Command key on Apple platforms and
        // as Ctrl elsewhere.
        mac_cmd: command && cfg!(target_vendor = "apple"),
        ctrl: command && !cfg!(target_vendor = "apple"),
        alt: false,
    }
}
