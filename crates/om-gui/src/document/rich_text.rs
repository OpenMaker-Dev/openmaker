//! The layout's static styled text is distinct from a field's displayed value.
//! Never reuse source offsets after merge/calculation/edit text has changed.

use std::{cell::RefCell, ops::Range, sync::Arc};

use egui::{
    epaint::text::FontsView,
    text::{Galley, LayoutJob},
    Align, Color32, FontData, FontId, Stroke, TextFormat,
};
use om_data::host::layout::TextSubstitution;
use om_layout::{CharacterStyle, Color, ResolvedStyle, StyledText};
use skrifa::MetadataProvider;

use crate::theme;

/// Keep stored runs on literal text. A replacement uses the style at its
/// opening delimiter, so boundaries inside it move to the end of the
/// displayed value (layout-model.md 8.2).
pub(super) fn substitute(
    rich: &StyledText,
    shown: &str,
    substitutions: &[TextSubstitution],
) -> StyledText {
    fn boundary(at: usize, substitutions: &[TextSubstitution]) -> usize {
        let passed = substitutions.partition_point(|s| s.source.end <= at);
        if let Some(s) = substitutions.get(passed).filter(|s| s.source.start < at) {
            return s.display.end;
        }
        if passed == 0 {
            at
        } else {
            let last = &substitutions[passed - 1];
            last.display.end + (at - last.source.end)
        }
    }

    fn runs<T: Clone>(
        runs: &[om_layout::StyledRun<T>],
        source: &str,
        display: &str,
        substitutions: &[TextSubstitution],
    ) -> Vec<om_layout::StyledRun<T>> {
        runs.iter()
            .filter_map(|run| {
                source.get(run.range.clone())?;
                let start = boundary(run.range.start, substitutions);
                let end = boundary(run.range.end, substitutions);
                (start < end && display.get(start..end).is_some()).then(|| om_layout::StyledRun {
                    range: start..end,
                    style: run.style.clone(),
                })
            })
            .collect()
    }

    StyledText {
        text: shown.to_owned(),
        character_runs: runs(&rich.character_runs, &rich.text, shown, substitutions),
        paragraph_runs: runs(&rich.paragraph_runs, &rich.text, shown, substitutions),
    }
}

/// Calculation values keep their own styles as text is sliced or replaced.
/// Convert those runs rather than borrowing a layout label's unrelated runs.
pub(super) fn value_text(text: &om_data::calc::Text) -> StyledText {
    StyledText {
        text: text.as_str().to_owned(),
        character_runs: text
            .styled_runs()
            .into_iter()
            .map(|(range, style)| om_layout::StyledRun {
                range,
                style: CharacterStyle {
                    font_family: style.font,
                    font_size: style.size.map(f64::from),
                    color: style.color.map(|rgb| Color {
                        red: ((rgb >> 16) & 255) as f32 / 255.0,
                        green: ((rgb >> 8) & 255) as f32 / 255.0,
                        blue: (rgb & 255) as f32 / 255.0,
                        alpha: 1.0,
                    }),
                    faces: (style.flags != 0).then_some(style.flags as u16),
                    face_mask: (style.flags != 0)
                        .then_some((style.flags | (style.flags >> 16)) as u16),
                    ..Default::default()
                },
            })
            .collect(),
        ..Default::default()
    }
}

/// FileMaker applies conditional formatting after stored character styles.
/// A condition changes only the attributes it specifies; the remaining
/// character-level font, size, color and faces keep their stored values.
pub(super) fn apply_conditional(text: &mut StyledText, style: &ResolvedStyle) {
    for run in &mut text.character_runs {
        let character = &mut run.style;
        if let Some(family) = &style.font_family {
            character.font_family = Some(family.clone());
        }
        if let Some(size) = style.font_size {
            character.font_size = Some(size);
        }
        if let Some(color) = style.color {
            character.color = Some(color);
        }
        for (value, mask, enabled) in [
            (
                style.font_weight,
                256,
                style.font_weight.is_some_and(|w| w == 2 || w >= 600),
            ),
            (
                style.font_style,
                512,
                matches!(style.font_style, Some(2 | 3)),
            ),
        ] {
            if value.is_some() {
                let inherited_mask = if character.faces.is_some() {
                    u16::MAX
                } else {
                    0
                };
                *character.face_mask.get_or_insert(inherited_mask) |= mask;
                let faces = character.faces.get_or_insert(0);
                *faces = if enabled {
                    *faces | mask
                } else {
                    *faces & !mask
                };
            }
        }
    }
}

pub(super) fn layout_job(
    fonts: &mut FontsView<'_>,
    text: &str,
    style: &ResolvedStyle,
    rich: Option<&StyledText>,
    wrap_width: f32,
) -> LayoutJob {
    layout_job_with_pitch(text, style, rich, wrap_width, |font| {
        natural_line_pitch(fonts, font)
    })
}

