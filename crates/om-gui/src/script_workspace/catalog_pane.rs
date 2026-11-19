//! The scripts pane (capture 35): the catalog's scripts, folders and
//! separators in display order, with search, disclosure triangles,
//! multi-select, drag to reorder and into folders, and the commands of the
//! pane's header and context menu. FileMaker commits these changes to the
//! catalog at once (no Save); here each one produces the whole new catalog
//! for the provider to store.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use egui::{pos2, vec2, Align2, Color32, Key, Rect, Role, Sense, Stroke, Ui, WidgetInfo};
use om_data::{Script, ScriptCatalog, ScriptKind};

use super::glyphs::{self, Glyph};
use crate::theme;

const ROW: f32 = 22.0;
/// Indentation of one folder level.
const LEVEL: f32 = 16.0;

/// Member flags of a folder and its end (scripts.md; script-steps.md 1).
const FOLDER_FLAGS: u8 = 0x01;
const FOLDER_END_FLAGS: u8 = 0x02;

/// What the pane asks of the workspace.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum PaneRequest {
    /// Show the script in the preview tab (single click).
    Preview(u64),
    /// Open the script for editing (double click).
    Open(u64),
    /// Store this catalog (a new, renamed, moved, duplicated member...).
    Change(ScriptCatalog),
    /// Ask before deleting these members.
    Delete(Vec<u64>),
    /// Run the script.
    Run(u64),
    /// Copy these scripts' steps as text.
    CopyText(Vec<u64>),
}

/// Where a dragged member lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// Before this member.
    Before(u64),
    /// At the start of this folder.
    Into(u64),
    /// At the end of the catalog.
    End,
}

/// The positions a member spans: a folder runs to its end marker.
pub(super) fn span(scripts: &[Script], index: usize) -> Range<usize> {
    if scripts[index].kind != ScriptKind::Folder {
        return index..index + 1;
    }
    let mut depth = 0usize;
    for (i, s) in scripts.iter().enumerate().skip(index) {
        match s.kind {
            ScriptKind::Folder => depth += 1,
            ScriptKind::FolderEnd => {
                depth -= 1;
                if depth == 0 {
                    return index..i + 1;
                }
            }
            _ => {}
        }
    }
    index..scripts.len()
}

fn position(catalog: &ScriptCatalog, id: u64) -> Option<usize> {
    catalog.scripts.iter().position(|s| s.id == id)
}

/// The next free member id.
pub(super) fn next_id(catalog: &ScriptCatalog) -> u64 {
    catalog.scripts.iter().map(|s| s.id).max().unwrap_or(0) + 1
}

/// `base`, or `base 2`, `base 3`... when taken.
fn unique_name(catalog: &ScriptCatalog, base: &str) -> String {
    let taken = |name: &str| {
        catalog
            .scripts
            .iter()
            .any(|s| s.kind == ScriptKind::Script && s.name.eq_ignore_ascii_case(name))
    };
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken(name))
        .expect("a free name")
}

fn member(id: u64, name: &str, kind: ScriptKind) -> Script {
    use om_data::runtime::OwnedNode;
    let metadata = match kind {
        // A folder keeps an empty step list, as FileMaker stores one.
        ScriptKind::Folder => vec![
            OwnedNode::leaf([4], Vec::new()),
            OwnedNode::leaf([0xf0], [FOLDER_FLAGS]),
        ],
        ScriptKind::FolderEnd => vec![OwnedNode::leaf([0xf0], [FOLDER_END_FLAGS])],
        _ => Vec::new(),
    };
    Script {
        id,
        name: name.to_owned(),
        kind,
        metadata,
        ..Script::default()
    }
}

/// Where new members go: after the selected member's span, or at the
/// start of a selected open folder; else at the end.
fn insertion(catalog: &ScriptCatalog, after: Option<u64>, open_folder: bool) -> usize {
    match after.and_then(|id| position(catalog, id)) {
        Some(i) if open_folder && catalog.scripts[i].kind == ScriptKind::Folder => i + 1,
        Some(i) => span(&catalog.scripts, i).end,
        None => catalog.scripts.len(),
    }
}

