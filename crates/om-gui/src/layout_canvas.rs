//! Layout mode's scrollable design surface. Geometry is read from the data
//! provider; selecting an object never changes the document. The Field tool
//! draws a new field's frame ([`NewField`]) and the Button tool a new
//! button's ([`NewButton`]), the Portal tool a new portal's ([`NewPortal`]),
//! the Selection tool moves a movable object by
//! dragging it and resizes it by its handles ([`MovedObjects`]), and Delete
//! or Edit > Clear deletes it ([`DeletedObjects`]); only Save Layout stores
//! them.

use std::collections::{BTreeMap, BTreeSet};
use std::f32::consts::FRAC_PI_2;

use egui::emath::{GuiRounding as _, TSTransform};
use egui::epaint::TextShape;
use egui::scroll_area::{DragScroll, ScrollSource};
use egui::{
    pos2, vec2, Align2, CornerRadius, Pos2, Rect, Role, Sense, Stroke, StrokeKind, Ui, WidgetInfo,
};
use om_data::{layout_move, GeomRect, LayoutGeometry, ObjectGeom, PartGeom};

use crate::app::LayoutTool;
use crate::document::{zoom, DocumentState};
use crate::session::{Geometry, Session};
use crate::theme;

use om_data::layout_selection::{
    self, Coverage, MarqueeFrame, SelectAll, Target as SelectionTarget,
};

mod history;
pub(crate) use history::DraftEdit;

const PART_STRIP_WIDTH: f32 = 22.0;
const PASTEBOARD_MARGIN: f32 = 48.0;
// Approximate the contrast of Run C's blue rectangle on the right pasteboard.
const PASTEBOARD_ARTWORK_OPACITY: f32 = 0.55;

const CANNOT_MOVE_SELECTION: &str = "This selection includes an object OpenMaker cannot move. \
    No objects were moved.";

/// A field the Field tool can place: the table it was chosen from (the
/// table the window's schema showed for the layout's occurrence), its id in
/// that table and its name. Save Layout sends the table with it, so an
/// occurrence that shows another table by then is refused, not given that
/// table's field of the same id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FieldChoice {
    pub table: u64,
    pub field: u64,
    pub name: String,
    /// A date field, which a drop-down calendar can show.
    pub date: bool,
}

/// A field being placed with the Field tool: its frame, dragged on the
/// canvas and set in the Inspector, the field chosen for it there, and the
/// revision of the layout it was drawn on. Nothing is stored until Save
/// Layout places it with the shared operation
/// (`DataProvider::place_layout_field`, `om_data::layout_placement`), which
/// the host refuses if the layout changed since that revision; Revert
/// Layout drops it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NewField {
    /// The frame in layout coordinates, whole points.
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    /// The chosen field.
    pub field: Option<FieldChoice>,
    /// Place a label with it (the Inspector's Create label, on at first):
    /// the field's name, left of the field ([`Self::label_frame`]).
    pub label: bool,
    /// Its control (a display type of
    /// `om_data::layout_objects::FIELD_CONTROLS`, an edit box at first) and
    /// value list key: Save Layout places the field, then sets them as for a
    /// stored field ([`ControlChanges`]).
    pub control: u32,
    pub value_list: Option<u64>,
    /// The revision of the layout the frame was drawn on; `None` when the
    /// provider gave none, and then the field cannot be saved.
    pub revision: Option<om_data::layout_placement::Revision>,
}

/// A new field's label: [`LABEL_WIDTH`] wide, [`LABEL_GAP`] left of the
/// field, as high as the field, narrower where the layout's left edge
/// comes first, and none under [`MIN_LABEL_WIDTH`] (OpenMaker's choice;
/// where FileMaker Pro puts a new field's label is not measured).
const LABEL_WIDTH: f64 = 90.0;
const LABEL_GAP: f64 = 4.0;
const MIN_LABEL_WIDTH: f64 = 20.0;

impl NewField {
    pub(crate) fn frame(&self) -> om_layout::Rect {
        frame(self.left, self.top, self.width, self.height)
    }

    /// Where its label goes, if there is room left of it.
    pub(crate) fn label_frame(&self) -> Option<om_layout::Rect> {
        let width = LABEL_WIDTH.min(self.left - LABEL_GAP);
        (width >= MIN_LABEL_WIDTH)
            .then(|| frame(self.left - LABEL_GAP - width, self.top, width, self.height))
    }
}

/// A text being placed with the Text tool: its frame, dragged on the
/// canvas (or a click's default size), its one line of text, typed on the
/// canvas, and the revision of the layout it was drawn on. As with
/// [`NewField`], nothing is stored until Save Layout places it
/// (`DataProvider::place_layout_text`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NewText {
    /// The frame in layout coordinates, whole points.
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub text: String,
    pub revision: Option<om_data::layout_placement::Revision>,
    /// Just drawn: the text takes the keyboard.
    focus: bool,
}

/// A text placed with a click rather than a drag: this wide and high
/// (OpenMaker's choice; FileMaker Pro's Text tool grows the text as it is
/// typed).
const CLICKED_TEXT: (f64, f64) = (120.0, 20.0);

impl NewText {
    pub(crate) fn frame(&self) -> om_layout::Rect {
        frame(self.left, self.top, self.width, self.height)
    }
}

/// A button being placed with the Button tool: its frame, dragged on the
/// canvas, the label and action Button Setup gave it, and the revision of
/// the layout it was drawn on. As with [`NewField`], nothing is stored
/// until Save Layout places it (`DataProvider::place_layout_button`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NewButton {
    /// The frame in layout coordinates, whole points.
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub label: String,
    /// Its action, a Perform Script step; `None` until Button Setup gave
    /// it one.
    pub step: Option<om_data::ScriptStep>,
    /// The action as the Script Workspace shows the step.
    pub action: String,
    pub revision: Option<om_data::layout_placement::Revision>,
}

impl NewButton {
    pub(crate) fn frame(&self) -> om_layout::Rect {
        frame(self.left, self.top, self.width, self.height)
    }
}

/// A portal being placed with the Portal tool: its frame, dragged on the
/// canvas, what Portal Setup and Add Fields to Portal set for it (`None`
/// until they have), and the revision of the layout it was drawn on. As
/// with [`NewField`], nothing is stored until Save Layout places it
/// (`DataProvider::place_layout_portal`, authoring operation 21).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NewPortal {
    /// The frame in layout coordinates, whole points.
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub setup: Option<crate::portal_setup::PortalChoice>,
    pub revision: Option<om_data::layout_placement::Revision>,
}

/// A web viewer being placed with the Web Viewer tool: its frame, dragged
/// on the canvas, what Web Viewer Setup set for it (`None` until it has),
/// and the revision of the layout it was drawn on. Nothing is stored until
/// Save Layout places it (`DataProvider::place_layout_web_viewer`,
/// authoring operation 28).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NewWebViewer {
    /// The frame in layout coordinates, whole points.
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub setup: Option<crate::web_viewer_setup::WebViewerChoice>,
    pub revision: Option<om_data::layout_placement::Revision>,
}

impl NewWebViewer {
    pub(crate) fn frame(&self) -> om_layout::Rect {
        frame(self.left, self.top, self.width, self.height)
    }
}

impl NewPortal {
    pub(crate) fn frame(&self) -> om_layout::Rect {
        frame(self.left, self.top, self.width, self.height)
    }

    /// Its fields' frames relative to it, as operation 21 places them.
    fn field_frames(&self) -> Vec<(String, om_layout::Rect)> {
        let Some(setup) = &self.setup else {
            return Vec::new();
        };
        let placement = om_data::layout_placement::PortalPlacement {
            occurrence: setup.occurrence,
            frame: self.frame(),
            rows: setup.rows,
            fields: setup.fields.iter().map(|(id, _)| *id).collect(),
        };
        om_data::layout_placement::portal_field_frames(&placement)
            .map(|frames| {
                setup
                    .fields
                    .iter()
                    .map(|(_, name)| name.clone())
                    .zip(frames)
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn frame(left: f64, top: f64, width: f64, height: f64) -> om_layout::Rect {
    om_layout::Rect {
        top,
        left,
        bottom: top + height,
        right: left + width,
        ..om_layout::Rect::default()
    }
}

/// Objects moved or resized with the Selection tool or the Inspector and
/// not saved yet: each object's new frame by its key, in whole points, and
/// the revision of the layout they were changed on. Save Layout sends them
/// with the shared operation (`DataProvider::set_layout_object_frames`,
/// `om_data::layout_objects`), which the host refuses if the layout changed
/// since; Revert Layout drops them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MovedObjects {
    pub frames: std::collections::BTreeMap<u64, GeomRect>,
    pub revision: Option<om_data::layout_placement::Revision>,
}

/// Objects deleted with Delete or Edit > Clear and not saved yet: their
/// keys, and the revision of the layout they were deleted on. The canvas
/// and the Objects pane no longer show them. Save Layout sends them with the
/// shared operation (`DataProvider::delete_layout_objects`,
/// `om_data::layout_objects`) after any moves; Revert Layout drops them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DeletedObjects {
    pub keys: std::collections::BTreeSet<u64>,
    pub revision: Option<om_data::layout_placement::Revision>,
}

/// Field objects given another control style or value list in the
/// Inspector and not saved yet: by object key, the control (a display type
/// of `om_data::layout_objects::FIELD_CONTROLS`) and the value list key
/// (`None`: as stored), and the revision of the layout the first was made
/// on. Save Layout sends them one object at a time with the shared
/// operation (`DataProvider::set_field_control`) after any deletions;
/// Revert Layout drops them.
/// Text objects retyped on the canvas and not saved yet: by object key,
/// the new words, and the revision of the layout the first was made on.
/// Save Layout sends them one object at a time with the shared operation
/// (`DataProvider::set_layout_text`) after any control changes; Revert
/// Layout drops them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WordChanges {
    pub objects: std::collections::BTreeMap<u64, String>,
    pub revision: Option<om_data::layout_placement::Revision>,
}

/// A stored text being retyped on the canvas: its key, the words being
/// typed and the words it stores.
#[derive(Clone, Debug, PartialEq)]
struct Retyping {
    key: u64,
    text: String,
    stored: String,
    revision: Option<om_data::layout_placement::Revision>,
    focus: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ControlChanges {
    pub objects: std::collections::BTreeMap<u64, (u32, Option<u64>)>,
    pub revision: Option<om_data::layout_placement::Revision>,
}

/// What a Selection tool drag does: move the object, or resize it by the
/// handle on these edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Edges {
    left: bool,
    top: bool,
    right: bool,
    bottom: bool,
}

/// A selection handle drag in progress, in layout points.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drag {
    key: u64,
    edges: Edges,
    from: Pos2,
    start: GeomRect,
}

#[derive(Default)]
struct Selection(BTreeSet<usize>, Option<&'static str>, SelectionOverlay);

#[derive(Default)]
struct SelectionOverlay {
    aggregate: bool,
    tab_controls: BTreeMap<u64, usize>,
}

impl Selection {
    fn single(&self) -> Option<usize> {
        if self.0.len() == 1 {
            self.0.first().copied()
        } else {
            None
        }
    }

    fn set(&mut self, index: Option<usize>) {
        self.replace(index);
    }

    fn replace(&mut self, indices: impl IntoIterator<Item = usize>) {
        self.1 = None;
        self.2 = SelectionOverlay::default();
        self.0.clear();
        self.0.extend(indices);
    }

    fn replace_with_overlay(
        &mut self,
        indices: impl IntoIterator<Item = usize>,
        overlay: SelectionOverlay,
    ) {
        self.replace(indices);
        self.2 = overlay;
    }

    fn start_drag(&mut self, index: usize, shift: bool) {
        if !self.0.contains(&index) {
            if shift {
                self.0.insert(index);
                self.1 = None;
                self.2 = SelectionOverlay::default();
            } else {
                self.set(Some(index));
            }
        }
    }

    fn click(&mut self, index: Option<usize>, shift: bool) {
        self.1 = None;
        self.2 = SelectionOverlay::default();
        if let Some(index) = index.filter(|_| shift) {
            if !self.0.remove(&index) {
                self.0.insert(index);
            }
        } else {
            self.set(index);
        }
    }
}

/// Every member starts from the same pointer position, so moving a mixed
/// stored/draft selection preserves its spacing even at the layout's edges.
struct SelectionDrag {
    from: Pos2,
    frames: Vec<(usize, GeomRect)>,
}

/// Selection belongs to one exact layout revision. Keeping a copy of the
/// geometry prevents a refreshed or replaced layout from reusing a stale
/// selected index.
/// The layout's width as its right edge was dragged, not saved yet, and the
/// revision of the layout it was changed on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NewWidth {
    pub width: f64,
    pub revision: Option<om_data::layout_placement::Revision>,
}

#[derive(Default)]
pub(crate) struct CanvasState {
    identity: Option<(String, String, Option<u32>)>,
    geometry: Option<LayoutGeometry>,
    history: history::History,
    front_panels: BTreeMap<u64, u64>,
    panel_generation: u64,
    new_text_editor: Option<egui::Id>,
    selected: Selection,
    scroll_to_selected: bool,
    shown_zoom: zoom::Zoom,
    new_fields: Vec<NewField>,
    field_drag: Option<usize>,
    field_finished: bool,
    /// The Button tool's new button and the Text tool's new text; one new
    /// object (field, button or text) at a time.
    new_button: Option<NewButton>,
    new_text: Option<NewText>,
    /// The Portal tool's new portal.
    new_portal: Option<NewPortal>,
    /// The Web Viewer tool's new web viewer.
    new_web_viewer: Option<NewWebViewer>,
    /// Where a Field or Button tool drag began, in layout coordinates.
    drag_from: Option<Pos2>,
    /// The Button tool finished drawing, or the new button was double
    /// clicked: Button Setup opens.
    button_setup: bool,
    /// The Portal tool finished drawing, or the new portal was double
    /// clicked: Portal Setup opens.
    portal_setup: bool,
    /// The Web Viewer tool finished drawing, or the new viewer was double
    /// clicked: Web Viewer Setup opens.
    web_viewer_setup: bool,
    moved: MovedObjects,
    deleted: DeletedObjects,
    controls: ControlChanges,
    words: WordChanges,
    retyping: Option<Retyping>,
    drag: Option<Drag>,
    selection_drag: Option<SelectionDrag>,
    /// Where the Selection tool's box began, in layout coordinates.
    box_from: Option<Pos2>,
    width: Option<NewWidth>,
    /// A part's lower boundary being dragged with the Selection tool, and a
    /// height dragged to, for the window to send.
    part_drag: Option<PartDrag>,
    part_resize: Option<PartResize>,
    /// The Part tool was used on the layout, at this layout point's y (in
    /// points): Part Definition opens.
    part_tool: Option<f64>,
    /// A layout read again no longer has objects that unsaved control or
    /// text changes were made to; the changes are kept.
    orphaned_drafts: bool,
    /// The objects of the layout as last read (kept while it is read
    /// again): an object is deleted when a read lacks one of them, not when
    /// a read is older than an object just placed.
    known_keys: BTreeSet<u64>,
}

/// The stored part whose boundary began the drag, and the current height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PartDrag {
    part: om_data::PartIdentity,
    height: f64,
}

/// A part's new height, dragged with the Selection tool: the part's key
/// and type and the layout's revision it was dragged on. The window sends
/// it at once, as Part Setup's changes go (authoring operation 22).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PartResize {
    pub part: u64,
    pub part_type: i32,
    pub size: f64,
    pub revision: Option<om_data::layout_placement::Revision>,
}

