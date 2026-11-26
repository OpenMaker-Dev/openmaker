//! OpenMaker's chrome design tokens: the colors, type sizes and metrics that
//! `docs/design.md` defines, the bundled UI font, and the egui [`Style`]
//! built from them.
//!
//! Only application chrome is styled here. The content area renders the
//! file's layout, whose colors and fonts are document data. Chrome code uses
//! these tokens rather than its own color literals, so the palette stays one
//! scheme and a later dark appearance changes one file. Region colors marked
//! "measured" were sampled from FileMaker Pro 26 on macOS
//! (`docs/re/client-ui.md`); the rest follow Apple's Human Interface
//! Guidelines.

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke,
    Style, TextStyle, Theme, Vec2, Visuals,
};

// Surfaces. Region fills are measured from captures 10, 14 and 28.
pub(crate) const TOOLBAR_BG: Color32 = Color32::from_rgb(0xF4, 0xF4, 0xF4);
pub(crate) const LAYOUT_BAR_BG: Color32 = Color32::from_rgb(0xDE, 0xDE, 0xDF);
pub(crate) const LAYOUT_BAR_TOP_RULE: Color32 = Color32::from_rgb(0xB8, 0xB8, 0xB9);
pub(crate) const LAYOUT_BAR_BOTTOM_RULE: Color32 = Color32::from_rgb(0xAE, 0xAE, 0xAE);
pub(crate) const MENU_BAR_RULE: Color32 = Color32::from_rgb(0xE2, 0xE2, 0xE2);
pub(crate) const PASTEBOARD: Color32 = Color32::from_rgb(0xE5, 0xE5, 0xE5);
pub(crate) const PRINT_WORKSPACE: Color32 = Color32::from_rgb(0x9C, 0x9C, 0x9C);
pub(crate) const INSPECTOR_BG: Color32 = Color32::WHITE;
/// Window background: dialogs, panes and sheets.
pub(crate) const DIALOG_BG: Color32 = Color32::from_rgb(0xF0, 0xF0, 0xF0);
pub(crate) const SIDEBAR_BG: Color32 = Color32::from_rgb(0xE3, 0xE3, 0xE4);
pub(crate) const CONTENT_BG: Color32 = Color32::WHITE;
/// Lists, tables, text fields and other large controls.
pub(crate) const CONTROL_BG: Color32 = Color32::WHITE;
/// Every other row of a striped list (Apple's alternating content color).
pub(crate) const ALTERNATE_ROW: Color32 = Color32::from_rgb(0xF4, 0xF5, 0xF5);
/// Button strips under lists and the bars inside dialogs.
pub(crate) const BAR_BG: Color32 = Color32::from_rgb(0xEC, 0xEC, 0xEC);
/// A hovered or pressed borderless button, and a selection in a list that
/// does not have keyboard focus.
pub(crate) const HOVER_FILL: Color32 = Color32::from_rgb(0xDC, 0xDC, 0xDC);
pub(crate) const MENU_FILL: Color32 = Color32::from_rgb(0xF5, 0xF5, 0xF5);
pub(crate) const SEARCH_FILL: Color32 = Color32::from_rgb(0xEE, 0xEE, 0xEE);
/// Capsule buttons: Home's Create and Open, measured.
pub(crate) const CAPSULE_FILL: Color32 = Color32::from_rgb(0xEC, 0xEC, 0xEC);
/// The selected row of Home's sidebar, measured in an inactive window.
pub(crate) const NAV_SELECTION: Color32 = Color32::from_rgb(0xE9, 0xE9, 0xEB);
/// The dimming behind a modal dialog.
pub(crate) const SCRIM: Color32 = Color32::from_black_alpha(24);

// Text and glyphs. Each text color keeps 4.5:1 on the surfaces above,
// except `WEAK_TEXT` (hints and placeholders) and `DISABLED`.
pub(crate) const TEXT: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
pub(crate) const CAPTION: Color32 = Color32::from_rgb(0x5E, 0x5E, 0x5E);
pub(crate) const WEAK_TEXT: Color32 = Color32::from_rgb(0x8A, 0x8A, 0x8A);
pub(crate) const DISABLED: Color32 = Color32::from_rgb(0xC4, 0xC4, 0xC4);
pub(crate) const ICON: Color32 = Color32::from_rgb(0x7D, 0x7D, 0x7D);
pub(crate) const VIEW_GLYPH: Color32 = Color32::from_rgb(0xA5, 0xA5, 0xA6);
/// Text and glyphs on an `ACCENT` or `RED` fill.
pub(crate) const ON_ACCENT: Color32 = Color32::WHITE;