/// A new script named "New Script" (numbered when taken); its id.
pub(super) fn new_script(
    catalog: &mut ScriptCatalog,
    after: Option<u64>,
    open_folder: bool,
) -> u64 {
    let id = next_id(catalog);
    let name = unique_name(catalog, "New Script");
    let at = insertion(catalog, after, open_folder);
    catalog
        .scripts
        .insert(at, member(id, &name, ScriptKind::Script));
    id
}

/// A new folder with its end marker; the folder's id.
pub(super) fn new_folder(
    catalog: &mut ScriptCatalog,
    after: Option<u64>,
    open_folder: bool,
) -> u64 {
    let id = next_id(catalog);
    let at = insertion(catalog, after, open_folder);
    catalog.scripts.splice(
        at..at,
        [
            member(id, "New Folder", ScriptKind::Folder),
            member(id + 1, "--", ScriptKind::FolderEnd),
        ],
    );
    id
}

pub(super) fn new_separator(
    catalog: &mut ScriptCatalog,
    after: Option<u64>,
    open_folder: bool,
) -> u64 {
    let id = next_id(catalog);
    let at = insertion(catalog, after, open_folder);
    catalog
        .scripts
        .insert(at, member(id, "-", ScriptKind::Separator));
    id
}

/// Remove members (a folder with everything in it).
pub(super) fn delete(catalog: &mut ScriptCatalog, ids: &[u64]) {
    let mut gone = BTreeSet::new();
    for id in ids {
        if let Some(i) = position(catalog, *id) {
            gone.extend(span(&catalog.scripts, i));
        }
    }
    let mut i = 0;
    catalog.scripts.retain(|_| {
        i += 1;
        !gone.contains(&(i - 1))
    });
}

/// Copies of members after the last one: scripts named "<name> Copy",
/// folders with everything in them; new ids and UUIDs. The copies' ids.
pub(super) fn duplicate(catalog: &mut ScriptCatalog, ids: &[u64]) -> Vec<u64> {
    let mut copies = Vec::new();
    let mut next = next_id(catalog);
    let mut last_end = 0;
    for id in ids {
        let Some(i) = position(catalog, *id) else {
            continue;
        };
        let range = span(&catalog.scripts, i);
        last_end = last_end.max(range.end);
        for (n, original) in catalog.scripts[range].iter().enumerate() {
            let mut copy = original.clone();
            copy.id = next;
            next += 1;
            if n == 0 && copy.kind != ScriptKind::Separator {
                copy.name = format!("{} Copy", copy.name);
            }
            // A copy is a new member: a fresh UUID (d8).
            for node in &mut copy.metadata {
                if node.segment == [0xd8] {
                    node.value = Some(super::editor::fresh_uuid().to_vec());
                }
            }
            copies.push(copy);
        }
    }
    let ids = copies
        .iter()
        .filter(|c| c.kind != ScriptKind::FolderEnd)
        .map(|c| c.id)
        .collect();
    catalog.scripts.splice(last_end..last_end, copies);
    ids
}

/// Move members (folders with their contents) to `target`; nothing moves
/// into itself.
pub(super) fn move_members(catalog: &mut ScriptCatalog, ids: &[u64], target: Target) -> bool {
    let mut moving = BTreeSet::new();
    for id in ids {
        if let Some(i) = position(catalog, *id) {
            moving.extend(span(&catalog.scripts, i));
        }
    }
    let anchor = match target {
        Target::Before(id) | Target::Into(id) => position(catalog, id),
        Target::End => None,
    };
    if anchor.is_some_and(|a| moving.contains(&a)) {
        return false;
    }
    let (kept, moved): (Vec<_>, Vec<_>) = catalog
        .scripts
        .drain(..)
        .enumerate()
        .partition(|(i, _)| !moving.contains(i));
    let mut kept: Vec<Script> = kept.into_iter().map(|(_, s)| s).collect();
    let moved: Vec<Script> = moved.into_iter().map(|(_, s)| s).collect();
    let at = match target {
        Target::Before(id) => kept.iter().position(|s| s.id == id),
        Target::Into(id) => kept.iter().position(|s| s.id == id).map(|i| i + 1),
        Target::End => None,
    }
    .unwrap_or(kept.len());
    kept.splice(at..at, moved);
    catalog.scripts = kept;
    true
}