/// Whether part boundaries may be dragged, and the active and finished
/// drag; and whether the Part tool was used.
struct PartEdges<'a> {
    enabled: bool,
    drag: &'a mut Option<PartDrag>,
    resize: &'a mut Option<PartResize>,
    tool: &'a mut Option<f64>,
}

/// The Part tool's button dragged from the status toolbar: dropped on the
/// layout, it adds a part as a click with the tool does.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PartToolDrag;

impl CanvasState {
    /// A part's height dragged to, since the last call.
    pub(crate) fn take_part_resize(&mut self) -> Option<PartResize> {
        self.part_resize.take()
    }

    /// Whether a read found unsaved changes to objects the layout no longer
    /// has since the last call (another client deleted the objects).
    pub(crate) fn take_orphaned_drafts(&mut self) -> bool {
        std::mem::take(&mut self.orphaned_drafts)
    }

    /// Where the Part tool was used on the layout since the last call (a
    /// click with it, or its button dropped there): the layout point's y,
    /// in points.
    pub(crate) fn take_part_tool(&mut self) -> Option<f64> {
        self.part_tool.take()
    }

    /// Whether the layout holds changes not saved yet: a part boundary is
    /// dragged only without them, since a part that moves moves objects
    /// whose new frames they would hold.
    fn unsaved(&self) -> bool {
        !self.moved.frames.is_empty()
            || !self.deleted.keys.is_empty()
            || !self.controls.objects.is_empty()
            || !self.words.objects.is_empty()
            || self.retyping.is_some()
            || self.width.is_some()
            || !self.new_fields.is_empty()
            || self.new_button.is_some()
            || self.new_text.is_some()
            || self.new_portal.is_some()
            || self.new_web_viewer.is_some()
    }

    /// The layout's new width, not saved yet.
    pub(crate) fn new_width(&self) -> Option<NewWidth> {
        self.width
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_width(&mut self) {
        self.width = None;
    }

    pub(crate) fn new_field(&self) -> Option<&NewField> {
        self.new_fields.first()
    }

    pub(crate) fn new_field_mut(&mut self) -> Option<&mut NewField> {
        let index = self
            .selected
            .single()?
            .checked_sub(self.geometry.as_ref()?.objects.len())?;
        self.new_fields.get_mut(index)
    }

    pub(crate) fn stage_field(&mut self, session: &Session, field: NewField) {
        self.synchronize(session);
        let index = self.new_fields.len();
        self.new_fields.push(field);
        self.selected
            .set(self.geometry.as_ref().map(|g| g.objects.len() + index));
    }

    pub(crate) fn fields(&self) -> &[NewField] {
        &self.new_fields
    }

    pub(crate) fn take_field_finished(&mut self) -> bool {
        std::mem::take(&mut self.field_finished)
    }

    /// Only an acknowledged placement consumes a draft. A failed Save keeps it.
    pub(crate) fn field_placed(&mut self) {
        self.selection_drag = None;
        self.field_drag = None;
        self.drag_from = None;
        if !self.new_fields.is_empty() {
            self.new_fields.remove(0);
        }
        self.selected.set(None);
    }

    pub(crate) fn new_button(&self) -> Option<&NewButton> {
        self.new_button.as_ref()
    }

    pub(crate) fn new_button_mut(&mut self) -> Option<&mut NewButton> {
        self.new_button.as_mut()
    }

    pub(crate) fn new_text(&self) -> Option<&NewText> {
        self.new_text.as_ref()
    }

    pub(crate) fn new_portal(&self) -> Option<&NewPortal> {
        self.new_portal.as_ref()
    }

    pub(crate) fn new_portal_mut(&mut self) -> Option<&mut NewPortal> {
        self.new_portal.as_mut()
    }

    pub(crate) fn new_web_viewer(&self) -> Option<&NewWebViewer> {
        self.new_web_viewer.as_ref()
    }

    pub(crate) fn new_web_viewer_mut(&mut self) -> Option<&mut NewWebViewer> {
        self.new_web_viewer.as_mut()
    }

    /// Whether Web Viewer Setup was asked for since the last call.
    pub(crate) fn take_web_viewer_setup(&mut self) -> bool {
        std::mem::take(&mut self.web_viewer_setup) && self.new_web_viewer.is_some()
    }

    /// Whether Portal Setup was asked for since the last call.
    pub(crate) fn take_portal_setup(&mut self) -> bool {
        std::mem::take(&mut self.portal_setup) && self.new_portal.is_some()
    }

    pub(crate) fn new_text_mut(&mut self) -> Option<&mut NewText> {
        self.new_text.as_mut()
    }

    /// Drop the new field, button, text or portal.
    pub(crate) fn drop_new_object(&mut self) {
        self.new_fields.clear();
        self.selected.set(None);
        self.selection_drag = None;
        self.field_drag = None;
        self.field_finished = false;
        self.new_button = None;
        self.new_text = None;
        self.new_portal = None;
        self.new_web_viewer = None;
        self.drag_from = None;
        self.button_setup = false;
        self.portal_setup = false;
        self.web_viewer_setup = false;
    }

    /// Open Button Setup for the new button.
    pub(crate) fn request_button_setup(&mut self) {
        self.button_setup = self.new_button.is_some();
    }

    /// Whether Button Setup was asked for since the last call.
    pub(crate) fn take_button_setup(&mut self) -> bool {
        std::mem::take(&mut self.button_setup) && self.new_button.is_some()
    }

    /// The objects moved or resized and not saved yet.
    pub(crate) fn moved(&self) -> &MovedObjects {
        &self.moved
    }

    /// Set object `key`'s frame, changed on the layout at `revision` (the
    /// first change's revision is kept: the host compares it at Save
    /// Layout). A frame back where the object is stored is no change.
    pub(crate) fn set_frame(
        &mut self,
        key: u64,
        frame: GeomRect,
        stored: GeomRect,
        revision: Option<om_data::layout_placement::Revision>,
    ) {
        if self.moved.frames.is_empty() {
            self.moved.revision = revision;
        }
        if frame == stored {
            self.moved.frames.remove(&key);
        } else {
            self.moved.frames.insert(key, frame);
        }
        if self.moved.frames.is_empty() {
            self.moved.revision = None;
        }
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_moves(&mut self) {
        self.moved = MovedObjects::default();
        self.selected.set(None);
        self.drag = None;
        self.selection_drag = None;
    }

    /// The objects deleted and not saved yet.
    pub(crate) fn deleted(&self) -> &DeletedObjects {
        &self.deleted
    }

    /// Delete object `key`, shown on the layout at `revision` (the first
    /// deletion's revision is kept: the host compares it at Save Layout). A
    /// move of it is dropped, and the selection with it.
    pub(crate) fn delete_object(
        &mut self,
        key: u64,
        revision: Option<om_data::layout_placement::Revision>,
    ) {
        if self.deleted.keys.is_empty() {
            self.deleted.revision = revision;
        }
        self.deleted.keys.insert(key);
        if self.moved.frames.remove(&key).is_some() && self.moved.frames.is_empty() {
            self.moved.revision = None;
        }
        self.control_sent(key);
        self.words_sent(key);
        if self.drag.is_some_and(|drag| drag.key == key) {
            self.drag = None;
        }
        self.selected.set(None);
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_deletions(&mut self) {
        self.deleted = DeletedObjects::default();
    }

    /// The field objects given another control and not saved yet.
    pub(crate) fn controls(&self) -> &ControlChanges {
        &self.controls
    }

    /// Show object `key`'s field as `control` with value list `value_list`
    /// (`None`: as stored), set on the layout at `revision` (the first
    /// change's revision is kept, as for [`Self::set_frame`]). The control
    /// and list the object stores (`stored`) are no change.
    pub(crate) fn set_control(
        &mut self,
        key: u64,
        (control, value_list): (u32, Option<u64>),
        stored: (u32, u64),
        revision: Option<om_data::layout_placement::Revision>,
    ) {
        if self.controls.objects.is_empty() {
            self.controls.revision = revision;
        }
        if control == stored.0 && value_list.is_none_or(|list| list == stored.1) {
            self.controls.objects.remove(&key);
        } else {
            self.controls.objects.insert(key, (control, value_list));
        }
        if self.controls.objects.is_empty() {
            self.controls.revision = None;
        }
    }

    /// The host's answer for object `key`'s control: it is no longer to
    /// send.
    pub(crate) fn control_sent(&mut self, key: u64) {
        self.controls.objects.remove(&key);
        if self.controls.objects.is_empty() {
            self.controls.revision = None;
        }
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_controls(&mut self) {
        self.controls = ControlChanges::default();
    }

    /// The text objects retyped and not saved yet.
    pub(crate) fn words(&self) -> &WordChanges {
        &self.words
    }

    /// The host's answer for object `key`'s words.
    pub(crate) fn words_sent(&mut self, key: u64) {
        self.words.objects.remove(&key);
        if self.words.objects.is_empty() {
            self.words.revision = None;
        }
    }

    /// Revert Layout, or the host's answer to Save Layout.
    pub(crate) fn drop_words(&mut self) {
        self.words = WordChanges::default();
        self.retyping = None;
    }

    /// Keep the words of the text being typed, as Return would: Save Layout
    /// saves them with the rest.
    pub(crate) fn finish_retyping(&mut self) {
        if let Some(done) = self.retyping.take() {
            keep_words(&mut self.words, done);
        }
    }

    /// Whether a text being typed holds other words than it stores.
    pub(crate) fn retyping_changed(&self) -> bool {
        self.retyping.as_ref().is_some_and(|r| r.text != r.stored)
    }

    /// The host applied a change of the layout at revision `from` and left
    /// it at `to`: the moves, deletions and new field or button made on
    /// `from` are now on `to`. Those made on another revision keep theirs, for the
    /// host to compare.
    pub(crate) fn rebase(
        &mut self,
        from: om_data::layout_placement::Revision,
        to: om_data::layout_placement::Revision,
    ) {
        self.clear_history();
        let rebase = |revision: &mut Option<om_data::layout_placement::Revision>| {
            if *revision == Some(from) {
                *revision = Some(to);
            }
        };
        rebase(&mut self.moved.revision);
        rebase(&mut self.deleted.revision);
        rebase(&mut self.controls.revision);
        rebase(&mut self.words.revision);
        if let Some(retyping) = self.retyping.as_mut() {
            rebase(&mut retyping.revision);
        }
        if let Some(width) = self.width.as_mut() {
            rebase(&mut width.revision);
        }
        for field in &mut self.new_fields {
            rebase(&mut field.revision);
        }
        if let Some(button) = self.new_button.as_mut() {
            rebase(&mut button.revision);
        }
        if let Some(text) = self.new_text.as_mut() {
            rebase(&mut text.revision);
        }
        if let Some(portal) = self.new_portal.as_mut() {
            rebase(&mut portal.revision);
        }
        if let Some(viewer) = self.new_web_viewer.as_mut() {
            rebase(&mut viewer.revision);
        }
    }

    #[cfg(test)]
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.selected.single()
    }

    /// Select a document object from the Objects pane and reveal it in the
    /// canvas. Invalid indices leave the canvas without a selection.
    pub(crate) fn select_object(&mut self, session: &Session, index: usize, shift: bool) {
        self.synchronize(session);
        let index = self
            .geometry
            .as_ref()
            .filter(|geometry| index < geometry.objects.len() + self.new_fields.len())
            .map(|_| index);
        if let Some((contents, object)) = session
            .panel_contents()
            .zip(index.and_then(|index| self.geometry.as_ref()?.objects.get(index)))
        {
            contents.reveal(object.key, &mut self.front_panels);
        }
        self.selected.click(index, shift);
        self.scroll_to_selected = !self.selected.0.is_empty();
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected.set(None);
        self.selection_drag = None;
        self.box_from = None;
        self.drag = None;
    }

    /// Replace the window's selection using the active tool's selection rule.
    pub(crate) fn select_all(&mut self, session: &Session, filter: SelectAll) {
        self.synchronize(session);
        let Some(geometry) = &self.geometry else {
            return;
        };
        let shown = |object: &ObjectGeom| {
            filter
                .includes(object.selection)
                .then(|| shown_frame(object, &self.moved, &self.deleted))
                .flatten()
        };
        let fields = if filter.includes(SelectionTarget::Object(om_layout::ObjectKind::Field)) {
            self.new_fields.as_slice()
        } else {
            &[]
        };
        self.selected.replace_with_overlay(
            selectable_frames(geometry, fields, shown).map(|(index, _)| index),
            SelectionOverlay {
                aggregate: true,
                ..Default::default()
            },
        );
        self.scroll_to_selected = false;
    }

    /// Stage one arrow-key move on the current layout. Like a drag, this
    /// refuses the whole selection if any member cannot be moved.
    pub(crate) fn nudge_selection(&mut self, session: &Session, delta: layout_move::Delta) -> bool {
        if self.selection_count(session) == 0
            || self.drag.is_some()
            || self.selection_drag.is_some()
            || self.box_from.is_some()
        {
            return false;
        }
        let geometry = self.geometry.as_ref().expect("checked selection");
        let Some(frames) = movable_frames(
            &self.selected,
            geometry,
            &self.new_fields,
            &self.moved,
            &self.deleted,
        ) else {
            self.selected.1 = Some(CANNOT_MOVE_SELECTION);
            return false;
        };
        let Ok(plan) = layout_move::plan(frames.iter().map(|(_, frame)| *frame), delta) else {
            self.selected.1 = Some(CANNOT_MOVE_SELECTION);
            return false;
        };
        self.selected.1 = None;
        if plan.delta == layout_move::Delta::default() {
            return false;
        }
        stage_selection_frames(
            frames.iter().map(|(index, _)| *index).zip(plan.frames),
            geometry,
            &mut self.new_fields,
            &mut self.moved,
        );
        true
    }

    pub(crate) fn selection_notice(&self) -> Option<&'static str> {
        self.selected.1
    }

    pub(crate) fn is_selected(&self, index: usize) -> bool {
        self.selected.0.contains(&index)
    }

    pub(crate) fn selection_count(&self, session: &Session) -> usize {
        let Geometry::Loaded(geometry) = &session.geometry else {
            return 0;
        };
        if !self.selection_matches(session, geometry) {
            return 0;
        }
        self.selected.0.len()
    }

    /// The selected objects Arrange's Align, Distribute and Resize To
    /// apply to: each a stored object that can be moved, with its frame as
    /// shown (moved or stored) and as stored. `None` when anything else is
    /// selected, such as a new field or an object in a group or panel:
    /// what FileMaker Pro does with those is unmeasured (client-ui.md).
    fn arrangeable(&self, session: &Session) -> Option<Vec<(u64, GeomRect, GeomRect)>> {
        if self.selection_count(session) == 0 {
            return None;
        }
        let geometry = self.geometry.as_ref()?;
        geometry.revision?;
        self.selected
            .0
            .iter()
            .map(|index| {
                let object = geometry.objects.get(*index)?;
                if !object.movable || object.key == 0 || self.deleted.keys.contains(&object.key) {
                    return None;
                }
                let shown = self
                    .moved
                    .frames
                    .get(&object.key)
                    .copied()
                    .unwrap_or(object.rect);
                om_data::layout_arrange::arrangeable(&layout_rect(shown)).then_some((
                    object.key,
                    shown,
                    object.rect,
                ))
            })
            .collect()
    }

    /// How many selected objects Arrange applies to: 0 when it applies to
    /// none of the selection.
    pub(crate) fn arrangeable_count(&self, session: &Session) -> usize {
        self.arrangeable(session).map_or(0, |objects| objects.len())
    }

    /// Arrange > Align, Distribute or Resize To on the selection: the new
    /// frames are changes Save Layout stores, as a drag's are. Returns
    /// whether a frame changed.
    pub(crate) fn arrange(
        &mut self,
        session: &Session,
        arrangement: om_data::layout_arrange::Arrangement,
    ) -> Result<bool, om_data::layout_placement::Refusal> {
        let Some(objects) = self.arrangeable(session) else {
            return Ok(false);
        };
        // One change of the draft: a pending move keeps the revision it was
        // made on, so frames derived from it conflict at Save Layout if the
        // layout changed since, whatever order the objects are applied in.
        let revision = if self.moved.frames.is_empty() {
            self.geometry.as_ref().and_then(|g| g.revision)
        } else {
            self.moved.revision
        };
        let frames: Vec<_> = objects
            .iter()
            .map(|&(object, shown, _)| om_data::layout_objects::ObjectFrame {
                object,
                frame: layout_rect(shown),
            })
            .collect();
        let changed = om_data::layout_arrange::arrange(&frames, arrangement)?;
        for new in &changed {
            let Some(&(_, _, stored)) = objects.iter().find(|o| o.0 == new.object) else {
                continue;
            };
            let frame = GeomRect {
                top: new.frame.top,
                left: new.frame.left,
                bottom: new.frame.bottom,
                right: new.frame.right,
            };
            self.set_frame(new.object, frame, stored, revision);
            if !self.moved.frames.is_empty() {
                self.moved.revision = revision;
            }
        }
        Ok(!changed.is_empty())
    }

    pub(crate) fn can_delete_selection(&self, session: &Session) -> bool {
        self.selection_count(session) > 0
            && self.selected.0.iter().all(|index| {
                let geometry = self.geometry.as_ref().expect("checked selection");
                if let Some(object) = geometry.objects.get(*index) {
                    object.deletable && object.key != 0 && geometry.revision.is_some()
                } else {
                    index
                        .checked_sub(geometry.objects.len())
                        .is_some_and(|i| i < self.new_fields.len())
                }
            })
    }

    pub(crate) fn delete_selection(&mut self, session: &Session) -> bool {
        if !self.can_delete_selection(session) {
            return false;
        }
        let geometry = self.geometry.as_ref().expect("checked selection");
        let stored = geometry.objects.len();
        let revision = geometry.revision;
        let indices: Vec<_> = self.selected.0.iter().copied().collect();
        let keys: Vec<_> = indices
            .iter()
            .filter_map(|i| geometry.objects.get(*i).map(|o| o.key))
            .collect();
        for key in keys {
            self.delete_object(key, revision);
        }
        for index in indices
            .into_iter()
            .rev()
            .filter_map(|i| i.checked_sub(stored))
        {
            self.new_fields.remove(index);
        }
        self.field_drag = None;
        self.drag_from = None;
        self.selection_drag = None;
        self.selected.set(None);
        true
    }

    pub(crate) fn selected_object<'a>(&self, session: &'a Session) -> Option<&'a ObjectGeom> {
        let Geometry::Loaded(geometry) = &session.geometry else {
            return None;
        };
        if !self.selection_matches(session, geometry) {
            return None;
        }
        geometry.objects.get(self.selected.single()?)
    }

