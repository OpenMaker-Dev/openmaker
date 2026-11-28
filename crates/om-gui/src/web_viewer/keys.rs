//! The keys a painted page gets: egui's keys as the Linux input event codes
//! (`linux/input-event-codes.h`) the page host's keyboard sends, which its
//! keymap, the session's, turns into characters as any Wayland client's
//! does.

use egui::Key;

/// The event code of the key at `key`'s place on the keyboard (egui names
/// keys by their US layout's), or `None` for keys a page does not get.
pub(crate) fn code(key: Key) -> Option<u32> {
    use Key::*;
    Some(match key {
        Escape => 1,
        Num1 => 2,
        Num2 => 3,
        Num3 => 4,
        Num4 => 5,
        Num5 => 6,
        Num6 => 7,
        Num7 => 8,
        Num8 => 9,
        Num9 => 10,
        Num0 => 11,
        Minus => 12,
        Equals | Plus => 13,
        Backspace => 14,
        Tab => 15,
        Q => 16,
        W => 17,
        E => 18,
        R => 19,
        T => 20,
        Y => 21,
        U => 22,
        I => 23,
        O => 24,
        P => 25,
        OpenBracket | OpenCurlyBracket => 26,
        CloseBracket | CloseCurlyBracket => 27,
        Enter => 28,
        ControlLeft => 29,
        A => 30,
        S => 31,
        D => 32,
        F => 33,
        G => 34,
        H => 35,
        J => 36,
        K => 37,
        L => 38,
        Semicolon | Colon => 39,
        Quote => 40,
        Backtick => 41,
        ShiftLeft => 42,
        Backslash | Pipe => 43,
        Z => 44,
        X => 45,
        C => 46,
        V => 47,
        B => 48,
        N => 49,
        M => 50,
        Comma => 51,
        Period => 52,
        Slash | Questionmark => 53,
        ShiftRight => 54,
        AltLeft => 56,
        Space => 57,
        CapsLock => 58,
        F1 => 59,
        F2 => 60,
        F3 => 61,
        F4 => 62,
        F5 => 63,
        F6 => 64,
        F7 => 65,
        F8 => 66,
        F9 => 67,
        F10 => 68,
        NumLock => 69,
        ScrollLock => 70,
        IntlBackslash => 86,
        F11 => 87,
        F12 => 88,
        ControlRight => 97,
        PrintScreen => 99,
        AltRight => 100,
        Home => 102,
        ArrowUp => 103,
        PageUp => 104,
        ArrowLeft => 105,
        ArrowRight => 106,
        End => 107,
        ArrowDown => 108,
        PageDown => 109,
        Insert => 110,
        Delete => 111,
        Pause => 119,
        SuperLeft => 125,
        SuperRight => 126,
        Menu => 127,
        F13 => 183,
        F14 => 184,
        F15 => 185,
        F16 => 186,
        F17 => 187,
        F18 => 188,
        F19 => 189,
        F20 => 190,
        F21 => 191,
        F22 => 192,
        F23 => 193,
        F24 => 194,
        _ => return None,
    })
}

/// The modifier keys, as the codes their left keys send, in the order a
/// page hears them pressed.
pub(crate) const SHIFT: u32 = 42;
pub(crate) const CONTROL: u32 = 29;
pub(crate) const ALT: u32 = 56;
pub(crate) const SUPER: u32 = 125;

/// Which modifier keys `modifiers` holds down, as codes.
pub(crate) fn held(modifiers: egui::Modifiers) -> Vec<u32> {
    [
        (modifiers.shift, SHIFT),
        (modifiers.ctrl, CONTROL),
        (modifiers.alt, ALT),
        // egui's `mac_cmd` is never set off the Mac; `command` is Ctrl there.
        (modifiers.mac_cmd, SUPER),
    ]
    .into_iter()
    .filter_map(|(down, code)| down.then_some(code))
    .collect()
}
