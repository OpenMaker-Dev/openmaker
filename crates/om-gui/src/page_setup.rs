//! Portable Page Setup; native records are preserved as provenance, never
//! dispatched to a printer or interpreted as layout margin preferences.
use om_layout::paper::{Orientation, PaperSettings};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Settings {
    pub paper: PaperSettings,
    /// Original key 87 bytes, including opaque platform chunks. UI changes do
    /// not rewrite this source record; only `paper` controls portable reports.
    pub native_page_format: Option<Vec<u8>>,
}

impl Settings {
    pub fn new(paper: PaperSettings) -> Self {
        Self {
            paper,
            native_page_format: None,
        }
    }

    /// The paper a file's stored page setup gives (`[2, 87]`,
    /// `CalcSchema::page_format`), when its common format reads: the file
    /// session's paper until a Page Setup is accepted in it.
    pub fn stored_paper(
        schema: Option<&om_data::host::CalcSchema>,
    ) -> Option<om_layout::paper::PaperSettings> {
        let bytes = schema?.page_format.as_deref()?;
        Self::restored(bytes).ok().map(|settings| settings.paper)
    }

    pub fn restored(bytes: &[u8]) -> Result<Self, String> {
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
            let (left, top, right, bottom) = (values[1], values[2], values[3], values[4]);
            // The rectangle is the upright sheet in either orientation, while
            // the printable size is the page's own: a landscape US Letter
            // stores 734 x 576 on the sheet -18, -18 to 594, 774 [O: Comm
            // chunks FileMaker Pro 26 for macOS saved, #817]. Turned upright,
            // it is checked against the sheet like a portrait one.
            let (printable_w, printable_h) = match orientation {
                Orientation::Portrait => (values[5], values[6]),
                Orientation::Landscape => (values[6], values[5]),
            };
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
        Ok(Self {
            paper: paper.ok_or("Page format has no supported Comm chunk")?,
            native_page_format: Some(bytes.to_vec()),
        })
    }
}

