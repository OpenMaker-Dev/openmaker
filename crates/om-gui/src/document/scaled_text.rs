//! Layer zoom must scale glyph rasterization as well as text geometry.
//! Build atlas glyphs at their final size, then return logical coordinates
//! for document painting and cursor hit testing. Application chrome uses
//! its ordinary font view and is unaffected.

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Weak},
};

use egui::{
    emath::TSTransform,
    epaint::{text::FontsView, TextShape},
    text::{Galley, LayoutJob},
    Color32, Pos2,
};

const MAX_ENTRIES: usize = 512;
const MAX_VERTICES: usize = 262_144;
const MAX_TEXT_BYTES: usize = 1_048_576;

struct Entry {
    // The font cache owns this key. Asking it for the scaled job on every
    // call keeps the atlas alive; an atlas/font reset produces a new key.
    source: Weak<Galley>,
    logical: Arc<Galley>,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<(usize, u32), Entry>,
    vertices: usize,
    text_bytes: usize,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

pub(super) fn layout_galley(fonts: &mut FontsView<'_>, job: LayoutJob, scale: f32) -> Arc<Galley> {
    if !scale.is_finite() || scale <= 0.0 || scale == 1.0 {
        return super::rich_text::layout_galley(fonts, job);
    }
    let scale = scale.clamp(0.25, 4.0);
    let logical_job = job;
    let mut scaled_job = logical_job.clone();
    // Preserve epaint's effective wrapping tolerance in logical units.
    let tolerance = if scaled_job.round_output_to_gui {
        0.5
    } else {
        0.0
    };
    scaled_job.wrap.max_width = (scaled_job.wrap.max_width + tolerance) * scale - tolerance;
    scaled_job.first_row_min_height *= scale;
    for section in &mut scaled_job.sections {
        section.leading_space *= scale;
        let format = &mut section.format;
        format.font_id.size *= scale;
        format.extra_letter_spacing *= scale;
        format.line_height = format.line_height.map(|height| height * scale);
        format.expand_bg *= scale;
        format.underline.width *= scale;
        format.strikethrough.width *= scale;
    }
    let source = fonts.layout_job(scaled_job.clone());
    let key = Arc::downgrade(&source);
    let address = (Arc::as_ptr(&source) as usize, scale.to_bits());
    if let Some(cached) = CACHE.with(|cache| {
        cache
            .borrow()
            .entries
            .get(&address)
            .filter(|entry| Weak::ptr_eq(&entry.source, &key) && *entry.logical.job == logical_job)
            .map(|entry| entry.logical.clone())
    }) {
        return cached;
    }

    let original = super::rich_text::layout_galley(fonts, logical_job.clone());
    // Font hinting can change a near-edge word break when the font size
    // changes. Keep the logical line plan and rasterize each of those
    // lines at the final size. Refuse to reconstruct unsupported shaping
    // or elision rather than changing source character/cursor offsets.
    let Some(forced_job) = force_logical_rows(&scaled_job, &original) else {
        return original;
    };
    let scaled = super::rich_text::layout_galley(fonts, forced_job);
    if scaled.rows.len() != original.rows.len()
        || scaled.rows.iter().zip(&original.rows).any(|(a, b)| {
            !a.glyphs
                .iter()
                .map(|g| g.chr)
                .eq(b.glyphs.iter().map(|g| g.chr))
        })
    {
        return original;
    }
    let mut shape = TextShape::new(Pos2::ZERO, scaled, Color32::PLACEHOLDER);
    let inverse = scale.recip();
    shape.transform(TSTransform::from_scaling(inverse));
    let logical = Arc::make_mut(&mut shape.galley);
    logical.job = Arc::new(logical_job);
    // TextShape::transform scales meshes/rows/intrinsic_size, but not the
    // public glyph metrics. Keep caret and selection in the same units.
    for (row, original_row) in logical.rows.iter_mut().zip(&original.rows) {
        row.ends_with_newline = original_row.ends_with_newline;
        for glyph in &mut Arc::make_mut(&mut row.row).glyphs {
            glyph.pos *= inverse;
            glyph.advance_width *= inverse;
            glyph.line_height *= inverse;
            glyph.font_ascent *= inverse;
            glyph.font_height *= inverse;
            glyph.font_face_ascent *= inverse;
            glyph.font_face_height *= inverse;
            glyph.uv_rect.offset *= inverse;
            glyph.uv_rect.size *= inverse;
            // min/max are atlas texels, and deliberately stay full size.
        }
    }
    let logical = shape.galley;
    let text_bytes = logical.job.text.len();
    if logical.num_vertices <= MAX_VERTICES && text_bytes <= MAX_TEXT_BYTES {
        CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            // The font cache rounds wrap widths. Different logical jobs
            // can therefore have the same raster key, even at one scale.
            if let Some(previous) = cache.entries.remove(&address) {
                cache.vertices -= previous.logical.num_vertices;
                cache.text_bytes -= previous.logical.job.text.len();
            }
            if cache.entries.len() >= MAX_ENTRIES
                || cache.vertices + logical.num_vertices > MAX_VERTICES
                || cache.text_bytes + text_bytes > MAX_TEXT_BYTES
            {
                cache.entries.clear();
                cache.vertices = 0;
                cache.text_bytes = 0;
            }
            cache.vertices += logical.num_vertices;
            cache.text_bytes += text_bytes;
            cache.entries.insert(
                address,
                Entry {
                    source: key,
                    logical: logical.clone(),
                },
            );
        });
    }
    logical
}