    fn matches(&self, session: &Session) -> bool {
        self.identity.as_ref().is_some_and(|(host, file, layout)| {
            host == &session.target.host
                && file == &session.target.file
                && *layout == session.layout
        })
    }

    fn selection_matches(&self, session: &Session, geometry: &LayoutGeometry) -> bool {
        self.matches(session)
            && self.panel_generation == session.layout_generation
            && self.geometry.as_ref() == Some(geometry)
    }

    pub(crate) fn synchronize(&mut self, session: &Session) {
        let geometry = match &session.geometry {
            Geometry::Loaded(geometry) => Some(geometry),
            Geometry::Loading | Geometry::Unavailable => None,
        };
        let moved = !self.matches(session);
        let revisited = self.panel_generation != session.layout_generation;
        let geometry_changed = self.geometry.as_ref() != geometry;
        if moved || revisited {
            self.front_panels.clear();
            self.panel_generation = session.layout_generation;
        }
        if moved || geometry_changed {
            self.clear_history();
            self.drag = None;
            self.field_drag = None;
            self.drag_from = None;
            self.part_drag = None;
            self.part_resize = None;
            // A new field or button and moved objects belong to their
            // layout; another layout or file drops them.
            if moved {
                self.drop_new_object();
                self.drop_moves();
                self.drop_deletions();
                self.drop_controls();
                self.drop_words();
                self.drop_width();
                self.known_keys.clear();
            }
            self.identity = Some((
                session.target.host.clone(),
                session.target.file.clone(),
                session.layout,
            ));
            self.geometry = geometry.cloned();
            // Changes to objects the layout no longer has (another window
            // or client deleted them) cannot be saved, but they are the
            // user's: they stay, so Save Layout reports them, Revert Layout
            // discards them, and closing or quitting waits for one or the
            // other. A text being typed in such an object is kept as its
            // change if the words changed, and its editor closes. The window
            // is told (`take_orphaned_drafts`).
            if let Some(geometry) = &self.geometry {
                let present: BTreeSet<u64> = geometry.objects.iter().map(|o| o.key).collect();
                let known = std::mem::replace(&mut self.known_keys, present);
                let gone = |key: &u64| known.contains(key) && !self.known_keys.contains(key);
                if let Some(done) = self.retyping.take_if(|r| gone(&r.key)) {
                    keep_words(&mut self.words, done);
                }
                self.orphaned_drafts |=
                    self.controls.objects.keys().any(gone) || self.words.objects.keys().any(gone);
            }
        }
        if moved || geometry_changed || revisited {
            self.clear_selection();
            self.scroll_to_selected = false;
        }
    }
}

/// `frozen`: Save Layout's placement is with the host, so the new field or
/// button takes no change until it answers: the canvas takes no drag or
/// click.
pub(crate) fn show(
    ui: &mut Ui,
    session: &Session,
    tool: LayoutTool,
    fields: &[FieldChoice],
    state: &mut CanvasState,
    frozen: bool,
    document: &mut DocumentState,
) {
    let incoming = egui::DragAndDrop::payload::<crate::layout_objects::FieldDrag>(ui.ctx())
        .filter(|drag| drag.matches(session, fields));
    let contents = session.panel_contents();
    ui.painter()
        .rect_filled(ui.max_rect(), CornerRadius::ZERO, theme::PASTEBOARD);

    match &session.geometry {
        Geometry::Loading => {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.spinner();
                ui.label("Loading layout...");
            });
        }
        Geometry::Loaded(geometry) => {
            // Keep the coordinate system in which an in-flight gesture
            // began through its release, even if the window zoom changes.
            if state.shown_zoom != document.zoom {
                if ui.input(|input| input.pointer.any_released()) {
                    ui.request_repaint();
                } else if !ui.input(|input| input.pointer.any_down()) {
                    state.shown_zoom = document.zoom;
                }
            }
            let factor = state.shown_zoom.factor();
            let (design, scroll) = document.design_surface(session);
            zoom::show(ui, ui.available_size() / factor, factor, |ui| {
                let unsaved = state.unsaved();
                let previewed_popovers =
                    contents
                        .map(|panels| {
                            panels.containing_popovers(
                                state.selected.0.iter().filter_map(|&index| {
                                    geometry.objects.get(index).map(|o| o.key)
                                }),
                            )
                        })
                        .unwrap_or_default();
                let visible = |key| {
                    contents.is_none_or(|panels| {
                        panels.visible(key, &state.front_panels, &previewed_popovers)
                    })
                };
                let stack = contents
                    .filter(|_| !previewed_popovers.is_empty())
                    .map(|panels| {
                        let (mut layout, popovers): (Vec<_>, Vec<_>) = (0..geometry.objects.len())
                            .partition(|&index| {
                                !panels.in_previewed_popover(
                                    geometry.objects[index].key,
                                    &previewed_popovers,
                                )
                            });
                        let first_popover = layout.len();
                        layout.extend(popovers);
                        (layout, first_popover)
                    });
                let dropped = draw(
                    ui,
                    geometry,
                    tool,
                    incoming.as_deref(),
                    &mut state.selected,
                    &mut state.scroll_to_selected,
                    New {
                        fields: &mut state.new_fields,
                        field_drag: &mut state.field_drag,
                        field_finished: &mut state.field_finished,
                        button: &mut state.new_button,
                        text: &mut state.new_text,
                        text_editor: &mut state.new_text_editor,
                        portal: &mut state.new_portal,
                        web_viewer: &mut state.new_web_viewer,
                        drag_from: &mut state.drag_from,
                        setup: &mut state.button_setup,
                        portal_setup: &mut state.portal_setup,
                        web_viewer_setup: &mut state.web_viewer_setup,
                    },
                    (
                        &mut state.moved,
                        &state.deleted,
                        &mut state.drag,
                        &mut state.selection_drag,
                        &mut state.box_from,
                        &state.controls,
                    ),
                    Texts {
                        words: &mut state.words,
                        retyping: &mut state.retyping,
                        stored: &|key| plain_words(session, key),
                    },
                    &mut state.width,
                    PartEdges {
                        enabled: !unsaved && geometry.revision.is_some(),
                        drag: &mut state.part_drag,
                        resize: &mut state.part_resize,
                        tool: &mut state.part_tool,
                    },
                    // Frozen, the canvas still paints but takes no drag or click.
                    !frozen,
                    &visible,
                    stack
                        .as_ref()
                        .map(|(order, first)| (order.as_slice(), *first)),
                    design.as_ref(),
                    scroll,
                );
                if let Some(field) = dropped {
                    state.stage_field(session, field);
                }
            });
        }
        Geometry::Unavailable => {
            // A provider with no layout geometry can still show a useful
            // default field layout. It is deliberately not inspectable as
            // actual document objects.
            let geometry = fallback_geometry(session);
            let _ = draw(
                ui,
                &geometry,
                tool,
                None,
                &mut Selection::default(),
                &mut false,
                New {
                    fields: &mut Vec::new(),
                    field_drag: &mut None,
                    field_finished: &mut false,
                    button: &mut None,
                    text: &mut None,
                    text_editor: &mut None,
                    portal: &mut None,
                    drag_from: &mut None,
                    setup: &mut false,
                    portal_setup: &mut false,
                    web_viewer: &mut None,
                    web_viewer_setup: &mut false,
                },
                (
                    &mut MovedObjects::default(),
                    &DeletedObjects::default(),
                    &mut None,
                    &mut None,
                    &mut None,
                    &ControlChanges::default(),
                ),
                Texts {
                    words: &mut WordChanges::default(),
                    retyping: &mut None,
                    stored: &|_| None,
                },
                &mut None,
                PartEdges {
                    enabled: false,
                    drag: &mut None,
                    resize: &mut None,
                    tool: &mut None,
                },
                false,
                &|_| true,
                None,
                None,
                &mut document.scroll,
            );
        }
    }
}

/// The words a stored text object shows, when they are plain words
/// OpenMaker retypes (`om_data::layout_placement::plan_words`): one line,
/// unstyled, no merge field.
fn plain_words(session: &Session, key: u64) -> Option<String> {
    let crate::session::RenderState::Loaded(document) = &session.render else {
        return None;
    };
    let object = document
        .layout
        .objects
        .objects
        .iter()
        .find(|o| o.id == key)?;
    (object.kind() == om_layout::ObjectKind::Text && object.rich_text.is_none())
        .then(|| object.text.clone())
        .flatten()
        .filter(|text| !text.contains("<<") && !text.chars().any(char::is_control))
}

/// Stored texts retyped on the canvas: the changes not saved yet, the one
/// being typed, and a stored text's plain words by key.
struct Texts<'a> {
    words: &'a mut WordChanges,
    retyping: &'a mut Option<Retyping>,
    stored: &'a dyn Fn(u64) -> Option<String>,
}

/// What the Field, Button, Text, Portal and Web Viewer tools draw: one new
/// object at a time.
struct New<'a> {
    fields: &'a mut Vec<NewField>,
    field_drag: &'a mut Option<usize>,
    field_finished: &'a mut bool,
    button: &'a mut Option<NewButton>,
    text: &'a mut Option<NewText>,
    text_editor: &'a mut Option<egui::Id>,
    portal: &'a mut Option<NewPortal>,
    web_viewer: &'a mut Option<NewWebViewer>,
    drag_from: &'a mut Option<Pos2>,
    setup: &'a mut bool,
    portal_setup: &'a mut bool,
    web_viewer_setup: &'a mut bool,
}

