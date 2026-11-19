//! The character style flags of the text formatting functions (spec
//! `docs/re/calc-functions.md` 5.2 and the formatting oracle answers in
//! section 9): what `TextStyleAdd` and `TextStyleRemove` do to a
//! [`CharStyle`], and how font names compare.
//!
//! [`CharStyle::flags`] holds two 16-bit halves:
//!
//! - the low half is the styles that are on, in the bits of the style
//!   keywords (`Bold` = 256, `Italic` = 512, ...);
//! - the high half is the styles explicitly set to normal, in the same bit
//!   positions, which `TextStyleAdd(text; Plain)` sets for every style and
//!   `GetAsCSS` shows as `font-weight: normal;` and the like. A style with
//!   neither bit inherits the field's.
//!
//! Some styles form groups: the case transforms (`Uppercase` 16,
//! `Lowercase` 32, `Titlecase` 48 = both), the underlines (`Underline`
//! 1024, `WordUnderline` 64, `DoubleUnderline` 128), the vertical position
//! (`Superscript` 4, `Subscript` 8) and the spacing (`Condense` 8192,
//! `Extend` 16384). Adding any style of a group replaces the group's
//! current one (`Superscript` then `Subscript` is subscript); adding two of
//! a group at once keeps both bits (`Superscript + Subscript` shows as
//! superscript). Removing clears a case transform only when the removed
//! bits cover it (removing `Uppercase` leaves `Titlecase`, removing
//! `Titlecase` clears `Uppercase`) and clears the other groups when the
//! removed bits overlap them.

use super::CharStyle;

pub const STRIKETHROUGH: u16 = 1;
pub const SMALL_CAPS: u16 = 2;
pub const SUPERSCRIPT: u16 = 4;
pub const SUBSCRIPT: u16 = 8;
pub const UPPERCASE: u16 = 16;
pub const LOWERCASE: u16 = 32;
pub const TITLECASE: u16 = 48;
pub const WORD_UNDERLINE: u16 = 64;
pub const DOUBLE_UNDERLINE: u16 = 128;
pub const BOLD: u16 = 256;
pub const ITALIC: u16 = 512;
pub const UNDERLINE: u16 = 1024;
pub const HIGHLIGHT_YELLOW: u16 = 4096;
pub const CONDENSE: u16 = 8192;
pub const EXTEND: u16 = 16384;
pub const ALL_STYLES: u16 = 32767;

/// The case transform group.
pub const CASE: u16 = TITLECASE;
/// The underline group (strikethrough is separate).
pub const UNDERLINES: u16 = UNDERLINE | WORD_UNDERLINE | DOUBLE_UNDERLINE;
/// The vertical position group.
pub const POSITION: u16 = SUPERSCRIPT | SUBSCRIPT;
/// The spacing group.
pub const SPACING: u16 = CONDENSE | EXTEND;
/// Groups whose explicit normal state `TextStyleRemove` never drops.
const KEPT_NORMAL: u16 = POSITION | SPACING;

impl CharStyle {
    /// The styles that are on.
    pub fn styles_on(&self) -> u16 {
        self.flags as u16
    }

    /// The styles explicitly set to normal.
    pub fn styles_normal(&self) -> u16 {
        (self.flags >> 16) as u16
    }

    fn with_flags(&self, on: u16, normal: u16) -> CharStyle {
        CharStyle {
            flags: u32::from(on) | u32::from(normal) << 16,
            ..self.clone()
        }
    }

    /// `TextStyleAdd`: `styles` 0 (`Plain`) sets every style to normal;
    /// otherwise each group `styles` touches takes its new value and loses
    /// its explicit normal state.
    pub fn with_styles_added(&self, styles: u16) -> CharStyle {
        if styles == 0 {
            return self.with_flags(0, u16::MAX);
        }
        let (mut on, mut normal) = (self.styles_on(), self.styles_normal());
        for group in [CASE, UNDERLINES, POSITION, SPACING] {
            if styles & group != 0 {
                on &= !group;
                normal &= !group;
            }
        }
        on |= styles;
        normal &= !styles;
        self.with_flags(on, normal)
    }

