//! Monochrome line glyphs for the chrome, drawn with the painter on a 20x20
//! design grid so they stay crisp at any scale and need no icon font.
//!
//! They approximate FileMaker's toolbar artwork closely enough to read the
//! same at a glance; they are not copies of it.

use std::f32::consts::TAU;

use egui::epaint::{CircleShape, PathShape, PathStroke};
use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

/// Every glyph the chrome draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    ChevronLeft,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    ShowAll,
    NewRecord,
    DeleteRecord,
    Find,
    Sort,
    Share,
    NewRequest,
    DeleteRequest,
    PerformFind,
    CancelFind,
    SavedFinds,
    SaveExcel,
    SavePdf,
    Print,
    PageSetup,
    Objects,
    NewLayout,
    Selection,
    TextTool,
    ShapeTool,
    Rectangle,
    RoundedRectangle,
    Oval,
    FieldTool,
    ButtonTool,
    PanelTool,
    Portal,
    Chart,
    WebViewer,
    FormatPainter,
    PartTool,
    Manage,
    Inspector,
    ViewForm,
    ViewList,
    ViewTable,
    Pencil,
    Search,
    Position,
    Styles,
    Appearance,
    Data,
    Tiles,
    Lines,
    /// A file in a container field.
    File,
    /// A drop-down calendar's icon.
    Calendar,
    /// Home's own row in its sidebar: a house.
    Home,
    /// A host, and a file on one: a server of two stacked units.
    Server,
}

/// Maps the 20x20 design grid into a target rect.
struct Grid {
    origin: Pos2,
    scale: f32,
    stroke: Stroke,
}

impl Grid {
    fn new(rect: Rect, color: Color32) -> Self {
        let side = rect.width().min(rect.height());
        let scale = side / 20.0;
        let origin = rect.center() - Vec2::splat(side / 2.0);
        Self {
            origin,
            scale,
            stroke: Stroke::new((1.2 * scale).max(1.0), color),
        }
    }

    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x, y) * self.scale
    }

    fn r(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_two_pos(self.p(x0, y0), self.p(x1, y1))
    }

    fn color(&self) -> Color32 {
        self.stroke.color
    }

    fn line(&self, painter: &Painter, points: &[(f32, f32)]) {
        let points = points.iter().map(|&(x, y)| self.p(x, y)).collect();
        painter.add(PathShape::line(points, PathStroke::from(self.stroke)));
    }

    fn closed(&self, painter: &Painter, points: &[(f32, f32)]) {
        let points = points.iter().map(|&(x, y)| self.p(x, y)).collect();
        painter.add(PathShape::closed_line(
            points,
            PathStroke::from(self.stroke),
        ));
    }

    fn filled(&self, painter: &Painter, points: &[(f32, f32)], fill: Color32) {
        let points = points.iter().map(|&(x, y)| self.p(x, y)).collect();
        painter.add(PathShape::convex_polygon(points, fill, Stroke::NONE));
    }

    fn rect(&self, painter: &Painter, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32) {
        painter.rect_stroke(
            self.r(x0, y0, x1, y1),
            CornerRadius::same((radius * self.scale).round() as u8),
            self.stroke,
            StrokeKind::Middle,
        );
    }

    fn fill_rect(&self, painter: &Painter, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32) {
        painter.rect_filled(
            self.r(x0, y0, x1, y1),
            CornerRadius::same((radius * self.scale).round() as u8),
            self.color(),
        );
    }

    fn circle(&self, painter: &Painter, x: f32, y: f32, radius: f32) {
        painter.add(CircleShape::stroke(
            self.p(x, y),
            radius * self.scale,
            self.stroke,
        ));
    }

    fn dot(&self, painter: &Painter, x: f32, y: f32, radius: f32) {
        painter.circle_filled(self.p(x, y), radius * self.scale, self.color());
    }

    fn disc(&self, painter: &Painter, x: f32, y: f32, radius: f32, fill: Color32) {
        painter.circle_filled(self.p(x, y), radius * self.scale, fill);
    }

    fn magnifier(&self, painter: &Painter, x: f32, y: f32, radius: f32) {
        self.circle(painter, x, y, radius);
        let reach = radius * 0.72;
        self.line(
            painter,
            &[
                (x + reach, y + reach),
                (x + reach + radius * 0.8, y + reach + radius * 0.8),
            ],
        );
    }

    /// A page outline with a folded top-right corner.
    fn document(&self, painter: &Painter) {
        self.closed(
            painter,
            &[
                (5.0, 2.5),
                (12.0, 2.5),
                (15.5, 6.0),
                (15.5, 17.5),
                (5.0, 17.5),
            ],
        );
        self.line(painter, &[(12.0, 2.5), (12.0, 6.0), (15.5, 6.0)]);
    }

    /// FileMaker's record glyph: a tray with two stacked cards above it.
    fn record_tray(&self, painter: &Painter) {
        self.rect(painter, 5.5, 7.5, 18.0, 17.0, 2.0);
        let lid = Stroke::new(self.stroke.width * 1.4, self.color());
        painter.line_segment([self.p(7.5, 5.0), self.p(16.0, 5.0)], lid);
        painter.line_segment([self.p(9.0, 2.6), self.p(14.5, 2.6)], lid);
    }
}

