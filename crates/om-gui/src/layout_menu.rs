//! Menu presentation only; script navigation keeps the unfiltered layout list.
use om_data::LayoutMenuEntry;

use crate::{
    app::{Action, Mode},
    menubar::MenuRows,
    session::Session,
};

pub(crate) fn show(
    rows: &mut dyn MenuRows,
    session: &Session,
    mode: Mode,
    actions: &mut Vec<Action>,
) {
    if mode == Mode::Layout {
        for layout in &session.layouts {
            row(rows, layout.id, &layout.name, session.layout, actions);
        }
    } else {
        entries(rows, &session.layout_menu, session.layout, actions, 0);
    }
}

fn row(
    rows: &mut dyn MenuRows,
    id: u32,
    name: &str,
    current: Option<u32>,
    actions: &mut Vec<Action>,
) {
    let selected = current == Some(id);
    if rows.item(name, None, selected, true) && !selected {
        actions.push(Action::SelectLayout(id));
    }
}

fn entries(
    rows: &mut dyn MenuRows,
    items: &[LayoutMenuEntry],
    current: Option<u32>,
    actions: &mut Vec<Action>,
    depth: usize,
) {
    let mut i = 0;
    while i < items.len() {
        let entry = &items[i];
        if entry.flags & 1 != 0 {
            let mut end = i + 1;
            let mut nesting = 1usize;
            while end < items.len() {
                if items[end].flags & 1 != 0 {
                    nesting += 1;
                } else if items[end].flags & 2 != 0 {
                    nesting -= 1;
                }
                if nesting == 0 {
                    break;
                }
                end += 1;
            }
            // Folders commonly also carry 0x08; it suppresses layouts,
            // not the structural folder row. Bound malformed nesting.
            if depth < 64 {
                rows.submenu(&entry.name, true, &mut |rows| {
                    entries(rows, &items[i + 1..end], current, actions, depth + 1)
                });
            }
            i = end;
        } else if entry.flags & (2 | 8) == 0 {
            if entry.flags & 0x10 != 0 {
                rows.separator();
            } else {
                row(rows, entry.id, &entry.name, current, actions);
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_flag_draws_a_rule_and_never_navigates() {
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            entries(
                &mut crate::menubar::Drawn(ui),
                &[LayoutMenuEntry {
                    id: 99,
                    name: "-".into(),
                    flags: 0x10,
                }],
                None,
                &mut actions,
                0,
            );
        });
        output.textures_delta.clear();
        assert!(actions.is_empty());
        assert!(output.shapes.iter().any(|s| matches!(&s.shape,
            egui::Shape::LineSegment { points, stroke }
                if points[0].y == points[1].y && stroke.color == egui::Color32::from_gray(0xDA))));
    }
}
