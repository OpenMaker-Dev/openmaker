//! FileMaker-style visuals: the chrome palette and metrics sampled from the
//! FileMaker Pro 26 captures (`docs/re/client-ui.md`), the bundled UI font,
//! and the egui [`Style`] built from them.
//!
//! Only application chrome is styled here. The content area renders the
//! file's layout, whose colors and fonts are document data.

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke,
    Style, TextStyle, Theme, Vec2, Visuals,
};

// Chrome backgrounds and rules, sampled from captures 10, 14 and 28.
pub(crate) const TOOLBAR_BG: Color32 = Color32::from_rgb(0xF4, 0xF4, 0xF4);
pub(crate) const LAYOUT_BAR_BG: Color32 = Color32::from_rgb(0xDE, 0xDE, 0xDF);
pub(crate) const LAYOUT_BAR_TOP_RULE: Color32 = Color32::from_rgb(0xB8, 0xB8, 0xB9);
pub(crate) const LAYOUT_BAR_BOTTOM_RULE: Color32 = Color32::from_rgb(0xAE, 0xAE, 0xAE);
pub(crate) const MENU_BAR_RULE: Color32 = Color32::from_rgb(0xE2, 0xE2, 0xE2);
pub(crate) const PASTEBOARD: Color32 = Color32::from_rgb(0xE5, 0xE5, 0xE5);
pub(crate) const PRINT_WORKSPACE: Color32 = Color32::from_rgb(0x9C, 0x9C, 0x9C);
pub(crate) const INSPECTOR_BG: Color32 = Color32::WHITE;
pub(crate) const DIALOG_BG: Color32 = Color32::from_rgb(0xF0, 0xF0, 0xF0);
pub(crate) const SIDEBAR_BG: Color32 = Color32::from_rgb(0xE3, 0xE3, 0xE4);
pub(crate) const CONTENT_BG: Color32 = Color32::WHITE;

// Text and glyphs.
pub(crate) const TEXT: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
pub(crate) const CAPTION: Color32 = Color32::from_rgb(0x5E, 0x5E, 0x5E);
pub(crate) const WEAK_TEXT: Color32 = Color32::from_rgb(0x8A, 0x8A, 0x8A);
pub(crate) const DISABLED: Color32 = Color32::from_rgb(0xC4, 0xC4, 0xC4);
pub(crate) const ICON: Color32 = Color32::from_rgb(0x7D, 0x7D, 0x7D);
pub(crate) const VIEW_GLYPH: Color32 = Color32::from_rgb(0xA5, 0xA5, 0xA6);
pub(crate) const DIVIDER: Color32 = Color32::from_rgb(0xB4, 0xB4, 0xB5);
pub(crate) const HAIRLINE: Color32 = Color32::from_rgb(0xD6, 0xD6, 0xD6);

// Blues: selected controls, the found-set ring, menu highlight, default
// buttons and focus rings.
pub(crate) const SELECTED: Color32 = Color32::from_rgb(0x2F, 0x54, 0xE5);
pub(crate) const RING: Color32 = Color32::from_rgb(0x7A, 0xAF, 0xE6);
pub(crate) const HIGHLIGHT: Color32 = Color32::from_rgb(0x3D, 0x86, 0xF5);
pub(crate) const DEFAULT_BUTTON: Color32 = Color32::from_rgb(0x3E, 0x62, 0xEC);
pub(crate) const FOCUS: Color32 = Color32::from_rgb(0x7C, 0x8C, 0xF4);
pub(crate) const CURRENT_ROW: Color32 = Color32::from_rgb(0xE6, 0xEF, 0xFB);

// Controls.
pub(crate) const FIELD_BORDER: Color32 = Color32::from_rgb(0xC5, 0xC5, 0xC5);
pub(crate) const BUTTON_BORDER: Color32 = Color32::from_rgb(0xB9, 0xB9, 0xBA);
pub(crate) const POPUP_BORDER: Color32 = Color32::from_rgb(0x9B, 0x9B, 0x9C);
pub(crate) const SEARCH_FILL: Color32 = Color32::from_rgb(0xEE, 0xEE, 0xEE);
pub(crate) const SEARCH_BORDER: Color32 = Color32::from_rgb(0xE0, 0xE0, 0xE0);
pub(crate) const MENU_FILL: Color32 = Color32::from_rgb(0xF5, 0xF5, 0xF5);