/// One visible row of the pane.
struct Row {
    id: u64,
    depth: usize,
    kind: ScriptKind,
    name: String,
}

#[derive(Default)]
pub(super) struct ScriptsPane {
    pub search: String,
    pub selected: BTreeSet<u64>,
    anchor: Option<u64>,
    /// Folders shown closed.
    pub collapsed: BTreeSet<u64>,
    /// A member being renamed and its new name.
    pub renaming: Option<(u64, String)>,
    rename_focus: bool,
    /// Show the "Include in menu" checkboxes.
    pub checkboxes: bool,
    drop: Option<Target>,
}

impl ScriptsPane {
    /// Follow members the host stored under other ids than the pane gave
    /// them (`ids`: the pane's id to the host's).
    pub fn remap(&mut self, ids: &BTreeMap<u64, u64>) {
        let map = |id: u64| ids.get(&id).copied().unwrap_or(id);
        self.selected = self.selected.iter().map(|id| map(*id)).collect();
        self.collapsed = self.collapsed.iter().map(|id| map(*id)).collect();
        self.anchor = self.anchor.map(map);
        if let Some((id, _)) = &mut self.renaming {
            *id = map(*id);
        }
    }

    pub fn rename(&mut self, catalog: &ScriptCatalog, id: u64) {
        if !catalog.writable {
            return;
        }
        if let Some(s) = catalog.scripts.iter().find(|s| s.id == id) {
            self.renaming = Some((id, s.name.clone()));
            self.rename_focus = true;
        }
    }

    /// The single selected member, for the commands that act after it.
    fn after(&self) -> Option<u64> {
        self.selected.last().copied()
    }

    fn rows(&self, catalog: &ScriptCatalog) -> Vec<Row> {
        let query = self.search.trim().to_lowercase();
        let mut rows = Vec::new();
        let mut depth = 0usize;
        let mut hidden_below: Option<usize> = None;
        for s in &catalog.scripts {
            let visible = hidden_below.is_none_or(|d| depth < d);
            match s.kind {
                ScriptKind::FolderEnd => {
                    depth = depth.saturating_sub(1);
                    if hidden_below == Some(depth + 1) {
                        hidden_below = None;
                    }
                    continue;
                }
                ScriptKind::Folder => {
                    if visible && query.is_empty() {
                        rows.push(Row {
                            id: s.id,
                            depth,
                            kind: s.kind,
                            name: s.name.clone(),
                        });
                    }
                    depth += 1;
                    if visible && query.is_empty() && self.collapsed.contains(&s.id) {
                        hidden_below = Some(depth);
                    }
                    continue;
                }
                _ => {}
            }
            if !query.is_empty() {
                // Search lists the matching scripts alone.
                if s.kind == ScriptKind::Script && s.name.to_lowercase().contains(&query) {
                    rows.push(Row {
                        id: s.id,
                        depth: 0,
                        kind: s.kind,
                        name: s.name.clone(),
                    });
                }
            } else if visible {
                rows.push(Row {
                    id: s.id,
                    depth,
                    kind: s.kind,
                    name: s.name.clone(),
                });
            }
        }
        rows
    }

    fn select(&mut self, rows: &[Row], id: u64, extend: bool, toggle: bool) {
        if extend {
            let anchor = self.anchor.unwrap_or(id);
            let a = rows.iter().position(|r| r.id == anchor);
            let b = rows.iter().position(|r| r.id == id);
            if let (Some(a), Some(b)) = (a, b) {
                self.selected = rows[a.min(b)..=a.max(b)].iter().map(|r| r.id).collect();
                return;
            }
        }
        if toggle {
            if !self.selected.remove(&id) {
                self.selected.insert(id);
            }
        } else {
            self.selected = BTreeSet::from([id]);
        }
        self.anchor = Some(id);
    }

