//! Line objects have one oriented stroke, independent of rectangle borders.
//! Native placement is documented in layout-model.md section 5.3.1.

use egui::{emath::TSTransform, pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke};
use om_layout::ResolvedStyle;

pub(super) fn paint(painter: &Painter, bounds: Rect, flags: u64, style: &ResolvedStyle) {
    let width = style.border_width[0].unwrap_or(1.0) as f32;
    if !width.is_finite() || width <= 0.0 || !bounds.is_finite() {
        return;
    }
    let color = style.border_color[0]
        .or(style.color)
        .map(super::color)
        .unwrap_or(Color32::BLACK);
    // Native CalculateFrame expands the unshifted bounds by width / sqrt(2).
    // The view clips that frame after snapping it outwards to screen pixels.
    // Keep this separate from the half-point placement of the stroke itself:
    // a narrow line can lie partly or wholly outside its frame at some zooms.
    let transform = painter
        .ctx()
        .layer_transform_to_global(painter.layer_id())
        .unwrap_or(TSTransform::IDENTITY);
    let clip = frame_clip(bounds, width, transform, painter.ctx().pixels_per_point());
    let painter = painter.with_clip_rect(painter.clip_rect().intersect(clip));
    // A Path preserves the saved subpixel position. LineSegment would
    // independently snap axis-aligned strokes and their endpoints in egui.
    painter.add(Shape::line(
        stroke_points(bounds.translate(placement(bounds, width)), flags, width).to_vec(),
        Stroke::new(width, color),
    ));
}

fn frame_clip(bounds: Rect, width: f32, transform: TSTransform, pixels_per_point: f32) -> Rect {
    let frame = transform * bounds.expand(width * std::f32::consts::FRAC_1_SQRT_2);
    let snapped = Rect::from_min_max(
        (frame.min.to_vec2() * pixels_per_point).floor().to_pos2() / pixels_per_point,
        (frame.max.to_vec2() * pixels_per_point).ceil().to_pos2() / pixels_per_point,
    );
    transform.inverse() * snapped
}

fn placement(bounds: Rect, width: f32) -> egui::Vec2 {
    if width < 1.0 {
        if bounds.width() < bounds.height() {
            vec2(0.5, 0.0)
        } else {
            vec2(0.0, 0.5)
        }
    } else if width.round() % 2.0 == 1.0 {
        vec2(0.5, 0.5)
    } else {
        egui::Vec2::ZERO
    }
}

