//! Container ancestry for showing a decoded layout without changing its objects.

use std::collections::{BTreeMap, BTreeSet};

use crate::{ObjectKind, ObjectStore};

/// A read-only index of panels, popovers, and the objects they contain.
#[derive(Debug, Default)]
pub struct PanelContents {
    parents: BTreeMap<u64, u64>,
    controls: BTreeMap<u64, u64>,
    initial: BTreeMap<u64, u64>,
    popovers: BTreeSet<u64>,
}

impl PanelContents {
    pub fn new(store: &ObjectStore) -> Self {
        use ObjectKind::{SlideControl, SlidePanel, TabControl, TabPanel};

        let index: BTreeMap<_, _> = store.objects.iter().rev().map(|o| (o.id, o)).collect();
        let mut contents = Self::default();
        for control in index.values() {
            let kind = match control.kind() {
                ObjectKind::Popover => {
                    contents.popovers.insert(control.id);
                    continue;
                }
                TabControl => TabPanel,
                SlideControl => SlidePanel,
                _ => continue,
            };
            let mut first = None;
            for &key in &control.children {
                if index
                    .get(&key)
                    .is_some_and(|panel| panel.kind() == kind && panel.parent == Some(control.id))
                {
                    first.get_or_insert(key);
                    contents.controls.insert(key, control.id);
                }
            }
            if let Some(first) = first {
                let initial = control
                    .tab
                    .as_ref()
                    .into_iter()
                    .flat_map(|tab| [tab.current_panel_key, tab.default_panel_key])
                    .find(|key| *key != 0 && contents.controls.get(key) == Some(&control.id))
                    .unwrap_or(first);
                contents.initial.insert(control.id, initial);
            }
        }
        if !contents.controls.is_empty() || !contents.popovers.is_empty() {
            contents.parents = index
                .values()
                .filter_map(|o| Some((o.id, o.parent?)))
                .collect();
        }
        contents
    }

    /// Confirm a window's remembered panel still belongs to the control.
    fn contains(&self, control: u64, panel: u64) -> bool {
        self.controls.get(&panel) == Some(&control)
    }

    /// Whether every enclosing panel is frontmost and every enclosing
    /// popover is open. The popover itself is subject to the same rule.
    pub fn visible(
        &self,
        object: u64,
        front: &BTreeMap<u64, u64>,
        open_popovers: &BTreeSet<u64>,
    ) -> bool {
        self.walk(object, |key| {
            (!self.popovers.contains(&key) || open_popovers.contains(&key))
                && self.controls.get(&key).is_none_or(|&control| {
                    front
                        .get(&control)
                        .filter(|&&panel| self.contains(control, panel))
                        .or_else(|| self.initial.get(&control))
                        == Some(&key)
                })
        })
    }

    /// Popovers containing any of these objects, including a popover when
    /// the object is its surface. Unfinished chains reveal nothing.
    pub fn containing_popovers(&self, objects: impl IntoIterator<Item = u64>) -> BTreeSet<u64> {
        let mut popovers = BTreeSet::new();
        if self.popovers.is_empty() {
            return popovers;
        }
        for object in objects {
            let mut enclosing = Vec::new();
            if self.walk(object, |key| {
                if self.popovers.contains(&key) {
                    enclosing.push(key);
                }
                true
            }) {
                popovers.extend(enclosing);
            }
        }
        popovers
    }

    /// Whether this surface or object belongs to the raised popover layer.
    pub fn in_previewed_popover(&self, object: u64, previewed: &BTreeSet<u64>) -> bool {
        let mut raised = false;
        self.walk(object, |key| {
            raised |= previewed.contains(&key);
            true
        }) && raised
    }

    /// Bring the object's enclosing panels to the front in the window's
    /// view. The store stays untouched.
    pub fn reveal(&self, object: u64, front: &mut BTreeMap<u64, u64>) {
        let mut enclosing = Vec::new();
        if self.walk(object, |key| {
            if let Some(&control) = self.controls.get(&key) {
                enclosing.push((control, key));
            }
            true
        }) {
            front.extend(enclosing);
        }
    }

    fn walk(&self, mut object: u64, mut visit: impl FnMut(u64) -> bool) -> bool {
        // Match the bound on layout container walks. An unfinished chain can
        // still contain a hidden panel, and may come from a malformed cycle.
        for _ in 0..64 {
            if !visit(object) {
                return false;
            }
            let Some(&parent) = self.parents.get(&object) else {
                return true;
            };
            object = parent;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unfinished_panel_chain_stays_hidden_and_cannot_change_the_front() {
        let contents = PanelContents {
            parents: (1..68).map(|key| (key, key + 1)).collect(),
            controls: [(2, 3), (67, 68), (69, 68)].into(),
            initial: [(3, 2), (68, 69)].into(),
            popovers: [5].into(),
        };
        let mut front = BTreeMap::new();
        assert!(!contents.visible(1, &front, &BTreeSet::new()));
        contents.reveal(1, &mut front);
        assert!(front.is_empty());
        assert!(contents.containing_popovers([1]).is_empty());
    }

    #[test]
    fn a_popover_is_visible_only_when_previewed_and_its_panel_is_front() {
        let store = ObjectStore {
            objects: [
                (1, 11, None, vec![2, 3]),
                (2, 12, Some(1), vec![]),
                (3, 12, Some(1), vec![4]),
                (4, 15, Some(3), vec![5]),
                (5, 8, Some(4), vec![6]),
                (6, 2, Some(5), vec![]),
                (7, 5, None, vec![]),
            ]
            .into_iter()
            .map(|(id, object_type, parent, children)| crate::LayoutObject {
                id,
                object_type,
                parent,
                children,
                ..Default::default()
            })
            .collect(),
            ..Default::default()
        };
        let contents = PanelContents::new(&store);
        let mut front = BTreeMap::new();
        let closed = BTreeSet::new();
        let previewed = contents.containing_popovers([6]);
        assert_eq!(previewed, [4].into());
        assert!(contents.in_previewed_popover(4, &previewed));
        assert!(contents.in_previewed_popover(6, &previewed));
        assert!(!contents.in_previewed_popover(7, &previewed));
        assert!(!contents.visible(6, &front, &previewed));
        contents.reveal(6, &mut front);
        assert!(!contents.visible(4, &front, &closed));
        assert!(!contents.visible(6, &front, &closed));
        assert!(contents.visible(4, &front, &previewed));
        assert!(contents.visible(6, &front, &previewed));
        assert!(contents.visible(7, &front, &closed));
    }
}