// Lines, lightest to darkest.
/// Rules between rows and sections inside a pane (Apple's grid color).
pub(crate) const SEPARATOR: Color32 = Color32::from_rgb(0xE6, 0xE6, 0xE6);
pub(crate) const SEARCH_BORDER: Color32 = Color32::from_rgb(0xE0, 0xE0, 0xE0);
/// Pane edges and the frames of grouped content.
pub(crate) const HAIRLINE: Color32 = Color32::from_rgb(0xD6, 0xD6, 0xD6);
/// Text fields, list boxes and the frames of in-window dialogs.
pub(crate) const BORDER: Color32 = Color32::from_rgb(0xC5, 0xC5, 0xC5);
pub(crate) const BUTTON_BORDER: Color32 = Color32::from_rgb(0xB9, 0xB9, 0xBA);
/// Dividers between toolbar groups and bar sections.
pub(crate) const DIVIDER: Color32 = Color32::from_rgb(0xB4, 0xB4, 0xB5);
pub(crate) const POPUP_BORDER: Color32 = Color32::from_rgb(0x9B, 0x9B, 0x9C);

// Accent. One blue for every selected, default, highlighted and focused
// state: Apple's Blue in its increased-contrast variant, on which white
// labels keep 4.5:1. Apple's default Blue (#0088FF) gives them 3.5:1.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(0x1E, 0x6E, 0xF4);
/// Selected controls, rows and view glyphs.
pub(crate) const SELECTED: Color32 = ACCENT;
/// The hovered or open menu row.
pub(crate) const HIGHLIGHT: Color32 = ACCENT;
/// The default button's fill.
pub(crate) const DEFAULT_BUTTON: Color32 = ACCENT;
/// The keyboard focus ring: the accent at three quarters over white, the
/// palest that keeps 3:1 against a white field.
pub(crate) const FOCUS: Color32 = Color32::from_rgb(0x56, 0x92, 0xF7);
/// The current row of a list: the accent at a tenth over white.
pub(crate) const CURRENT_ROW: Color32 = Color32::from_rgb(0xE8, 0xF0, 0xFE);
/// Selected text (Apple's selected text background).
pub(crate) const SELECTED_TEXT_BG: Color32 = Color32::from_rgb(0xB4, 0xD5, 0xFE);
/// The found-set ring, measured.
pub(crate) const RING: Color32 = Color32::from_rgb(0x7A, 0xAF, 0xE6);

// Status: Apple's system colors in their increased-contrast variants, so
// text in them keeps 4.5:1 on white. Tints are 10-16% over white.
/// Errors, failures and destructive actions: fills, bars and glyphs.
pub(crate) const RED: Color32 = Color32::from_rgb(0xE9, 0x15, 0x2D);
pub(crate) const RED_TINT: Color32 = Color32::from_rgb(0xFD, 0xE8, 0xEA);
/// Red text, such as an error message, on any light surface or
/// `RED_TINT`: the red darkened to keep 4.5:1 on all of them (`RED` has
/// 4.0:1 on `DIALOG_BG`).
pub(crate) const RED_INK: Color32 = Color32::from_rgb(0xB8, 0x0E, 0x28);
/// Warning text.
pub(crate) const YELLOW: Color32 = Color32::from_rgb(0xA1, 0x6A, 0x00);
pub(crate) const YELLOW_TINT: Color32 = Color32::from_rgb(0xFF, 0xF7, 0xD6);
/// Text on `YELLOW_TINT`: the yellow darkened to keep 4.5:1 there.
pub(crate) const YELLOW_INK: Color32 = Color32::from_rgb(0x86, 0x57, 0x00);
/// The caution glyph's fill: Apple's default Yellow, with dark ink.
pub(crate) const CAUTION: Color32 = Color32::from_rgb(0xFF, 0xCC, 0x00);