/// Stored flags choose opposite corners. The native stroke extends the
/// endpoints by half the width, split over the two axes by absolute slope.
fn stroke_points(bounds: Rect, flags: u64, width: f32) -> [Pos2; 2] {
    let left_to_right = flags & 0x1000 != 0;
    let top_to_bottom = flags & 0x2000 != 0;
    let mut a = pos2(
        if left_to_right {
            bounds.left()
        } else {
            bounds.right()
        },
        if top_to_bottom {
            bounds.top()
        } else {
            bounds.bottom()
        },
    );
    let mut b = pos2(
        if left_to_right {
            bounds.right()
        } else {
            bounds.left()
        },
        if top_to_bottom {
            bounds.bottom()
        } else {
            bounds.top()
        },
    );
    let delta = b - a;
    let half = width * 0.5;
    let extent = if delta.x == 0.0 {
        vec2(0.0, half)
    } else {
        let slope = (delta.y / delta.x).abs();
        let x = half / (1.0 + slope);
        vec2(x, slope * x)
    };
    let outward = vec2(
        if delta.x > 0.0 { -extent.x } else { extent.x },
        if delta.y > 0.0 { -extent.y } else { extent.y },
    );
    a += outward;
    b -= outward;
    [a, b]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_clipping_matches_native_axis_stroke_coverage_at_three_zooms() {
        // Invented scratch-file coordinates, captured in native Browse at
        // 2 physical pixels per point. Coverage is the total dark-pixel
        // area across the stroke, away from its endpoints (JPEG +/- 1).
        // At 150%, a whole-point quarter line vanishes, but its half-point
        // neighbour retains 0.375px. A width cutoff cannot express this.
        let cases = [
            (1.0, 0.0, 0.25, 0.25),
            (1.0, 0.5, 0.25, 0.25),
            (1.0, 0.0, 0.5, 0.5),
            (1.0, 0.5, 0.5, 0.5),
            (1.0, 0.0, 1.0, 2.0),
            (1.0, 0.5, 1.0, 2.0),
            (1.0, 0.0, 2.0, 4.0),
            (1.0, 0.5, 2.0, 4.0),
            (1.5, 0.0, 0.25, 0.0),
            (1.5, 0.5, 0.25, 0.375),
            (1.5, 0.0, 0.5, 1.25),
            (1.5, 0.5, 0.5, 0.75),
            (1.5, 0.0, 1.0, 3.0),
            (1.5, 0.5, 1.0, 2.5),
            (1.5, 0.0, 2.0, 6.0),
            (1.5, 0.5, 2.0, 6.0),
            (4.0, 0.0, 0.25, 0.0),
            (4.0, 0.0, 0.5, 1.0),
            (4.0, 0.0, 1.0, 6.0),
        ];
        for (zoom, phase, width, expected) in cases {
            let bounds = Rect::from_min_size(pos2(100.0, 200.0 + phase), vec2(200.0, 0.0));
            let transform =
                TSTransform::from_translation(vec2(50.0, 50.0)) * TSTransform::from_scaling(zoom);
            let clip = transform * frame_clip(bounds, width, transform, 2.0);
            let point = transform
                * stroke_points(bounds.translate(placement(bounds, width)), 0x3000, width)[0];
            let half = width * zoom;
            let coverage = ((point.y * 2.0 + half).min(clip.bottom() * 2.0)
                - (point.y * 2.0 - half).max(clip.top() * 2.0))
            .max(0.0);
            assert!(
                (coverage - expected).abs() < 0.0002,
                "zoom={zoom} phase={phase} width={width}: {coverage} != {expected}"
            );
        }
    }

    #[test]
    fn frame_snaps_outwards_in_screen_pixels_and_preserves_diagonals() {
        for zoom in [0.25, 1.0, 1.5, 4.0] {
            let transform = TSTransform::from_translation(vec2(-31.25, 17.75))
                * TSTransform::from_scaling(zoom);
            for pixels_per_point in [1.0, 1.5, 2.0] {
                for bounds in [
                    Rect::from_min_size(pos2(-10.25, 20.5), vec2(0.0, 100.0)),
                    Rect::from_min_size(pos2(-10.25, 20.5), vec2(100.0, 100.0)),
                ] {
                    let clip = frame_clip(bounds, 0.25, transform, pixels_per_point);
                    let pixels = (transform * clip) * pixels_per_point;
                    for value in [pixels.left(), pixels.top(), pixels.right(), pixels.bottom()] {
                        assert!((value - value.round()).abs() < 0.0001);
                    }
                    assert!(clip.contains_rect(bounds));
                    if bounds.width() > 0.0 {
                        let points =
                            stroke_points(bounds.translate(placement(bounds, 0.25)), 0x3000, 0.25);
                        assert!(clip.contains(points[0].lerp(points[1], 0.5)));
                    }
                }
            }
        }
    }

    #[test]
    fn painted_line_uses_its_frame_without_expanding_the_parent_clip() {
        let context = egui::Context::default();
        let output = context.run_ui(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.set_clip_rect(Rect::from_min_max(pos2(10.0, 10.0), pos2(100.0, 100.0)));
                paint(
                    ui.painter(),
                    Rect::from_min_max(pos2(20.0, 20.0), pos2(120.0, 20.0)),
                    0x3000,
                    &ResolvedStyle {
                        border_width: [Some(0.25); 4],
                        ..Default::default()
                    },
                );
            });
        });
        let clips: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| matches!(shape.shape, Shape::Path(_)).then_some(shape.clip_rect))
            .collect();
        output.drop_without_applying_deltas();
        assert_eq!(
            clips,
            [Rect::from_min_max(pos2(19.0, 19.0), pos2(100.0, 21.0))]
        );
    }

    #[test]
    fn stroke_endpoints_match_native_geometry_for_all_directions_and_widths() {
        // Measured with native CalcLineStrokePoints in the macOS arm64
        // FileMaker 26 engine. These coordinates contain no file data.
        for row in include_str!("line_native.csv").lines().skip(1) {
            let values: Vec<f32> = row.split(',').map(|v| v.parse().unwrap()).collect();
            let bounds =
                Rect::from_min_size(pos2(values[0], values[1]), vec2(values[2], values[3]));
            let points = stroke_points(bounds, values[4] as u64, values[5]);
            for (actual, expected) in [points[0].x, points[0].y, points[1].x, points[1].y]
                .into_iter()
                .zip(&values[6..])
            {
                assert!((actual - expected).abs() < 0.00002, "{row}: {points:?}");
            }
        }
    }

    #[test]
    fn half_point_placement_depends_on_thickness_and_orientation() {
        let horizontal = Rect::from_min_size(pos2(10.0, 20.0), vec2(100.0, 0.0));
        let vertical = Rect::from_min_size(pos2(10.0, 20.0), vec2(0.0, 100.0));
        for width in [0.25, 0.5, 0.75] {
            assert_eq!(placement(horizontal, width), vec2(0.0, 0.5));
            assert_eq!(placement(vertical, width), vec2(0.5, 0.0));
        }
        for width in [1.0, 2.5, 3.0] {
            assert_eq!(placement(horizontal, width), vec2(0.5, 0.5));
            assert_eq!(placement(vertical, width), vec2(0.5, 0.5));
        }
        for width in [1.5, 2.0, 4.0] {
            assert_eq!(placement(horizontal, width), egui::Vec2::ZERO);
        }
    }

    #[test]
    fn line_painting_emits_one_unsnapped_stroke_and_no_rectangle_fill() {
        for width in [0.25, 0.5, 1.0, 2.0] {
            let context = egui::Context::default();
            let output = context.run_ui(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::super::paint_box(
                        ui,
                        Rect::from_min_max(pos2(20.0, 20.5), pos2(120.0, 20.5)),
                        &ResolvedStyle {
                            border_width: [Some(width); 4],
                            background: Some(om_layout::Color {
                                red: 1.0,
                                green: 0.0,
                                blue: 0.0,
                                alpha: 1.0,
                            }),
                            ..Default::default()
                        },
                        om_layout::ObjectKind::Line,
                    );
                });
            });
            let paths: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    Shape::Path(path) => Some(path.clone()),
                    _ => None,
                })
                .collect();
            output.drop_without_applying_deltas();
            assert_eq!(paths.len(), 1);
            assert_eq!(paths[0].stroke.width, width as f32);
            assert!(!paths[0].closed);
            assert_eq!(paths[0].fill, Color32::TRANSPARENT);
            // Horizontal hairlines retain the saved half point plus the
            // native half-point placement, before any viewport transform.
            if width < 1.0 {
                assert_eq!(paths[0].points[0].y, 21.0);
            }
        }
    }
}
