//! OpenMaker's own icons, drawn for the console at FileMaker Server's sizes
//! (20 px line icons in the accent color; `docs/re/admin-console.md`
//! section 6). They are inline SVG, colored by the style sheet through
//! `currentColor`.

use maud::{html, Markup, PreEscaped};

/// A 20 px line icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    Api,
    Backup,
    Badge,
    Bell,
    Certificate,
    Check,
    Chevron,
    Chip,
    Clients,
    Database,
    Folder,
    Gear,
    Key,
    License,
    Link,
    Log,
    People,
    Person,
    Plugin,
    Preserved,
    Schedule,
    Shield,
    Web,
}

impl Icon {
    /// The drawing, in a 20x20 view box.
    fn shapes(self) -> &'static str {
        match self {
            Icon::Api => {
                r#"<path d="M7.5 3.5c-1.6 0-2.2.8-2.2 2.1v2.2c0 1.1-.6 1.8-1.8 2.2 1.2.4 1.8 1.1 1.8 2.2v2.2c0 1.3.6 2.1 2.2 2.1"/><path d="M12.5 3.5c1.6 0 2.2.8 2.2 2.1v2.2c0 1.1.6 1.8 1.8 2.2-1.2.4-1.8 1.1-1.8 2.2v2.2c0 1.3-.6 2.1-2.2 2.1"/>"#
            }
            Icon::Backup => {
                r#"<path d="M16.2 10.6a6.3 6.3 0 1 1-1.9-5"/><path d="M14.8 2.4v3.4h-3.4"/>"#
            }
            Icon::Badge => {
                r#"<path d="M11 3.5h5.5v13H11"/><path d="M3 10h9"/><path d="M9 7l3 3-3 3"/>"#
            }
            Icon::Bell => {
                r#"<path d="M10 2.8a4.6 4.6 0 0 0-4.6 4.6v3.4L3.8 13.6h12.4l-1.6-2.8V7.4A4.6 4.6 0 0 0 10 2.8z"/><path d="M8.2 15.8a1.9 1.9 0 0 0 3.6 0"/>"#
            }
            Icon::Certificate => {
                r#"<circle cx="10" cy="7.8" r="4.6"/><circle cx="10" cy="7.8" r="2"/><path d="M7.4 11.6 6.2 17.5l3.8-1.9 3.8 1.9-1.2-5.9"/>"#
            }
            Icon::Check => r#"<path d="M4.5 10.4l3.6 3.6 7.4-7.8"/>"#,
            Icon::Chevron => r#"<path d="M5.5 8l4.5 4.5L14.5 8"/>"#,
            Icon::Chip => {
                r#"<rect x="5.5" y="5.5" width="9" height="9" rx="1"/><rect x="8.2" y="8.2" width="3.6" height="3.6"/><path d="M8 3v2.5M12 3v2.5M8 14.5V17M12 14.5V17M3 8h2.5M3 12h2.5M14.5 8H17M14.5 12H17"/>"#
            }
            Icon::Clients => {
                r#"<rect x="2.5" y="4" width="10.5" height="7.8" rx="1"/><path d="M1.5 14.3h12.5"/><rect x="14.5" y="7.5" width="4.5" height="8.5" rx="1"/>"#
            }
            Icon::Database => {
                r#"<ellipse cx="10" cy="5" rx="6" ry="2.3"/><path d="M4 5v10c0 1.3 2.7 2.3 6 2.3s6-1 6-2.3V5"/><path d="M4 10c0 1.3 2.7 2.3 6 2.3s6-1 6-2.3"/>"#
            }
            Icon::Folder => r#"<path d="M2.5 5.2h5l1.6 1.8h8.4v8.8h-15z"/><path d="M2.5 8.8h15"/>"#,
            Icon::Gear => {
                r#"<circle cx="10" cy="10" r="2.3"/><circle cx="10" cy="10" r="5.2"/><circle cx="10" cy="10" r="6.6" stroke-width="2.4" stroke-dasharray="2.3 2.88" stroke-linecap="butt"/>"#
            }
            Icon::Key => {
                r#"<circle cx="6.5" cy="13.5" r="3.6"/><path d="M9.1 10.9 16.5 3.5"/><path d="M14.2 5.8l2 2M12.4 7.6l1.6 1.6"/>"#
            }
            Icon::License => {
                r#"<rect x="2" y="4.5" width="16" height="11" rx="1.2"/><circle cx="7" cy="8.8" r="1.8"/><path d="M4.4 13.2c.5-1.3 1.4-2 2.6-2s2.1.7 2.6 2"/><path d="M11.5 8.5h4.5M11.5 11.5h3.5"/>"#
            }
            Icon::Link => {
                r#"<path d="M8.4 11.6l3.2-3.2"/><path d="M9.3 6.2l1.5-1.5a3 3 0 0 1 4.3 4.3l-1.5 1.5"/><path d="M10.7 13.8l-1.5 1.5a3 3 0 0 1-4.3-4.3l1.5-1.5"/>"#
            }
            Icon::Log => {
                r#"<path d="M5 2.5h7l3 3v12H5z"/><path d="M12 2.5v3h3"/><path d="M7.5 9h5M7.5 11.5h5M7.5 14h3.5"/>"#
            }
            Icon::People => {
                r#"<circle cx="7.5" cy="7" r="2.8"/><path d="M2 16.5c.6-2.9 2.8-4.5 5.5-4.5s4.9 1.6 5.5 4.5"/><circle cx="14" cy="7.5" r="2.3"/><path d="M13.6 12.1c2.3-.1 4 1.3 4.5 3.9"/>"#
            }
            Icon::Person => {
                r#"<circle cx="10" cy="6.8" r="3.3"/><path d="M3.5 17.5c.8-3.4 3.4-5.3 6.5-5.3s5.7 1.9 6.5 5.3"/>"#
            }
            Icon::Plugin => {
                r#"<path d="M7.5 2.5v4M12.5 2.5v4"/><path d="M5 6.5h10v3.2a5 5 0 0 1-10 0z"/><path d="M10 14.7v2.8"/>"#
            }
            Icon::Preserved => {
                r#"<circle cx="10" cy="10" r="7.3"/><path d="M10 6v7.2M7 10.4l3 3 3-3"/>"#
            }
            Icon::Schedule => {
                r#"<rect x="2.5" y="4" width="15" height="13" rx="1.2"/><path d="M2.5 8h15M6.5 2.5v3M13.5 2.5v3"/><path d="M6 11h1.5M9.3 11h1.5M12.5 11H14M6 14h1.5M9.3 14h1.5"/>"#
            }
            Icon::Shield => {
                r#"<path d="M10 2.5l6 2.3v4.6c0 3.8-2.5 6.7-6 8.1-3.5-1.4-6-4.3-6-8.1V4.8z"/><path d="M7.4 10l1.8 1.8 3.5-3.6"/>"#
            }
            Icon::Web => {
                r#"<circle cx="10" cy="10" r="7.3"/><ellipse cx="10" cy="10" rx="3.1" ry="7.3"/><path d="M2.7 10h14.6M3.8 6.3h12.4M3.8 13.7h12.4"/>"#
            }
        }
    }

    /// The icon at `size` px.
    pub(crate) fn render(self, size: u32) -> Markup {
        html! {
            svg.icon width=(size) height=(size) viewBox="0 0 20 20" fill="none"
                stroke="currentColor" stroke-width="1.3" stroke-linecap="round"
                stroke-linejoin="round" aria-hidden="true" focusable="false" {
                (PreEscaped(self.shapes()))
            }
        }
    }
}