// The window controls drawn on in-window dialogs, as macOS colors them.
pub(crate) const WINDOW_CLOSE: Color32 = Color32::from_rgb(0xFF, 0x5F, 0x57);
pub(crate) const WINDOW_MINIMIZE: Color32 = Color32::from_rgb(0xFE, 0xBC, 0x2E);
pub(crate) const WINDOW_ZOOM: Color32 = Color32::from_rgb(0x28, 0xC8, 0x40);

// Metrics, in points.
pub(crate) const MENU_BAR_HEIGHT: f32 = 24.0;
pub(crate) const TOOLBAR_HEIGHT: f32 = 54.0;
pub(crate) const LAYOUT_BAR_HEIGHT: f32 = 25.0;
pub(crate) const FORMATTING_BAR_HEIGHT: f32 = 27.0;
pub(crate) const MENU_ROW_HEIGHT: f32 = 22.0;
pub(crate) const INSPECTOR_WIDTH: f32 = 252.0;

// Corner radii, in points.
/// Buttons, fields, list rows and other controls.
pub(crate) const CONTROL_RADIUS: u8 = 5;
/// Menus, popovers and grouped lists.
pub(crate) const MENU_RADIUS: u8 = 7;
/// Windows, sheets and dialogs.
pub(crate) const WINDOW_RADIUS: u8 = 10;

// Type sizes, in points: macOS's built-in text styles.
/// Subheadline: toolbar captions, layout-bar labels and other small chrome
/// text.
pub(crate) const SMALL: f32 = 11.0;
/// Body: menus, dialogs and body text.
pub(crate) const BODY: f32 = 13.0;
/// Title 3: section headings inside a window.
pub(crate) const TITLE3: f32 = 15.0;
/// Title 2: the app's name in Home's header.
pub(crate) const TITLE2: f32 = 17.0;
/// A page's heading inside a window, as Home's: measured, between Title 2
/// and Title 1.
pub(crate) const PAGE_TITLE: f32 = 19.0;

/// The semibold UI face, for titles and the app menu.
pub(crate) fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// The family Preview draws a report line in: the bundled face the PDF
/// embeds for it (`report::ReportFace`), so both show the same glyphs.
pub(crate) fn report_family(face: crate::report::ReportFace) -> FontFamily {
    use crate::report::ReportFace;
    match face {
        ReportFace::Inter => FontFamily::Proportional,
        ReportFace::InterBold => semibold(),
        ReportFace::Sans => FontFamily::Name("report-liberation-sans".into()),
        ReportFace::SansBold => FontFamily::Name("report-liberation-sans-bold".into()),
        ReportFace::SansItalic => FontFamily::Name("report-liberation-sans-italic".into()),
        ReportFace::SansBoldItalic => FontFamily::Name("report-liberation-sans-bold-italic".into()),
    }
}

/// Use a document's installed font when available. The bundled open font is
/// the fallback; operating-system font files are never copied into the repo.
pub(crate) fn document_font(family: Option<&str>, bold: bool, italic: bool) -> FontFamily {
    // Classic layouts omit the family on some labels and inherit the
    // document's Helvetica default, rather than the application UI font.
    let name = family
        .unwrap_or("helvetica")
        .split(',')
        .next()
        .unwrap_or("")
        .trim_matches([' ', '\'', '"'])
        .to_ascii_lowercase();
    let name = match name.as_str() {
        "helvetica neue" => "helvetica-neue",
        "arial" | "helvetica" => name.as_str(),
        _ if bold => return semibold(),
        _ => return FontFamily::Proportional,
    };
    FontFamily::Name(format!("document-{name}-{}-{}", u8::from(bold), u8::from(italic)).into())
}

pub(crate) fn small() -> FontId {
    FontId::proportional(SMALL)
}

pub(crate) fn body() -> FontId {
    FontId::proportional(BODY)
}

/// Install the FileMaker look on `ctx`: the bundled UI font and the light
/// chrome style. FileMaker's captured chrome is light, so OpenMaker pins the
/// light theme. Library images decode with `egui_extras`' raster loaders
/// and OpenMaker's own SVG loader (`svg_loader`).
pub fn install(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
    crate::svg_loader::install(ctx);
    ctx.set_fonts(fonts());
    ctx.set_theme(Theme::Light);
    ctx.set_style_of(Theme::Light, style());
}