    /// `TextStyleRemove`: turns off the styles in `styles` (with the group
    /// rules above). Explicit normal states survive only for the styles
    /// removed, except that the position and spacing ones always survive
    /// and the case one survives only the removal of `Uppercase` or
    /// `Lowercase` alone.
    pub fn with_styles_removed(&self, styles: u16) -> CharStyle {
        let (mut on, normal) = (self.styles_on(), self.styles_normal());
        let case = on & CASE;
        if case != 0 && case & !styles == 0 {
            on &= !CASE;
        }
        for group in [UNDERLINES, POSITION, SPACING] {
            if on & group & styles != 0 {
                on &= !group;
            }
        }
        on &= !(styles & !(CASE | UNDERLINES | POSITION | SPACING));
        let mut kept = (normal & styles & !(CASE | KEPT_NORMAL)) | (normal & KEPT_NORMAL);
        let removed_case = styles & CASE;
        if removed_case == UPPERCASE || removed_case == LOWERCASE {
            kept |= normal & CASE;
        }
        self.with_flags(on, kept)
    }
}

/// Whether two font names name the same font: FileMaker compares them
/// ignoring case and width (`"ARIAL"`, `"arial"` and `"Ａｒｉａｌ"` are one
/// font, `" Arial"` another), as `TextFontRemove` matches them.
pub fn same_font(a: &str, b: &str) -> bool {
    let fold = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
                '\u{3000}' => ' ',
                _ => c,
            })
            .flat_map(char::to_lowercase)
            .collect()
    };
    fold(a) == fold(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn css_groups(s: &CharStyle) -> (u16, u16) {
        (s.styles_on(), s.styles_normal())
    }

    #[test]
    fn groups_replace_on_add() {
        let base = CharStyle::default();
        let s = base
            .with_styles_added(SUPERSCRIPT)
            .with_styles_added(SUBSCRIPT);
        assert_eq!(s.styles_on(), SUBSCRIPT);
        let s = base
            .with_styles_added(UPPERCASE)
            .with_styles_added(LOWERCASE);
        assert_eq!(s.styles_on(), LOWERCASE);
        let s = base
            .with_styles_added(UNDERLINE)
            .with_styles_added(STRIKETHROUGH);
        assert_eq!(s.styles_on(), UNDERLINE | STRIKETHROUGH);
        let s = base.with_styles_added(CONDENSE | EXTEND);
        assert_eq!(s.styles_on(), CONDENSE | EXTEND);
    }

    #[test]
    fn removal_rules() {
        let base = CharStyle::default();
        let title = base.with_styles_added(TITLECASE);
        assert_eq!(title.with_styles_removed(UPPERCASE).styles_on(), TITLECASE);
        let upper = base.with_styles_added(UPPERCASE);
        assert_eq!(upper.with_styles_removed(TITLECASE).styles_on(), 0);
        assert_eq!(upper.with_styles_removed(LOWERCASE).styles_on(), UPPERCASE);
        let both = base.with_styles_added(SUPERSCRIPT | SUBSCRIPT);
        assert_eq!(both.with_styles_removed(SUBSCRIPT).styles_on(), 0);
        let ul = base
            .with_styles_added(UNDERLINE)
            .with_styles_added(DOUBLE_UNDERLINE);
        assert_eq!(
            ul.with_styles_removed(UNDERLINE).styles_on(),
            DOUBLE_UNDERLINE
        );
        assert_eq!(base.with_styles_removed(BOLD), base);
    }

    #[test]
    fn plain_and_normal_states() {
        let plain = CharStyle::default().with_styles_added(0);
        assert_eq!(css_groups(&plain), (0, u16::MAX));
        // Removing Bold keeps only its normal state and the position and
        // spacing ones.
        let r = plain.with_styles_removed(BOLD);
        assert_eq!(css_groups(&r), (0, BOLD | POSITION | SPACING));
        assert_eq!(
            plain.with_styles_removed(UPPERCASE).styles_normal() & CASE,
            CASE
        );
        assert_eq!(
            plain.with_styles_removed(TITLECASE).styles_normal() & CASE,
            0
        );
        // Adding a style clears its group's normal state.
        let b = plain.with_styles_added(BOLD);
        assert_eq!(css_groups(&b), (BOLD, !BOLD));
        assert_eq!(
            css_groups(&b.with_styles_removed(BOLD)),
            (0, POSITION | SPACING)
        );
        let sup = plain
            .with_styles_added(SUPERSCRIPT)
            .with_styles_removed(SUPERSCRIPT);
        assert_eq!(sup.styles_normal() & POSITION, 0);
    }

    #[test]
    fn font_names() {
        assert!(same_font("Arial", "ARIAL"));
        assert!(same_font("Arial", "Ａｒｉａｌ"));
        assert!(same_font("Ärial", "ärial"));
        assert!(!same_font(" Arial", "Arial"));
        assert!(same_font("", ""));
    }
}