/// OpenMaker's mark: a ring on an accent-blue square.
pub(crate) fn mark(size: u32) -> Markup {
    html! {
        svg.mark width=(size) height=(size) viewBox="0 0 32 32" aria-hidden="true" focusable="false" {
            rect width="32" height="32" rx="7" fill="#2f54e5" {}
            circle cx="16" cy="16" r="7.5" fill="none" stroke="#ffffff" stroke-width="3.6" {}
        }
    }
}

/// The mark as a stand-alone SVG document, served as the favicon.
pub(crate) const FAVICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#2f54e5"/><circle cx="16" cy="16" r="7.5" fill="none" stroke="#ffffff" stroke-width="3.6"/></svg>"##;

/// An alert's severity, as an 18 px status icon.
pub(crate) fn severity(severity: crate::Severity) -> Markup {
    let (class, label, glyph) = match severity {
        crate::Severity::Critical => ("critical", "Critical", "!"),
        crate::Severity::Warning => ("warning", "Warning", "!"),
        crate::Severity::Information => ("information", "Information", "i"),
    };
    html! {
        span class={"severity " (class)} role="img" aria-label=(label) { (glyph) }
    }
}

/// A hosted file's status as an 18 px icon (`docs/re/admin-console.md`
/// 4.2): open is a circled play mark, paused two bars, closed a square,
/// and a change under way three dots. The status names it, for its
/// tooltip and screen readers.
pub(crate) fn database_status(status: crate::DatabaseStatus) -> Markup {
    use crate::DatabaseStatus::*;
    let (class, inner) = match status {
        Normal => (
            "open",
            r#"<path d="M7.3 5.9 12.3 9l-5 3.1z" fill="currentColor"/>"#,
        ),
        Paused => (
            "paused",
            r#"<path d="M7.2 6v6M10.8 6v6" stroke-width="1.6"/>"#,
        ),
        Closed => (
            "closed",
            r#"<rect x="6.4" y="6.4" width="5.2" height="5.2" fill="currentColor" stroke="none"/>"#,
        ),
        Opening | Closing | Pausing | Resuming => (
            "changing",
            r#"<circle cx="6" cy="9" r=".9" fill="currentColor"/><circle cx="9" cy="9" r=".9" fill="currentColor"/><circle cx="12" cy="9" r=".9" fill="currentColor"/>"#,
        ),
    };
    html! {
        span class={"db-status " (class)} title=(status.label()) {
            svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor"
                stroke-width="1.2" role="img" aria-label=(status.label()) focusable="false" {
                circle cx="9" cy="9" r="7" {}
                (PreEscaped(inner))
            }
        }
    }
}