// Keep the unrounded metrics: epaint rounds each metric to 1/32 point,
// turning Arial 10's 11.499-point pitch into 11.5 before native rounding.
thread_local! {
    static FONT_PITCH: RefCell<Vec<(Arc<FontData>, Option<f32>)>> = const { RefCell::new(Vec::new()) };
}

fn natural_line_pitch(fonts: &mut FontsView<'_>, font: &FontId) -> f32 {
    let definitions = fonts.definitions();
    let data = definitions.families.get(&font.family).and_then(|names| {
        names
            .iter()
            .find_map(|name| definitions.font_data.get(name))
    });
    let ratio = data.and_then(|data| {
        FONT_PITCH.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some((_, ratio)) = cache.iter().find(|(saved, _)| Arc::ptr_eq(saved, data)) {
                return *ratio;
            }
            let ratio = skrifa::FontRef::from_index(data.bytes(), data.index)
                .ok()
                .and_then(|face| {
                    let metrics = face.metrics(
                        skrifa::instance::Size::unscaled(),
                        skrifa::instance::LocationRef::default(),
                    );
                    let ratio = (metrics.ascent - metrics.descent + metrics.leading)
                        * data.tweak.scale
                        / f32::from(metrics.units_per_em);
                    (ratio.is_finite() && ratio > 0.0).then_some(ratio)
                });
            if cache.len() == 64 {
                cache.remove(0);
            }
            cache.push((data.clone(), ratio));
            ratio
        })
    });
    ratio
        .map_or_else(
            || fonts.row_height(font).round(),
            |ratio| (ratio * font.size).round(),
        )
        .max(1.0)
}

