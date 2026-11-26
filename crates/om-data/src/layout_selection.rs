//! The layout hierarchy that Edit > Select All operates on.

use om_layout::ObjectKind;

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

/// Selection scope carried along the provider's existing geometry walk.
#[derive(Clone, Copy, Debug, Default)]
pub enum Scope {
    #[default]
    Layout,
    TabControl,
    SlideControl,
    Excluded,
}

impl Scope {
    /// The current object's selection role and the scope of its children.
    pub fn visit(self, kind: ObjectKind) -> (Target, Scope) {
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