impl New<'_> {
    fn covers(&self, point: Pos2) -> bool {
        [
            self.button.as_ref().map(NewButton::frame),
            self.text.as_ref().map(NewText::frame),
            self.portal.as_ref().map(NewPortal::frame),
            self.web_viewer.as_ref().map(NewWebViewer::frame),
        ]
        .into_iter()
        .flatten()
        .any(|rect| object_rect(Pos2::ZERO, &geom_rect(rect)).contains(point))
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    ui: &mut Ui,
    geometry: &LayoutGeometry,
    tool: LayoutTool,
    incoming: Option<&crate::layout_objects::FieldDrag>,
    selected: &mut Selection,
    scroll_to_selected: &mut bool,
    new: New<'_>,
    (moved, deleted, drag, selection_drag, box_from, controls): (
        &mut MovedObjects,
        &DeletedObjects,
        &mut Option<Drag>,
        &mut Option<SelectionDrag>,
        &mut Option<Pos2>,
        &ControlChanges,
    ),
    texts: Texts<'_>,
    new_width: &mut Option<NewWidth>,
    edges: PartEdges<'_>,
    selectable: bool,
    visible: &dyn Fn(u64) -> bool,
    stack: Option<(&[usize], usize)>,
    design: Option<&crate::document::design::Surface<'_>>,
    scroll_state: &mut crate::document::scroll::Scroll,
) -> Option<NewField> {
    let order = stack.map(|(order, _)| order);
    let mut dropped = None;
    let width = finite_extent(new_width.map_or(geometry.width, |w| w.width));
    let height = finite_extent(geometry.height);
    let frame = scroll_state.begin(ui, "layout_canvas", None);
    let factor = crate::document::layer_scale(ui);
    let mut scroll = egui::ScrollArea::both().wheel_scroll_multiplier(vec2(1.0, 1.0) / factor);
    if factor != 1.0 {
        // ScrollArea's touch drag uses a window-space delta. At a non-unit
        // scale it would scroll farther or less far than the finger.
        scroll = scroll.scroll_source(ScrollSource {
            drag: DragScroll::Never,
            ..Default::default()
        });
    }
    if let Some(offset) = frame.offset() {
        scroll = scroll.scroll_offset(offset);
    }
    let scroll = scroll.stick_to_bottom(frame.at_end());
    let output = scroll
        .id_salt("layout_canvas")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let size = vec2(
                (PART_STRIP_WIDTH + width + PASTEBOARD_MARGIN).max(ui.available_width()),
                (height + PASTEBOARD_MARGIN).max(ui.available_height()),
            );
            // Fields accumulate in the draft; other tools keep their existing
            // single unfinished-object setup until Save Layout.
            // The tool draws when no other tool's new object waits.
            let waiting = [
                (LayoutTool::Field, !new.fields.is_empty()),
                (LayoutTool::Button, new.button.is_some()),
                (LayoutTool::Text, new.text.is_some()),
                (LayoutTool::Portal, new.portal.is_some()),
                (LayoutTool::WebViewer, new.web_viewer.is_some()),
            ];
            let drawing = selectable
                && waiting.iter().any(|(drawer, _)| *drawer == tool)
                && !waiting
                    .iter()
                    .any(|(drawer, waits)| *drawer != tool && *waits);
            let selection_tool = selectable && tool == LayoutTool::Selection;
            let (area, background) = ui.allocate_exact_size(
                size,
                if drawing {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                },
            );
            background.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Layout canvas"));
            let origin = pos2(area.left() + PART_STRIP_WIDTH, area.top())
                .round_to_pixels(ui.pixels_per_point());
            let surface = Rect::from_min_size(origin, vec2(width, height));
            // Response positions already use this layer's coordinates.
            // Raw input (press origins and positions during a drag) does not.
            let from_global = ui
                .ctx()
                .layer_transform_from_global(ui.layer_id())
                .unwrap_or(TSTransform::IDENTITY);
            let pointer = ui
                .input(|input| input.pointer.interact_pos())
                .map(|point| from_global * point);
            let press = ui
                .input(|input| input.pointer.press_origin())
                .map(|point| from_global * point);
            // The pasteboard keeps ScrollArea's drag behavior. Only the
            // layout surface owns a Selection tool box gesture.
            let box_response = (selection_tool && incoming.is_none()).then(|| {
                ui.interact(
                    surface,
                    background.id.with("selection_box"),
                    Sense::click_and_drag(),
                )
            });
            if selectable
                && new.button.is_none()
                && new.text.is_none()
                && new.portal.is_none()
                && new.web_viewer.is_none()
            {
                if let Some(payload) = incoming {
                    if let Some(pointer) = pointer
                        .filter(|point| surface.contains(*point) && ui.clip_rect().contains(*point))
                    {
                        if background
                            .dnd_release_payload::<crate::layout_objects::FieldDrag>()
                            .is_some()
                        {
                            dropped = Some(NewField {
                                left: f64::from((pointer.x - origin.x).round()),
                                top: f64::from((pointer.y - origin.y).round()),
                                width: 120.0,
                                // Leave room for Universal Touch edit-box padding.
                                height: 44.0,
                                field: Some(payload.field.clone()),
                                label: true,
                                control: 0,
                                value_list: None,
                                revision: geometry.revision,
                            });
                        }
                    }
                }
            }
            // The Part tool's button dropped on the layout, as Claris
            // documents it ("drag the Part tool ... to the layout"), or a
            // click on the layout with the tool chosen (OpenMaker's): Part
            // Definition opens for the point, which may be below the parts,
            // where FileMaker Pro 26.1.1 adds a part reaching down to it.
            // Also on a frozen canvas, where the window refuses it with the
            // reason.
            let on_layout = pointer.filter(|point| {
                surface.x_range().contains(point.x)
                    && area.contains(*point)
                    && ui.clip_rect().contains(*point)
            });
            let dropped_tool = background.dnd_release_payload::<PartToolDrag>().is_some();
            if let Some(point) = on_layout {
                if dropped_tool || (tool == LayoutTool::Part && background.clicked()) {
                    *edges.tool = Some(f64::from(point.y - origin.y));
                }
            }
            let painter = ui.painter().clone();
            painter.rect_filled(
                surface,
                CornerRadius::ZERO,
                design
                    .and_then(|design| design.background())
                    .unwrap_or(theme::CONTENT_BG),
            );
            if let Some(design) = design {
                design.paint_parts(ui, surface);
            }
            let strip = Rect::from_min_max(area.min, pos2(origin.x, area.bottom()));
            painter.rect_filled(strip, CornerRadius::ZERO, theme::BAR_BG);
            painter.vline(
                origin.x - 0.5,
                strip.y_range(),
                Stroke::new(1.0, theme::BORDER),
            );
            for part in &geometry.parts {
                draw_part(ui, origin, width, part);
            }
            let mut part_guide = None;
            // A part's lower boundary: the Selection tool drags it to set the
            // part's height, in whole points, down to the lowest object in
            // it; the window sends it when the drag ends.
            if selection_tool && edges.enabled {
                // A frame can end one drag and begin another. Finish the
                // former from its own state regardless of part draw order.
                let previous_drag = *edges.drag;
                for part in &geometry.parts {
                    let Some(identity) = part.identity else {
                        continue;
                    };
                    let bottom = origin.y + (part.top + part.height) as f32;
                    let band = Rect::from_x_y_ranges(
                        origin.x - PART_STRIP_WIDTH..=origin.x + width,
                        bottom - 3.0..=bottom + 3.0,
                    );
                    if !band.is_finite() {
                        continue;
                    }
                    let id = ui
                        .scope_id()
                        .with(("part_edge", identity.key, identity.part_type));
                    let response = ui
                        .interact(band, id, Sense::drag())
                        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
                    let label = format!("{} boundary", part.kind_name);
                    response.widget_info(|| WidgetInfo::labeled(Role::Splitter, true, &label));
                    let lowest = geometry
                        .objects
                        .iter()
                        .filter(|o| {
                            o.key != 0
                                && o.rect.top >= part.top
                                && o.rect.top < part.top + part.height
                        })
                        .map(|o| o.rect.bottom)
                        .fold(part.top + 1.0, f64::max);
                    let min = (lowest - part.top).ceil().max(1.0);
                    // The parts together stay within the coordinate range.
                    let others = geometry.parts.iter().map(|p| p.height).sum::<f64>() - part.height;
                    let max = (om_data::layout_placement::MAX_COORDINATE - others).max(min);
                    // A geometry refresh clears the gesture. Egui can keep
                    // reporting a held drag, so only its start may bind a part.
                    if response.drag_started() {
                        *edges.drag = Some(PartDrag {
                            part: identity,
                            height: part.height,
                        });
                    }
                    let active = edges.drag.as_mut().filter(|drag| drag.part == identity);
                    if let Some((drag, pointer)) = active.zip(
                        response
                            .interact_pointer_pos()
                            .filter(|_| response.dragged()),
                    ) {
                        drag.height = (f64::from(pointer.y - origin.y) - part.top)
                            .round()
                            .clamp(min, max);
                    }
                    if response.drag_stopped() {
                        if edges.drag.is_some_and(|d| d.part == identity) {
                            *edges.drag = None;
                        }
                        let dragged = previous_drag.filter(|d| d.part == identity);
                        if let Some(drag) = dragged.filter(|d| d.height != part.height) {
                            *edges.resize = Some(PartResize {
                                part: identity.key,
                                part_type: identity.part_type,
                                size: drag.height,
                                revision: geometry.revision,
                            });
                        }
                    }
                    if let Some(drag) = edges.drag.filter(|d| d.part == identity) {
                        let y = origin.y + (part.top + drag.height) as f32;
                        part_guide = Some((y, part.kind_name.as_str(), drag.height));
                    }
                }
            }
            // The layout's right edge: the Selection tool drags it to set
            // the layout's width, in whole points, as FileMaker Pro's
            // Layout mode does; Save Layout saves it.
            if selectable && tool == LayoutTool::Selection {
                let edge = Rect::from_x_y_ranges(
                    surface.right() - 3.0..=surface.right() + 3.0,
                    surface.y_range(),
                );
                let id = ui.scope_id().with("layout_width");
                let response = ui
                    .interact(edge, id, Sense::drag())
                    .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                response.widget_info(|| WidgetInfo::labeled(Role::Splitter, true, "Layout width"));
                if let Some(pointer) = response
                    .interact_pointer_pos()
                    .filter(|_| response.dragged())
                {
                    // As far as the host accepts (1 to its largest coordinate).
                    let dragged = f64::from((pointer.x - origin.x).round())
                        .clamp(1.0, om_data::layout_placement::MAX_COORDINATE);
                    let stored = geometry.width;
                    *new_width = (dragged != stored).then(|| NewWidth {
                        width: dragged,
                        revision: new_width.map_or(geometry.revision, |w| w.revision),
                    });
                }
            }

            if drawing {
                // The tool draws the new object's frame, in whole layout
                // points, from where the drag began. A button drawn opens
                // Button Setup, as FileMaker Pro opens it.
                let at = |pos: Pos2| pos2((pos.x - origin.x).round(), (pos.y - origin.y).round());
                if background.drag_started() {
                    *new.field_drag = None;
                    *new.drag_from = press.or(background.interact_pointer_pos()).map(at);
                }
                let mut drawn = false;
                if background.dragged() || background.drag_stopped() {
                    if let (Some(from), Some(to)) =
                        (*new.drag_from, background.interact_pointer_pos().map(at))
                    {
                        let frame = Rect::from_two_pos(from, to);
                        if frame.width() >= 1.0 && frame.height() >= 1.0 {
                            drawn = true;
                            // The new object is what the tools act on now:
                            // a stored object selected before is not (Edit >
                            // Clear would delete it).
                            selected.set(None);
                            let (left, top, width, height) = (
                                f64::from(frame.left()),
                                f64::from(frame.top()),
                                f64::from(frame.width()),
                                f64::from(frame.height()),
                            );
                            let revision = geometry.revision;
                            if tool == LayoutTool::Text {
                                let text = new.text.take().map(|t| t.text).unwrap_or_default();
                                *new.text = Some(NewText {
                                    left,
                                    top,
                                    width,
                                    height,
                                    text,
                                    revision,
                                    focus: true,
                                });
                            } else if tool == LayoutTool::WebViewer {
                                let setup = new.web_viewer.take().and_then(|v| v.setup);
                                *new.web_viewer = Some(NewWebViewer {
                                    left,
                                    top,
                                    width,
                                    height,
                                    setup,
                                    revision,
                                });
                            } else if tool == LayoutTool::Portal {
                                let setup = new.portal.take().and_then(|p| p.setup);
                                *new.portal = Some(NewPortal {
                                    left,
                                    top,
                                    width,
                                    height,
                                    setup,
                                    revision,
                                });
                            } else if tool == LayoutTool::Button {
                                let (label, step, action) = new
                                    .button
                                    .take()
                                    .map(|b| (b.label, b.step, b.action))
                                    .unwrap_or_default();
                                *new.button = Some(NewButton {
                                    left,
                                    top,
                                    width,
                                    height,
                                    label,
                                    step,
                                    action,
                                    revision,
                                });
                            } else {
                                let index = *new.field_drag.get_or_insert_with(|| {
                                    let index = new.fields.len();
                                    new.fields.push(NewField {
                                        left,
                                        top,
                                        width,
                                        height,
                                        field: None,
                                        label: true,
                                        control: 0,
                                        value_list: None,
                                        revision,
                                    });
                                    index
                                });
                                let field = &mut new.fields[index];
                                (field.left, field.top, field.width, field.height) =
                                    (left, top, width, height);
                                selected.set(Some(geometry.objects.len() + index));
                            }
                        }
                    }
                }
                if background.drag_stopped() {
                    *new.drag_from = None;
                    if new.field_drag.take().is_some() {
                        *new.field_finished = true;
                    }
                    if drawn && tool == LayoutTool::Button {
                        *new.setup = true;
                    }
                    if drawn && tool == LayoutTool::Portal {
                        *new.portal_setup = true;
                    }
                    // Also when the drag ended on an empty frame after drawing
                    // one: the viewer drawn last waits for its setup.
                    if tool == LayoutTool::WebViewer
                        && (drawn || new.web_viewer.as_ref().is_some_and(|v| v.setup.is_none()))
                    {
                        *new.web_viewer_setup = true;
                    }
                }
                // A click with the Text tool starts a text where it is.
                if tool == LayoutTool::Text && background.clicked() {
                    if let Some(at) = background.interact_pointer_pos().map(at) {
                        selected.set(None);
                        let text = new.text.take().map(|t| t.text).unwrap_or_default();
                        *new.text = Some(NewText {
                            left: f64::from(at.x.max(0.0)),
                            top: f64::from(at.y.max(0.0)),
                            width: CLICKED_TEXT.0,
                            height: CLICKED_TEXT.1,
                            text,
                            revision: geometry.revision,
                            focus: true,
                        });
                    }
                }
            }

            // Where the pointer is, in whole layout points.
            let at = |pos: Pos2| pos2((pos.x - origin.x).round(), (pos.y - origin.y).round());
            // A drag in progress sets its object's frame.
            if let Some(active) = *drag {
                if let Some(pointer) = pointer.map(at) {
                    let frame = dragged(active, pointer - active.from);
                    let stored = geometry
                        .objects
                        .iter()
                        .find(|o| o.key == active.key)
                        .map_or(active.start, |o| o.rect);
                    if moved.frames.is_empty() {
                        moved.revision = geometry.revision;
                    }
                    if frame == stored {
                        moved.frames.remove(&active.key);
                    } else {
                        moved.frames.insert(active.key, frame);
                    }
                    if moved.frames.is_empty() {
                        moved.revision = None;
                    }
                }
                if !ui.input(|input| input.pointer.any_down()) {
                    *drag = None;
                }
            }
            if !selectable {
                *selection_drag = None;
            }
            if let Some(active) = selection_drag.as_ref() {
                if let Some(pointer) = pointer.map(at) {
                    drag_selection(active, pointer, geometry, new.fields, moved);
                }
                if !ui.input(|i| i.pointer.any_down()) {
                    *selection_drag = None;
                }
            }
            let present = |object: &ObjectGeom| shown_frame(object, moved, deleted);
            let shown = |object: &ObjectGeom| {
                if !visible(object.key) {
                    return None;
                }
                present(object)
            };
            let mut marquee = None;
            if let Some(response) = &box_response {
                let primary = egui::PointerButton::Primary;
                if response.drag_started_by(primary) {
                    // A click-only widget can be above a draggable background.
                    // The box must still start on empty layout surface.
                    *box_from = press
                        .filter(|&point| surface.contains(point))
                        .map(|point| point - origin.to_vec2())
                        .filter(|&point| {
                            hit_test(geometry, order, shown, point).is_none() && !new.covers(point)
                        });
                }
                if let Some(from) = *box_from {
                    let box_rect = response
                        .interact_pointer_pos()
                        .map(|pointer| Rect::from_two_pos(from + origin.to_vec2(), pointer));
                    if response.drag_stopped_by(primary) {
                        if let Some(rect) = box_rect {
                            let contained = ui.input(|input| {
                                input
                                    .events
                                    .iter()
                                    .rev()
                                    .find_map(|event| match event {
                                        egui::Event::PointerButton {
                                            button: egui::PointerButton::Primary,
                                            pressed: false,
                                            modifiers,
                                            ..
                                        } => Some(modifiers.command),
                                        _ => None,
                                    })
                                    .unwrap_or(input.modifiers.command)
                            });
                            let bounds = rect.translate(-origin.to_vec2());
                            let bounds = geometry_frame(bounds);
                            let coverage = if contained {
                                Coverage::Contains
                            } else {
                                Coverage::Intersects
                            };
                            let selected_frames = layout_selection::marquee(
                                bounds,
                                coverage,
                                &marquee_frames(geometry, new.fields, present, visible),
                            );
                            selected.replace_with_overlay(
                                selected_frames.indices,
                                SelectionOverlay {
                                    aggregate: selected_frames.aggregate,
                                    tab_controls: selected_frames.tab_controls,
                                },
                            );
                            *scroll_to_selected = false;
                        }
                        *box_from = None;
                    } else if response.dragged_by(primary) {
                        marquee = box_rect;
                    } else {
                        *box_from = None;
                    }
                }
            } else {
                *box_from = None;
            }
            let clicked = box_response
                .as_ref()
                .filter(|response| response.clicked())
                .or_else(|| background.clicked().then_some(&background));
            if let Some(response) = clicked.filter(|_| selection_tool) {
                let index = response
                    .interact_pointer_pos()
                    .and_then(|point| hit_test(geometry, order, shown, point - origin.to_vec2()));
                selected.click(index, ui.input(|i| i.modifiers.shift));
            }

            let right_pasteboard =
                Rect::from_min_max(surface.right_top(), pos2(area.right(), surface.bottom()));
            let artwork_bounds = surface.union(right_pasteboard);
            let mut artwork = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("design_artwork")
                    .max_rect(artwork_bounds),
            );
            artwork.set_clip_rect(ui.clip_rect().intersect(artwork_bounds));
            let mut tabs = crate::document::design::TabArtwork::default();
            for (rank, index) in stacking_order(geometry.objects.len(), order).enumerate() {
                let object = &geometry.objects[index];
                let Some(frame) = shown(object) else {
                    continue;
                };
                let rect = object_rect(origin, &frame);
                if !rect.is_finite() {
                    continue;
                }
                let changed = moved.frames.contains_key(&object.key)
                    || controls.objects.contains_key(&object.key)
                    || texts.words.objects.contains_key(&object.key);
                let outline = if changed && object.key != 0 {
                    Stroke::new(1.0, theme::SELECTED)
                } else {
                    Stroke::new(1.0, theme::BUTTON_BORDER)
                };
                let field_caption = object
                    .field_name
                    .as_deref()
                    .map(|name| name.find("::").map_or(name, |at| &name[at..]))
                    .unwrap_or(&object.kind_name);
                let draft_words = texts.words.objects.get(&object.key).map(String::as_str);
                let tab_selected = selected.2.tab_controls.contains_key(&object.key);
                let painted = artwork_bounds.intersects(rect)
                    && design.is_some_and(|design| {
                        design.paint_object(
                            &mut artwork,
                            object.key,
                            rect,
                            field_caption,
                            draft_words,
                            (visible, &mut tabs, tab_selected),
                        )
                    });
                if !painted || changed || !artwork_bounds.contains_rect(rect) {
                    if rect.width() < 1.0 || rect.height() < 1.0 {
                        painter.line_segment([rect.min, rect.max], outline);
                    } else {
                        painter.rect_stroke(rect, CornerRadius::ZERO, outline, StrokeKind::Inside);
                    }
                }
                if !painted && rect.width() >= 1.0 && rect.height() >= 1.0 {
                    let stored_words = (!texts.words.objects.contains_key(&object.key))
                        .then(|| (texts.stored)(object.key))
                        .flatten();
                    let caption = draft_words
                        .or(stored_words.as_deref())
                        .unwrap_or(field_caption);
                    painter
                        .with_clip_rect(rect.shrink(3.0).intersect(ui.clip_rect()))
                        .text(
                            rect.left_top() + vec2(5.0, 4.0),
                            Align2::LEFT_TOP,
                            caption,
                            theme::small(),
                            theme::TEXT,
                        );
                }

                // Register in stacking order. The last (front-most) object
                // receives pointer input where objects overlap.
                let hit = hit_rect(rect);
                let movable = selection_tool && object.movable && object.key != 0;
                let raised = stack.is_some_and(|(_, first_popover)| rank >= first_popover);
                // A click-only object can pass drags through to a widget
                // underneath it on the same layer.
                let response = ui.interact(
                    hit,
                    ui.scope_id().with(("layout_object", index)),
                    if (raised && selectable) || movable || (selection_tool && selected.0.len() > 1)
                    {
                        Sense::click_and_drag()
                    } else if selection_tool {
                        Sense::click()
                    } else {
                        Sense::hover()
                    },
                );
                // Dragging a movable object moves it.
                if selection_tool
                    && (!raised || movable)
                    && response.drag_started()
                    && drag.is_none()
                    && selection_drag.is_none()
                {
                    selected.start_drag(index, ui.input(|i| i.modifiers.shift));
                    let from = press.or(response.interact_pointer_pos()).map(at);
                    if let Some(from) = from {
                        *selection_drag = begin_selection_drag(
                            selected, geometry, new.fields, moved, deleted, from,
                        );
                        if selection_drag.is_none() {
                            selected.1 = Some(CANNOT_MOVE_SELECTION);
                        }
                    }
                }
                let label = object_label(index, object);
                response.widget_info(|| {
                    WidgetInfo::selected(
                        Role::Button,
                        selection_tool,
                        selected.0.contains(&index),
                        &label,
                    )
                });
                if response.clicked() && selection_tool {
                    let index = response
                        .interact_pointer_pos()
                        .and_then(|point| {
                            hit_test(geometry, order, shown, point - origin.to_vec2())
                        })
                        .or(Some(index));
                    selected.click(index, ui.input(|i| i.modifiers.shift));
                }
                // A double click on a text with plain words types into it.
                if response.double_clicked()
                    && selection_tool
                    && selected.0.len() == 1
                    && object.key != 0
                {
                    if let Some(stored) = (texts.stored)(object.key) {
                        let text = texts.words.objects.get(&object.key).cloned();
                        *texts.retyping = Some(Retyping {
                            key: object.key,
                            text: text.unwrap_or_else(|| stored.clone()),
                            stored,
                            revision: geometry.revision,
                            focus: true,
                        });
                    }
                }
            }
            if design.is_some() && right_pasteboard.is_positive() {
                // Fade the finished artwork in one pass. Crossing objects
                // keep one stacking position, text label and hit target.
                painter.rect_filled(
                    right_pasteboard,
                    CornerRadius::ZERO,
                    theme::PASTEBOARD.gamma_multiply(1.0 - PASTEBOARD_ARTWORK_OPACITY),
                );
            }
            let retyped = texts.retyping.as_ref().and_then(|r| {
                let object = geometry.objects.iter().find(|o| o.key == r.key)?;
                shown(object).map(|frame| object_rect(origin, &frame))
            });
            if let Some(rect) = retyped.filter(|_| selectable) {
                retype(ui, &painter, rect, texts.words, texts.retyping);
            }

            let mut draft_drag = None;
            for (index, field) in new.fields.iter().enumerate() {
                let selected_index = geometry.objects.len() + index;
                let response = draw_new_field(ui, &painter, origin, field, index, selection_tool);
                if response.clicked() {
                    selected.click(Some(selected_index), ui.input(|i| i.modifiers.shift));
                }
                if response.drag_started()
                    && selection_tool
                    && drag.is_none()
                    && selection_drag.is_none()
                {
                    selected.start_drag(selected_index, ui.input(|i| i.modifiers.shift));
                    draft_drag = press.or(response.interact_pointer_pos()).map(at);
                }
                if selected.0.contains(&selected_index) {
                    painter.rect_stroke(
                        response.rect,
                        CornerRadius::ZERO,
                        Stroke::new(2.0, theme::SELECTED),
                        StrokeKind::Outside,
                    );
                }
            }
            if let Some(from) = draft_drag {
                *selection_drag =
                    begin_selection_drag(selected, geometry, new.fields, moved, deleted, from);
                if selection_drag.is_none() {
                    selected.1 = Some(CANNOT_MOVE_SELECTION);
                }
            }
            if let Some(text) = new.text.as_mut() {
                *new.text_editor = draw_new_text(ui, &painter, origin, text, selectable);
            }
            if let Some(button) = new.button.as_ref() {
                if draw_new_button(ui, &painter, origin, button, selectable) {
                    *new.setup = true;
                }
            }
            if let Some(portal) = new.portal.as_ref() {
                if draw_new_portal(ui, &painter, origin, portal, selectable) {
                    *new.portal_setup = true;
                }
            }
            if let Some(viewer) = new.web_viewer.as_ref() {
                if draw_new_web_viewer(ui, &painter, origin, viewer, selectable) {
                    *new.web_viewer_setup = true;
                }
            }

            // Guides stay visible over both stored and draft artwork.
            painter.vline(
                surface.right() - 0.5,
                surface.y_range(),
                Stroke::new(1.0, theme::DIVIDER),
            );
            if let Some((y, name, height)) = part_guide {
                painter.hline(
                    origin.x..=origin.x + width,
                    y,
                    Stroke::new(1.0, theme::SELECTED),
                );
                painter.text(
                    pos2(origin.x + 4.0, y - 2.0),
                    Align2::LEFT_BOTTOM,
                    format!("{name} {height} pt"),
                    theme::small(),
                    theme::SELECTED,
                );
            }
            if let Some(rect) = marquee {
                painter.rect_stroke(
                    rect,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, theme::SELECTED),
                    StrokeKind::Inside,
                );
            }

            let aggregate_selection = selected.2.aggregate;
            let selection_frame = |object: &ObjectGeom| {
                if aggregate_selection {
                    present(object)
                } else {
                    shown(object)
                }
            };
            if selected.0.len() > 1 || !selected.2.tab_controls.is_empty() {
                let mut bounds = Rect::NOTHING;
                let mut unlocked = false;
                for &index in selected.2.tab_controls.values() {
                    let Some((object, frame)) = geometry
                        .objects
                        .get(index)
                        .and_then(|object| Some((object, shown(object)?)))
                    else {
                        continue;
                    };
                    let rect = object_rect(origin, &frame);
                    if rect.is_finite() {
                        draw_selection_outline(&painter, rect, object.locked);
                        bounds = bounds.union(rect);
                        unlocked |= !object.locked;
                    }
                }
                if !selected.2.tab_controls.is_empty() {
                    for rect in tabs.selected_labels {
                        if rect.is_positive() {
                            painter.rect_stroke(
                                rect,
                                CornerRadius::ZERO,
                                Stroke::new(1.0, theme::TEXT),
                                StrokeKind::Inside,
                            );
                        }
                    }
                }
                for &index in &selected.0 {
                    let Some(object) = geometry.objects.get(index) else {
                        if let Some(field) = new
                            .fields
                            .get(index - geometry.objects.len())
                            .filter(|_| aggregate_selection)
                        {
                            let rect = object_rect(origin, &geom_rect(field.frame()));
                            if rect.is_finite() {
                                bounds = bounds.union(rect);
                                unlocked = true;
                            }
                        }
                        continue;
                    };
                    let Some(frame) = selection_frame(object) else {
                        continue;
                    };
                    let rect = object_rect(origin, &frame);
                    if !rect.is_finite() {
                        continue;
                    }
                    draw_selection_outline(&painter, rect, object.locked);
                    if aggregate_selection {
                        // Inactive children have outlines but do not extend
                        // the handles around the objects on screen.
                        if visible(object.key) {
                            bounds = bounds.union(rect);
                            unlocked |= !object.locked;
                        }
                    } else if !object.locked {
                        draw_selection_handles(&painter, rect);
                    }
                }
                if unlocked && bounds.is_finite() {
                    draw_selection_handles(&painter, bounds);
                }
            }
            if let Some((object, frame)) = selected
                .single()
                .filter(|_| selected.2.tab_controls.is_empty())
                .and_then(|index| geometry.objects.get(index))
                .and_then(|object| Some((object, selection_frame(object)?)))
            {
                let rect = object_rect(origin, &frame);
                draw_selection_outline(&painter, rect, object.locked);
                if !object.locked && rect.is_finite() {
                    draw_selection_handles(&painter, rect);
                }
                // A movable object's handles resize it.
                if selection_tool
                    && visible(object.key)
                    && object.movable
                    && object.key != 0
                    && rect.is_finite()
                {
                    for (name, position, edges) in handles(rect) {
                        let handle = ui.interact(
                            Rect::from_center_size(position, vec2(9.0, 9.0)),
                            ui.scope_id().with(("layout_handle", object.key, name)),
                            Sense::drag(),
                        );
                        handle.widget_info(|| {
                            WidgetInfo::labeled(Role::Button, true, format!("Resize {name}"))
                        });
                        if handle.drag_started() && drag.is_none() {
                            let from = press.or(handle.interact_pointer_pos()).map(at);
                            if let Some(from) = from {
                                *drag = Some(Drag {
                                    key: object.key,
                                    edges,
                                    from,
                                    start: frame,
                                });
                            }
                        }
                    }
                }
                if *scroll_to_selected && rect.is_finite() {
                    ui.scroll_to_rect(rect.expand(12.0), Some(egui::Align::Center));
                }
            }
            if *scroll_to_selected {
                if let Some(field) = selected
                    .single()
                    .and_then(|index| index.checked_sub(geometry.objects.len()))
                    .and_then(|index| new.fields.get(index))
                {
                    let rect = Rect::from_min_size(
                        origin + vec2(field.left as f32, field.top as f32),
                        vec2(field.width as f32, field.height as f32),
                    );
                    if rect.is_finite() {
                        ui.scroll_to_rect(rect.expand(12.0), Some(egui::Align::Center));
                    }
                }
            }
            *scroll_to_selected = false;
            frame.hold(ui);
        });
    scroll_state.finish(frame, &output);
    dropped
}