fn layout_job_with_pitch(
    text: &str,
    style: &ResolvedStyle,
    rich: Option<&StyledText>,
    wrap_width: f32,
    mut line_pitch: impl FnMut(&FontId) -> f32,
) -> LayoutJob {
    let rich = rich.filter(|rich| rich.text == text);
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width.max(0.0);
    let paragraph_align = rich
        .and_then(|rich| rich.paragraph_runs.first())
        .and_then(|run| run.style.text_align);
    let align = style.text_align.or(paragraph_align);
    job.halign = match align {
        Some(2) => Align::Center,
        Some(3) => Align::Max,
        _ => Align::Min,
    };
    job.justify = align == Some(4);

    // Split on both character and paragraph boundaries, preserving any plain
    // gaps. Public structs can be supplied by callers, so validate ranges too.
    let mut boundaries = vec![0, text.len()];
    if let Some(rich) = rich {
        for range in rich
            .character_runs
            .iter()
            .map(|run| &run.range)
            .chain(rich.paragraph_runs.iter().map(|run| &run.range))
        {
            if range.start <= range.end
                && range.end <= text.len()
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end)
            {
                boundaries.extend([range.start, range.end]);
            }
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    for range in boundaries.windows(2) {
        let range = range[0]..range[1];
        let character = rich.and_then(|rich| {
            rich.character_runs
                .iter()
                .find(|run| run.range.start <= range.start && run.range.end >= range.end)
                .map(|run| &run.style)
        });
        let paragraph = rich.and_then(|rich| {
            rich.paragraph_runs
                .iter()
                .find(|run| run.range.start <= range.start && run.range.end >= range.end)
                .map(|run| &run.style)
        });
        let mut format = text_format(style, character);
        let paragraph_height = paragraph
            .and_then(|p| p.line_height)
            .filter(|height| height.is_finite() && *height > 0.0);
        // KFormat's saved fontHeight is metadata, not the runtime line pitch.
        // Explicit line multiples use rounded natural font pitch. With no
        // spacing data, leave epaint's natural leading alone.
        format.line_height = style
            .line_height
            .as_ref()
            .and_then(|height| {
                if !matches!(height.auto_value, 0 | 1) {
                    return None;
                }
                let size = f64::from(format.font_id.size);
                let points = match height.unit {
                    0 | 9 => height.value * f64::from(line_pitch(&format.font_id)),
                    7 => height.value * size,
                    1 => height.value * size / 100.0,
                    2 => height.value * 72.0 / 2.54,
                    3 => height.value * 72.0,
                    4 => height.value * 72.0 / 25.4,
                    5 => height.value * 12.0,
                    6 | 8 => height.value,
                    _ => return None,
                };
                (points.is_finite() && points > 0.0).then_some(points as f32)
            })
            .or_else(|| {
                match (
                    paragraph_height,
                    paragraph
                        .and_then(|paragraph| paragraph.line_height_unit)
                        .unwrap_or(0),
                ) {
                    (Some(height), 1) => Some(height),
                    (Some(height), 2) => Some(height * 72.0 / 2.54),
                    (Some(height), 3) => Some(height * 72.0),
                    (Some(height), 0) => Some(height * line_pitch(&format.font_id)),
                    _ => None,
                }
            });
        let normalized = text[range].replace('\r', "\n");
        const CASE_GROUP: u16 = 0x0030;
        let run_case = character.and_then(|run| {
            let mask = run.face_mask?;
            (mask & CASE_GROUP == CASE_GROUP).then_some(run.faces? & CASE_GROUP)
        });
        use om_data::calc::text::case::{lower_char, upper_char};
        let rendered: String = match (style.text_transform, run_case) {
            (Some(3), _) => normalized.to_uppercase(),
            (_, Some(0x0010)) => normalized.chars().map(upper_char).collect(),
            (_, Some(0x0020)) => normalized.chars().map(lower_char).collect(),
            (_, Some(0x0030)) => {
                // The whole-object title style keeps hyphens and apostrophes
                // within words. Reusing that rule for runs is inferred.
                let mut starts_word = true;
                normalized
                    .chars()
                    .map(|character| {
                        let cased = if starts_word {
                            upper_char(character)
                        } else {
                            lower_char(character)
                        };
                        starts_word = character.is_whitespace();
                        cased
                    })
                    .collect()
            }
            _ => normalized,
        };
        job.append(&rendered, 0.0, format);
    }
    // Native narrow date cells fill the row before splitting the year;
    // a slash is not a preferred break in an otherwise numeric date.
    // Keep ordinary prose and paths on the normal word-wrapping policy.
    if job.text.bytes().filter(|byte| *byte == b'/').count() == 2
        && job.text.split('/').all(|part| {
            !part.is_empty() && part.len() <= 4 && part.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        job.wrap.break_anywhere = true;
    }
    job
}

/// FileMaker allows the latest fitting hyphen and space as equal word-break
/// opportunities. epaint prefers any earlier space over a later hyphen. Keep
/// epaint's shaping, meshes and caches, correcting that ASCII soft wrap only.
/// No characters are inserted or replaced: AX/cursor offsets still address
/// the original text, with hard and soft row endings kept distinct.
pub(super) fn layout_galley(fonts: &mut FontsView<'_>, job: LayoutJob) -> Arc<Galley> {
    let mut galley = layout_wrapped_galley(fonts, job);
    if galley
        .job
        .sections
        .iter()
        .all(|section| section.format.line_height.is_none())
    {
        return galley;
    }
    // Keep the original cached galley when epaint already placed its rows
    // correctly (the common whole-pixel 1-line case).
    if row_positions(&galley)
        .zip(&galley.rows)
        .all(|((y, height), row)| row.pos.y == y && row.size.y == height)
    {
        return galley;
    }
    let positions: Vec<_> = row_positions(&galley).collect();
    let out = Arc::make_mut(&mut galley);
    out.rect = egui::Rect::ZERO;
    out.mesh_bounds = egui::Rect::NOTHING;
    for (placed, (y, height)) in out.rows.iter_mut().zip(positions) {
        placed.pos.y = y;
        Arc::make_mut(&mut placed.row).size.y = height;
        out.rect |= placed.rect();
        out.mesh_bounds |= placed.visuals.mesh_bounds.translate(placed.pos.to_vec2());
    }
    galley
}

fn row_positions(galley: &Galley) -> impl Iterator<Item = (f32, f32)> + '_ {
    let mut y = 0.0;
    let mut byte = 0;
    galley.rows.iter().enumerate().map(move |(index, placed)| {
        let empty_height = galley
            .job
            .sections
            .iter()
            .find(|section| section.byte_range.start.0 <= byte && byte < section.byte_range.end.0)
            .and_then(|section| section.format.line_height);
        let height = placed
            .glyphs
            .iter()
            .map(|glyph| glyph.line_height)
            .reduce(f32::max)
            .or(empty_height)
            .unwrap_or(placed.size.y)
            .max(if index == 0 {
                galley.job.first_row_min_height
            } else {
                0.0
            });
        byte += placed
            .glyphs
            .iter()
            .map(|glyph| glyph.chr.len_utf8())
            .sum::<usize>()
            + usize::from(placed.ends_with_newline);
        // Accumulate exact pitch, snapping each total rather than rounding
        // every pitch: 33.6px becomes native's 34,33,34 pixel row gaps.
        let snapped = (y * galley.pixels_per_point).round() / galley.pixels_per_point;
        y += height;
        (snapped, height)
    })
}

fn layout_wrapped_galley(fonts: &mut FontsView<'_>, job: LayoutJob) -> Arc<Galley> {
    let original = fonts.layout_job(job.clone());
    if !job.text.is_ascii()
        || !job.text.contains('-')
        || !job.wrap.max_width.is_finite()
        || job.wrap.max_width <= 0.0
        || job.wrap.break_anywhere
        || job.wrap.max_rows != usize::MAX
        || job.first_row_min_height != 0.0
        || job.justify
        || job
            .sections
            .iter()
            .any(|section| section.leading_space != 0.0)
        || original.elided
        || original.rows.len() <= job.text.bytes().filter(|byte| *byte == b'\n').count() + 1
    {
        return original;
    }
    let mut measure_job = job.clone();
    measure_job.wrap.max_width = f32::INFINITY;
    measure_job.halign = Align::Min;
    measure_job.justify = false;
    let measured = fonts.layout_job(measure_job);
    let mut plan = Vec::new();
    let mut offset = 0;
    for paragraph in &measured.rows {
        let ranges = hyphen_rows(&paragraph.glyphs, job.effective_wrap_width());
        let count = ranges.len();
        for (index, range) in ranges.into_iter().enumerate() {
            plan.push((
                offset + range.start..offset + range.end,
                index + 1 == count && paragraph.ends_with_newline,
            ));
        }
        offset += paragraph.glyphs.len() + usize::from(paragraph.ends_with_newline);
    }
    if offset != job.text.len()
        || (plan.len() == original.rows.len()
            && plan.iter().zip(&original.rows).all(|((range, hard), row)| {
                range.len() == row.glyphs.len() && *hard == row.ends_with_newline
            }))
    {
        return original;
    }

    let mut galley = (*original).clone();
    galley.rows.clear();
    galley.rect = egui::Rect::ZERO;
    galley.mesh_bounds = egui::Rect::NOTHING;
    galley.num_vertices = 0;
    galley.num_indices = 0;
    let mut y = 0.0;
    for (range, hard) in plan {
        let line = fonts.layout_job(fragment_job(&job, range));
        let Some(first) = line.rows.first() else {
            return original;
        };
        let mut row = first.clone();
        row.pos.y = y;
        row.ends_with_newline = hard;
        y += row.size.y;
        galley.rect |= row.rect();
        galley.mesh_bounds |= row.visuals.mesh_bounds.translate(row.pos.to_vec2());
        galley.num_vertices += row.visuals.mesh.vertices.len();
        galley.num_indices += row.visuals.mesh.indices.len();
        galley.rows.push(row);
    }
    Arc::new(galley)
}

fn hyphen_rows(glyphs: &[egui::epaint::text::Glyph], width: f32) -> Vec<Range<usize>> {
    if glyphs.is_empty() {
        return std::iter::once(0..0).collect();
    }
    let mut rows = Vec::new();
    let mut start = 0;
    while start < glyphs.len() {
        let mut candidate = None;
        let mut punctuation = None;
        let mut end = start;
        while end < glyphs.len() {
            let glyph = &glyphs[end];
            if end > start && glyph.max_x() - glyphs[start].pos.x > width {
                break;
            }
            if glyph.chr.is_ascii_whitespace() || glyph.chr == '-' {
                candidate = Some(end + 1);
            } else if glyph.chr.is_ascii_punctuation() {
                punctuation = Some(end + 1);
            }
            end += 1;
        }
        let end = if end == glyphs.len() {
            end
        } else {
            candidate.or(punctuation).unwrap_or(end)
        };
        rows.push(start..end);
        start = end;
    }
    rows
}

fn fragment_job(job: &LayoutJob, range: Range<usize>) -> LayoutJob {
    let mut fragment = job.clone();
    fragment.text = job.text[range.clone()].to_owned();
    fragment.wrap.max_width = f32::INFINITY;
    fragment.sections = job
        .sections
        .iter()
        .filter_map(|section| {
            let start = section.byte_range.start.0.max(range.start);
            let end = section.byte_range.end.0.min(range.end);
            if start >= end {
                return None;
            }
            let mut section = section.clone();
            section.byte_range = (start - range.start).into()..(end - range.start).into();
            Some(section)
        })
        .collect();
    if fragment.sections.is_empty() {
        if let Some(section) = job
            .sections
            .iter()
            .find(|section| {
                section.byte_range.start.0 <= range.start && section.byte_range.end.0 > range.start
            })
            .or_else(|| job.sections.last())
        {
            fragment.append("", 0.0, section.format.clone());
        }
    }
    fragment
}

fn text_format(base: &ResolvedStyle, character: Option<&CharacterStyle>) -> TextFormat {
    let faces = character.and_then(|style| style.faces);
    let mask = character
        .and_then(|style| style.face_mask)
        .unwrap_or_else(|| if faces.is_some() { u16::MAX } else { 0 });
    let bold = if mask & 256 != 0 {
        faces.unwrap_or(0) & 256 != 0
    } else {
        base.font_weight
            .is_some_and(|weight| weight == 2 || weight >= 600)
    };
    let italic = if mask & 512 != 0 {
        faces.unwrap_or(0) & 512 != 0
    } else {
        matches!(base.font_style, Some(2 | 3))
    };
    let size = character
        .and_then(|style| style.font_size)
        .or(base.font_size)
        .filter(|size| size.is_finite())
        .unwrap_or(14.0)
        .clamp(1.0, 512.0) as f32;
    let family = character
        .and_then(|style| style.font_family.as_deref())
        .or(base.font_family.as_deref());
    let color = character
        .and_then(|style| style.color)
        .or(base.color)
        .map(color)
        .unwrap_or(Color32::BLACK);
    TextFormat {
        font_id: FontId::new(size, theme::document_font(family, bold, italic)),
        color,
        underline: if faces.is_some_and(|faces| faces & (64 | 128 | 1024) != 0) {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        strikethrough: if faces.is_some_and(|faces| faces & 1 != 0) {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        ..Default::default()
    }
}

fn color(color: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(
        (color.red.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.green.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
        (color.alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests of run conversion use an exact one-em face; font-metric tests
    // below exercise the installed faces through the production entry point.
    fn layout_job(
        text: &str,
        style: &ResolvedStyle,
        rich: Option<&StyledText>,
        width: f32,
    ) -> LayoutJob {
        layout_job_with_pitch(text, style, rich, width, |font| font.size)
    }

    #[test]
    fn stored_partial_face_override_inherits_the_other_layout_attributes() {
        use om_data::calc::{Ctx, Date, Locale, Value, ValueType};
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, Date::from_ymd(2024, 1, 1).unwrap());
        let stored = StyledText {
            text: "a".into(),
            character_runs: vec![om_layout::StyledRun {
                range: 0..1,
                style: CharacterStyle {
                    faces: Some(0),
                    face_mask: Some(256),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let Value::Text(value) = om_data::host::styled_value(&stored, ValueType::Text, &cx) else {
            panic!("text")
        };
        let rich = value_text(&value);
        let style = ResolvedStyle {
            font_family: Some("Helvetica".into()),
            font_size: Some(9.0),
            font_weight: Some(2),
            font_style: Some(2),
            color: Some(Color {
                red: 1.0,
                alpha: 1.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let job = layout_job("a", &style, Some(&rich), 100.0);
        assert_eq!(job.sections[0].format.font_id.size, 9.0);
        assert_eq!(
            job.sections[0].format.font_id.family,
            theme::document_font(Some("Helvetica"), false, true)
        );
        assert_eq!(job.sections[0].format.color, Color32::RED);
    }

    #[test]
    fn conditional_text_color_wins_over_stored_color_without_resetting_font() {
        let mut text = value_text(&om_data::calc::Text::styled(
            "Heading",
            om_data::calc::CharStyle {
                font: Some("Helvetica".into()),
                size: Some(14),
                color: Some(0),
                flags: 256,
            },
        ));
        let red = Color {
            red: 1.0,
            alpha: 1.0,
            ..Default::default()
        };
        apply_conditional(
            &mut text,
            &ResolvedStyle {
                color: Some(red),
                ..Default::default()
            },
        );
        let run = &text.character_runs[0].style;
        assert_eq!(run.color, Some(red));
        assert_eq!(run.font_size, Some(14.0));
        assert_eq!(run.font_family.as_deref(), Some("Helvetica"));
        assert_eq!(run.faces, Some(256));
        let job = layout_job("Heading", &ResolvedStyle::default(), Some(&text), 400.0);
        assert_eq!(job.sections[0].format.color, Color32::RED);
    }
    use om_layout::{ParagraphStyle, StyledRun};

    #[test]
    fn static_spans_preserve_inline_sizes_colors_faces_and_returns() {
        let base = ResolvedStyle {
            font_size: Some(14.0),
            ..Default::default()
        };
        let text = "a\rβ";
        let rich = StyledText {
            text: text.into(),
            character_runs: vec![
                StyledRun {
                    range: 0..2,
                    style: CharacterStyle {
                        font_size: Some(9.0),
                        color: Some(Color {
                            blue: 1.0,
                            alpha: 1.0,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                },
                StyledRun {
                    range: 2..4,
                    style: CharacterStyle {
                        font_size: Some(12.0),
                        faces: Some(256 | 1024 | 1),
                        ..Default::default()
                    },
                },
            ],
            paragraph_runs: vec![StyledRun {
                range: 0..4,
                style: ParagraphStyle {
                    text_align: Some(2),
                    line_height: Some(1.25),
                    ..Default::default()
                },
            }],
        };
        let job = layout_job(text, &base, Some(&rich), 300.0);
        assert_eq!(job.text, "a\nβ");
        assert_eq!(job.halign, Align::Center);
        assert_eq!(job.sections.len(), 2);
        assert_eq!(job.sections[0].format.font_id.size, 9.0);
        assert_eq!(job.sections[0].format.color, Color32::BLUE);
        assert_eq!(job.sections[0].format.line_height, Some(11.25));
        assert_eq!(
            job.sections[1].format.font_id.family,
            theme::document_font(None, true, false)
        );
        assert_eq!(job.sections[1].format.underline.width, 1.0);
        assert_eq!(job.sections[1].format.strikethrough.width, 1.0);
        assert_eq!(job.sections[1].byte_range.start.0, 2);
        assert_eq!(job.sections[1].byte_range.end.0, 4);
    }

    #[test]
    fn merged_calculated_and_edited_values_do_not_inherit_source_offsets() {
        let base = ResolvedStyle {
            font_size: Some(10.0),
            ..Default::default()
        };
        let rich = StyledText {
            text: "<<field>>".into(),
            character_runs: vec![StyledRun {
                range: 0..9,
                style: CharacterStyle {
                    font_size: Some(40.0),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let job = layout_job("actual value", &base, Some(&rich), 200.0);
        assert_eq!(job.text, "actual value");
        assert_eq!(job.sections.len(), 1);
        assert_eq!(job.sections[0].format.font_id.size, 10.0);
    }

    #[test]
    fn saved_case_styles_transform_only_their_character_runs() {
        let text = "Start ﬂx MiXeD o'NEIL mAC-sMITH End";
        let style = |part: &str, faces| {
            let start = text.find(part).unwrap();
            StyledRun {
                range: start..start + part.len(),
                style: CharacterStyle {
                    faces: Some(faces),
                    face_mask: Some(0x0030),
                    ..Default::default()
                },
            }
        };
        let rich = StyledText {
            text: text.into(),
            character_runs: vec![
                style("ﬂx", 0x0010),
                style("MiXeD", 0x0020),
                style("o'NEIL mAC-sMITH", 0x0030),
            ],
            ..Default::default()
        };
        let job = layout_job(text, &ResolvedStyle::default(), Some(&rich), 300.0);
        assert_eq!(job.text, "Start ﬂX mixed O'neil Mac-smith End");
        assert_eq!(
            job.sections.last().unwrap().byte_range.end.0,
            job.text.len()
        );
    }

    #[test]
    fn transforms_rebuild_section_ranges_after_unicode_expansion() {
        let style = ResolvedStyle {
            text_transform: Some(3),
            text_align: Some(3),
            ..Default::default()
        };
        let job = layout_job("straße\rnext", &style, None, 100.0);
        assert_eq!(job.text, "STRASSE\nNEXT");
        assert_eq!(job.sections[0].byte_range.start.0, 0);
        assert_eq!(job.sections[0].byte_range.end.0, job.text.len());
        assert_eq!(job.halign, Align::Max);
    }

    #[test]
    fn calculation_text_keeps_its_own_style_and_plain_gaps() {
        let mut value = om_data::calc::Text::plain("a");
        value.push(&om_data::calc::Text::styled(
            "β",
            om_data::calc::CharStyle {
                color: Some(0xff0000),
                size: Some(8),
                flags: 512,
                ..Default::default()
            },
        ));
        let rich = value_text(&value);
        let job = layout_job("aβ", &ResolvedStyle::default(), Some(&rich), 100.0);
        assert_eq!(job.sections.len(), 2);
        assert_eq!(job.sections[0].format.font_id.size, 14.0);
        assert_eq!(job.sections[1].format.font_id.size, 8.0);
        assert_eq!(job.sections[1].format.color, Color32::RED);
    }

    #[test]
    fn merge_substitutions_keep_styles_on_both_sides_of_the_replacement() {
        let text = "é {{1032}} | <<$$Empty>> Ω";
        let shown = "é Ad\rmin |  Ω";
        let size = |points| CharacterStyle {
            font_size: Some(points),
            ..Default::default()
        };
        let rich = StyledText {
            text: text.into(),
            character_runs: vec![
                StyledRun {
                    range: 0..3,
                    style: size(12.0),
                },
                StyledRun {
                    range: 3..5,
                    style: size(18.0),
                },
                StyledRun {
                    range: 5..11,
                    style: size(8.0),
                },
                StyledRun {
                    range: 14..25,
                    style: size(7.0),
                },
                StyledRun {
                    range: 25..text.len(),
                    style: size(10.0),
                },
            ],
            ..Default::default()
        };
        let painted = substitute(
            &rich,
            shown,
            &[
                TextSubstitution {
                    source: 3..11,
                    display: 3..9,
                },
                TextSubstitution {
                    source: 14..25,
                    display: 12..12,
                },
            ],
        );
        let job = layout_job(shown, &ResolvedStyle::default(), Some(&painted), 100.0);
        let styles: Vec<_> = job
            .sections
            .iter()
            .map(|section| {
                (
                    &job.text[section.byte_range.start.0..section.byte_range.end.0],
                    section.format.font_id.size,
                )
            })
            .collect();
        assert_eq!(
            styles,
            [("é ", 12.0), ("Ad\nmin", 18.0), (" | ", 14.0), (" Ω", 10.0)]
        );
    }

    #[test]
    fn paragraph_line_multiples_use_each_runs_pitch() {
        let style = ResolvedStyle {
            font_size: Some(9.0),
            ..Default::default()
        };
        let rich = StyledText {
            text: "ab".into(),
            character_runs: vec![StyledRun {
                range: 1..2,
                style: CharacterStyle {
                    font_size: Some(12.0),
                    ..Default::default()
                },
            }],
            paragraph_runs: vec![StyledRun {
                range: 0..2,
                style: ParagraphStyle {
                    line_height: Some(1.0),
                    ..Default::default()
                },
            }],
        };
        let job = layout_job("ab", &style, Some(&rich), 100.0);
        assert_eq!(job.sections[0].format.line_height, Some(9.0));
        assert_eq!(job.sections[1].format.line_height, Some(12.0));
        let natural = layout_job("ab", &style, Some(&rich), 100.0);
        assert_eq!(natural.sections[0].format.line_height, Some(9.0));
    }

    #[test]
    fn absent_leading_keeps_the_fonts_natural_height() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.fonts_mut(|fonts| {
                let mut style = ResolvedStyle {
                    font_family: Some("Inter".into()),
                    font_size: Some(10.0),
                    ..Default::default()
                };
                let job = super::layout_job(fonts, "one\ntwo", &style, None, 200.0);
                assert_eq!(job.sections[0].format.line_height, None);
                let natural = fonts.row_height(&job.sections[0].format.font_id);
                let plain = fonts.layout_job(job.clone());
                let galley = layout_galley(fonts, job);
                assert_eq!(galley.rows[1].pos.y, plain.rows[1].pos.y);
                assert!(natural > 12.0, "Inter's pitch must not become one em");
                style.line_height = Some(om_layout::Length {
                    unit: 9,
                    value: 0.8,
                    ..Default::default()
                });
                let job = super::layout_job(fonts, "one\ntwo", &style, None, 200.0);
                // Inter's 2478/2048 natural pitch rounds to 12 points first.
                assert!((job.sections[0].format.line_height.unwrap() - 9.6).abs() < 0.001);
            });
        })
        .drop_without_applying_deltas();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_arial_pitch_rounds_before_applying_line_multiple() {
        if !std::path::Path::new("/System/Library/Fonts/Supplemental/Arial.ttf").exists() {
            return;
        }
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(2.0);
        ctx.run_ui(input, |ui| {
            ui.fonts_mut(|fonts| {
                // FileMaker Pro scratch-file captures, CSS sizes verified
                // independently: 10pt -> 11pt, 16pt -> 18pt, 18pt -> 21pt.
                for (size, multiple, expected) in [
                    (10.0, 1.0, 11.0),
                    (16.0, 1.0, 18.0),
                    (18.0, 1.0, 21.0),
                    (18.0, 0.8, 16.8),
                ] {
                    let style = ResolvedStyle {
                        font_family: Some("Arial".into()),
                        font_size: Some(size),
                        line_height: Some(om_layout::Length {
                            unit: 9,
                            value: multiple,
                            ..Default::default()
                        }),
                        ..Default::default()
                    };
                    let job =
                        super::layout_job(fonts, "Hgj 1\nHgj 2\nHgj 3\nHgj 4", &style, None, 200.0);
                    assert!(
                        (job.sections[0].format.line_height.unwrap() - expected).abs() < 0.001,
                        "size={size}, multiple={multiple}"
                    );
                    let galley = layout_galley(fonts, job);
                    assert_eq!(galley.pixels_per_point, 2.0);
                    for (index, row) in galley.rows.iter().enumerate() {
                        let expected_pixel = (index as f32 * expected * 2.0).round();
                        assert!(
                            (row.pos.y * 2.0 - expected_pixel).abs() < 0.001,
                            "size={size}, multiple={multiple}, row={index}, y={}",
                            row.pos.y
                        );
                    }
                    if multiple == 0.8 {
                        let gaps: Vec<_> = galley
                            .rows
                            .windows(2)
                            .map(|rows| ((rows[1].pos.y - rows[0].pos.y) * 2.0).round() as i32)
                            .collect();
                        assert_eq!(gaps, [34, 33, 34]);
                    }
                }
            });
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn whole_pixel_leading_reuses_epaints_cached_galley() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.fonts_mut(|fonts| {
                let style = ResolvedStyle {
                    font_family: Some("Inter".into()),
                    font_size: Some(10.0),
                    line_height: Some(om_layout::Length {
                        unit: 9,
                        value: 1.0,
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let job = super::layout_job(fonts, "one\ntwo", &style, None, 200.0);
                let cached = fonts.layout_job(job.clone());
                let actual = layout_galley(fonts, job);
                assert!(Arc::ptr_eq(&cached, &actual));
            });
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn style_leading_controls_proof_and_comment_line_pitch() {
        let mut style = ResolvedStyle {
            font_size: Some(9.0),
            line_height: Some(om_layout::Length {
                value: 0.8,
                unit: 9,
                ..Default::default()
            }),
            ..Default::default()
        };
        let proof = layout_job("one\rtwo", &style, None, 170.0);
        assert!((proof.sections[0].format.line_height.unwrap() - 7.2).abs() < 0.0001);
        style.font_size = Some(10.0);
        style.line_height.as_mut().unwrap().value = 1.0;
        let comments = layout_job("one\rtwo", &style, None, 201.0);
        assert_eq!(comments.sections[0].format.line_height, Some(10.0));
    }

    #[test]
    fn absolute_paragraph_leading_is_not_a_font_height_multiplier() {
        let style = ResolvedStyle {
            font_size: Some(9.0),
            ..Default::default()
        };
        let mut rich = StyledText {
            text: "a\rb".into(),
            paragraph_runs: vec![StyledRun {
                range: 0..3,
                style: ParagraphStyle {
                    text_align: Some(4),
                    line_height: Some(15.0),
                    line_height_unit: Some(1),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let job = layout_job("a\rb", &style, Some(&rich), 100.0);
        assert_eq!(job.sections[0].format.line_height, Some(15.0));
        assert!(job.justify);
        rich.paragraph_runs[0].style.line_height = Some(0.25);
        rich.paragraph_runs[0].style.line_height_unit = Some(3);
        let inches = layout_job("a\rb", &style, Some(&rich), 100.0);
        assert_eq!(inches.sections[0].format.line_height, Some(18.0));
    }

    #[test]
    fn latest_fitting_hyphen_wraps_at_170_points_without_changing_text_or_indices() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.fonts_mut(|fonts| {
                let source = "1-PDF to designer@example.com 5-Samples to Review\n\nNext paragraph";
                let style = ResolvedStyle {
                    font_family: Some("Inter".into()),
                    font_size: Some(9.0),
                    text_align: Some(2),
                    line_height: Some(om_layout::Length {
                        value: 0.8,
                        unit: 9,
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let job = layout_job(source, &style, None, 170.0);
                let galley = layout_galley(fonts, job.clone());
                let first: String = galley.rows[0]
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.chr)
                    .collect();
                assert!(first.ends_with("5-"), "first row: {first:?}");
                assert_eq!(galley.job.text, source);
                assert_eq!(galley.job.sections, job.sections);
                let mut reconstructed = String::new();
                for row in &galley.rows {
                    reconstructed.extend(row.glyphs.iter().map(|glyph| glyph.chr));
                    if row.ends_with_newline {
                        reconstructed.push('\n');
                    }
                }
                assert_eq!(reconstructed, source);
                assert!(galley
                    .rows
                    .iter()
                    .all(|row| row.rect().center().x.abs() < 0.51));
            });
        })
        .drop_without_applying_deltas();
    }

    #[test]
    fn a_narrow_numeric_date_fills_the_row_instead_of_breaking_at_the_slash() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.fonts_mut(|fonts| {
                let source = "4/4/2017";
                let style = ResolvedStyle {
                    font_family: Some("Inter".into()),
                    font_size: Some(9.0),
                    ..Default::default()
                };
                let measured = fonts.layout_job(layout_job(source, &style, None, f32::INFINITY));
                // Six characters fit in the native capture. Measure that
                // width so the regression is independent of fallback fonts.
                let width = measured.rows[0].glyphs[5].max_x() + 0.01;
                let job = layout_job(source, &style, None, width);
                let galley = layout_galley(fonts, job);
                let first: String = galley.rows[0]
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.chr)
                    .collect();
                assert_eq!(first, "4/4/20");
                assert_eq!(
                    galley
                        .rows
                        .iter()
                        .flat_map(|row| row.glyphs.iter().map(|glyph| glyph.chr))
                        .collect::<String>(),
                    source
                );
                assert_eq!(galley.job.text, source);
            });
        })
        .drop_without_applying_deltas();
    }
}