/// Inter stands in for the macOS system font (SF Pro): similar metrics, an
/// open license, and identical rendering on every platform.
fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "Inter-SemiBold".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/Inter-SemiBold.ttf"
        ))),
    );
    // Keep egui's symbol and emoji fonts as fallbacks after Inter.
    let fallbacks: Vec<String> = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "Inter".to_owned());
    let mut semibold = vec!["Inter-SemiBold".to_owned()];
    semibold.extend(fallbacks.iter().cloned());
    fonts.families.insert(self::semibold(), semibold);
    // Liberation Sans for reports (Preview): the same bytes the PDF embeds
    // for Arial and Helvetica text. Browse also draws Arial with it where
    // macOS's Arial is not installed (below).
    for (name, bytes) in [
        ("report-liberation-sans", crate::report::SANS),
        ("report-liberation-sans-bold", crate::report::SANS_BOLD),
        ("report-liberation-sans-italic", crate::report::SANS_ITALIC),
        (
            "report-liberation-sans-bold-italic",
            crate::report::SANS_BOLD_ITALIC,
        ),
    ] {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
        let mut family = vec![name.to_owned()];
        family.extend(fallbacks.iter().cloned());
        fonts.families.insert(FontFamily::Name(name.into()), family);
    }
    // Arial for document text: the system font where macOS ships it, else
    // Liberation Sans, which has Arial's metrics, so text takes the height
    // FileMaker Pro sizes its fields for (Inter's taller descenders cut
    // 16 pt text in a 24 pt field, #853). Then Inter and the same symbol
    // and emoji fallbacks as the UI font.
    for (bold, italic, file) in [
        (false, false, "Arial.ttf"),
        (true, false, "Arial Bold.ttf"),
        (false, true, "Arial Italic.ttf"),
        (true, true, "Arial Bold Italic.ttf"),
    ] {
        let name = format!("document-arial-{}-{}", u8::from(bold), u8::from(italic));
        let inter = if bold { "Inter-SemiBold" } else { "Inter" };
        let liberation = if bold {
            "report-liberation-sans-bold"
        } else {
            "report-liberation-sans"
        };
        let family = system_arial(&mut fonts, &name, file)
            .into_iter()
            .chain([liberation.to_owned(), inter.to_owned()])
            .chain(fallbacks.iter().cloned())
            .collect();
        fonts.families.insert(FontFamily::Name(name.into()), family);
    }
    // Both Helvetica collections use these four indices for the regular,
    // bold, italic and bold italic faces. Substituting the UI font can
    // wrap the final word of a Text label into a clipped line.
    for (font, file) in [
        ("helvetica", "Helvetica.ttc"),
        ("helvetica-neue", "HelveticaNeue.ttc"),
    ] {
        let installed = system_font_collection(file);
        for (bold, italic, index) in [
            (false, false, 0),
            (true, false, 1),
            (false, true, 2),
            (true, true, 3),
        ] {
            let name = format!("document-{font}-{}-{}", u8::from(bold), u8::from(italic));
            let mut family = Vec::new();
            if let Some(data) = &installed {
                let mut data = data.clone();
                data.index = index;
                fonts.font_data.insert(name.clone(), Arc::new(data));
                family.push(name.clone());
            }
            family.push(if bold { "Inter-SemiBold" } else { "Inter" }.to_owned());
            family.extend(fallbacks.iter().cloned());
            fonts.families.insert(FontFamily::Name(name.into()), family);
        }
    }
    fonts
}

fn system_font_collection(file: &str) -> Option<FontData> {
    #[cfg(target_os = "macos")]
    {
        let directory = std::path::Path::new("/System/Library/Fonts");
        std::fs::read(directory.join(file))
            .ok()
            .map(FontData::from_owned)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        None
    }
}

/// Load macOS's Arial `file` as font `name`, when it is installed.
fn system_arial(fonts: &mut FontDefinitions, name: &str, file: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let directory = std::path::Path::new("/System/Library/Fonts/Supplemental");
        let bytes = std::fs::read(directory.join(file)).ok()?;
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_owned(bytes)));
        Some(name.to_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (fonts, name, file);
        None
    }
}

