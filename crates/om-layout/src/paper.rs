//! File-scoped paper preferences, independent of layout margins and renderer.
use crate::{pagination::PageSize, Layout, Rect};

/// The printable margins a layout prints within when Layout Setup's "Use
/// fixed page margins" is off: the printer's. FileMaker Pro 26 for macOS
/// prints the starter layouts, which have it off, at these (top 18, bottom 40,
/// left and right 18 points) on US Letter with Any Printer (layout-model.md
/// 4.1). OpenMaker has no printer and uses them for every paper.
pub fn printer_margins() -> Rect {
    Rect {
        top: 18.0,
        left: 18.0,
        bottom: 40.0,
        right: 18.0,
        ..Rect::default()
    }
}

/// `LayoutDef.flags` bit `0x40`: Layout Setup > Printing's "Use fixed page
/// margins" is checked, and the layout prints within its stored
/// `customMargins` (layout-model.md 4.2, #609).
pub const FIXED_PAGE_MARGINS: u64 = 0x40;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    #[default]
    Portrait,
    Landscape,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaperSettings {
    /// Unrotated sheet dimensions, in points (72 points = one inch).
    pub width: f64,
    pub height: f64,
    pub orientation: Orientation,
}

impl Default for PaperSettings {
    fn default() -> Self {
        Self {
            width: 612.0,
            height: 792.0,
            orientation: Orientation::Portrait,
        }
    }
}

impl PaperSettings {
    /// Bound custom paper to 1–200 inches on each axis. Scale is deliberately
    /// absent: the report pipeline currently supports only 100 percent.
    pub fn validate(&self) -> Result<(), String> {
        if [self.width, self.height]
            .iter()
            .any(|v| !v.is_finite() || !(72.0..=14400.0).contains(v))
        {
            return Err(
                "Paper dimensions must be between 72 and 14400 points (1–200 inches).".into(),
            );
        }
        Ok(())
    }

    /// The paper of a stored page format (`87`: a Print Setup step's, or a
    /// file's `[2, 87]`; script-steps.md 11.6): its platform-neutral `Comm`
    /// chunk. Platform chunks are skipped. A format without one, with two,
    /// or with a scale other than 100% is refused.
    pub fn from_page_format(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 1024 * 1024 {
            return Err("Page format exceeds 1 MiB".into());
        }
        let mut rest = bytes;
        let mut paper = None;
        while !rest.is_empty() {
            let header = rest.get(..8).ok_or("Truncated page format chunk header")?;
            // stored_specs.rs::print_record and its re-encoder use BE lengths.
            let len = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
            let payload = rest
                .get(8..)
                .and_then(|r| r.get(..len))
                .ok_or("Truncated page format chunk")?;
            rest = &rest[8 + len..];
            if &header[..4] != b"Comm" {
                continue;
            }
            if paper.is_some() || payload.len() != 29 {
                return Err("Invalid or duplicate common page format".into());
            }
            // Swap stream scalar order, inferred for f32 from the BE stream
            // convention (script-steps.md 11.0) and rich_text.rs paragraph f32s.
            // Native common-format acceptance remains unmeasured.
            let values: Vec<f64> = payload[1..]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|n| f64::from(f32::from_be_bytes(*n)))
                .collect();
            if values.iter().any(|v| !v.is_finite()) {
                return Err("Non-finite page format number".into());
            }
            if values[0] != 1.0 {
                return Err("Page Setup scale other than 100% is unsupported".into());
            }
            let orientation = match payload[0] {
                0 => Orientation::Portrait,
                2 => Orientation::Landscape,
                _ => return Err("Unsupported page orientation".into()),
            };
            let (left, top, right, bottom, printable_w, printable_h) = (
                values[1], values[2], values[3], values[4], values[5], values[6],
            );
            let (width, height) = (right - left, bottom - top);
            // The printable origin must lie inside the sheet. Printer insets
            // are validated but not substituted for fixed layout margins.
            if left > 0.0
                || top > 0.0
                || printable_w <= 0.0
                || printable_h <= 0.0
                || printable_w > right + 0.1
                || printable_h > bottom + 0.1
            {
                return Err("Invalid paper rectangle or printable dimensions".into());
            }
            let candidate = PaperSettings {
                width,
                height,
                orientation,
            };
            candidate.validate()?;
            paper = Some(candidate);
        }
        paper.ok_or_else(|| "Page format has no supported Comm chunk".into())
    }

    /// Before a file has explicit settings, seed its dialog from the existing
    /// layout orientation. Once accepted, one preference applies to all layouts.
    pub fn initial(layout: &Layout) -> Result<Self, String> {
        let def = layout.def.as_ref().ok_or("The layout has no definition")?;
        // 1 and 2 are portrait and landscape as macOS (`kPMPortrait`,
        // `kPMLandscape`) and Windows (`DMORIENT_*`) number them; every
        // starter solution layout stores 1. 0 is read as portrait too.
        let orientation = match def.orientation {
            0 | 1 => Orientation::Portrait,
            2 => Orientation::Landscape,
            _ => return Err("Unsupported layout page orientation".into()),
        };
        Ok(Self {
            orientation,
            ..Self::default()
        })
    }

    pub fn for_layout(&self, layout: &Layout) -> Result<PageSize, String> {
        self.validate()?;
        let def = layout.def.as_ref().ok_or("The layout has no definition")?;
        let (width, height) = match self.orientation {
            Orientation::Portrait => (self.width, self.height),
            Orientation::Landscape => (self.height, self.width),
        };
        // The stored `customMargins` apply only with "Use fixed page
        // margins" on. FileMaker Pro stores them with it off too (every
        // starter layout keeps 36 points) and then prints within the
        // printer's margins (#609). With the box checked and no rectangle
        // stored, the printer's margins are OpenMaker's choice.
        let fixed = def
            .custom_margins
            .as_ref()
            .filter(|_| def.flags & FIXED_PAGE_MARGINS != 0);
        let margins = match fixed {
            Some(m) => {
                let sides = [m.top, m.left, m.bottom, m.right];
                if sides.iter().any(|v| !v.is_finite() || *v < 0.0)
                    || m.left + m.right >= width
                    || m.top + m.bottom >= height
                {
                    return Err("The layout's fixed page margins leave no printable area".into());
                }
                Rect {
                    top: m.top,
                    left: m.left,
                    bottom: m.bottom,
                    right: m.right,
                    ..Rect::default()
                }
            }
            None => printer_margins(),
        };
        Ok(PageSize {
            width,
            height,
            margins,
        })
    }
}
