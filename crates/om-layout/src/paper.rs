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

/// The scales Page Setup accepts, 1.0 = 100%. FileMaker Pro stores 40%,
/// 60% and 100% (script-steps.md 11.6, #817); its limits are unmeasured
/// (#871), and 1% to 1000% is OpenMaker's choice.
pub const SCALES: std::ops::RangeInclusive<f64> = 0.01..=10.0;

#[derive(Clone, Debug, PartialEq)]
pub struct PaperSettings {
    /// Unrotated sheet dimensions, in points (72 points = one inch).
    pub width: f64,
    pub height: f64,
    pub orientation: Orientation,
    /// Page Setup's scale, 1.0 = 100%: the layout prints at this fraction of
    /// its size, so a page holds `1 / scale` as much of it each way.
    pub scale: f64,
}

impl Default for PaperSettings {
    fn default() -> Self {
        Self {
            width: 612.0,
            height: 792.0,
            orientation: Orientation::Portrait,
            scale: 1.0,
        }
    }
}

impl PaperSettings {
    /// Bound custom paper to 1–200 inches on each axis, and the scale to
    /// [`SCALES`].
    pub fn validate(&self) -> Result<(), String> {
        if [self.width, self.height]
            .iter()
            .any(|v| !v.is_finite() || !(72.0..=14400.0).contains(v))
        {
            return Err(
                "Paper dimensions must be between 72 and 14400 points (1–200 inches).".into(),
            );
        }
        if !SCALES.contains(&self.scale) {
            return Err("Scale must be between 1% and 1000%.".into());
        }
        Ok(())
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

    /// The page as the layout sees it, in layout points: the sheet divided
    /// by the scale, as FileMaker stores the printable size (script-steps.md
    /// 11.6). The whole page is scaled, the printer's margins too, so they
    /// stay the same on paper. That a layout's fixed page margins are in
    /// layout points too, and so print scaled, is inferred: unmeasured.
    pub fn for_layout(&self, layout: &Layout) -> Result<PageSize, String> {
        self.validate()?;
        let def = layout.def.as_ref().ok_or("The layout has no definition")?;
        let (width, height) = match self.orientation {
            Orientation::Portrait => (self.width, self.height),
            Orientation::Landscape => (self.height, self.width),
        };
        let (width, height) = (width / self.scale, height / self.scale);
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
            None => {
                let m = printer_margins();
                Rect {
                    top: m.top / self.scale,
                    left: m.left / self.scale,
                    bottom: m.bottom / self.scale,
                    right: m.right / self.scale,
                    ..Rect::default()
                }
            }
        };
        Ok(PageSize {
            width,
            height,
            margins,
            scale: self.scale,
        })
    }
}