/// A frame in layout points as `om_layout` holds one.
fn layout_rect(frame: GeomRect) -> om_layout::Rect {
    om_layout::Rect {
        top: frame.top,
        left: frame.left,
        bottom: frame.bottom,
        right: frame.right,
        ..om_layout::Rect::default()
    }
}

fn geom_rect(frame: om_layout::Rect) -> GeomRect {
    GeomRect {
        top: frame.top,
        left: frame.left,
        bottom: frame.bottom,
        right: frame.right,
    }
}

fn shown_frame(
    object: &ObjectGeom,
    moved: &MovedObjects,
    deleted: &DeletedObjects,
) -> Option<GeomRect> {
    if object.key == 0 {
        return Some(object.rect);
    }
    (!deleted.keys.contains(&object.key)).then(|| {
        moved
            .frames
            .get(&object.key)
            .copied()
            .unwrap_or(object.rect)
    })
}

/// The same stored objects and field drafts that the canvas lets a user
/// select individually, in the frames currently shown.
fn selectable_frames<'a>(
    geometry: &'a LayoutGeometry,
    fields: &'a [NewField],
    shown: impl Fn(&ObjectGeom) -> Option<GeomRect> + 'a,
) -> impl Iterator<Item = (usize, Rect)> + 'a {
    let drawn = |frame| object_rect(Pos2::ZERO, &frame);
    geometry
        .objects
        .iter()
        .enumerate()
        .filter_map(move |(index, object)| Some((index, drawn(shown(object)?))))
        .chain(fields.iter().enumerate().map(move |(index, field)| {
            (
                geometry.objects.len() + index,
                drawn(geom_rect(field.frame())),
            )
        }))
        .filter(|(_, frame)| frame.is_finite())
}

fn marquee_frames<'a>(
    geometry: &'a LayoutGeometry,
    fields: &[NewField],
    present: impl Fn(&ObjectGeom) -> Option<GeomRect>,
    visible: impl Fn(u64) -> bool,
) -> Vec<MarqueeFrame<'a>> {
    geometry
        .objects
        .iter()
        .enumerate()
        .filter_map(|(index, object)| {
            Some(MarqueeFrame {
                index,
                object: Some(object),
                rect: present(object)?,
                visible: visible(object.key),
            })
        })
        .chain(
            fields
                .iter()
                .enumerate()
                .map(|(index, field)| MarqueeFrame {
                    index: geometry.objects.len() + index,
                    object: None,
                    rect: geom_rect(field.frame()),
                    visible: true,
                }),
        )
        .filter_map(|mut entry| {
            let shown = object_rect(Pos2::ZERO, &entry.rect);
            entry.rect = geometry_frame(shown);
            shown.is_finite().then_some(entry)
        })
        .collect()
}