/// Paint `icon` centered in `rect` in `color`. `background` is the surface
/// behind the glyph, used to knock out overlapping strokes.
pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    icon: Icon,
    color: Color32,
    background: Color32,
) {
    let g = Grid::new(rect, color);
    match icon {
        Icon::ChevronLeft => g.line(painter, &[(12.5, 3.5), (7.0, 10.0), (12.5, 16.5)]),
        Icon::ChevronRight => g.line(painter, &[(7.5, 3.5), (13.0, 10.0), (7.5, 16.5)]),
        Icon::ChevronDown => g.line(painter, &[(5.0, 7.5), (10.0, 12.5), (15.0, 7.5)]),
        Icon::ChevronUp => g.line(painter, &[(5.0, 12.5), (10.0, 7.5), (15.0, 12.5)]),
        Icon::ShowAll => {
            // Three cards in perspective, fanned to the upper left; each
            // hides the one behind it.
            for step in [6.0_f32, 3.0, 0.0] {
                let (x, y) = (10.0 - step, 5.0 - step * 0.4);
                let card = [
                    (x, y + 3.0),
                    (x + 6.5, y),
                    (x + 6.5, y + 9.5),
                    (x, y + 12.5),
                ];
                g.filled(painter, &card, background);
                g.closed(painter, &card);
            }
        }
        Icon::NewRecord | Icon::DeleteRecord => {
            g.record_tray(painter);
            // A solid disc with a knocked-out plus or minus, cut into the
            // tray's corner.
            g.disc(painter, 6.5, 15.0, 4.6, background);
            g.disc(painter, 6.5, 15.0, 3.6, color);
            let knockout = Stroke::new(g.stroke.width * 1.1, background);
            painter.line_segment([g.p(4.6, 15.0), g.p(8.4, 15.0)], knockout);
            if icon == Icon::NewRecord {
                painter.line_segment([g.p(6.5, 13.1), g.p(6.5, 16.9)], knockout);
            }
        }
        Icon::Find | Icon::Search => g.magnifier(painter, 8.5, 8.5, 5.5),
        Icon::Sort => {
            g.line(painter, &[(6.0, 3.0), (6.0, 16.5)]);
            g.line(painter, &[(3.0, 13.5), (6.0, 16.5), (9.0, 13.5)]);
            g.line(painter, &[(11.0, 5.0), (17.5, 5.0)]);
            g.line(painter, &[(11.0, 9.0), (16.0, 9.0)]);
            g.line(painter, &[(11.0, 13.0), (14.5, 13.0)]);
        }
        Icon::Share => {
            g.closed(
                painter,
                &[(2.5, 9.0), (17.5, 2.5), (11.0, 17.5), (9.0, 11.0)],
            );
            g.line(painter, &[(9.0, 11.0), (17.5, 2.5)]);
        }
        Icon::NewRequest | Icon::DeleteRequest => {
            g.rect(painter, 3.5, 3.5, 16.5, 16.5, 1.5);
            g.line(painter, &[(6.5, 10.0), (13.5, 10.0)]);
            if icon == Icon::NewRequest {
                g.line(painter, &[(10.0, 6.5), (10.0, 13.5)]);
            }
        }
        Icon::PerformFind => {
            g.magnifier(painter, 11.0, 8.5, 5.0);
            // A circular "go" arrow wrapping the lens on the left.
            let center = g.p(11.0, 8.5);
            let radius = 7.5 * g.scale;
            let points: Vec<Pos2> = (0..=10)
                .map(|i| {
                    let angle = TAU * (0.30 + 0.30 * i as f32 / 10.0);
                    center + Vec2::angled(angle) * radius
                })
                .collect();
            let end = *points.last().unwrap_or(&center);
            painter.add(PathShape::line(points, PathStroke::from(g.stroke)));
            painter.line_segment([end, end + Vec2::new(2.5, 0.5) * g.scale], g.stroke);
            painter.line_segment([end, end + Vec2::new(0.5, 2.8) * g.scale], g.stroke);
        }
        Icon::CancelFind => {
            g.line(painter, &[(4.5, 4.5), (15.5, 15.5)]);
            g.line(painter, &[(15.5, 4.5), (4.5, 15.5)]);
        }
        Icon::SavedFinds => {
            g.closed(
                painter,
                &[
                    (2.0, 4.0),
                    (16.0, 4.0),
                    (16.0, 14.0),
                    (8.0, 14.0),
                    (5.0, 17.0),
                    (5.0, 14.0),
                    (2.0, 14.0),
                ],
            );
            g.magnifier(painter, 8.0, 8.5, 2.6);
            g.dot(painter, 13.0, 9.0, 0.9);
        }
        Icon::SaveExcel => {
            g.document(painter);
            g.line(painter, &[(7.5, 9.5), (13.0, 9.5)]);
            g.line(painter, &[(7.5, 12.5), (13.0, 12.5)]);
            g.line(painter, &[(7.5, 15.0), (13.0, 15.0)]);
            g.line(painter, &[(10.2, 8.0), (10.2, 15.5)]);
        }
        Icon::SavePdf => {
            g.document(painter);
            g.fill_rect(painter, 7.5, 9.0, 13.0, 15.0, 0.5);
        }
        Icon::Print => {
            g.rect(painter, 2.5, 8.0, 17.5, 14.5, 1.5);
            g.rect(painter, 6.0, 3.0, 14.0, 8.0, 0.5);
            g.rect(painter, 6.0, 11.5, 14.0, 17.5, 0.5);
        }
        Icon::PageSetup => {
            g.document(painter);
            g.rect(painter, 7.5, 9.0, 13.0, 12.5, 0.5);
            g.line(painter, &[(7.5, 15.0), (13.0, 15.0)]);
        }
        Icon::Objects => {
            g.rect(painter, 2.5, 4.0, 17.5, 16.0, 1.5);
            g.line(painter, &[(7.0, 4.0), (7.0, 16.0)]);
        }
        Icon::NewLayout | Icon::File => g.document(painter),
        Icon::Selection => {
            let arrow = [
                (6.0, 3.0),
                (6.0, 16.0),
                (9.2, 12.8),
                (11.5, 17.5),
                (13.5, 16.5),
                (11.2, 12.0),
                (15.5, 12.0),
            ];
            if background == color {
                g.closed(painter, &arrow);
            } else {
                // Concave outline: fill it as two convex halves.
                g.filled(
                    painter,
                    &[(6.0, 3.0), (6.0, 16.0), (9.2, 12.8), (15.5, 12.0)],
                    color,
                );
                g.filled(
                    painter,
                    &[(9.2, 12.8), (11.5, 17.5), (13.5, 16.5), (11.2, 12.0)],
                    color,
                );
            }
        }
        Icon::TextTool => {
            g.line(painter, &[(4.5, 3.0), (2.5, 3.0), (2.5, 17.0), (4.5, 17.0)]);
            g.line(
                painter,
                &[(15.5, 3.0), (17.5, 3.0), (17.5, 17.0), (15.5, 17.0)],
            );
            g.line(painter, &[(6.0, 5.5), (14.0, 5.5)]);
            g.line(painter, &[(10.0, 5.5), (10.0, 14.5)]);
        }
        Icon::ShapeTool => g.line(painter, &[(4.0, 16.0), (16.0, 4.0)]),
        Icon::Rectangle => g.rect(painter, 3.0, 5.0, 17.0, 15.0, 0.0),
        Icon::RoundedRectangle => g.rect(painter, 3.0, 5.0, 17.0, 15.0, 3.0),
        Icon::Oval => {
            painter.add(egui::epaint::EllipseShape::stroke(
                g.p(10.0, 10.0),
                Vec2::new(7.0, 5.0) * g.scale,
                g.stroke,
            ));
        }
        Icon::FieldTool => {
            g.rect(painter, 2.5, 5.0, 17.5, 15.0, 1.0);
            g.line(painter, &[(5.0, 8.5), (15.0, 8.5)]);
            g.line(painter, &[(5.0, 11.5), (12.0, 11.5)]);
        }
        Icon::ButtonTool => {
            g.rect(painter, 2.5, 6.0, 17.5, 14.0, 4.0);
            g.dot(painter, 6.5, 10.0, 1.4);
            g.line(painter, &[(9.5, 10.0), (14.5, 10.0)]);
        }
        Icon::PanelTool => {
            g.rect(painter, 2.5, 7.0, 17.5, 16.5, 1.0);
            g.line(painter, &[(2.5, 7.0), (2.5, 3.5), (8.5, 3.5), (8.5, 7.0)]);
            g.line(painter, &[(8.5, 3.5), (14.0, 3.5), (14.0, 7.0)]);
        }
        Icon::Portal => {
            g.rect(painter, 2.5, 3.5, 17.5, 16.5, 1.0);
            g.line(painter, &[(2.5, 7.5), (17.5, 7.5)]);
            g.line(painter, &[(2.5, 12.0), (17.5, 12.0)]);
            g.line(painter, &[(8.0, 3.5), (8.0, 16.5)]);
        }
        Icon::Chart => {
            g.line(painter, &[(2.5, 17.0), (17.5, 17.0)]);
            g.fill_rect(painter, 4.0, 9.0, 6.5, 16.0, 0.0);
            g.fill_rect(painter, 8.7, 4.0, 11.2, 16.0, 0.0);
            g.fill_rect(painter, 13.4, 7.0, 15.9, 16.0, 0.0);
        }
        Icon::WebViewer => {
            g.circle(painter, 10.0, 10.0, 7.5);
            painter.add(egui::epaint::EllipseShape::stroke(
                g.p(10.0, 10.0),
                Vec2::new(3.3, 7.5) * g.scale,
                g.stroke,
            ));
            g.line(painter, &[(2.5, 10.0), (17.5, 10.0)]);
        }
        Icon::FormatPainter => {
            g.line(painter, &[(15.5, 2.5), (9.0, 11.0)]);
            g.closed(
                painter,
                &[(9.0, 11.0), (6.0, 11.0), (4.0, 17.5), (10.5, 14.0)],
            );
        }
        Icon::PartTool => {
            g.rect(painter, 2.5, 3.5, 15.0, 16.5, 1.0);
            g.line(painter, &[(2.5, 8.0), (15.0, 8.0)]);
            g.line(painter, &[(2.5, 12.5), (15.0, 12.5)]);
            g.line(
                painter,
                &[(15.0, 6.0), (17.5, 6.0), (17.5, 10.0), (15.0, 10.0)],
            );
        }
        Icon::Manage => {
            g.circle(painter, 10.0, 10.0, 2.6);
            g.circle(painter, 10.0, 10.0, 5.6);
            for tooth in 0..8 {
                let direction = Vec2::angled(TAU * tooth as f32 / 8.0);
                let center = g.p(10.0, 10.0);
                painter.line_segment(
                    [
                        center + direction * 5.6 * g.scale,
                        center + direction * 7.8 * g.scale,
                    ],
                    Stroke::new(g.stroke.width * 1.6, color),
                );
            }
        }
        Icon::Inspector => {
            g.rect(painter, 2.5, 4.0, 17.5, 16.0, 1.5);
            g.line(painter, &[(12.0, 4.0), (12.0, 16.0)]);
        }
        Icon::ViewForm => {
            g.rect(painter, 2.5, 4.0, 17.5, 16.0, 0.5);
            g.line(painter, &[(2.5, 7.5), (17.5, 7.5)]);
        }
        Icon::ViewList | Icon::Lines => {
            for y in [5.0, 8.3, 11.6, 14.9] {
                g.line(painter, &[(2.5, y), (17.5, y)]);
            }
        }
        Icon::Home => {
            g.closed(
                painter,
                &[
                    (2.5, 9.5),
                    (10.0, 2.5),
                    (17.5, 9.5),
                    (15.5, 9.5),
                    (15.5, 17.5),
                    (4.5, 17.5),
                    (4.5, 9.5),
                ],
            );
            g.rect(painter, 8.25, 12.0, 11.75, 17.5, 0.5);
        }
        Icon::Server => {
            g.rect(painter, 2.5, 3.5, 17.5, 9.5, 1.5);
            g.rect(painter, 2.5, 10.5, 17.5, 16.5, 1.5);
            g.dot(painter, 5.5, 6.5, 0.9);
            g.dot(painter, 5.5, 13.5, 0.9);
            g.line(painter, &[(9.5, 6.5), (14.5, 6.5)]);
            g.line(painter, &[(9.5, 13.5), (14.5, 13.5)]);
        }
        Icon::Calendar => {
            g.rect(painter, 2.5, 4.0, 17.5, 17.0, 1.5);
            g.line(painter, &[(2.5, 8.0), (17.5, 8.0)]);
            g.line(painter, &[(6.5, 2.0), (6.5, 5.5)]);
            g.line(painter, &[(13.5, 2.0), (13.5, 5.5)]);
            for (x, y) in [
                (6.5, 11.0),
                (10.0, 11.0),
                (13.5, 11.0),
                (6.5, 14.0),
                (10.0, 14.0),
            ] {
                g.dot(painter, x, y, 0.9);
            }
        }
        Icon::ViewTable => {
            g.rect(painter, 2.5, 4.0, 17.5, 16.0, 0.5);
            g.line(painter, &[(2.5, 8.0), (17.5, 8.0)]);
            g.line(painter, &[(2.5, 12.0), (17.5, 12.0)]);
            g.line(painter, &[(7.5, 4.0), (7.5, 16.0)]);
            g.line(painter, &[(12.5, 4.0), (12.5, 16.0)]);
        }
        Icon::Pencil => {
            g.closed(
                painter,
                &[
                    (4.0, 16.0),
                    (5.0, 12.5),
                    (14.0, 3.5),
                    (16.5, 6.0),
                    (7.5, 15.0),
                ],
            );
            g.line(painter, &[(12.0, 5.5), (14.5, 8.0)]);
        }
        Icon::Position => {
            g.rect(painter, 2.5, 6.5, 17.5, 13.5, 1.0);
            for x in [5.5, 8.5, 11.5, 14.5] {
                g.line(painter, &[(x, 6.5), (x, 9.5)]);
            }
        }
        Icon::Styles => {
            g.line(painter, &[(15.5, 2.5), (10.0, 10.0)]);
            g.closed(
                painter,
                &[(10.0, 10.0), (7.5, 10.0), (4.0, 17.0), (11.0, 12.5)],
            );
        }
        Icon::Appearance => {
            g.circle(painter, 10.0, 10.0, 7.0);
            for (x, y) in [(7.0, 7.5), (10.5, 6.0), (13.5, 8.5), (7.0, 12.0)] {
                g.dot(painter, x, y, 1.1);
            }
        }
        Icon::Data => {
            painter.add(egui::epaint::EllipseShape::stroke(
                g.p(10.0, 5.0),
                Vec2::new(6.0, 2.2) * g.scale,
                g.stroke,
            ));
            g.line(painter, &[(4.0, 5.0), (4.0, 15.0)]);
            g.line(painter, &[(16.0, 5.0), (16.0, 15.0)]);
            let bottom: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let angle = TAU * 0.5 * i as f32 / 12.0;
                    g.p(10.0, 15.0) + Vec2::new(angle.cos() * 6.0, angle.sin() * 2.2) * g.scale
                })
                .collect();
            painter.add(PathShape::line(bottom, PathStroke::from(g.stroke)));
        }
        Icon::Tiles => {
            g.rect(painter, 3.5, 3.5, 9.0, 9.0, 1.0);
            g.rect(painter, 11.0, 3.5, 16.5, 9.0, 1.0);
            g.rect(painter, 3.5, 11.0, 9.0, 16.5, 1.0);
            g.rect(painter, 11.0, 11.0, 16.5, 16.5, 1.0);
        }
    }
}