fn force_logical_rows(job: &LayoutJob, original: &Galley) -> Option<LayoutJob> {
    if original.elided || job.justify || job.sections.iter().any(|s| s.leading_space != 0.0) {
        return None;
    }
    let mut reconstructed = String::new();
    for row in &original.rows {
        reconstructed.extend(row.glyphs.iter().map(|glyph| glyph.chr));
        if row.ends_with_newline {
            reconstructed.push('\n');
        }
    }
    if reconstructed != job.text {
        return None;
    }
    let mut forced = job.clone();
    forced.clear();
    forced.wrap.max_width = f32::INFINITY;
    let mut start = 0;
    for (index, row) in original.rows.iter().enumerate() {
        let end = start
            + row
                .glyphs
                .iter()
                .map(|glyph| glyph.chr.len_utf8())
                .sum::<usize>();
        for section in &job.sections {
            let from = section.byte_range.start.0.max(start);
            let to = section.byte_range.end.0.min(end);
            if from < to {
                forced.append(&job.text[from..to], 0.0, section.format.clone());
            }
        }
        if index + 1 < original.rows.len() {
            let format = job.format_at_byte(end.into()).clone();
            forced.append("\n", 0.0, format);
        }
        start = end + usize::from(row.ends_with_newline);
    }
    // Keep a format for an all-empty string too.
    if forced.sections.is_empty() {
        forced.append("", 0.0, job.sections.first()?.format.clone());
    }
    Some(forced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{text::CharIndex, FontId, Vec2};

    fn with_fonts(test: impl FnOnce(&mut FontsView<'_>)) {
        let mut fonts = egui::epaint::text::Fonts::new(
            egui::epaint::TextOptions::default(),
            egui::FontDefinitions::default(),
        );
        test(&mut fonts.with_pixels_per_point(2.0));
    }

    #[test]
    fn zoom_uses_larger_atlas_glyphs_with_logical_cursor_coordinates() {
        with_fonts(|fonts| {
            let job = LayoutJob::simple(
                "Welcome!".into(),
                FontId::proportional(12.0),
                Color32::BLACK,
                200.0,
            );
            let plain = super::super::rich_text::layout_galley(fonts, job.clone());
            for scale in [0.5, 1.5, 2.0, 4.0] {
                let zoomed = layout_galley(fonts, job.clone(), scale);
                assert_eq!(*zoomed.job, job);
                assert_eq!(zoomed.pixels_per_point, 2.0);
                assert_eq!(zoomed.rows.len(), plain.rows.len());
                assert!(
                    (zoomed.size() - plain.size()).length() < 1.0,
                    "scale={scale}"
                );
                for row in &zoomed.rows {
                    for (index, glyph) in row.glyphs.iter().enumerate() {
                        let cursor = zoomed.cursor_from_pos(
                            row.pos.to_vec2()
                                + Vec2::new(
                                    glyph.pos.x + glyph.advance_width * 0.1,
                                    row.size.y * 0.5,
                                ),
                        );
                        assert_eq!(cursor.index, CharIndex(index), "scale={scale}");
                    }
                }
                let first = &zoomed.rows[0].glyphs[0];
                let width_px = f32::from(first.uv_rect.max[0] - first.uv_rect.min[0]);
                assert!((width_px - first.uv_rect.size.x * 2.0 * scale).abs() < 0.001);
                if scale > 1.0 {
                    let base = &plain.rows[0].glyphs[0];
                    assert!(
                        first.uv_rect.max[0] - first.uv_rect.min[0]
                            > base.uv_rect.max[0] - base.uv_rect.min[0]
                    );
                }
            }
        });
    }

    #[test]
    fn zoom_cache_reuses_converted_galley_and_tracks_font_cache_identity() {
        with_fonts(|fonts| {
            let job = LayoutJob::simple(
                "cached".into(),
                FontId::proportional(12.0),
                Color32::BLACK,
                200.0,
            );
            let first = layout_galley(fonts, job.clone(), 1.5);
            let second = layout_galley(fonts, job.clone(), 1.5);
            assert!(Arc::ptr_eq(&first, &second));
            let other_scale = layout_galley(fonts, job.clone(), 2.0);
            assert!(!Arc::ptr_eq(&first, &other_scale));
            with_fonts(|other_fonts| {
                let other_atlas = layout_galley(other_fonts, job, 1.5);
                assert!(!Arc::ptr_eq(&first, &other_atlas));
            });
        });
    }

    #[test]
    fn equal_raster_jobs_do_not_share_different_logical_scales() {
        with_fonts(|fonts| {
            let mut first = LayoutJob::simple(
                "same raster".into(),
                FontId::proportional(12.0),
                Color32::BLACK,
                200.0,
            );
            first.round_output_to_gui = false;
            first.sections[0].format.expand_bg = 0.0;
            let a = layout_galley(fonts, first, 1.5);
            let mut second = LayoutJob::simple(
                "same raster".into(),
                FontId::proportional(9.0),
                Color32::BLACK,
                150.0,
            );
            second.round_output_to_gui = false;
            second.sections[0].format.expand_bg = 0.0;
            let b = layout_galley(fonts, second.clone(), 2.0);
            assert_eq!(*b.job, second);
            assert!((a.size().x * 0.75 - b.size().x).abs() < 0.01);
        });
    }

    #[test]
    fn rounded_raster_width_does_not_replace_the_logical_job() {
        with_fonts(|fonts| {
            let first = LayoutJob::simple(
                "same raster".into(),
                FontId::proportional(12.0),
                Color32::BLACK,
                200.1,
            );
            let a = layout_galley(fonts, first, 0.5);
            let second = LayoutJob::simple(
                "same raster".into(),
                FontId::proportional(12.0),
                Color32::BLACK,
                200.2,
            );
            let b = layout_galley(fonts, second.clone(), 0.5);
            assert_eq!(*b.job, second);
            assert!(!Arc::ptr_eq(&a, &b));
        });
    }

    #[test]
    fn explicit_pitch_snaps_to_the_final_physical_pixel_grid() {
        with_fonts(|fonts| {
            let mut job = LayoutJob::simple(
                "one\ntwo\nthree\nfour".into(),
                FontId::proportional(12.0),
                Color32::BLACK,
                200.0,
            );
            job.sections[0].format.line_height = Some(16.8);
            for scale in [0.5, 1.5, 2.0, 4.0] {
                let zoomed = layout_galley(fonts, job.clone(), scale);
                for (index, row) in zoomed.rows.iter().enumerate() {
                    assert!(
                        (row.pos.y * scale * 2.0 - (index as f32 * 16.8 * scale * 2.0).round())
                            .abs()
                            < 0.001
                    );
                }
            }
        });
    }

    #[test]
    fn zoom_keeps_wrapping_and_multiline_cursor_offsets_in_document_units() {
        with_fonts(|fonts| {
            for text in [
                "A customer gets 5-Samples in the next package",
                "naïve café 👋 e\u{301}\nSecond line of text",
            ] {
                for width in [40.0, 60.0, 90.0, 150.0] {
                    let job = LayoutJob::simple(
                        text.into(),
                        FontId::proportional(12.0),
                        Color32::BLACK,
                        width,
                    );
                    let plain = super::super::rich_text::layout_galley(fonts, job.clone());
                    let expected: Vec<_> = plain.rows.iter().map(|row| row.text()).collect();
                    for scale in [0.5, 1.5, 2.0, 4.0] {
                        let zoomed = layout_galley(fonts, job.clone(), scale);
                        let actual: Vec<_> = zoomed.rows.iter().map(|row| row.text()).collect();
                        assert_eq!(actual, expected, "scale={scale}, width={width}");
                        let mut start = 0;
                        for row in &zoomed.rows {
                            for (index, glyph) in row.glyphs.iter().enumerate() {
                                if glyph.advance_width <= 0.0 {
                                    // Combining/continuation characters share
                                    // a caret position with the next glyph.
                                    continue;
                                }
                                let cursor = zoomed.cursor_from_pos(
                                    row.pos.to_vec2()
                                        + Vec2::new(
                                            glyph.pos.x + glyph.advance_width * 0.1,
                                            row.size.y * 0.5,
                                        ),
                                );
                                assert_eq!(cursor.index, CharIndex(start + index), "scale={scale}");
                            }
                            start += row.glyphs.len() + usize::from(row.ends_with_newline);
                        }
                    }
                }
            }
        });
    }
}
