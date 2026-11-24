//! Native accessibility elements for the visible fields, labels and buttons of
//! the frame. Names, values and actions come from the desktop widget tree.
use egui::accesskit::{Action, NodeId, Rect, Role, TreeUpdate};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, uniffi::Enum)]
pub enum ElementKind {
    Button,
    Field,
    Label,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Element {
    pub id: u64,
    pub kind: ElementKind,
    pub label: String,
    pub value: String,
    pub enabled: bool,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

pub fn visible(update: &TreeUpdate, viewport: Rect) -> Vec<Element> {
    let Some(tree) = &update.tree else {
        return Vec::new();
    };
    let nodes: HashMap<_, _> = update.nodes.iter().map(|(id, n)| (*id, n)).collect();
    let mut seen = HashSet::new();
    let mut pending = vec![(tree.root, viewport)];
    let mut result = Vec::new();
    while let Some((NodeId(id), clip)) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(&NodeId(id)).copied() else {
            continue;
        };
        if node.is_hidden() {
            continue;
        }
        let bounds = node.bounds().map(|r| clip.intersect(r));
        let child_clip = if node.clips_children() {
            bounds.unwrap_or(clip)
        } else {
            clip
        };
        if !child_clip.is_empty() {
            pending.extend(node.children().iter().rev().map(|id| (*id, child_clip)));
        }
        let Some(rect) = bounds.filter(|r| !r.is_empty()) else {
            continue;
        };
        let (kind, action) = match node.role() {
            Role::Label => (ElementKind::Label, None),
            Role::Button
            | Role::DefaultButton
            | Role::CheckBox
            | Role::RadioButton
            | Role::MenuItem
            // A tab panel's tab: activating it shows its panel.
            | Role::Tab => (ElementKind::Button, Some(Action::Click)),
            Role::TextInput | Role::MultilineTextInput | Role::PasswordInput => {
                (ElementKind::Field, Some(Action::Focus))
            }
            _ => continue,
        };
        let mut label = node.label().unwrap_or_default().to_owned();
        if label.is_empty() {
            // An editor egui names by the label beside it (`labelled_by`,
            // as a dialog's input is).
            label = node
                .labelled_by()
                .iter()
                .filter_map(|by| nodes.get(by))
                .filter_map(|by| by.label().or(by.value()))
                .collect::<Vec<_>>()
                .join(" ");
        }
        let value = if node.role() == Role::PasswordInput {
            String::new()
        } else {
            node.value().unwrap_or_default().to_owned()
        };
        // A field with no name and nothing in it (an empty or password
        // input) can still be focused and typed into.
        if kind != ElementKind::Field && label.is_empty() && value.is_empty() {
            continue;
        }
        result.push(Element {
            id,
            kind,
            label,
            value,
            enabled: !node.is_disabled() && action.is_some_and(|a| node.supports_action(a)),
            left: rect.x0,
            top: rect.y0,
            width: rect.width(),
            height: rect.height(),
        });
    }
    result
}