pub(crate) fn dialog(
    ui: &mut egui::Ui,
    settings: &mut Settings,
) -> Option<crate::scripting::Answer> {
    use crate::{scripting::Answer, widgets};
    let paper = &mut settings.paper;
    ui.heading("Page Setup");
    ui.label("Paper settings apply to every layout in this file for this open session.");
    ui.add_space(8.0);
    let sizes = [
        ("US Letter", 612.0, 792.0),
        ("US Legal", 612.0, 1008.0),
        ("Tabloid", 792.0, 1224.0),
        ("A4", 595.2756, 841.8898),
        ("A3", 841.8898, 1190.5512),
        ("A5", 419.5276, 595.2756),
    ];
    let selected = sizes
        .iter()
        .find(|(_, w, h)| (paper.width - w).abs() < 0.1 && (paper.height - h).abs() < 0.1)
        .map_or("Custom", |s| s.0);
    egui::ComboBox::from_label("Paper size")
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (name, width, height) in sizes {
                if ui.selectable_label(selected == name, name).clicked() {
                    paper.width = width;
                    paper.height = height;
                }
            }
        });
    ui.horizontal(|ui| {
        let width_label = ui.label("Width (pt)");
        ui.add(
            egui::DragValue::new(&mut paper.width)
                .speed(1.0)
                .range(72.0..=14400.0),
        )
        .labelled_by(width_label.id);
        let height_label = ui.label("Height (pt)");
        ui.add(
            egui::DragValue::new(&mut paper.height)
                .speed(1.0)
                .range(72.0..=14400.0),
        )
        .labelled_by(height_label.id);
    });
    ui.label("Custom dimensions: 72 points = 1 inch; enter unrotated sheet dimensions.");
    ui.horizontal(|ui| {
        ui.label("Orientation");
        ui.selectable_value(&mut paper.orientation, Orientation::Portrait, "Portrait");
        ui.selectable_value(&mut paper.orientation, Orientation::Landscape, "Landscape");
    });
    ui.label("Scale: 100%. Layout margins are preserved.");
    let valid = paper.validate();
    if let Err(message) = &valid {
        ui.colored_label(crate::theme::RED_INK, message);
    }
    ui.add_space(12.0);
    let mut answer = None;
    ui.horizontal(|ui| {
        if widgets::dialog_button(ui, "Cancel", false, true).clicked()
            || ui.input(|i| i.key_pressed(egui::Key::Escape))
        {
            answer = Some(Answer::PageSetup(None));
        }
        if (widgets::dialog_button(ui, "OK", true, valid.is_ok()).clicked()
            || ui.input(|i| i.key_pressed(egui::Key::Enter)))
            && valid.is_ok()
            && answer.is_none()
        {
            answer = Some(Answer::PageSetup(Some(settings.clone())));
        }
    });
    answer
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    // Literal synthetic Swap-stream fixture: A4 portrait, 100%, 8.5pt insets,
    // plus an opaque platform record. No catalog/customer bytes are used.
    pub(crate) const A4: &[u8] = b"Comm\x00\x00\x00\x1d\x00\x3f\x80\x00\x00\xc1\x08\x00\x00\xc1\x08\x00\x00\x44\x12\xb1\x48\x44\x50\x57\x0a\x44\x10\x91\x48\x44\x4e\x37\x0aM_PM\x00\x00\x00\x03raw";

    /// The `Comm` chunk FileMaker Pro 26 for macOS stored in a file's page
    /// setup `[2, 87]` (before its `M_PM` print record, left out here): US
    /// Letter portrait at 100%, the sheet -18, -18 to 594, 774 around a
    /// 576 x 734 printable area, the printer's 18 / 40 / 18 / 18 margins.
    pub(crate) const STORED_LETTER: &[u8] = b"Comm\x00\x00\x00\x1d\x00\x3f\x80\x00\x00\xc1\x90\x00\x00\xc1\x90\x00\x00\x44\x14\x80\x00\x44\x41\x80\x00\x44\x10\x00\x00\x44\x37\x80\x00";

    /// The same, landscape, from a Print Setup step FileMaker Pro 26 for
    /// macOS saved: the sheet is still upright, and the printable size is
    /// the page's own, 734 x 576 (#817).
    const STORED_LETTER_LANDSCAPE: &[u8] = b"Comm\x00\x00\x00\x1d\x02\x3f\x80\x00\x00\xc1\x90\x00\x00\xc1\x90\x00\x00\x44\x14\x80\x00\x44\x41\x80\x00\x44\x37\x80\x00\x44\x10\x00\x00";

    /// `format` (one `Comm` chunk first) turned landscape the way FileMaker
    /// stores it: the orientation byte, and the printable size swapped.
    pub(crate) fn landscape(format: &[u8]) -> Vec<u8> {
        let mut bytes = format.to_vec();
        bytes[8] = 2;
        let (width, height) = (bytes[29..33].to_vec(), bytes[33..37].to_vec());
        bytes[29..33].copy_from_slice(&height);
        bytes[33..37].copy_from_slice(&width);
        bytes
    }

    #[test]
    fn page_setup_common_format_is_bounded_and_preserves_native_records() {
        let settings = Settings::restored(A4).unwrap();
        assert!((settings.paper.width - 595.27).abs() < 0.01);
        assert!((settings.paper.height - 841.86).abs() < 0.01);
        assert_eq!(settings.native_page_format.as_deref(), Some(A4));
        // Native bytes read as the sheet they describe: the big-endian f32
        // reading holds for what FileMaker Pro writes.
        let stored = Settings::restored(STORED_LETTER).unwrap();
        assert_eq!(
            (
                stored.paper.width,
                stored.paper.height,
                stored.paper.orientation
            ),
            (612.0, 792.0, Orientation::Portrait)
        );
        let stored = Settings::restored(STORED_LETTER_LANDSCAPE).unwrap();
        assert_eq!(
            (
                stored.paper.width,
                stored.paper.height,
                stored.paper.orientation
            ),
            (612.0, 792.0, Orientation::Landscape)
        );
        assert_eq!(landscape(STORED_LETTER), STORED_LETTER_LANDSCAPE);
        for bad in [
            A4[..A4.len() - 1].to_vec(),
            {
                let mut b = A4.to_vec();
                b[9..13].copy_from_slice(&0.5f32.to_be_bytes());
                b
            },
            {
                let mut b = A4.to_vec();
                b[13..17].copy_from_slice(&f32::NAN.to_be_bytes());
                b
            },
            {
                let mut b = A4.to_vec();
                b.extend_from_slice(A4);
                b
            },
        ] {
            assert!(Settings::restored(&bad).is_err());
        }
        let mut layout = om_layout::Layout {
            def: Some(om_layout::LayoutDef {
                custom_margins: Some(om_layout::Rect {
                    left: 20.0,
                    top: 30.0,
                    right: 40.0,
                    bottom: 50.0,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut landscape = settings.paper.clone();
        landscape.orientation = Orientation::Landscape;
        // Stored margins apply only with "Use fixed page margins" on, flag
        // 0x40 (#609): off, the page uses the printer's.
        let page = landscape.for_layout(&layout).unwrap();
        assert_eq!(page.margins, om_layout::paper::printer_margins());
        layout.def.as_mut().unwrap().flags |= om_layout::paper::FIXED_PAGE_MARGINS;
        let page = landscape.for_layout(&layout).unwrap();
        let m = page.margins;
        assert_eq!((m.top, m.left, m.bottom, m.right), (30.0, 20.0, 50.0, 40.0));
        layout.def.as_mut().unwrap().flags &= !om_layout::paper::FIXED_PAGE_MARGINS;
        assert_eq!(
            (page.width, page.height),
            (landscape.height, landscape.width)
        );
        layout.def.as_mut().unwrap().orientation = 99;
        assert!(
            landscape.for_layout(&layout).is_ok(),
            "explicit file preference overrides layout orientation"
        );
    }
}
