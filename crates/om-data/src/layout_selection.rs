//! Select All and marquee rules for the layout hierarchy.

use std::collections::{BTreeMap, BTreeSet};

use om_layout::ObjectKind;

use crate::{GeomRect, ObjectGeom};

pub const ID: &str = "select-all-layout-objects";
pub const LABEL: &str = "Select All";

/// The kind of selection requested by the active tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectAll {
    Layout,
    Kind(ObjectKind),
}

/// A frame's role in Select All. Providers that supply only geometry can
/// still participate in the all-object selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Target {
    #[default]
    Unclassified,
    Object(ObjectKind),
    Panel,
    Excluded,
}

impl SelectAll {
    /// Whether to include a present, finite frame. Selection is independent
    /// of whether edits are available for the object or its panel is front.
    pub fn includes(self, target: Target) -> bool {
        match (self, target) {
            (_, Target::Excluded) => false,
            (Self::Layout, _) => true,
            (Self::Kind(requested), Target::Object(kind)) => kind == requested,
            (Self::Kind(_), _) => false,
        }
    }
}

/// The object's selection unit when hit by a marquee.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MarqueeRole {
    #[default]
    Frame,
    GroupMember(u64),
    TabControl,
    OnTab(u64),
}

#[derive(Clone, Copy, Debug, Default)]
enum Branch {
    #[default]
    Layout,
    TabControl,
    SlideControl,
    Excluded,
}

impl Branch {
    fn visit(self, kind: ObjectKind) -> (Target, Self) {
        use ObjectKind::*;
        match (self, kind) {
            (Self::Layout, TabControl) => (Target::Excluded, Self::TabControl),
            (Self::Layout, SlideControl) => (Target::Excluded, Self::SlideControl),
            (Self::TabControl, TabPanel) | (Self::SlideControl, SlidePanel) => {
                (Target::Panel, Self::Layout)
            }
            (Self::Layout, Popover | TabPanel | SlidePanel) => (Target::Excluded, Self::Excluded),
            // A group or other ordinary container is one selection unit.
            (Self::Layout, kind) => (Target::Object(kind), Self::Excluded),
            _ => (Target::Excluded, Self::Excluded),
        }
    }
}

/// Selection scope carried along the provider's existing geometry walk.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scope {
    branch: Branch,
    enclosing: MarqueeRole,
}

impl Scope {
    /// The object's roles and the scope of its children.
    pub fn visit(self, id: u64, kind: ObjectKind) -> (Target, MarqueeRole, Self) {
        let (target, branch) = self.branch.visit(kind);
        let (role, enclosing) = match (self.branch, kind) {
            (Branch::Layout, ObjectKind::Group) => (self.enclosing, MarqueeRole::GroupMember(id)),
            (Branch::Layout, ObjectKind::TabControl) => {
                (MarqueeRole::TabControl, MarqueeRole::OnTab(id))
            }
            _ => (self.enclosing, self.enclosing),
        };
        (target, role, Self { branch, enclosing })
    }
}

/// Whether to include partial intersections or only fully enclosed frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    Intersects,
    Contains,
}

/// An object or field draft's current frame in layout points.
pub struct MarqueeFrame<'a> {
    pub index: usize,
    pub object: Option<&'a ObjectGeom>,
    pub rect: GeomRect,
    pub visible: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MarqueeSelection {
    pub indices: BTreeSet<usize>,
    /// Draw one set of handles and include the selected inactive panel outlines.
    pub aggregate: bool,
}

/// Select from the finite frames provided by the window. A group is one
/// selection unit even when only a member is crossed. A fully enclosed tab
/// control selects its panels and their content without switching panels.
pub fn marquee(
    bounds: GeomRect,
    coverage: Coverage,
    frames: &[MarqueeFrame<'_>],
) -> MarqueeSelection {
    let by_key: BTreeMap<_, _> = frames
        .iter()
        .rev()
        .filter_map(|frame| {
            let key = frame.object?.key;
            (key != 0).then_some((key, frame))
        })
        .collect();
    let enclosed_tabs: BTreeSet<_> = frames
        .iter()
        .filter_map(|frame| {
            let object = frame.object?;
            (frame.visible
                && object.marquee == MarqueeRole::TabControl
                && contains(bounds, frame.rect))
            .then_some(object.key)
        })
        .collect();
    let mut result = MarqueeSelection::default();
    for frame in frames {
        let role = frame
            .object
            .map_or(MarqueeRole::Frame, |object| object.marquee);
        match role {
            MarqueeRole::OnTab(key) if enclosed_tabs.contains(&key) => {
                if frame
                    .object
                    .is_some_and(|object| SelectAll::Layout.includes(object.selection))
                {
                    result.indices.insert(frame.index);
                    result.aggregate = true;
                }
            }
            MarqueeRole::TabControl
                if frame
                    .object
                    .is_some_and(|object| enclosed_tabs.contains(&object.key)) => {}
            _ if !frame.visible => {}
            MarqueeRole::GroupMember(key) => {
                if let Some(group) = by_key.get(&key).filter(|group| group.visible) {
                    let hit = match coverage {
                        Coverage::Contains => contains(bounds, group.rect),
                        Coverage::Intersects => intersects(bounds, frame.rect),
                    };
                    if hit {
                        result.indices.insert(group.index);
                        result.aggregate = true;
                    }
                }
            }
            _ => {
                let hit = match coverage {
                    Coverage::Contains => contains(bounds, frame.rect),
                    Coverage::Intersects => intersects(bounds, frame.rect),
                };
                if hit {
                    result.indices.insert(frame.index);
                    result.aggregate |= frame.object.is_some_and(|object| {
                        object.selection == Target::Object(ObjectKind::Group)
                    });
                }
            }
        }
    }
    result
}

fn contains(bounds: GeomRect, frame: GeomRect) -> bool {
    bounds.left <= frame.left
        && bounds.right >= frame.right
        && bounds.top <= frame.top
        && bounds.bottom >= frame.bottom
}

fn intersects(bounds: GeomRect, frame: GeomRect) -> bool {
    bounds.left <= frame.right
        && bounds.right >= frame.left
        && bounds.top <= frame.bottom
        && bounds.bottom >= frame.top
}