fn style() -> Style {
    let mut style = Style {
        text_styles: [
            (TextStyle::Small, FontId::proportional(SMALL)),
            (TextStyle::Body, FontId::proportional(BODY)),
            (TextStyle::Button, FontId::proportional(BODY)),
            (TextStyle::Heading, FontId::new(TITLE3, semibold())),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into(),
        visuals: visuals(),
        ..Style::default()
    };

    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(6.0, 4.0);
    spacing.button_padding = Vec2::new(10.0, 2.0);
    spacing.interact_size = Vec2::new(24.0, 22.0);
    spacing.menu_margin = Margin::same(5);
    spacing.menu_spacing = 0.0;
    style
}

fn visuals() -> Visuals {
    let mut visuals = Visuals::light();
    visuals.panel_fill = CONTENT_BG;
    visuals.window_fill = DIALOG_BG;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_corner_radius = CornerRadius::same(WINDOW_RADIUS);
    visuals.window_shadow = Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: Color32::from_black_alpha(60),
    };
    visuals.menu_corner_radius = CornerRadius::same(MENU_RADIUS);
    visuals.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(45),
    };
    visuals.extreme_bg_color = CONTROL_BG;
    visuals.faint_bg_color = ALTERNATE_ROW;
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = SELECTED_TEXT_BG;
    visuals.selection.stroke = Stroke::new(1.0, FOCUS);
    visuals.text_cursor.stroke = Stroke::new(1.5, ACCENT);
    visuals.override_text_color = None;
    visuals.weak_text_color = Some(WEAK_TEXT);

    // Compact white rounded buttons with a gray outline, as in FileMaker's
    // bars and dialogs.
    let radius = CornerRadius::same(CONTROL_RADIUS);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.noninteractive.corner_radius = radius;
    widgets.inactive.weak_bg_fill = CONTROL_BG;
    widgets.inactive.bg_fill = CONTROL_BG;
    widgets.inactive.bg_stroke = Stroke::new(1.0, BUTTON_BORDER);
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.inactive.corner_radius = radius;
    widgets.hovered.weak_bg_fill = Color32::from_gray(0xFA);
    widgets.hovered.bg_fill = Color32::from_gray(0xFA);
    widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(0xA8));
    widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.hovered.corner_radius = radius;
    widgets.hovered.expansion = 0.0;
    widgets.active.weak_bg_fill = HOVER_FILL;
    widgets.active.bg_fill = HOVER_FILL;
    widgets.active.bg_stroke = Stroke::new(1.0, POPUP_BORDER);
    widgets.active.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.active.corner_radius = radius;
    widgets.active.expansion = 0.0;
    widgets.open = widgets.active;
    visuals
}

/// The menu-bar title buttons: transparent until hovered or open, then a
/// rounded gray pill, as macOS highlights an open menu title.
pub(crate) fn menu_bar_style(style: &mut Style) {
    egui::containers::menu::menu_style(style);
    style.spacing.button_padding = Vec2::new(8.0, 2.0);
    style.spacing.item_spacing.x = 0.0;
    let fill = HOVER_FILL;
    for widget in [
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.weak_bg_fill = fill;
        widget.bg_fill = fill;
        widget.corner_radius = CornerRadius::same(4);
        widget.expansion = 0.0;
    }
}

/// Menu popups: pale rounded panels, 22 pt rows, and a blue highlight with
/// white text on the hovered or open row.
pub(crate) fn menu_popup_style(style: &mut Style) {
    egui::containers::menu::menu_style(style);
    style.spacing.button_padding = Vec2::new(4.0, 3.0);
    style.spacing.item_spacing = Vec2::new(6.0, 0.0);
    style.spacing.interact_size.y = MENU_ROW_HEIGHT;
    style.visuals.window_fill = MENU_FILL;
    style.visuals.window_stroke = Stroke::new(1.0, HAIRLINE);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    for widget in [
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.weak_bg_fill = HIGHLIGHT;
        widget.bg_fill = HIGHLIGHT;
        widget.fg_stroke = Stroke::new(1.0, ON_ACCENT);
        widget.corner_radius = CornerRadius::same(CONTROL_RADIUS);
        widget.expansion = 0.0;
    }
}

#[cfg(test)]
mod tests {
    /// Preview lays out report lines in these families; a missing one would
    /// panic in egui.
    #[test]
    fn every_report_face_has_a_preview_family() {
        let fonts = super::fonts();
        for face in crate::report::ReportFace::ALL {
            let family = super::report_family(face);
            assert!(
                matches!(family, egui::FontFamily::Proportional)
                    || fonts.families.contains_key(&family),
                "{face:?}"
            );
        }
    }
}