fn geometry_frame(rect: Rect) -> GeomRect {
    GeomRect {
        left: f64::from(rect.left()),
        top: f64::from(rect.top()),
        right: f64::from(rect.right()),
        bottom: f64::from(rect.bottom()),
    }
}

fn begin_selection_drag(
    selected: &Selection,
    geometry: &LayoutGeometry,
    fields: &[NewField],
    moved: &MovedObjects,
    deleted: &DeletedObjects,
    from: Pos2,
) -> Option<SelectionDrag> {
    let frames = movable_frames(selected, geometry, fields, moved, deleted)?;
    Some(SelectionDrag { from, frames })
}

fn movable_frames(
    selected: &Selection,
    geometry: &LayoutGeometry,
    fields: &[NewField],
    moved: &MovedObjects,
    deleted: &DeletedObjects,
) -> Option<Vec<(usize, GeomRect)>> {
    let frames = selected
        .0
        .iter()
        .map(|index| {
            let frame = if let Some(object) = geometry.objects.get(*index) {
                if !object.movable || object.key == 0 || geometry.revision.is_none() {
                    return None;
                }
                shown_frame(object, moved, deleted)?
            } else {
                let field = fields.get(index.checked_sub(geometry.objects.len())?)?;
                geom_rect(field.frame())
            };
            [frame.left, frame.top, frame.right, frame.bottom]
                .iter()
                .all(|v| v.is_finite())
                .then_some((*index, frame))
        })
        .collect::<Option<Vec<_>>>()?;
    (!frames.is_empty()).then_some(frames)
}

fn drag_selection(
    active: &SelectionDrag,
    pointer: Pos2,
    geometry: &LayoutGeometry,
    fields: &mut [NewField],
    moved: &mut MovedObjects,
) {
    let delta = pointer - active.from;
    let requested = layout_move::Delta {
        dx: f64::from(delta.x),
        dy: f64::from(delta.y),
    };
    if let Ok(plan) = layout_move::plan(active.frames.iter().map(|(_, frame)| *frame), requested) {
        stage_selection_frames(
            active
                .frames
                .iter()
                .map(|(index, _)| *index)
                .zip(plan.frames),
            geometry,
            fields,
            moved,
        );
    }
}

fn stage_selection_frames(
    frames: impl Iterator<Item = (usize, GeomRect)>,
    geometry: &LayoutGeometry,
    fields: &mut [NewField],
    moved: &mut MovedObjects,
) {
    // If the first member goes back to its stored frame but another moves,
    // the batch stays on the original revision of the pending frames.
    let revision = if moved.frames.is_empty() {
        geometry.revision
    } else {
        moved.revision
    };
    for (index, frame) in frames {
        if let Some(object) = geometry.objects.get(index) {
            if frame == object.rect {
                moved.frames.remove(&object.key);
            } else {
                moved.frames.insert(object.key, frame);
            }
        } else if let Some(field) = index
            .checked_sub(geometry.objects.len())
            .and_then(|i| fields.get_mut(i))
        {
            field.left = frame.left;
            field.top = frame.top;
        }
    }
    moved.revision = if moved.frames.is_empty() {
        None
    } else {
        revision
    };
}

/// Resize the handle's edges with a moved top or left at a whole point and
/// at least one point of width and height (a line keeps its empty side).
fn dragged(drag: Drag, delta: egui::Vec2) -> GeomRect {
    let (dx, dy) = (f64::from(delta.x).round(), f64::from(delta.y).round());
    let start = drag.start;
    let (width, height) = (start.width(), start.height());
    let mut frame = start;
    let wide = width > 0.0;
    let high = height > 0.0;
    if drag.edges.left && wide {
        // At most a point short of the right edge, never past the layout's
        // left: a frame under a point wide keeps its left at 0.
        let most = (start.right - 1.0).floor().max(0.0);
        frame.left = (start.left + dx).round().clamp(0.0, most);
    }
    if drag.edges.right && wide {
        frame.right = (start.right + dx).max(frame.left + 1.0);
    }
    if drag.edges.top && high {
        let most = (start.bottom - 1.0).floor().max(0.0);
        frame.top = (start.top + dy).round().clamp(0.0, most);
    }
    if drag.edges.bottom && high {
        frame.bottom = (start.bottom + dy).max(frame.top + 1.0);
    }
    frame
}

/// A selected object's eight handles: their names, where they are and the
/// edges each moves.
fn handles(rect: Rect) -> [(&'static str, Pos2, Edges); 8] {
    let e = |left, top, right, bottom| Edges {
        left,
        top,
        right,
        bottom,
    };
    [
        ("top left", rect.left_top(), e(true, true, false, false)),
        ("top", rect.center_top(), e(false, true, false, false)),
        ("top right", rect.right_top(), e(false, true, true, false)),
        ("right", rect.right_center(), e(false, false, true, false)),
        (
            "bottom right",
            rect.right_bottom(),
            e(false, false, true, true),
        ),
        ("bottom", rect.center_bottom(), e(false, false, false, true)),
        (
            "bottom left",
            rect.left_bottom(),
            e(true, false, false, true),
        ),
        ("left", rect.left_center(), e(true, false, false, false)),
    ]
}

fn finite_extent(value: f64) -> f32 {
    if value.is_finite() && value > 0.0 {
        (value as f32).min(1_000_000.0)
    } else {
        0.0
    }
}

fn draw_part(ui: &mut Ui, origin: Pos2, width: f32, part: &PartGeom) {
    let top = origin.y + part.top as f32;
    let bottom = top + part.height as f32;
    if !top.is_finite() || !bottom.is_finite() || bottom <= top {
        return;
    }
    let painter = ui.painter();
    painter.hline(
        origin.x..=origin.x + width,
        bottom - 0.5,
        Stroke::new(1.0, theme::POPUP_BORDER),
    );
    let tab = Rect::from_min_max(
        pos2(origin.x - PART_STRIP_WIDTH + 2.0, top + 2.0),
        pos2(origin.x - 2.0, bottom - 2.0),
    );
    if !tab.is_positive() {
        return;
    }
    painter.rect_stroke(
        tab,
        CornerRadius::same(2),
        Stroke::new(1.0, theme::BUTTON_BORDER),
        StrokeKind::Inside,
    );
    let galley = painter.layout_no_wrap(part.kind_name.clone(), theme::small(), theme::CAPTION);
    let text_pos = pos2(
        tab.center().x - galley.size().y / 2.0,
        tab.center().y + galley.size().x / 2.0,
    );
    painter
        .with_clip_rect(tab.intersect(ui.clip_rect()))
        .add(TextShape::new(text_pos, galley, theme::CAPTION).with_angle(-FRAC_PI_2));
    // A painted rotated caption still has an accessibility representation.
    let response = ui.interact(
        tab,
        ui.scope_id()
            .with(("layout_part", part.kind_name.as_str(), top.to_bits())),
        Sense::hover(),
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &part.kind_name));
}

/// The Field tool's new field, before Save Layout: a dashed frame on top of
/// the layout's objects, captioned with the chosen field.
fn draw_new_field(
    ui: &Ui,
    painter: &egui::Painter,
    origin: Pos2,
    field: &NewField,
    index: usize,
    selectable: bool,
) -> egui::Response {
    let rect = Rect::from_min_size(
        origin + vec2(field.left as f32, field.top as f32),
        vec2(field.width as f32, field.height as f32),
    );
    if !rect.is_finite() || !rect.is_positive() {
        return ui.interact(
            Rect::NOTHING,
            ui.scope_id().with(("new_field", index)),
            Sense::hover(),
        );
    }
    painter.rect_filled(rect, CornerRadius::ZERO, theme::CURRENT_ROW);
    draw_dashed(painter, rect);
    let name = field.field.as_ref().map(|choice| choice.name.as_str());
    painter
        .with_clip_rect(rect.shrink(2.0).intersect(ui.clip_rect()))
        .text(
            rect.left_top() + vec2(5.0, 4.0),
            Align2::LEFT_TOP,
            name.unwrap_or("Field"),
            theme::small(),
            theme::SELECTED,
        );
    let label = match name {
        Some(name) => format!("New field {name}"),
        None => "New field".to_owned(),
    };
    let response = ui.interact(
        rect,
        ui.scope_id().with(("new_field", index)),
        if selectable {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
    // Its label, once a field is chosen and Create label is on.
    if let (true, Some(name), Some(frame)) = (field.label, name, field.label_frame()) {
        let rect = object_rect(
            origin,
            &GeomRect {
                left: frame.left,
                top: frame.top,
                right: frame.right,
                bottom: frame.bottom,
            },
        );
        if rect.is_finite() && rect.is_positive() {
            draw_dashed(painter, rect);
            painter
                .with_clip_rect(rect.shrink(1.0).intersect(ui.clip_rect()))
                .text(
                    rect.left_top() + vec2(5.0, 4.0),
                    Align2::LEFT_TOP,
                    name,
                    theme::small(),
                    theme::SELECTED,
                );
            ui.interact(
                rect,
                ui.scope_id().with(("new_field_label", index)),
                Sense::hover(),
            )
            .widget_info(|| WidgetInfo::labeled(Role::Label, true, format!("New label {name}")));
        }
    }
    response
}

/// A dashed frame around `rect`, as a new object's.
fn draw_dashed(painter: &egui::Painter, rect: Rect) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
        rect.left_top(),
    ];
    painter.extend(egui::Shape::dashed_line(
        &corners,
        Stroke::new(1.0, theme::SELECTED),
        4.0,
        3.0,
    ));
}

/// A stored text being retyped in `rect`: Return or a click elsewhere keeps
/// the words as an unsaved change (the stored words, or none, are none),
/// Escape drops them.
fn retype(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    words: &mut WordChanges,
    retyping: &mut Option<Retyping>,
) {
    let Some(editing) = retyping.as_mut() else {
        return;
    };
    painter.rect_filled(rect, CornerRadius::ZERO, theme::CURRENT_ROW);
    draw_dashed(painter, rect);
    // At the caption's inset, so the words stay where they were drawn.
    let id = ui.scope_id().with(("retype", editing.key));
    if editing.focus {
        egui::TextEdit::store_state(ui.ctx(), id, egui::text_edit::TextEditState::default());
    }
    let edit = ui.put(
        rect.shrink2(vec2(5.0, 1.0)),
        egui::TextEdit::singleline(&mut editing.text)
            .id(id)
            .frame(egui::Frame::NONE)
            .char_limit(om_data::layout_placement::MAX_TEXT_CHARS)
            .font(theme::small()),
    );
    crate::widgets::accessible_label(ui, edit.id, "Text being typed");
    if std::mem::take(&mut editing.focus) {
        edit.request_focus();
    }
    if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        *retyping = None;
        return;
    }
    if !edit.lost_focus() {
        return;
    }
    if let Some(done) = retyping.take() {
        keep_words(words, done);
    }
}

/// Keep `done`'s words as an unsaved change, as typed: blank words, or the
/// stored ones, are none.
fn keep_words(words: &mut WordChanges, done: Retyping) {
    if done.text.trim().is_empty() || done.text == done.stored {
        words.objects.remove(&done.key);
    } else {
        if words.objects.is_empty() {
            words.revision = done.revision;
        }
        words.objects.insert(done.key, done.text);
    }
    if words.objects.is_empty() {
        words.revision = None;
    }
}

/// The new text: a dashed frame with its text typed in place (`editable`:
/// the canvas takes input).
fn draw_new_text(
    ui: &mut Ui,
    painter: &egui::Painter,
    origin: Pos2,
    text: &mut NewText,
    editable: bool,
) -> Option<egui::Id> {
    let rect = Rect::from_min_size(
        origin + vec2(text.left as f32, text.top as f32),
        vec2(text.width as f32, text.height as f32),
    );
    if !rect.is_finite() || !rect.is_positive() {
        return None;
    }
    painter.rect_filled(rect, CornerRadius::ZERO, theme::CURRENT_ROW);
    draw_dashed(painter, rect);
    let id = ui.scope_id().with("new_text");
    if text.focus {
        egui::TextEdit::store_state(ui.ctx(), id, egui::text_edit::TextEditState::default());
    }
    let edit = ui.put(
        rect.shrink2(vec2(3.0, 1.0)),
        egui::TextEdit::singleline(&mut text.text)
            .id(id)
            .frame(egui::Frame::NONE)
            .interactive(editable)
            .char_limit(om_data::layout_placement::MAX_LABEL_CHARS)
            .font(theme::small())
            .hint_text("Text"),
    );
    crate::widgets::accessible_label(ui, edit.id, "New text");
    if std::mem::take(&mut text.focus) && editable {
        edit.request_focus();
    }
    Some(edit.id)
}

/// The new button: a rounded frame with its label, dashed as the new
/// field is. A double click opens Button Setup (`setup`: the canvas takes
/// clicks). Returns whether it was double clicked.
fn draw_new_button(
    ui: &Ui,
    painter: &egui::Painter,
    origin: Pos2,
    button: &NewButton,
    setup: bool,
) -> bool {
    let rect = Rect::from_min_size(
        origin + vec2(button.left as f32, button.top as f32),
        vec2(button.width as f32, button.height as f32),
    );
    if !rect.is_finite() || !rect.is_positive() {
        return false;
    }
    let radius = CornerRadius::same(4);
    painter.rect_filled(rect, radius, theme::CURRENT_ROW);
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, theme::SELECTED),
        StrokeKind::Inside,
    );
    let shown = if button.label.is_empty() {
        "Button"
    } else {
        button.label.as_str()
    };
    painter
        .with_clip_rect(rect.shrink(2.0).intersect(ui.clip_rect()))
        .text(
            rect.center(),
            Align2::CENTER_CENTER,
            shown,
            theme::small(),
            theme::SELECTED,
        );
    let label = if button.label.is_empty() {
        "New button".to_owned()
    } else {
        format!("New button {}", button.label)
    };
    let response = ui.interact(
        rect,
        ui.scope_id().with("new_button"),
        if setup {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, setup, &label));
    setup && response.double_clicked()
}

/// The Portal tool's new portal, before Save Layout: a dashed frame on top
/// of the layout's objects, its rows divided, its fields in its first row
/// as operation 21 places them, captioned with their names, and its
/// occurrence's name below the first row. A double click opens Portal
/// Setup again.
fn draw_new_portal(
    ui: &Ui,
    painter: &egui::Painter,
    origin: Pos2,
    portal: &NewPortal,
    setup: bool,
) -> bool {
    let rect = Rect::from_min_size(
        origin + vec2(portal.left as f32, portal.top as f32),
        vec2(portal.width as f32, portal.height as f32),
    );
    if !rect.is_finite() || !rect.is_positive() {
        return false;
    }
    painter.rect_filled(rect, CornerRadius::ZERO, theme::CURRENT_ROW);
    draw_dashed(painter, rect);
    let clip = painter.with_clip_rect(rect.intersect(ui.clip_rect()));
    if let Some(choice) = &portal.setup {
        let rows = choice.rows.max(1);
        let row = rect.height() / rows as f32;
        for index in 1..rows {
            let y = rect.top() + row * index as f32;
            clip.hline(rect.x_range(), y, Stroke::new(1.0, theme::RING));
        }
        for (name, field) in portal.field_frames() {
            let field = Rect::from_min_max(
                rect.min + vec2(field.left as f32, field.top as f32),
                rect.min + vec2(field.right as f32, field.bottom as f32),
            );
            draw_dashed(&clip, field);
            clip.with_clip_rect(field.shrink(2.0).intersect(ui.clip_rect()))
                .text(
                    field.left_top() + vec2(5.0, 4.0),
                    Align2::LEFT_TOP,
                    format!("::{name}"),
                    theme::small(),
                    theme::SELECTED,
                );
        }
        clip.text(
            pos2(rect.left() + 5.0, rect.top() + row + 4.0),
            Align2::LEFT_TOP,
            choice.name.as_str(),
            theme::small(),
            theme::SELECTED,
        );
    }
    let label = match &portal.setup {
        Some(choice) => format!("New portal {}", choice.name),
        None => "New portal".to_owned(),
    };
    let response = ui.interact(
        rect,
        ui.scope_id().with("new_portal"),
        if setup {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, setup, &label));
    setup && response.double_clicked()
}