// Metrics, in points.
pub(crate) const MENU_BAR_HEIGHT: f32 = 24.0;
pub(crate) const TOOLBAR_HEIGHT: f32 = 54.0;
pub(crate) const LAYOUT_BAR_HEIGHT: f32 = 25.0;
pub(crate) const FORMATTING_BAR_HEIGHT: f32 = 27.0;
pub(crate) const MENU_ROW_HEIGHT: f32 = 22.0;
pub(crate) const INSPECTOR_WIDTH: f32 = 252.0;

/// Toolbar captions, layout-bar labels and other small chrome text.
pub(crate) const SMALL: f32 = 11.0;
/// Menus, dialogs and body text.
pub(crate) const BODY: f32 = 13.0;

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
    if matches!(name.as_str(), "arial" | "helvetica") {
        FontFamily::Name(format!("document-{name}-{}-{}", u8::from(bold), u8::from(italic)).into())
    } else if bold {
        semibold()
    } else {
        FontFamily::Proportional
    }
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
    #[cfg(target_os = "macos")]
    let helvetica = std::fs::read("/System/Library/Fonts/Helvetica.ttc")
        .ok()
        .map(FontData::from_owned);
    // Legacy FileMaker files commonly use Helvetica. Its metrics differ
    // enough from the UI font to clip small labels and alter line breaks.
    for (bold, italic, index) in [
        (false, false, 0),
        (true, false, 1),
        (false, true, 2),
        (true, true, 3),
    ] {
        let name = format!("document-helvetica-{}-{}", u8::from(bold), u8::from(italic));
        let mut family = Vec::new();
        #[cfg(target_os = "macos")]
        if let Some(data) = &helvetica {
            let mut data = data.clone();
            data.index = index;
            fonts.font_data.insert(name.clone(), Arc::new(data));
            family.push(name.clone());
        }
        #[cfg(not(target_os = "macos"))]
        let _ = index;
        family.push(if bold { "Inter-SemiBold" } else { "Inter" }.to_owned());
        family.extend(fallbacks.iter().cloned());
        fonts.families.insert(FontFamily::Name(name.into()), family);
    }
    fonts
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
            (TextStyle::Heading, FontId::new(15.0, semibold())),
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
    visuals.window_stroke = Stroke::new(1.0, Color32::from_gray(0xC8));
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.window_shadow = Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: Color32::from_black_alpha(60),
    };
    visuals.menu_corner_radius = CornerRadius::same(7);
    visuals.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(45),
    };
    visuals.extreme_bg_color = Color32::WHITE;
    visuals.faint_bg_color = Color32::from_rgb(0xF5, 0xF5, 0xF5);
    visuals.hyperlink_color = SELECTED;
    visuals.selection.bg_fill = Color32::from_rgb(0xB4, 0xD5, 0xFE);
    visuals.selection.stroke = Stroke::new(1.0, FOCUS);
    visuals.text_cursor.stroke = Stroke::new(1.5, SELECTED);
    visuals.override_text_color = None;
    visuals.weak_text_color = Some(WEAK_TEXT);

    // Compact white rounded buttons with a gray outline, as in FileMaker's
    // bars and dialogs.
    let radius = CornerRadius::same(5);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.noninteractive.corner_radius = radius;
    widgets.inactive.weak_bg_fill = Color32::WHITE;
    widgets.inactive.bg_fill = Color32::WHITE;
    widgets.inactive.bg_stroke = Stroke::new(1.0, BUTTON_BORDER);
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.inactive.corner_radius = radius;
    widgets.hovered.weak_bg_fill = Color32::from_gray(0xFA);
    widgets.hovered.bg_fill = Color32::from_gray(0xFA);
    widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(0xA8));
    widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.hovered.corner_radius = radius;
    widgets.hovered.expansion = 0.0;
    widgets.active.weak_bg_fill = Color32::from_gray(0xE4);
    widgets.active.bg_fill = Color32::from_gray(0xE4);
    widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_gray(0x9C));
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
    let fill = Color32::from_gray(0xDD);
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
    style.visuals.window_stroke = Stroke::new(1.0, Color32::from_gray(0xD2));
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(0xDE));
    for widget in [
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.weak_bg_fill = HIGHLIGHT;
        widget.bg_fill = HIGHLIGHT;
        widget.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        widget.corner_radius = CornerRadius::same(5);
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
