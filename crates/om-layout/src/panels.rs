//! Panel ancestry for showing a decoded layout without changing its objects.

use std::collections::BTreeMap;

use crate::{ObjectKind, ObjectStore};

/// A read-only index of tab and slide controls and the objects they contain.
#[derive(Debug, Default)]
pub struct PanelContents {
    parents: BTreeMap<u64, u64>,
    controls: BTreeMap<u64, u64>,
    initial: BTreeMap<u64, u64>,
}

impl PanelContents {
    pub fn new(store: &ObjectStore) -> Self {
        use ObjectKind::{SlideControl, SlidePanel, TabControl, TabPanel};

        let index: BTreeMap<_, _> = store.objects.iter().rev().map(|o| (o.id, o)).collect();
        let mut contents = Self::default();
        for control in index.values() {
            let kind = match control.kind() {
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
        if !contents.controls.is_empty() {
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

    /// Whether an object is visible through every enclosing panel. Unknown
    /// objects have no panel restriction.
    pub fn visible(&self, object: u64, front: &BTreeMap<u64, u64>) -> bool {
        self.walk_panels(object, |control, panel| {
            front
                .get(&control)
                .filter(|&&key| self.contains(control, key))
                .or_else(|| self.initial.get(&control))
                == Some(&panel)
        })
    }

    /// Bring the object's enclosing panels to the front in the window's
    /// view. The store stays untouched.
    pub fn reveal(&self, object: u64, front: &mut BTreeMap<u64, u64>) {
        let mut enclosing = Vec::new();
        if self.walk_panels(object, |control, panel| {
            enclosing.push((control, panel));
            true
        }) {
            front.extend(enclosing);
        }
    }

    fn walk_panels(&self, mut object: u64, mut visit: impl FnMut(u64, u64) -> bool) -> bool {
        // Match the bound on layout container walks. An unfinished chain can
        // still contain a hidden panel, and may come from a malformed cycle.
        for _ in 0..64 {
            if let Some(&control) = self.controls.get(&object) {
                if !visit(control, object) {
                    return false;
                }
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
        };
        let mut front = BTreeMap::new();
        assert!(!contents.visible(1, &front));
        contents.reveal(1, &mut front);
        assert!(front.is_empty());
    }
}