/// A new web viewer: its frame, dashed, with its web address once Web
/// Viewer Setup has set one. A double click opens Web Viewer Setup again.
fn draw_new_web_viewer(
    ui: &Ui,
    painter: &egui::Painter,
    origin: Pos2,
    viewer: &NewWebViewer,
    setup: bool,
) -> bool {
    let rect = Rect::from_min_size(
        origin + vec2(viewer.left as f32, viewer.top as f32),
        vec2(viewer.width as f32, viewer.height as f32),
    );
    if !rect.is_finite() || !rect.is_positive() {
        return false;
    }
    painter.rect_filled(rect, CornerRadius::ZERO, theme::CURRENT_ROW);
    draw_dashed(painter, rect);
    let address = viewer.setup.as_ref().map(|choice| choice.address.as_str());
    painter
        .with_clip_rect(rect.shrink(2.0).intersect(ui.clip_rect()))
        .text(
            rect.left_top() + vec2(5.0, 4.0),
            Align2::LEFT_TOP,
            address.unwrap_or("Web Viewer"),
            theme::small(),
            theme::SELECTED,
        );
    let label = match address {
        Some(address) => format!("New web viewer {address}"),
        None => "New web viewer".to_owned(),
    };
    let response = ui.interact(
        rect,
        ui.scope_id().with("new_web_viewer"),
        if setup {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, setup, &label));
    setup && response.double_clicked()
}

fn object_rect(origin: Pos2, rect: &GeomRect) -> Rect {
    Rect::from_two_pos(
        origin + vec2(rect.left as f32, rect.top as f32),
        origin + vec2(rect.right as f32, rect.bottom as f32),
    )
}

fn hit_rect(rect: Rect) -> Rect {
    if rect.width() < 4.0 || rect.height() < 4.0 {
        rect.expand(3.0)
    } else {
        rect
    }
}

/// The front-most object at `position`, where `shown` shows it (moved, or
/// not at all once deleted).
fn hit_test(
    geometry: &LayoutGeometry,
    stack: Option<&[usize]>,
    shown: impl Fn(&ObjectGeom) -> Option<GeomRect>,
    position: Pos2,
) -> Option<usize> {
    stacking_order(geometry.objects.len(), stack)
        .rev()
        .find_map(|index| {
            let rect = object_rect(Pos2::ZERO, &shown(&geometry.objects[index])?);
            (rect.is_finite() && hit_rect(rect).contains(position)).then_some(index)
        })
}

/// Keep geometry indices stable when a popover is painted above the layout.
fn stacking_order(
    len: usize,
    raised: Option<&[usize]>,
) -> impl DoubleEndedIterator<Item = usize> + '_ {
    (0..len).map(move |index| raised.map_or(index, |stack| stack[index]))
}

fn object_label(index: usize, object: &ObjectGeom) -> String {
    match &object.field_name {
        Some(name) => format!("{}: {name} (object {})", object.kind_name, index + 1),
        None => format!("{} (object {})", object.kind_name, index + 1),
    }
}

fn draw_selection_outline(painter: &egui::Painter, rect: Rect, locked: bool) {
    if !rect.is_finite() {
        return;
    }
    painter.rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(
            1.0,
            if locked {
                theme::SELECTED.gamma_multiply(0.5)
            } else {
                theme::SELECTED
            },
        ),
        StrokeKind::Middle,
    );
}

fn draw_selection_handles(painter: &egui::Painter, rect: Rect) {
    for position in [
        rect.left_top(),
        rect.center_top(),
        rect.right_top(),
        rect.right_center(),
        rect.right_bottom(),
        rect.center_bottom(),
        rect.left_bottom(),
        rect.left_center(),
    ] {
        painter.rect(
            Rect::from_center_size(position, vec2(5.0, 5.0)),
            CornerRadius::ZERO,
            theme::CONTROL_BG,
            Stroke::new(1.0, theme::SELECTED),
            StrokeKind::Inside,
        );
    }
}