/// Whether a hosted file is encrypted, as a 14 px lock; an unencrypted
/// file's lock carries an orange bar.
pub(crate) fn encryption(encrypted: bool) -> Markup {
    let label = if encrypted {
        "Encrypted"
    } else {
        "Unencrypted"
    };
    html! {
        span.db-lock title=(label) {
            svg width="14" height="14" viewBox="0 0 14 14" role="img" aria-label=(label)
                focusable="false" {
                path.db-lock-shackle d="M4 6V4.4a2.6 2.6 0 0 1 5.2 0V6" fill="none" {}
                rect.db-lock-body x="2.6" y="6" width="8" height="6.2" rx="1" {}
                @if !encrypted {
                    circle.db-lock-badge cx="10.4" cy="10.4" r="3.2" {}
                    path.db-lock-bar d="M8.7 10.4h3.4" {}
                }
            }
        }
    }
}

/// The circled chevron that opens a row's menu, 18 px.
pub(crate) fn row_menu() -> Markup {
    html! {
        svg.row-menu-icon width="18" height="18" viewBox="0 0 18 18" fill="none"
            aria-hidden="true" focusable="false" {
            circle cx="9" cy="9" r="8.4" {}
            path d="M6.2 7.8 9 10.6l2.8-2.8" {}
        }
    }
}

/// The small chevron after a dropdown's label.
pub(crate) fn dropdown_chevron() -> Markup {
    html! {
        svg.dropdown-chevron width="8" height="8" viewBox="0 0 8 8" fill="none"
            stroke="currentColor" stroke-width="1.2" aria-hidden="true" focusable="false" {
            path d="M1.2 2.8 4 5.6l2.8-2.8" {}
        }
    }
}

/// The filled triangle of a select's closed list.
pub(crate) fn select_arrow() -> Markup {
    html! {
        svg.select-arrow width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"
            focusable="false" {
            path d="M1.5 3h7L5 7.2z" fill="currentColor" {}
        }
    }
}