    /// The header's commands: new script, new folder, checkboxes.
    fn header(
        &mut self,
        ui: &mut Ui,
        catalog: &ScriptCatalog,
        writable: bool,
        out: &mut Vec<PaneRequest>,
    ) {
        let full = ui.available_rect_before_wrap();
        let header = Rect::from_min_size(full.min, vec2(full.width(), 26.0));
        ui.painter().text(
            pos2(header.left() + 6.0, header.center().y),
            Align2::LEFT_CENTER,
            "Scripts",
            egui::FontId::new(theme::SMALL, theme::semibold()),
            theme::TEXT,
        );
        let buttons = [
            (Glyph::NewScript, "New Script"),
            (Glyph::NewFolder, "New Folder"),
            (Glyph::List, "Show Checkboxes"),
        ];
        for (n, (glyph, label)) in buttons.into_iter().enumerate() {
            let center = pos2(header.right() - 70.0 + n as f32 * 26.0, header.center().y);
            let rect = Rect::from_center_size(center, vec2(18.0, 18.0));
            let enabled = n == 2 || writable;
            let response = ui.interact(
                rect,
                ui.scope_id().with(label),
                if enabled {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            let on = n == 2 && self.checkboxes;
            glyphs::paint(
                ui.painter(),
                rect,
                glyph,
                if !enabled {
                    theme::DISABLED
                } else if on {
                    theme::SELECTED
                } else {
                    theme::ICON
                },
            );
            response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
            if response.clicked() && enabled {
                match n {
                    0 => self.create(catalog, ScriptKind::Script, writable, out),
                    1 => self.create(catalog, ScriptKind::Folder, writable, out),
                    _ => self.checkboxes = !self.checkboxes,
                }
            }
            response.on_hover_text(label);
        }
        ui.painter().hline(
            full.x_range(),
            header.bottom(),
            Stroke::new(1.0, theme::HAIRLINE),
        );
        ui.advance_cursor_after_rect(header);
    }

    /// Add a script, folder or separator after the selection.
    pub fn create(
        &mut self,
        catalog: &ScriptCatalog,
        kind: ScriptKind,
        writable: bool,
        out: &mut Vec<PaneRequest>,
    ) {
        if !writable || !catalog.writable {
            return;
        }
        let mut next = catalog.clone();
        let after = self.after();
        let open_folder = after.is_some_and(|id| !self.collapsed.contains(&id));
        let id = match kind {
            ScriptKind::Folder => new_folder(&mut next, after, open_folder),
            ScriptKind::Separator => new_separator(&mut next, after, open_folder),
            _ => new_script(&mut next, after, open_folder),
        };
        self.selected = BTreeSet::from([id]);
        self.anchor = Some(id);
        if kind != ScriptKind::Separator {
            self.renaming = next
                .scripts
                .iter()
                .find(|s| s.id == id)
                .map(|s| (id, s.name.clone()));
            self.rename_focus = true;
        }
        out.push(PaneRequest::Change(next));
        if kind == ScriptKind::Script {
            out.push(PaneRequest::Open(id));
        }
    }

    /// Draw the pane.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        catalog: &ScriptCatalog,
        writable: bool,
        focused: bool,
        open: &BTreeSet<u64>,
    ) -> Vec<PaneRequest> {
        let mut out = Vec::new();
        if !writable {
            self.drop = None;
            self.renaming = None;
        }
        // Whether a text field had the keyboard as the frame began: the
        // Return that ends a rename must not start another.
        let keys_free = focused && !ui.ctx().egui_wants_keyboard_input();
        self.header(ui, catalog, writable, &mut out);
        ui.add_space(6.0);
        super::search_field(ui, &mut self.search, "Search Scripts", true);
        ui.add_space(6.0);
        let rows = self.rows(catalog);
        egui::ScrollArea::vertical()
            .id_salt("scripts_pane")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in &rows {
                    self.row(ui, catalog, row, &rows, focused, open, &mut out);
                }
                // Dropping below the last row moves to the end.
                let (rest, response) = ui.allocate_exact_size(
                    vec2(ui.available_width(), ui.available_height().max(ROW)),
                    Sense::click(),
                );
                if response.clicked() {
                    self.selected.clear();
                }
                if self.drop.is_some() && ui.rect_contains_pointer(rest) {
                    self.drop = Some(Target::End);
                    ui.painter().hline(
                        rest.x_range().shrink(6.0),
                        rest.top(),
                        Stroke::new(2.0, theme::HIGHLIGHT),
                    );
                }
                response.context_menu(|ui| self.menu(ui, catalog, None, &mut out));
            });
        if keys_free && !ui.ctx().egui_wants_keyboard_input() {
            self.keys(ui, catalog, &rows, &mut out);
        }
        out
    }

    fn keys(&mut self, ui: &Ui, catalog: &ScriptCatalog, rows: &[Row], out: &mut Vec<PaneRequest>) {
        let pressed = |key: Key| ui.input(|i| i.key_pressed(key));
        let shift = ui.input(|i| i.modifiers.shift);
        let current = self
            .anchor
            .and_then(|id| rows.iter().position(|r| r.id == id));
        let go = |pane: &mut Self, index: usize, out: &mut Vec<PaneRequest>| {
            if let Some(row) = rows.get(index) {
                pane.select(rows, row.id, shift, false);
                if row.kind == ScriptKind::Script && !shift {
                    out.push(PaneRequest::Preview(row.id));
                }
            }
        };
        if pressed(Key::ArrowDown) {
            go(self, current.map_or(0, |i| i + 1), out);
        } else if pressed(Key::ArrowUp) {
            go(self, current.map_or(0, |i| i.saturating_sub(1)), out);
        } else if pressed(Key::ArrowRight) || pressed(Key::ArrowLeft) {
            if let Some(row) = current.and_then(|i| rows.get(i)) {
                if row.kind == ScriptKind::Folder {
                    if pressed(Key::ArrowRight) {
                        self.collapsed.remove(&row.id);
                    } else {
                        self.collapsed.insert(row.id);
                    }
                }
            }
        } else if catalog.writable
            && (pressed(Key::Delete) || pressed(Key::Backspace))
            && !self.selected.is_empty()
        {
            out.push(PaneRequest::Delete(self.selected.iter().copied().collect()));
        } else if catalog.writable && pressed(Key::Enter) {
            if let Some(id) = self.anchor {
                self.rename(catalog, id);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn row(
        &mut self,
        ui: &mut Ui,
        catalog: &ScriptCatalog,
        row: &Row,
        rows: &[Row],
        focused: bool,
        open: &BTreeSet<u64>,
        out: &mut Vec<PaneRequest>,
    ) {
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), ROW),
            if catalog.writable {
                Sense::click_and_drag()
            } else {
                Sense::click()
            },
        );
        let selected = self.selected.contains(&row.id);
        let painter = ui.painter().clone();
        let fill_rect = rect.shrink2(vec2(4.0, 1.0));
        if selected {
            painter.rect_filled(
                fill_rect,
                4.0,
                if focused {
                    theme::SELECTED
                } else {
                    Color32::from_gray(0xDC)
                },
            );
        }
        let text_color = if selected && focused {
            Color32::WHITE
        } else {
            theme::TEXT
        };
        let mut x = rect.left() + 8.0 + row.depth as f32 * LEVEL;
        let script = catalog.scripts.iter().find(|s| s.id == row.id);
        if self.checkboxes && row.kind != ScriptKind::Separator {
            let in_menu = script.is_some_and(Script::in_scripts_menu);
            let box_rect = Rect::from_center_size(pos2(x + 6.0, rect.center().y), vec2(12.0, 12.0));
            let check = ui.interact(
                box_rect,
                ui.scope_id().with(("in_menu", row.id)),
                if catalog.writable {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            painter.rect(
                box_rect,
                3.0,
                if in_menu {
                    theme::SELECTED
                } else {
                    Color32::WHITE
                },
                Stroke::new(1.0, theme::FIELD_BORDER),
                egui::StrokeKind::Inside,
            );
            if in_menu {
                glyphs::paint(
                    &painter,
                    box_rect.shrink(1.0),
                    Glyph::DisclosureDown,
                    Color32::WHITE,
                );
            }
            let label = format!("Include {} in Scripts Menu", row.name);
            check.widget_info(|| {
                WidgetInfo::selected(Role::CheckBox, catalog.writable, in_menu, &label)
            });
            if check.clicked() && catalog.writable {
                let mut next = catalog.clone();
                if let Some(s) = next.scripts.iter_mut().find(|s| s.id == row.id) {
                    s.set_in_scripts_menu(!in_menu);
                }
                out.push(PaneRequest::Change(next));
            }
            x += 18.0;
        }
        match row.kind {
            ScriptKind::Folder => {
                let closed = self.collapsed.contains(&row.id);
                let tri = Rect::from_center_size(pos2(x + 4.0, rect.center().y), vec2(12.0, 12.0));
                glyphs::paint(
                    &painter,
                    tri,
                    if closed {
                        Glyph::DisclosureRight
                    } else {
                        Glyph::DisclosureDown
                    },
                    theme::CAPTION,
                );
                let toggle = ui.interact(
                    tri.expand(3.0),
                    ui.scope_id().with(("disclose", row.id)),
                    Sense::click(),
                );
                if toggle.clicked() {
                    if closed {
                        self.collapsed.remove(&row.id);
                    } else {
                        self.collapsed.insert(row.id);
                    }
                }
                glyphs::paint(
                    &painter,
                    Rect::from_center_size(pos2(x + 22.0, rect.center().y), vec2(17.0, 17.0)),
                    Glyph::Folder,
                    if selected && focused {
                        Color32::WHITE
                    } else {
                        theme::ICON
                    },
                );
                x += 34.0;
            }
            ScriptKind::Script => {
                glyphs::paint(
                    &painter,
                    Rect::from_center_size(pos2(x + 22.0, rect.center().y), vec2(17.0, 17.0)),
                    Glyph::Script,
                    if selected && focused {
                        Color32::WHITE
                    } else {
                        theme::ICON
                    },
                );
                x += 34.0;
            }
            _ => {}
        }
        let renaming = self.renaming.as_ref().is_some_and(|(id, _)| *id == row.id);
        if renaming {
            let field = Rect::from_min_max(
                pos2(x - 2.0, rect.top() + 1.0),
                pos2(rect.right() - 6.0, rect.bottom() - 1.0),
            );
            let (_, name) = self.renaming.as_mut().expect("renaming");
            let edit = ui.put(
                field,
                egui::TextEdit::singleline(name).desired_width(field.width()),
            );
            crate::widgets::accessible_label(ui, edit.id, "Script Name");
            if self.rename_focus {
                edit.request_focus();
                self.rename_focus = false;
            }
            if edit.lost_focus() {
                let escape = ui.input(|i| i.key_pressed(Key::Escape));
                let (id, name) = self.renaming.take().expect("renaming");
                let name = name.trim().to_owned();
                let unchanged = script.is_some_and(|s| s.name == name);
                if !escape && !name.is_empty() && !unchanged {
                    let mut next = catalog.clone();
                    if let Some(s) = next.scripts.iter_mut().find(|s| s.id == id) {
                        s.name = name;
                    }
                    out.push(PaneRequest::Change(next));
                }
            }
        } else if row.kind == ScriptKind::Separator {
            painter.hline(
                x..=rect.right() - 10.0,
                rect.center().y,
                Stroke::new(1.0, theme::DIVIDER),
            );
        } else {
            let full_access = script.is_some_and(Script::runs_with_full_access);
            let mut name = row.name.clone();
            if open.contains(&row.id) && script.is_some() {
                // Unsaved scripts carry an asterisk in the tab, not here.
            }
            if full_access {
                name.push_str(" (full access)");
            }
            painter.text(
                pos2(x, rect.center().y),
                Align2::LEFT_CENTER,
                &row.name,
                theme::small(),
                text_color,
            );
            let _ = name;
        }
        let label = match row.kind {
            ScriptKind::Separator => "Separator".to_owned(),
            _ => row.name.clone(),
        };
        response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, &label));
        if response.clicked() {
            let (shift, command) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
            self.select(rows, row.id, shift, command);
            if row.kind == ScriptKind::Script && !shift && !command {
                out.push(PaneRequest::Preview(row.id));
            }
        }
        if response.double_clicked() {
            match row.kind {
                ScriptKind::Script => out.push(PaneRequest::Open(row.id)),
                ScriptKind::Folder if !self.collapsed.remove(&row.id) => {
                    self.collapsed.insert(row.id);
                }
                _ => {}
            }
        }
        // Dragging: before a row, or into a folder over its middle.
        if response.drag_started() && !selected {
            self.select(rows, row.id, false, false);
        }
        if response.dragged() {
            self.drop = Some(Target::End);
        }
        if self.drop.is_some() {
            if let Some(pos) = ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p)) {
                let into = row.kind == ScriptKind::Folder
                    && (pos.y - rect.top()) > rect.height() * 0.3
                    && (pos.y - rect.top()) < rect.height() * 0.7;
                if into {
                    self.drop = Some(Target::Into(row.id));
                    painter.rect_stroke(
                        fill_rect,
                        4.0,
                        Stroke::new(2.0, theme::HIGHLIGHT),
                        egui::StrokeKind::Inside,
                    );
                } else {
                    let before = if pos.y > rect.center().y {
                        rows.iter()
                            .position(|r| r.id == row.id)
                            .and_then(|i| rows.get(i + 1))
                            .map_or(Target::End, |r| Target::Before(r.id))
                    } else {
                        Target::Before(row.id)
                    };
                    self.drop = Some(before);
                    let y = if pos.y > rect.center().y {
                        rect.bottom()
                    } else {
                        rect.top()
                    };
                    painter.hline(
                        rect.x_range().shrink(6.0),
                        y,
                        Stroke::new(2.0, theme::HIGHLIGHT),
                    );
                }
            }
        }
        if response.drag_stopped() {
            if let Some(target) = self.drop.take() {
                let mut next = catalog.clone();
                let ids: Vec<u64> = self.selected.iter().copied().collect();
                if move_members(&mut next, &ids, target) && next != *catalog {
                    out.push(PaneRequest::Change(next));
                }
            }
        }
        response.context_menu(|ui| {
            if !self.selected.contains(&row.id) {
                self.select(rows, row.id, false, false);
            }
            self.menu(ui, catalog, Some(row), out);
        });
    }

    /// The context menu of a member or of the pane.
    fn menu(
        &mut self,
        ui: &mut Ui,
        catalog: &ScriptCatalog,
        row: Option<&Row>,
        out: &mut Vec<PaneRequest>,
    ) {
        ui.set_min_width(200.0);
        let writable = catalog.writable;
        if ui
            .add_enabled(writable, egui::Button::new("New Script"))
            .clicked()
        {
            self.create(catalog, ScriptKind::Script, writable, out);
            ui.close();
        }
        if ui
            .add_enabled(writable, egui::Button::new("New Folder"))
            .clicked()
        {
            self.create(catalog, ScriptKind::Folder, writable, out);
            ui.close();
        }
        if ui
            .add_enabled(writable, egui::Button::new("New Separator"))
            .clicked()
        {
            self.create(catalog, ScriptKind::Separator, writable, out);
            ui.close();
        }
        let Some(row) = row else {
            return;
        };
        ui.separator();
        if row.kind == ScriptKind::Script && ui.button("Perform").clicked() {
            out.push(PaneRequest::Run(row.id));
            ui.close();
        }
        if row.kind == ScriptKind::Script && ui.button("Copy as Text").clicked() {
            out.push(PaneRequest::CopyText(
                self.selected.iter().copied().collect(),
            ));
            ui.close();
        }
        if row.kind != ScriptKind::Separator
            && ui
                .add_enabled(writable, egui::Button::new("Rename"))
                .clicked()
        {
            self.rename(catalog, row.id);
            ui.close();
        }
        if ui
            .add_enabled(writable, egui::Button::new("Duplicate"))
            .clicked()
        {
            let mut next = catalog.clone();
            let ids: Vec<u64> = self.selected.iter().copied().collect();
            let copies = duplicate(&mut next, &ids);
            self.selected = copies.into_iter().collect();
            out.push(PaneRequest::Change(next));
            ui.close();
        }
        if ui
            .add_enabled(writable, egui::Button::new("Delete"))
            .clicked()
        {
            out.push(PaneRequest::Delete(self.selected.iter().copied().collect()));
            ui.close();
        }
        if row.kind == ScriptKind::Script {
            ui.separator();
            let script = catalog.scripts.iter().find(|s| s.id == row.id);
            let mut full = script.is_some_and(Script::runs_with_full_access);
            if ui
                .add_enabled(
                    writable,
                    egui::Checkbox::new(&mut full, "Grant Full Access Privileges"),
                )
                .clicked()
            {
                let mut next = catalog.clone();
                for s in next
                    .scripts
                    .iter_mut()
                    .filter(|s| self.selected.contains(&s.id))
                {
                    s.set_runs_with_full_access(full);
                }
                out.push(PaneRequest::Change(next));
                ui.close();
            }
        }
        if row.kind != ScriptKind::Separator {
            let script = catalog.scripts.iter().find(|s| s.id == row.id);
            let mut in_menu = script.is_some_and(Script::in_scripts_menu);
            if ui
                .add_enabled(
                    writable,
                    egui::Checkbox::new(&mut in_menu, "Include in Scripts Menu"),
                )
                .clicked()
            {
                let mut next = catalog.clone();
                for s in next
                    .scripts
                    .iter_mut()
                    .filter(|s| self.selected.contains(&s.id))
                {
                    s.set_in_scripts_menu(in_menu);
                }
                out.push(PaneRequest::Change(next));
                ui.close();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> ScriptCatalog {
        // Contacts (folder 1: scripts 2 and 3, end 4), then script 5.
        ScriptCatalog {
            scripts: vec![
                member(1, "Contacts", ScriptKind::Folder),
                member(2, "Go to Entry Field", ScriptKind::Script),
                member(3, "Sort by Name", ScriptKind::Script),
                member(4, "--", ScriptKind::FolderEnd),
                member(5, "Other", ScriptKind::Script),
            ],
            writable: true,
            unavailable_reason: None,
        }
    }

    fn ids(c: &ScriptCatalog) -> Vec<u64> {
        c.scripts.iter().map(|s| s.id).collect()
    }

    #[test]
    fn folders_span_to_their_end_marker() {
        let c = catalog();
        assert_eq!(span(&c.scripts, 0), 0..4);
        assert_eq!(span(&c.scripts, 2), 2..3);
    }

    #[test]
    fn new_members_go_after_the_selection_or_into_an_open_folder() {
        let mut c = catalog();
        let id = new_script(&mut c, Some(2), false);
        assert_eq!(id, 6);
        assert_eq!(ids(&c), [1, 2, 6, 3, 4, 5]);
        assert_eq!(c.scripts[2].name, "New Script");
        let again = new_script(&mut c, Some(1), true);
        assert_eq!(ids(&c)[1], again);
        assert_eq!(c.scripts[1].name, "New Script 2");
        let folder = new_folder(&mut c, Some(1), false);
        assert_eq!(c.scripts[span(&c.scripts, 0).end].id, folder);
        assert_eq!(c.scripts.last().unwrap().kind, ScriptKind::Script);
        // A new folder encodes as one: flags 1 with an empty step list.
        let node = c.scripts.iter().find(|s| s.id == folder).unwrap().encode();
        assert_eq!(
            node.child(&[0xf0]).unwrap().value.as_deref(),
            Some(&[1][..])
        );
    }

    #[test]
    fn moves_carry_folder_contents_and_never_nest_into_themselves() {
        let mut c = catalog();
        assert!(move_members(&mut c, &[5], Target::Into(1)));
        assert_eq!(ids(&c), [1, 5, 2, 3, 4]);
        assert!(move_members(&mut c, &[1], Target::End));
        assert_eq!(ids(&c), [1, 5, 2, 3, 4]);
        assert!(!move_members(&mut c, &[1], Target::Into(1)));
        assert!(move_members(&mut c, &[3], Target::Before(1)));
        assert_eq!(ids(&c), [3, 1, 5, 2, 4]);
    }

    #[test]
    fn deleting_or_duplicating_a_folder_takes_its_contents() {
        let mut c = catalog();
        let copies = duplicate(&mut c, &[1]);
        assert_eq!(copies, [6, 7, 8]);
        assert_eq!(ids(&c), [1, 2, 3, 4, 6, 7, 8, 9, 5]);
        assert_eq!(c.scripts[4].name, "Contacts Copy");
        assert_eq!(c.scripts[5].name, "Go to Entry Field");
        delete(&mut c, &[1, 5]);
        assert_eq!(ids(&c), [6, 7, 8, 9]);
    }
}