fn fallback_geometry(session: &Session) -> LayoutGeometry {
    let fields = session
        .page
        .as_ref()
        .map(|page| page.data.fields.as_slice())
        .unwrap_or_default();
    let height = (fields.len() as f64 * 34.0 + 40.0).max(160.0);
    let objects = fields
        .iter()
        .enumerate()
        .map(|(index, field)| ObjectGeom {
            kind_name: "Field".into(),
            rect: GeomRect {
                left: 166.0,
                top: 20.0 + index as f64 * 34.0,
                right: 520.0,
                bottom: 42.0 + index as f64 * 34.0,
            },
            field_name: Some(field.name.clone()),
            ..Default::default()
        })
        .collect();
    LayoutGeometry {
        revision: None,
        width: 560.0,
        height,
        parts: vec![PartGeom {
            identity: None,
            kind_name: "Body".into(),
            top: 0.0,
            height,
        }],
        objects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drags_snap_origins_and_never_invert_a_frame() {
        let drag = |start: GeomRect, edges: Edges| Drag {
            key: 1,
            edges,
            from: Pos2::ZERO,
            start,
        };
        let r = |left, top, right, bottom| GeomRect {
            top,
            left,
            bottom,
            right,
        };
        // A frame under a point wide: its left handle dragged either way.
        let left = Edges {
            left: true,
            top: false,
            right: false,
            bottom: false,
        };
        for dx in [-5.0, 5.0] {
            let f = dragged(drag(r(0.0, 0.0, 0.5, 10.0), left), vec2(dx, 0.0));
            assert!(f.left >= 0.0 && f.left <= f.right, "{f:?}");
        }
        let mut geometry = LayoutGeometry::default();
        let mut stored = object([0.5, 10.5, 50.5, 30.5]);
        stored.key = 7;
        geometry.objects.push(stored);
        let mut fields = vec![NewField {
            left: 10.0,
            top: 40.0,
            width: 30.0,
            height: 20.0,
            field: None,
            label: false,
            control: 0,
            value_list: None,
            revision: None,
        }];
        let active = SelectionDrag {
            from: Pos2::ZERO,
            frames: vec![
                (0, geometry.objects[0].rect),
                (1, r(10.0, 40.0, 40.0, 60.0)),
            ],
        };
        let mut moved = MovedObjects::default();
        for (pointer, expected, draft_origin) in [
            (pos2(10.0, 10.0), r(11.0, 21.0, 61.0, 41.0), (20.0, 50.0)),
            (pos2(-20.0, -40.0), r(0.0, 0.0, 50.0, 20.0), (9.0, 29.0)),
        ] {
            drag_selection(&active, pointer, &geometry, &mut fields, &mut moved);
            assert_eq!(moved.frames[&7], expected);
            assert_eq!((fields[0].left, fields[0].top), draft_origin);
        }
    }
    use crate::session::Target;

    fn object(rect: [f64; 4]) -> ObjectGeom {
        ObjectGeom {
            kind_name: "Field".into(),
            rect: GeomRect {
                left: rect[0],
                top: rect[1],
                right: rect[2],
                bottom: rect[3],
            },
            field_name: None,
            ..Default::default()
        }
    }

    /// Arrange is one change of the draft: a frame derived from a pending
    /// move made on an older revision keeps that revision, so Save Layout
    /// conflicts if the layout changed since, even when the pending move
    /// itself is undone first (it is first in canvas order).
    #[test]
    fn arrange_keeps_the_revision_of_a_pending_move() {
        let (older, shown) = ([0; 32], [1; 32]);
        let at = |left: f64| GeomRect {
            left,
            top: 0.0,
            right: left + 10.0,
            bottom: 10.0,
        };
        let movable = |key, left| ObjectGeom {
            key,
            movable: true,
            ..object([left, 0.0, left + 10.0, 10.0])
        };
        let mut session = Session::new(
            crate::session::Target {
                host: "test".into(),
                file: "test.fmp12".into(),
                local: None,
            },
            25,
        );
        session.layout = Some(1);
        session.geometry = Geometry::Loaded(LayoutGeometry {
            objects: vec![movable(1, 100.0), movable(2, 120.0)],
            revision: Some(shown),
            ..LayoutGeometry::default()
        });
        let mut state = CanvasState::default();
        state.select_object(&session, 0, false);
        state.select_object(&session, 1, true);
        // A's move to 80, made before the layout was read again with A at
        // 100.
        state.set_frame(1, at(80.0), at(100.0), Some(older));
        // Centers of 80..90 and 120..130 meet at 105: A goes back to its
        // stored 100, and B to 100, a frame that comes from A's draft.
        use om_data::layout_arrange::Arrangement;
        assert!(state
            .arrange(&session, Arrangement::AlignVerticalCenters)
            .unwrap());
        assert_eq!(state.moved().frames.get(&1), None);
        assert_eq!(state.moved().frames.get(&2), Some(&at(100.0)));
        assert_eq!(state.moved().revision, Some(older));
    }

    /// Save Layout's next step goes on the revision the host reported only
    /// for the changes made on the revision the step before was sent at: a
    /// deletion made before the layout was read again keeps its own, for
    /// the host to refuse if the layout changed meanwhile.
    #[test]
    fn a_saved_step_moves_on_only_the_changes_made_on_its_revision() {
        let (before, shown, saved) = ([0; 32], [1; 32], [2; 32]);
        let mut state = CanvasState::default();
        state.delete_object(7, Some(before));
        let at = |left| GeomRect {
            left,
            top: 10.0,
            right: left + 20.0,
            bottom: 30.0,
        };
        state.set_frame(3, at(40.0), at(10.0), Some(shown));
        state.new_fields = vec![NewField {
            left: 0.0,
            top: 0.0,
            width: 10.0,
            height: 10.0,
            field: None,
            label: true,
            control: 0,
            value_list: None,
            revision: Some(shown),
        }];
        state.rebase(shown, saved);
        assert_eq!(state.deleted().revision, Some(before));
        assert_eq!(state.moved().revision, Some(saved));
        assert_eq!(state.new_field().unwrap().revision, Some(saved));
    }

    /// Unsaved text and control changes to an object another client
    /// deleted are kept when the layout is read again without it, so
    /// closing waits for Save or Revert Layout, and the window is told; an
    /// editor opened on such an object and left unchanged is no change
    /// (#1048's re-checks).
    #[test]
    fn changes_to_objects_deleted_elsewhere_are_kept_and_reported() {
        let mut session = Session::new(
            Target {
                host: "test".into(),
                file: "test.fmp12".into(),
                local: None,
            },
            25,
        );
        session.layout = Some(1);
        let with = |keys: &[u64]| {
            Geometry::Loaded(LayoutGeometry {
                objects: keys
                    .iter()
                    .map(|&key| ObjectGeom {
                        key,
                        ..object([0.0, 0.0, 10.0, 10.0])
                    })
                    .collect(),
                ..LayoutGeometry::default()
            })
        };
        session.geometry = with(&[7, 8, 9]);
        let mut state = CanvasState::default();
        state.synchronize(&session);
        state.words.objects.insert(7, "Retyped".into());
        state.controls.objects.insert(8, (2, None));
        // An editor open on object 9 with its words untouched.
        state.retyping = Some(Retyping {
            key: 9,
            text: "Name".into(),
            stored: "Name".into(),
            revision: None,
            focus: false,
        });
        session.geometry = with(&[9]);
        state.synchronize(&session);
        assert_eq!(
            state.words().objects.get(&7).map(String::as_str),
            Some("Retyped")
        );
        assert!(state.controls().objects.contains_key(&8));
        assert!(state.take_orphaned_drafts());
        assert!(!state.take_orphaned_drafts());
        // The untouched editor's object goes: no change, nothing reported.
        let mut untouched = CanvasState::default();
        session.geometry = with(&[9]);
        untouched.synchronize(&session);
        untouched.retyping = Some(Retyping {
            key: 9,
            text: "Name".into(),
            stored: "Name".into(),
            revision: None,
            focus: false,
        });
        session.geometry = with(&[]);
        untouched.synchronize(&session);
        assert!(untouched.retyping.is_none() && untouched.words().objects.is_empty());
        assert!(!untouched.take_orphaned_drafts());
    }

    #[test]
    fn overlapping_objects_select_the_topmost_and_lines_have_a_hit_target() {
        let geometry = LayoutGeometry {
            objects: vec![
                object([0.0, 0.0, 100.0, 100.0]),
                object([10.0, 10.0, 30.0, 30.0]),
                object([50.0, 0.0, 50.0, 30.0]),
            ],
            ..LayoutGeometry::default()
        };
        let stored = |o: &ObjectGeom| Some(o.rect);
        assert_eq!(hit_test(&geometry, None, stored, pos2(20.0, 20.0)), Some(1));
        assert_eq!(hit_test(&geometry, None, stored, pos2(52.0, 10.0)), Some(2));
        assert_eq!(hit_test(&geometry, None, stored, pos2(200.0, 200.0)), None);
        // A raised surface wins over a layout object stored after it.
        assert_eq!(
            hit_test(&geometry, Some(&[1, 2, 0]), stored, pos2(20.0, 20.0)),
            Some(0)
        );
        // A deleted object is not hit: the one under it is.
        let without_second = |o: &ObjectGeom| (o.rect.left != 10.0).then_some(o.rect);
        assert_eq!(
            hit_test(&geometry, None, without_second, pos2(20.0, 20.0)),
            Some(0)
        );
    }

    #[test]
    fn a_drag_on_a_raised_popover_does_not_move_the_covered_object() {
        use egui_kittest::kittest::Queryable;
        use om_layout::{Layout, LayoutObject, ObjectStore};

        let mut session = Session::new(
            Target {
                host: "test".into(),
                file: "test.fmp12".into(),
                local: None,
            },
            25,
        );
        session.layout = Some(1);
        session.geometry = Geometry::Loaded(LayoutGeometry {
            width: 250.0,
            height: 140.0,
            revision: Some([1; 32]),
            objects: vec![
                ObjectGeom {
                    key: 1,
                    kind_name: "Popover".into(),
                    ..object([20.0, 20.0, 130.0, 80.0])
                },
                ObjectGeom {
                    key: 2,
                    kind_name: "Rectangle".into(),
                    movable: true,
                    ..object([20.0, 20.0, 130.0, 80.0])
                },
            ],
            ..Default::default()
        });
        session
            .accept_render_layout(
                1,
                Ok(Some(om_data::RenderLayout {
                    layout: Layout {
                        id: 1,
                        objects: ObjectStore {
                            objects: vec![LayoutObject {
                                id: 1,
                                object_type: 15,
                                ..Default::default()
                            }],
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    ..Default::default()
                })),
            )
            .unwrap();
        let mut canvas = CanvasState::default();
        canvas.select_object(&session, 0, false);
        let mut app = egui_kittest::Harness::builder()
            .with_size(vec2(350.0, 200.0))
            .with_context_setup(crate::theme::install)
            .build_ui_state(
                |ui, (state, document)| {
                    show(
                        ui,
                        &session,
                        LayoutTool::Selection,
                        &[],
                        state,
                        false,
                        document,
                    );
                },
                (canvas, DocumentState::default()),
            );
        let start = app.get_by_label("Popover (object 1)").rect().center();
        app.event(egui::Event::PointerMoved(start));
        app.event(egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        app.step();
        app.event(egui::Event::PointerMoved(start + vec2(15.0, 0.0)));
        app.step();
        assert_eq!(app.state().0.selected_index(), Some(0));
        assert!(app.state().0.selection_drag.is_none());
    }

    #[test]
    fn a_nudge_blocked_at_the_layout_edge_keeps_fractional_frames() {
        let mut session = Session::new(
            Target {
                host: "test".into(),
                file: "test.fmp12".into(),
                local: None,
            },
            25,
        );
        session.layout = Some(1);
        session.geometry = Geometry::Loaded(LayoutGeometry {
            revision: Some([1; 32]),
            objects: vec![ObjectGeom {
                key: 1,
                movable: true,
                ..object([0.0, 16.5, 0.0, 36.5])
            }],
            ..Default::default()
        });
        let mut state = CanvasState::default();
        state.select_all(&session, SelectAll::Layout);
        state.nudge_selection(&session, layout_move::Delta { dx: -1.0, dy: 0.0 });
        assert!(state.moved().frames.is_empty());
    }

    #[test]
    fn box_selection_uses_displayed_frames_and_does_not_move_objects() {
        let mut session = Session::new(
            Target {
                host: "test".into(),
                file: "test.fmp12".into(),
                local: None,
            },
            25,
        );
        session.layout = Some(1);
        session.geometry = Geometry::Loaded(LayoutGeometry {
            width: 180.0,
            height: 100.0,
            revision: Some([1; 32]),
            objects: vec![
                ObjectGeom {
                    key: 1,
                    movable: true,
                    ..object([110.0, 25.0, 140.0, 45.0])
                },
                ObjectGeom {
                    key: 2,
                    // Stored endpoints can be reversed. Marquee uses the
                    // frame the canvas draws, including for containment.
                    ..object([90.0, 25.0, 70.0, 45.0])
                },
                ObjectGeom {
                    key: 3,
                    ..object([30.0, 25.0, 40.0, 45.0])
                },
            ],
            ..Default::default()
        });
        let mut state = CanvasState::default();
        state.synchronize(&session);
        state.set_frame(
            1,
            object([50.0, 25.0, 60.0, 45.0]).rect,
            object([110.0, 25.0, 140.0, 45.0]).rect,
            Some([1; 32]),
        );
        state.delete_object(3, Some([1; 32]));
        let moves = state.moved().clone();
        let capture = std::env::var_os("OM_LAYOUT_BOX_ARTIFACT_DIR").map(std::path::PathBuf::from);
        let mut builder = egui_kittest::Harness::builder().with_size(vec2(300.0, 200.0));
        if capture.is_some() {
            builder = builder.renderer(crate::test_renderer::renderer());
        }
        let mut app = builder.build_ui_state(
            |ui, (state, document)| {
                show(
                    ui,
                    &session,
                    LayoutTool::Selection,
                    &[],
                    state,
                    false,
                    document,
                )
            },
            (state, DocumentState::default()),
        );
        let capture_frame = |app: &mut egui_kittest::Harness<'_, _>, name| {
            if let Some(dir) = &capture {
                std::fs::create_dir_all(dir).unwrap();
                app.set_render_cursor(false);
                app.render().unwrap().save(dir.join(name)).unwrap();
            }
        };
        capture_frame(&mut app, "before.png");
        for (modifiers, expected) in [
            (egui::Modifiers::NONE, vec![0, 1]),
            (egui::Modifiers::COMMAND, vec![0]),
        ] {
            let start = pos2(PART_STRIP_WIDTH + 20.0, 15.0);
            let end = pos2(PART_STRIP_WIDTH + 80.0, 55.0);
            app.event(egui::Event::ModifiersChanged(modifiers));
            app.event(egui::Event::PointerMoved(start));
            app.step();
            app.event(egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            });
            app.step();
            app.event(egui::Event::PointerMoved(end));
            app.step();
            assert!(
                app.state().0.box_from.is_some(),
                "drag began on blank layout"
            );
            if modifiers.is_none() {
                capture_frame(&mut app, "dragging.png");
            }
            app.input_mut().events.extend([
                egui::Event::PointerButton {
                    pos: end,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers,
                },
                egui::Event::ModifiersChanged(egui::Modifiers::NONE),
            ]);
            app.step();
            assert_eq!(
                app.state().0.selected.0.iter().copied().collect::<Vec<_>>(),
                expected
            );
            assert_eq!(app.state().0.moved(), &moves);
        }
        capture_frame(&mut app, "selected.png");
    }

    #[test]
    fn dragging_an_out_of_order_part_boundary_targets_the_drawn_part() {
        use egui_kittest::kittest::Queryable;
        use om_data::PartIdentity;

        let mut session = Session::new(
            Target {
                host: "test".into(),
                file: "test.fmp12".into(),
                local: None,
            },
            25,
        );
        session.layout = Some(1);
        let revision = [4; 32];
        // Storage order is Body then Header. The Header ends at y=40.
        let stored = [(3, 4, "Body", 40.0, 90.0), (9, 1, "Header", 0.0, 40.0)];
        session.geometry = Geometry::Loaded(LayoutGeometry {
            width: 250.0,
            height: 130.0,
            parts: stored
                .iter()
                .map(|&(key, part_type, name, top, height)| PartGeom {
                    identity: Some(PartIdentity { key, part_type }),
                    kind_name: name.into(),
                    top,
                    height,
                })
                .collect(),
            revision: Some(revision),
            ..Default::default()
        });
        session
            .accept_render_layout(
                1,
                Ok(Some(om_data::RenderLayout {
                    layout: om_layout::Layout {
                        id: 1,
                        parts: stored
                            .iter()
                            .map(|&(id, part_type, _, top, height)| om_layout::Part {
                                id,
                                part_type,
                                top,
                                height,
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    },
                    ..Default::default()
                })),
            )
            .unwrap();
        let mut app = egui_kittest::Harness::builder()
            .with_size(vec2(350.0, 200.0))
            .with_context_setup(crate::theme::install)
            .build_ui_state(
                |ui, (state, document, session)| {
                    state.synchronize(session);
                    show(
                        ui,
                        session,
                        LayoutTool::Selection,
                        &[],
                        state,
                        false,
                        document,
                    );
                },
                (CanvasState::default(), DocumentState::default(), session),
            );
        let start = app.get_by_label("Header boundary").rect().center();
        let end = start + vec2(0.0, 15.0);
        let body_start = app.get_by_label("Body boundary").rect().center();
        let press = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        app.event(egui::Event::PointerMoved(start));
        app.event(press(start, true));
        app.step();
        app.event(egui::Event::PointerMoved(end));
        app.step();
        // Release the Header and grab the Body in one input frame. The
        // Body draws first and must not replace the completed Header drag.
        app.event(press(end, false));
        app.event(egui::Event::PointerMoved(body_start));
        app.event(press(body_start, true));
        app.step();
        assert_eq!(
            app.state_mut().0.take_part_resize(),
            Some(PartResize {
                part: 9,
                part_type: 1,
                size: 55.0,
                revision: Some(revision),
            }),
        );

        let body_end = body_start + vec2(0.0, 10.0);
        app.event(egui::Event::PointerMoved(body_end));
        app.step();
        app.event(press(body_end, false));
        app.step();
        assert_eq!(
            app.state_mut().0.take_part_resize(),
            Some(PartResize {
                part: 3,
                part_type: 4,
                size: 100.0,
                revision: Some(revision),
            }),
        );

        // If the host refreshes geometry while the pointer is held, egui
        // keeps its grab but releasing it must not submit a new resize.
        app.event(egui::Event::PointerMoved(start));
        app.event(press(start, true));
        app.step();
        app.event(egui::Event::PointerMoved(end));
        app.step();
        assert!(app.state().0.part_drag.is_some());
        let Geometry::Loaded(geometry) = &mut app.state_mut().2.geometry else {
            unreachable!();
        };
        geometry.parts.reverse();
        geometry.revision = Some([5; 32]);
        app.step();
        let after_refresh = end + vec2(0.0, 10.0);
        app.event(egui::Event::PointerMoved(after_refresh));
        app.step();
        app.event(press(after_refresh, false));
        app.step();
        assert_eq!(app.state_mut().0.take_part_resize(), None);
    }

    #[test]
    fn only_the_front_panel_takes_canvas_input_until_objects_reveals_another() {
        use egui_kittest::kittest::Queryable;
        use om_layout::{Layout, LayoutObject, ObjectStore, TabObject};

        let node = |id, object_type, parent, children| LayoutObject {
            id,
            object_type,
            parent,
            children,
            ..Default::default()
        };
        let store = ObjectStore {
            root: vec![1],
            objects: vec![
                LayoutObject {
                    tab: Some(TabObject {
                        default_panel_key: 4,
                        current_panel_key: 2,
                        ..Default::default()
                    }),
                    ..node(1, 11, None, vec![2, 4])
                },
                node(2, 12, Some(1), vec![3]),
                node(3, 1, Some(2), vec![]),
                // Notes contains a two-panel slide control. Its second
                // slide holds the other field at the very same canvas point.
                node(4, 12, Some(1), vec![6]),
                node(6, 16, Some(4), vec![7, 8]),
                node(7, 17, Some(6), vec![]),
                node(8, 17, Some(6), vec![5]),
                node(5, 1, Some(8), vec![]),
            ],
            ..Default::default()
        };
        let mut session = Session::new(
            Target {
                host: "test".into(),
                file: "test.fmp12".into(),
                local: None,
            },
            25,
        );
        session.layout = Some(1);
        session.geometry = Geometry::Loaded(LayoutGeometry {
            width: 250.0,
            height: 140.0,
            objects: [(3, "Summary field"), (5, "Notes field")]
                .into_iter()
                .map(|(key, caption)| ObjectGeom {
                    key,
                    field_name: Some(caption.into()),
                    ..object([25.0, 40.0, 145.0, 68.0])
                })
                .collect(),
            ..Default::default()
        });
        session
            .accept_render_layout(
                1,
                Ok(Some(om_data::RenderLayout {
                    layout: Layout {
                        id: 1,
                        objects: store,
                        ..Default::default()
                    },
                    ..Default::default()
                })),
            )
            .unwrap();

        let mut app = egui_kittest::Harness::builder()
            .with_size(vec2(350.0, 200.0))
            .with_context_setup(crate::theme::install)
            .build_ui_state(
                |ui, (state, document)| {
                    show(
                        ui,
                        &session,
                        LayoutTool::Selection,
                        &[],
                        state,
                        false,
                        document,
                    );
                },
                (CanvasState::default(), DocumentState::default()),
            );
        let label = |index, field: &str| {
            object_label(
                index,
                &ObjectGeom {
                    field_name: Some(field.into()),
                    ..object([0.0; 4])
                },
            )
        };
        let first = label(0, "Summary field");
        let second = label(1, "Notes field");
        app.get_by_label(&first).click();
        app.run_steps(2);
        assert_eq!(app.state().0.selected_index(), Some(0));
        assert!(app.query_by_label(&second).is_none());

        app.state_mut().0.select_object(&session, 1, false);
        app.run_steps(2);
        assert!(app.query_by_label(&first).is_none());
        assert!(app.query_by_label(&second).is_some());
        app.state_mut().0.select_all(&session, SelectAll::Layout);
        assert_eq!(app.state().0.selected.0, BTreeSet::from([0, 1]));
        assert_eq!(app.state().0.front_panels.get(&1), Some(&4));
        assert!(!app.state().0.unsaved());

        // Changing layouts in Browse does not run the Layout canvas between
        // the two selections. The old panel still must not follow us back.
        let mut state = std::mem::take(&mut app.state_mut().0);
        drop(app);
        let Geometry::Loaded(geometry) = &session.geometry else {
            unreachable!()
        };
        let crate::session::RenderState::Loaded(render) = &session.render else {
            unreachable!()
        };
        let (geometry, render) = (geometry.clone(), (**render).clone());
        session.select_layout(2);
        session.select_layout(1);
        session.accept_geometry(1, Ok(Some(geometry))).unwrap();
        session.accept_render_layout(1, Ok(Some(render))).unwrap();
        assert!(state.selected_object(&session).is_none());
        state.synchronize(&session);
        assert!(state.selected_index().is_none());
        state.select_all(&session, SelectAll::Layout);
        assert_eq!(state.selected.0, BTreeSet::from([0, 1]));
        assert!(state.front_panels.is_empty());
    }

    #[test]
    fn selection_is_cleared_when_the_layout_or_geometry_changes() {
        let mut session = Session::new(
            Target {
                host: "test".into(),
                file: "test.fmp12".into(),
                local: None,
            },
            25,
        );
        session.layout = Some(1);
        session.geometry = Geometry::Loaded(LayoutGeometry {
            objects: vec![object([0.0, 0.0, 10.0, 10.0])],
            ..LayoutGeometry::default()
        });
        let mut state = CanvasState::default();
        state.select_object(&session, 0, false);
        assert!(state.scroll_to_selected);
        // Synchronizing on the first drawing pass must preserve a sidebar
        // selection made before that pass.
        state.synchronize(&session);
        assert!(state.selected_object(&session).is_some());
        state.stage_field(
            &session,
            NewField {
                left: 20.0,
                top: 20.0,
                width: 30.0,
                height: 20.0,
                field: None,
                label: false,
                control: 0,
                value_list: None,
                revision: None,
            },
        );
        state.select_object(&session, 0, true);
        assert_eq!(state.selection_count(&session), 2);
        assert!(!state.can_delete_selection(&session));
        assert!(!state.delete_selection(&session));
        assert_eq!(
            state.fields().len(),
            1,
            "an unsupported member must retain its draft peer"
        );
        assert!(begin_selection_drag(
            &state.selected,
            state.geometry.as_ref().unwrap(),
            state.fields(),
            state.moved(),
            state.deleted(),
            Pos2::ZERO
        )
        .is_none());
        state.nudge_selection(&session, layout_move::Delta { dx: 1.0, dy: 0.0 });
        assert_eq!(state.selection_notice(), Some(CANNOT_MOVE_SELECTION));
        assert!(state.moved().frames.is_empty());
        assert_eq!(state.fields()[0].left, 20.0);
        state.select_object(&session, 0, false);
        if let Geometry::Loaded(geometry) = &mut session.geometry {
            geometry.objects[0].rect.left = 1.0;
        }
        assert!(state.selected_object(&session).is_none());
        state.synchronize(&session);
        assert_eq!(state.selected_index(), None);
        state.select_object(&session, 5, false);
        assert_eq!(state.selected_index(), None);
        assert!(!state.scroll_to_selected);
        state.selected.set(Some(0));
        session.layout = Some(2);
        state.synchronize(&session);
        assert_eq!(state.selected_index(), None);
    }
}
