use egui::{pos2, vec2, Id, Rect, Sense, Ui};
use om_layout::{Layout, LayoutObject};

use super::color;

pub(super) fn navigation(
    ui: &Ui,
    layout: &Layout,
    control: &LayoutObject,
    panels: &[&LayoutObject],
    rect: Rect,
    scope: Id,
    selected: &mut u64,
) {
    if panels.is_empty() {
        return;
    }
    let normal = layout.object_style(control, 26, 1);
    let active = layout.object_style(control, 26, 7);
    let Some(inactive_fill) = normal.background else {
        return;
    };
    let active_fill = active.background.unwrap_or(inactive_fill);
    // For the measured theme the dot margin and its row's bottom inset
    // use the control's typography. The default is 12 pt in that theme.
    let font_size = layout.object_style(control, 1, 1).font_size.unwrap_or(12.0);
    let length = |component, property| {
        let length = layout
            .object_property(control, component, 1, property)?
            .length
            .as_ref()?;
        let points = if length.unit == 7 && matches!(length.auto_value, 0 | 1) {
            length.value * font_size
        } else {
            length.points()?
        } as f32;
        (points.is_finite() && points >= 0.0).then_some(points)
    };
    let Some((width, height)) = length(26, 109).zip(length(26, 110)) else {
        return;
    };
    let Some(row_height) = length(27, 110) else {
        return;
    };
    if width <= 0.0 || height <= 0.0 || row_height < height {
        return;
    }
    let gap = length(26, 108).unwrap_or(0.0);
    let bottom = length(27, 119).unwrap_or(0.0);
    let advance = width + gap;
    let span = advance * (panels.len() - 1) as f32;
    let x = rect.center().x - span / 2.0;
    let y = rect.bottom() - bottom - row_height / 2.0;
    if !x.is_finite() || !y.is_finite() {
        return;
    }
    let clip = ui.clip_rect().intersect(rect);
    let painter = ui.painter().with_clip_rect(clip);
    for (index, panel) in panels.iter().enumerate() {
        let center = pos2(x + advance * index as f32, y);
        let dot = Rect::from_center_size(center, vec2(width, height));
        let hit = dot.intersect(clip);
        if !hit.is_positive() {
            continue;
        }
        let response = ui.interact(
            hit,
            scope.with((layout.id, panel.id, "slide dot")),
            Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::Tab,
                ui.is_enabled(),
                *selected == panel.id,
                format!("Slide {}", index + 1),
            )
        });
        if response.clicked() {
            *selected = panel.id;
            ui.ctx().request_repaint();
        }
        let fill = if *selected == panel.id {
            active_fill
        } else {
            inactive_fill
        };
        painter.add(egui::epaint::EllipseShape::filled(
            center,
            dot.size() / 2.0,
            color(fill),
        ));
    }
}
