//! The Script Workspace's glyphs (captures 35, 35b), drawn with the painter
//! on a 20x20 grid like the chrome's (`icons.rs`). OpenMaker's own
//! drawings; they read like FileMaker's at a glance.

use egui::epaint::PathShape;
use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Glyph {
    /// New Script (toolbar).
    Plus,
    Run,
    Debug,
    /// The scripts pane's script icon: a sheet with a turned corner.
    Script,
    Folder,
    /// New script (scripts pane header): a sheet with lines.
    NewScript,
    NewFolder,
    /// Show the "Include in menu" checkboxes.
    List,
    Star,
    StarFilled,
    /// Sort steps alphabetically.
    Alphabetical,
    /// Preview (the editor's eye).
    Eye,
    /// Show compatibility.
    Compatibility,
    /// Show or hide the scripts pane.
    LeftPane,
    /// Show or hide the steps pane.
    RightPane,
    DisclosureRight,
    DisclosureDown,
    Help,
    Close,
}

/// Paint `glyph` centered in `rect`.
pub(super) fn paint(painter: &Painter, rect: Rect, glyph: Glyph, color: Color32) {
    let side = rect.width().min(rect.height());
    let origin = rect.center() - Vec2::splat(side / 2.0);
    let scale = side / 20.0;
    let p = |x: f32, y: f32| Pos2::new(origin.x + x * scale, origin.y + y * scale);
    let stroke = Stroke::new((1.2 * scale).max(1.0), color);
    let line = |points: &[(f32, f32)]| {
        painter.add(PathShape::line(
            points.iter().map(|(x, y)| p(*x, *y)).collect(),
            stroke,
        ));
    };
    let closed = |points: &[(f32, f32)]| {
        painter.add(PathShape::closed_line(
            points.iter().map(|(x, y)| p(*x, *y)).collect(),
            stroke,
        ));
    };
    let filled = |points: &[(f32, f32)], fill: Color32| {
        painter.add(PathShape::convex_polygon(
            points.iter().map(|(x, y)| p(*x, *y)).collect(),
            fill,
            Stroke::NONE,
        ));
    };
    match glyph {
        Glyph::Plus => {
            line(&[(10.0, 3.0), (10.0, 17.0)]);
            line(&[(3.0, 10.0), (17.0, 10.0)]);
        }
        Glyph::Run => filled(&[(6.0, 3.5), (16.0, 10.0), (6.0, 16.5)], color),
        Glyph::Debug => {
            painter.circle_stroke(p(10.0, 11.5), 4.0 * scale, stroke);
            painter.circle_stroke(p(10.0, 6.0), 2.0 * scale, stroke);
            line(&[(10.0, 8.5), (10.0, 15.5)]);
            for (y, dx) in [(9.0, 3.5), (12.0, 4.5), (15.0, 3.5)] {
                line(&[(10.0 - 4.0, y), (10.0 - 4.0 - dx * 0.5, y - 1.0)]);
                line(&[(10.0 + 4.0, y), (10.0 + 4.0 + dx * 0.5, y - 1.0)]);
            }
        }
        Glyph::Script => {
            // A rhombus sheet with a folded edge, FileMaker's script mark.
            closed(&[(10.0, 2.5), (17.0, 9.5), (10.0, 16.5), (3.0, 9.5)]);
            line(&[(6.5, 9.5), (10.0, 6.0), (13.5, 9.5)]);
            line(&[(8.0, 12.0), (10.0, 10.0), (12.0, 12.0)]);
        }
        Glyph::Folder | Glyph::NewFolder => {
            closed(&[
                (2.5, 5.0),
                (7.5, 5.0),
                (9.0, 6.5),
                (17.5, 6.5),
                (17.5, 15.5),
                (2.5, 15.5),
            ]);
            line(&[(2.5, 8.5), (17.5, 8.5)]);
            if glyph == Glyph::NewFolder {
                line(&[(10.0, 10.0), (10.0, 14.0)]);
                line(&[(8.0, 12.0), (12.0, 12.0)]);
            }
        }
        Glyph::NewScript => {
            closed(&[(4.0, 3.0), (16.0, 3.0), (16.0, 17.0), (4.0, 17.0)]);
            for y in [6.5, 9.5, 12.5] {
                line(&[(6.5, y), (13.5, y)]);
            }
            line(&[(6.5, 15.0), (10.5, 15.0)]);
        }
        Glyph::List => {
            for y in [5.5, 10.0, 14.5] {
                line(&[(3.5, y), (16.5, y)]);
            }
        }
        Glyph::Star | Glyph::StarFilled => {
            let points: Vec<(f32, f32)> = (0..10)
                .map(|i| {
                    let r = if i % 2 == 0 { 7.5 } else { 3.2 };
                    let a = std::f32::consts::PI * (i as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
                    (10.0 + r * a.cos(), 10.5 + r * a.sin())
                })
                .collect();
            if glyph == Glyph::StarFilled {
                // The star is not convex: fill its center and the five
                // points separately.
                let center: Vec<(f32, f32)> = points.iter().skip(1).step_by(2).copied().collect();
                filled(&center, color);
                for i in (0..10).step_by(2) {
                    filled(&[points[(i + 9) % 10], points[i], points[i + 1]], color);
                }
            }
            closed(&points);
        }
        Glyph::Alphabetical => {
            line(&[(5.5, 3.0), (5.5, 16.5)]);
            line(&[(3.0, 14.0), (5.5, 16.5), (8.0, 14.0)]);
            // A small "a" over a "z".
            painter.circle_stroke(p(13.5, 6.0), 2.0 * scale, stroke);
            line(&[(15.5, 4.0), (15.5, 8.0)]);
            line(&[(11.5, 11.0), (16.0, 11.0), (11.5, 16.0), (16.0, 16.0)]);
        }
        Glyph::Eye => {
            let upper: Vec<(f32, f32)> = (0..=8)
                .map(|i| {
                    let t = i as f32 / 8.0;
                    (
                        2.0 + 16.0 * t,
                        10.0 - 5.0 * (std::f32::consts::PI * t).sin(),
                    )
                })
                .collect();
            let lower: Vec<(f32, f32)> = upper.iter().map(|(x, y)| (*x, 20.0 - y)).collect();
            let mut outline = upper.clone();
            outline.extend(lower.iter().rev().skip(1));
            filled(&outline, color);
            painter.circle_filled(p(10.0, 10.0), 3.4 * scale, Color32::WHITE);
            painter.circle_filled(p(10.0, 10.0), 2.0 * scale, color);
        }
        Glyph::Compatibility => {
            closed(&[(6.5, 3.5), (16.5, 3.5), (16.5, 13.5), (6.5, 13.5)]);
            filled(&[(3.5, 6.5), (13.5, 6.5), (13.5, 16.5), (3.5, 16.5)], color);
        }
        Glyph::LeftPane | Glyph::RightPane => {
            closed(&[(2.5, 4.0), (17.5, 4.0), (17.5, 16.0), (2.5, 16.0)]);
            let x = if glyph == Glyph::LeftPane { 7.0 } else { 13.0 };
            line(&[(x, 4.0), (x, 16.0)]);
        }
        Glyph::DisclosureRight => line(&[(8.0, 6.0), (12.0, 10.0), (8.0, 14.0)]),
        Glyph::DisclosureDown => line(&[(6.0, 8.0), (10.0, 12.0), (14.0, 8.0)]),
        Glyph::Help => {
            painter.circle_stroke(p(10.0, 10.0), 7.0 * scale, stroke);
            line(&[
                (8.0, 8.0),
                (8.5, 6.5),
                (10.0, 6.0),
                (11.5, 6.5),
                (12.0, 8.0),
            ]);
            line(&[(12.0, 8.0), (10.0, 10.0), (10.0, 11.5)]);
            painter.circle_filled(p(10.0, 14.0), 0.8 * scale, color);
        }
        Glyph::Close => {
            line(&[(6.0, 6.0), (14.0, 14.0)]);
            line(&[(14.0, 6.0), (6.0, 14.0)]);
        }
    }
}

/// The options gear on a selected step (35b): a blue disc with a white
/// cog.
pub(super) fn gear(painter: &Painter, center: Pos2, radius: f32, fill: Color32) {
    painter.circle_filled(center, radius, fill);
    let body = radius * 0.46;
    // Eight square teeth around a white wheel with a blue hub.
    for i in 0..8 {
        let a = std::f32::consts::TAU * i as f32 / 8.0;
        let dir = Vec2::angled(a);
        let side = Vec2::angled(a + std::f32::consts::FRAC_PI_2) * radius * 0.13;
        let inner = center + dir * body * 0.8;
        let outer = center + dir * radius * 0.72;
        painter.add(PathShape::convex_polygon(
            vec![inner - side, outer - side, outer + side, inner + side],
            Color32::WHITE,
            Stroke::NONE,
        ));
    }
    painter.circle_filled(center, body, Color32::WHITE);
    painter.circle_filled(center, radius * 0.2, fill);
}
